//! Dump and restore of a database in the formats of mongodump:
//!
//! - a folder: per collection, `<name>.bson` (the documents one after the
//!   other) and `<name>.metadata.json` (options, indexes, uuid), optionally
//!   gzipped (`.bson.gz`); views only have the metadata;
//! - an archive (`mongodump --archive`): one file, optionally gzipped as a
//!   whole, holding a prelude with the metadata of the collections, then
//!   blocks of documents, each after a header naming its collection.
//!
//! mongorestore reads both, and the restore reads those of mongodump, whose
//! archives interleave the blocks of several collections. A time series is
//! dumped as its buckets, like mongodump does.
//!
//! Like the import and export, documents go by batches: nothing holds a
//! whole collection. Like the import, a restore reads the whole dump before
//! writing anything, and "drop first" replaces a collection only once its
//! copy is complete.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, anyhow, bail};
use bson::{Bson, Document, doc};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::mongo::{Connection, collect_cursor, error_code, error_message, is_unreadable_reply};
use crate::transfer::{BATCH, Batcher, Cancelled, Imported, Progress, ReadAhead, Replace, Stranded, Target, check_cancel, docs, for_each_batch, ignore_code, insert_with};

#[derive(Debug, Clone, PartialEq)]
pub struct DumpOptions {
    pub db: String,
    /// Empty: all the collections and views.
    pub collections: Vec<String>,
    /// The folder or the archive goes in it.
    pub dir: PathBuf,
    pub gzip: bool,
    /// One file, like `mongodump --archive`, instead of a folder.
    pub archive: bool,
}

impl DumpOptions {
    /// Where the dump goes: `<dir>/<db>`, or `<dir>/<db>.archive[.gz]`.
    pub fn target(&self) -> PathBuf {
        let gz = if self.gzip { ".gz" } else { "" };
        if self.archive { self.dir.join(format!("{}.archive{gz}", escape(&self.db))) } else { self.dir.join(escape(&self.db)) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RestoreOptions {
    /// Database to restore into.
    pub db: String,
    /// Folder of the `.bson` and `.metadata.json` files, or archive.
    pub from: PathBuf,
    /// Empty: all the collections of the dump.
    pub collections: Vec<String>,
    pub drop_first: bool,
    pub indexes: bool,
}

/// What a dump wrote.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dumped {
    pub collections: u64,
    pub documents: u64,
}

impl Dumped {
    pub fn summary(&self, target: &Path) -> String {
        format!("{} collection{} ({}) dumped to {}", self.collections, plural(self.collections), docs(self.documents), target.display())
    }
}

/// What a restore did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Restored {
    pub collections: u64,
    pub docs: Imported,
    /// The collections not restored, or not completely, and why.
    pub problems: Vec<String>,
}

impl Restored {
    pub fn summary(&self) -> String {
        let mut s = format!("{} collection{} restored: {}", self.collections, plural(self.collections), self.docs.summary());
        if !self.problems.is_empty() {
            s.push_str(&format!(". Not restored: {}", self.problems.join("; ")));
        }
        s
    }

    /// Whether something was not restored.
    pub fn incomplete(&self) -> bool {
        !self.problems.is_empty() || self.docs.failed > 0
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Characters kept in file names, those of Go's `url.QueryEscape` (mongodump).
const KEPT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');

/// File name of a collection, like mongodump: `/`, `$`… are percent-encoded, spaces become `+`.
pub fn escape(name: &str) -> String {
    utf8_percent_encode(name, KEPT).to_string().replace("%20", "+")
}

pub fn unescape(file: &str) -> String {
    percent_decode_str(&file.replace('+', " ")).decode_utf8_lossy().into_owned()
}

/// The name of the collection of the files named `file`. mongodump 100
/// writes a space as `+` and `+` as `%2B`; the versions before it kept `+`
/// and wrote a space `%20`. When the two readings differ, the name in the
/// metadata tells, or else the `prelude.json` that only mongodump 100 writes.
fn collection_name(file: &str, metadata: Option<&Path>, prelude: bool) -> anyhow::Result<String> {
    let spaces = unescape(file);
    let plus = percent_decode_str(file).decode_utf8_lossy().into_owned();
    if spaces == plus {
        return Ok(spaces);
    }
    let named = match metadata {
        Some(p) => read_metadata(p)?.get_str("collectionName").ok().map(str::to_string),
        None => None,
    };
    Ok(match named {
        Some(n) if n == spaces || n == plus => n,
        _ if prelude => spaces,
        _ => plus,
    })
}

/// The collections dumped: system collections are left out, but for stored functions.
fn dumped(name: &str) -> bool {
    !name.starts_with("system.") || name == "system.js"
}

fn is_view(meta: &Document) -> bool {
    meta.get_str("type").ok() == Some("view") || meta.get_document("options").is_ok_and(|o| o.contains_key("viewOn"))
}

/// The time field of a time series collection.
fn time_field(meta: &Document) -> Option<String> {
    meta.get_document("options").ok()?.get_document("timeseries").ok()?.get_str("timeField").ok().map(str::to_string)
}

/// Whether a document of a time series is a bucket, as mongodump writes
/// them, rather than a measurement.
fn is_bucket(d: &Document, time_field: &str) -> bool {
    !d.contains_key(time_field) && d.get_document("control").is_ok_and(|c| c.contains_key("version")) && d.contains_key("data")
}

/// First bytes of an archive (0x8199e26d, little-endian).
const MAGIC: [u8; 4] = [0x6d, 0xe2, 0x99, 0x81];
/// Ends the prelude and each block of an archive.
const TERMINATOR: [u8; 4] = [0xff; 4];

/// A collection to dump: name and metadata.
struct Collected {
    name: String,
    meta: Document,
}

/// The metadata of the collections of the database, as mongodump writes it.
async fn collected(conn: &Connection, o: &DumpOptions) -> anyhow::Result<Vec<Collected>> {
    let infos = collect_cursor(conn, &o.db, doc! { "listCollections": 1, "authorizedCollections": true, "cursor": {} }, 100_000, None).await?;
    let mut list = Vec::new();
    for info in infos {
        let Ok(name) = info.get_str("name") else { continue };
        if !dumped(name) || (!o.collections.is_empty() && !o.collections.iter().any(|c| c == name)) {
            continue;
        }
        let kind = info.get_str("type").unwrap_or("collection");
        let indexes = if kind == "view" {
            Vec::new()
        } else {
            collect_cursor(conn, &o.db, doc! { "listIndexes": name, "cursor": {} }, 10_000, None).await?
        };
        let mut meta = doc! {
            "options": info.get_document("options").cloned().unwrap_or_default(),
            "indexes": indexes,
        };
        if let Some(Bson::Binary(uuid)) = info.get_document("info").ok().and_then(|i| i.get("uuid")) {
            meta.insert("uuid", hex::encode(&uuid.bytes));
        }
        meta.insert("collectionName", name);
        meta.insert("type", kind);
        list.push(Collected { name: name.to_string(), meta });
    }
    Ok(list)
}

/// Where the documents of a collection are read, and the name they are
/// dumped under. A time series goes like mongodump writes it: its buckets,
/// named after it by servers that take rawData (MongoDB 8.2+), else
/// `system.buckets.<name>`; or its measurements, when the user may not read
/// its buckets, which the restore reads too.
async fn data_of(conn: &Connection, db: &str, c: &Collected) -> (String, String) {
    if time_field(&c.meta).is_none() {
        return (c.name.clone(), c.name.clone());
    }
    let readable = async |coll: &str, raw_data: bool| {
        let mut cmd = doc! { "find": coll, "limit": 1, "singleBatch": true };
        if raw_data {
            cmd.insert("rawData", true);
        }
        conn.run_command(db, cmd, None).await.is_ok()
    };
    let buckets = format!("system.buckets.{}", c.name);
    if !readable(&buckets, false).await {
        return (c.name.clone(), c.name.clone());
    }
    let named = if readable(&c.name, true).await { c.name.clone() } else { buckets.clone() };
    (buckets, named)
}

/// Writes the collections of a database. `progress` gets the number of
/// documents written so far.
pub async fn dump(conn: &Connection, o: &DumpOptions, cancel: &AtomicBool, progress: impl Fn(u64)) -> anyhow::Result<Dumped> {
    let list = collected(conn, o).await?;
    let mut done = Dumped::default();
    // The documents of a collection, read from `coll`, with what they are written to.
    let mut documents = async |c: &Collected, coll: &str, w: &mut Out, crc: &mut Crc64| -> anyhow::Result<()> {
        let before = done.documents;
        let cmd = doc! { "find": coll, "filter": {}, "batchSize": BATCH as i64 };
        for_each_batch(conn, &o.db, coll, cmd, cancel, done.documents, |batch| {
            for d in &batch {
                let raw = bson::to_vec(d)?;
                crc.update(&raw);
                w.write_all(&raw)?;
            }
            done.documents += batch.len() as u64;
            progress(done.documents);
            Ok(())
        })
        .await
        .with_context(|| format!("{}: {} written", c.name, docs(done.documents - before)))
    };
    let target = o.target();
    if o.archive {
        let mut w = writer(&target, o.gzip)?;
        w.write_all(&MAGIC)?;
        let version = conn.run_command("admin", doc! { "buildInfo": 1 }, None).await.ok().and_then(|b| b.get_str("version").ok().map(str::to_string));
        let header = doc! {
            "concurrent_collections": 1,
            "version": "0.1",
            "server_version": version.unwrap_or_default(),
            "tool_version": format!("rustyrobo {}", env!("CARGO_PKG_VERSION")),
        };
        header.to_writer(&mut w)?;
        for c in &list {
            let meta = metadata_json(&c.meta)?;
            doc! { "db": &o.db, "collection": &c.name, "metadata": meta, "size": 0, "type": c.meta.get_str("type").unwrap_or("collection") }.to_writer(&mut w)?;
        }
        w.write_all(&TERMINATOR)?;
        for c in &list {
            let (from, named) = data_of(conn, &o.db, c).await;
            let block = |eof: bool, crc: u64| doc! { "db": &o.db, "collection": &named, "EOF": eof, "CRC": crc as i64 };
            block(false, 0).to_writer(&mut w)?;
            let mut crc = Crc64::default();
            if !is_view(&c.meta) {
                documents(c, &from, &mut w, &mut crc).await?;
            }
            w.write_all(&TERMINATOR)?;
            block(true, crc.0).to_writer(&mut w)?;
            w.write_all(&TERMINATOR)?;
            done.collections += 1;
        }
        w.finish()?;
    } else {
        std::fs::create_dir_all(&target).with_context(|| format!("cannot create {}", target.display()))?;
        for c in &list {
            let file = |name: &str, ext: &str| target.join(format!("{}.{ext}{}", escape(name), if o.gzip { ".gz" } else { "" }));
            let mut w = writer(&file(&c.name, "metadata.json"), o.gzip)?;
            w.write_all(metadata_json(&c.meta)?.as_bytes())?;
            w.finish()?;
            if !is_view(&c.meta) {
                let (from, named) = data_of(conn, &o.db, c).await;
                let mut w = writer(&file(&named, "bson"), o.gzip)?;
                documents(c, &from, &mut w, &mut Crc64::default()).await?;
                w.finish()?;
            }
            done.collections += 1;
        }
    }
    Ok(done)
}

/// Metadata as mongodump writes it: canonical Extended JSON.
fn metadata_json(meta: &Document) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&Bson::Document(meta.clone()).into_canonical_extjson())?)
}

/// CRC-64 with the ECMA polynomial, the checksum of the blocks of an archive.
#[derive(Default, Clone)]
struct Crc64(u64);

const CRC_TABLE: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u64;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xC96C_5795_D787_0F42 } else { crc >> 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

impl Crc64 {
    fn update(&mut self, data: &[u8]) {
        let mut crc = !self.0;
        for &b in data {
            crc = CRC_TABLE[((crc as u8) ^ b) as usize] ^ (crc >> 8);
        }
        self.0 = !crc;
    }
}

enum Out {
    Plain(BufWriter<std::fs::File>),
    Gzip(Box<flate2::write::GzEncoder<BufWriter<std::fs::File>>>),
}

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Out::Plain(w) => w.write(buf),
            Out::Gzip(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Out::Plain(w) => w.flush(),
            Out::Gzip(w) => w.flush(),
        }
    }
}

impl Out {
    /// Writes the end of the file, which dropping it would do without errors.
    fn finish(self) -> std::io::Result<()> {
        let mut f = match self {
            Out::Plain(w) => w,
            Out::Gzip(w) => w.finish()?,
        };
        f.flush()
    }
}

fn writer(path: &Path, gzip: bool) -> anyhow::Result<Out> {
    let f = BufWriter::new(std::fs::File::create(path).with_context(|| format!("cannot create {}", path.display()))?);
    Ok(if gzip { Out::Gzip(Box::new(flate2::write::GzEncoder::new(f, flate2::Compression::default()))) } else { Out::Plain(f) })
}

/// Reads a file, gunzipped when it starts like a gzip file.
fn reader(path: &Path) -> anyhow::Result<Box<dyn BufRead + Send>> {
    let f = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut r = BufReader::new(f);
    Ok(if r.fill_buf()?.starts_with(&[0x1f, 0x8b]) { Box::new(BufReader::new(flate2::bufread::MultiGzDecoder::new(r))) } else { Box::new(r) })
}

/// A collection found in a dump.
#[derive(Debug, Clone, PartialEq)]
pub struct DumpEntry {
    pub name: String,
    pub view: bool,
}

/// The files of a collection in a dump folder.
struct FolderEntry {
    /// Name of the files, without their extensions.
    file: String,
    name: String,
    data: Option<PathBuf>,
    metadata: Option<PathBuf>,
}

/// The collections of a dump, folder or archive, sorted by name. An archive
/// of several databases is read for the one named `db`.
pub fn scan(path: &Path, db: &str) -> anyhow::Result<Vec<DumpEntry>> {
    let mut found: Vec<DumpEntry> = if path.is_file() {
        let (_, list) = archive_prelude(path, db)?;
        list.into_iter().map(|(name, meta)| DumpEntry { view: is_view(&meta), name }).collect()
    } else {
        scan_folder(path)?.into_iter().map(|e| DumpEntry { view: e.data.is_none() && e.metadata.is_some(), name: e.name }).collect()
    };
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

fn scan_folder(dir: &Path) -> anyhow::Result<Vec<FolderEntry>> {
    let mut found: Vec<FolderEntry> = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = e?.path();
        let Some(file) = path.file_name().and_then(|f| f.to_str()) else { continue };
        let file = file.strip_suffix(".gz").unwrap_or(file);
        let (file, data) = if let Some(n) = file.strip_suffix(".metadata.json") {
            (n, false)
        } else if let Some(n) = file.strip_suffix(".bson") {
            (n, true)
        } else {
            continue;
        };
        let entry = match found.iter_mut().position(|f| f.file == file) {
            Some(i) => &mut found[i],
            None => {
                found.push(FolderEntry { file: file.to_string(), name: String::new(), data: None, metadata: None });
                found.last_mut().unwrap()
            }
        };
        let slot = if data { &mut entry.data } else { &mut entry.metadata };
        // users.bson and users.bson.gz: which one is the dump?
        if let Some(other) = slot {
            bail!("two files for the same collection: {} and {}", other.display(), path.display());
        }
        *slot = Some(path);
    }
    let prelude = dir.join("prelude.json").is_file() || dir.join("prelude.json.gz").is_file();
    for f in &mut found {
        f.name = collection_name(&f.file, f.metadata.as_deref(), prelude)?;
    }
    // The buckets of a time series, from a server without rawData (before
    // MongoDB 8.2): system.buckets.<name>.bson next to <name>.metadata.json.
    let mut i = 0;
    while i < found.len() {
        let of = found[i].name.strip_prefix("system.buckets.").and_then(|ts| found.iter().position(|f| f.name == ts));
        let Some(j) = of else {
            i += 1;
            continue;
        };
        let buckets = found.remove(i);
        let ts = &mut found[if j > i { j - 1 } else { j }];
        if let (Some(a), Some(b)) = (&ts.data, &buckets.data) {
            bail!("two files for the documents of {}: {} and {}", ts.name, a.display(), b.display());
        }
        if buckets.data.is_some() {
            ts.data = buckets.data;
        }
    }
    found.retain(|f| dumped(&f.name));
    Ok(found)
}

/// Options, indexes and type of a collection, from its metadata file.
pub fn read_metadata(path: &Path) -> anyhow::Result<Document> {
    let mut text = String::new();
    reader(path)?.read_to_string(&mut text).with_context(|| format!("{}", path.display()))?;
    parse_metadata(&text).with_context(|| format!("{}", path.display()))
}

fn parse_metadata(text: &str) -> anyhow::Result<Document> {
    let v: serde_json::Value = serde_json::from_str(text).context("invalid JSON")?;
    match Bson::try_from(v).map_err(|e| anyhow::anyhow!("{e}"))? {
        Bson::Document(d) => Ok(d),
        _ => bail!("expected a document"),
    }
}

/// Reads one BSON document, or `None` at the end of the file. A terminator
/// of an archive is `Some(None)`.
fn next_raw(r: &mut impl Read, n: u64) -> anyhow::Result<Option<Option<Vec<u8>>>> {
    let mut len = [0u8; 4];
    // The end of the file only before a document: part of a size, or a
    // gzip stream cut short, is a damaged file.
    loop {
        match r.read(&mut len[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e).with_context(|| format!("document {n}: truncated or damaged file")),
        }
    }
    r.read_exact(&mut len[1..]).with_context(|| format!("document {n}: truncated file"))?;
    if len == TERMINATOR {
        return Ok(Some(None));
    }
    let size = i32::from_le_bytes(len);
    if !(5..=48 * 1024 * 1024).contains(&size) {
        bail!("document {n}: invalid size {size}");
    }
    let mut raw = vec![0u8; size as usize];
    raw[..4].copy_from_slice(&len);
    r.read_exact(&mut raw[4..]).with_context(|| format!("document {n}: truncated file"))?;
    Ok(Some(Some(raw)))
}

fn parse_raw(raw: &[u8], n: u64) -> anyhow::Result<Document> {
    Document::from_reader(raw).with_context(|| format!("document {n}"))
}

/// Reads the documents of a `.bson` file, in batches given to `send` until it returns false.
pub fn read_bson(mut r: impl Read, mut send: impl FnMut(Vec<Document>) -> bool) -> anyhow::Result<()> {
    let mut batch = Batcher::default();
    let mut n = 0;
    while let Some(raw) = next_raw(&mut r, n + 1)? {
        n += 1;
        let Some(raw) = raw else { bail!("document {n}: invalid size -1") };
        if let Some(full) = batch.push(parse_raw(&raw, n)?, raw.len())
            && !send(full)
        {
            return Ok(());
        }
    }
    if !batch.is_empty() {
        send(batch.take());
    }
    Ok(())
}

/// The prelude of an archive: its collections and their metadata, from the
/// database `db`, or the only one of the archive.
pub fn archive_prelude(path: &Path, db: &str) -> anyhow::Result<(String, Vec<(String, Document)>)> {
    read_prelude(&mut reader(path)?, db)
}

fn read_prelude(r: &mut impl Read, db: &str) -> anyhow::Result<(String, Vec<(String, Document)>)> {
    let mut magic = [0u8; 4];
    if r.read_exact(&mut magic).is_err() || magic != MAGIC {
        bail!("not a mongodump archive");
    }
    let mut all: Vec<(String, String, Document)> = Vec::new();
    let mut n = 0;
    // The header, then the metadata of the collections.
    loop {
        n += 1;
        let raw = match next_raw(r, n)? {
            None => bail!("truncated archive"),
            Some(None) => break,
            Some(Some(raw)) => raw,
        };
        if n == 1 {
            continue;
        }
        let d = parse_raw(&raw, n)?;
        let (Ok(d_db), Ok(coll)) = (d.get_str("db"), d.get_str("collection")) else { continue };
        let meta = parse_metadata(d.get_str("metadata").unwrap_or("{}")).with_context(|| format!("metadata of {coll}"))?;
        all.push((d_db.to_string(), coll.to_string(), meta));
    }
    let mut dbs: Vec<&str> = all.iter().map(|(d, _, _)| d.as_str()).collect();
    dbs.sort_unstable();
    dbs.dedup();
    let source = match dbs.as_slice() {
        [] => return Ok((db.to_string(), Vec::new())),
        [one] => one.to_string(),
        _ if dbs.contains(&db) => db.to_string(),
        _ => bail!("the archive holds several databases ({}): restore into one of them", dbs.join(", ")),
    };
    let list = all.into_iter().filter(|(d, c, _)| *d == source && dumped(c)).map(|(_, c, m)| (c, m)).collect();
    Ok((source, list))
}

/// What the reader of an archive gives to the restore.
enum ArchiveEvent {
    Docs(String, Vec<Document>),
    /// All the documents of the collection were given.
    End(String),
}

/// Reads the blocks of an archive, after its prelude, for the collections
/// `wanted` of `db`. Checks the checksum of each, and that each ends: an
/// archive cut between two blocks is not taken for a complete one.
fn read_archive_blocks(mut r: impl Read, db: &str, wanted: &[String], mut send: impl FnMut(ArchiveEvent) -> bool) -> anyhow::Result<()> {
    let wanted: HashSet<&str> = wanted.iter().map(String::as_str).collect();
    // The collection of the block being read; `None` in a block to skip.
    let mut block: Option<Option<String>> = None;
    let mut pending: HashMap<String, (Batcher, Crc64)> = HashMap::new();
    let mut ended: HashSet<String> = HashSet::new();
    let mut n = 0;
    while let Some(raw) = next_raw(&mut r, n + 1)? {
        n += 1;
        let Some(raw) = raw else {
            block = None;
            continue;
        };
        match &block {
            // A header.
            None => {
                let h = parse_raw(&raw, n)?;
                let coll = h.get_str("collection").unwrap_or_default();
                // The buckets of a time series, from a server without rawData.
                let coll = match coll.strip_prefix("system.buckets.") {
                    Some(ts) if wanted.contains(ts) => ts,
                    _ => coll,
                }
                .to_string();
                let ours = h.get_str("db").ok() == Some(db) && wanted.contains(coll.as_str());
                if !h.get_bool("EOF").unwrap_or(false) {
                    block = Some(ours.then_some(coll));
                    continue;
                }
                block = Some(None);
                if ours {
                    let (mut batch, crc) = pending.remove(&coll).unwrap_or_default();
                    let expected = h.get("CRC").and_then(crate::mongo::bson_as_i64).unwrap_or(0) as u64;
                    if crc.0 != expected {
                        bail!("{coll}: checksum mismatch, the archive is damaged");
                    }
                    if !batch.is_empty() && !send(ArchiveEvent::Docs(coll.clone(), batch.take())) {
                        return Ok(());
                    }
                    ended.insert(coll.clone());
                    if !send(ArchiveEvent::End(coll)) {
                        return Ok(());
                    }
                }
            }
            Some(None) => {}
            Some(Some(coll)) => {
                let (batch, crc) = pending.entry(coll.clone()).or_default();
                crc.update(&raw);
                if let Some(full) = batch.push(parse_raw(&raw, n)?, raw.len())
                    && !send(ArchiveEvent::Docs(coll.clone(), full))
                {
                    return Ok(());
                }
            }
        }
    }
    let mut missing: Vec<&&str> = wanted.iter().filter(|c| !ended.contains(**c)).collect();
    missing.sort();
    if let Some(coll) = missing.first() {
        bail!("{coll}: truncated archive, the documents of the collection do not end");
    }
    Ok(())
}

/// The `create` command of a collection, from its metadata.
pub fn create_command(name: &str, meta: &Document) -> Document {
    let mut cmd = doc! { "create": name };
    if let Ok(options) = meta.get_document("options") {
        for (k, v) in options {
            // Refused by the servers of today, and the default anyway.
            if k != "autoIndexId" {
                cmd.insert(k, v.clone());
            }
        }
    }
    cmd
}

/// The indexes to create, from the metadata: all but `_id`.
pub fn index_specs(meta: &Document) -> Vec<Document> {
    let Ok(list) = meta.get_array("indexes") else { return Vec::new() };
    list.iter()
        .filter_map(|i| i.as_document())
        .filter(|i| i.get_str("name").ok() != Some("_id_") && !i.get_bool("clustered").unwrap_or(false))
        .map(|i| {
            let mut i = i.clone();
            i.remove("ns");
            i
        })
        .collect()
}

/// A collection of the dump to restore.
struct Entry {
    name: String,
    meta: Document,
    /// In a folder: the file of its documents.
    data: Option<PathBuf>,
}

/// The collections of the dump to restore, views last, which may read them,
/// and the database of the archive they come from.
async fn entries(o: &RestoreOptions, archive: bool) -> anyhow::Result<(String, Vec<Entry>)> {
    let (source, mut list) = if archive {
        let (path, db) = (o.from.clone(), o.db.clone());
        let (source, list) = tokio::task::spawn_blocking(move || archive_prelude(&path, &db)).await??;
        (source, list.into_iter().map(|(name, meta)| Entry { name, meta, data: None }).collect::<Vec<_>>())
    } else {
        let mut list = Vec::new();
        for e in scan_folder(&o.from)? {
            let meta = match &e.metadata {
                Some(p) => read_metadata(p)?,
                None => Document::new(),
            };
            list.push(Entry { name: e.name, meta, data: e.data });
        }
        (o.db.clone(), list)
    };
    list.retain(|e| o.collections.is_empty() || o.collections.contains(&e.name));
    if list.is_empty() {
        bail!("no collection to restore in {}", o.from.display());
    }
    list.sort_by_key(|e| is_view(&e.meta));
    Ok((source, list))
}

/// Reads the whole dump, with the checksums of an archive: a damaged one
/// fails before anything is written. Returns the first documents of the
/// time series, tried on a copy before such a collection is dropped.
async fn check(o: &RestoreOptions, source: &str, list: &[Entry], cancel: &Arc<AtomicBool>, progress: &impl Fn(Progress)) -> anyhow::Result<HashMap<String, Vec<Document>>> {
    let tried: HashSet<String> = list.iter().filter(|e| o.drop_first && time_field(&e.meta).is_some()).map(|e| e.name.clone()).collect();
    let mut firsts: HashMap<String, Vec<Document>> = HashMap::new();
    let mut checked = 0;
    let mut keep = |name: &str, batch: Vec<Document>| {
        checked += batch.len() as u64;
        progress(Progress::Checked(checked));
        if tried.contains(name) && !firsts.contains_key(name) {
            firsts.insert(name.to_string(), batch);
        }
    };
    if o.from.is_file() {
        let wanted: Vec<String> = list.iter().filter(|e| !is_view(&e.meta)).map(|e| e.name.clone()).collect();
        let (path, source) = (o.from.clone(), source.to_string());
        let mut events = ReadAhead::spawn(cancel, move |send| {
            let mut r = reader(&path)?;
            read_prelude(&mut r, &source)?;
            read_archive_blocks(r, &source, &wanted, send)
        });
        while let Some(event) = events.next().await {
            if let ArchiveEvent::Docs(name, batch) = event.with_context(|| format!("{}", o.from.display()))? {
                keep(&name, batch);
            }
        }
    } else {
        for e in list {
            let Some(path) = e.data.clone() else { continue };
            let r = reader(&path)?;
            let mut batches = ReadAhead::spawn(cancel, move |send| read_bson(r, send));
            while let Some(batch) = batches.next().await {
                keep(&e.name, batch.with_context(|| format!("{}", path.display()))?);
            }
        }
    }
    if check_cancel(cancel).is_err() {
        bail!("cancelled: nothing was restored");
    }
    Ok(firsts)
}

/// Restores the collections of a dump, folder or archive: collections
/// first, then the views, which may read them. Write errors (duplicate
/// keys…) are counted, like with mongorestore; a collection the server
/// refuses is reported, and the others are restored.
pub async fn restore(conn: &Connection, o: &RestoreOptions, cancel: Arc<AtomicBool>, progress: impl Fn(Progress)) -> anyhow::Result<Restored> {
    let (source, list) = entries(o, o.from.is_file()).await?;
    let firsts = check(o, &source, &list, &cancel, &progress).await?;
    let mut run = Run { conn, o, inserts: Inserts::default(), done: Restored::default(), restored: Vec::new(), partial: Vec::new() };
    let (mut collections, views): (Vec<&Entry>, Vec<&Entry>) = list.iter().partition(|e| !is_view(&e.meta));
    // What is dropped before it is filled goes last: what stops the restore
    // before it does not touch it.
    collections.sort_by_key(|e| run.dropped_first(e));
    let written = if o.from.is_file() {
        run.archive(&source, &collections, &firsts, &cancel, &progress).await
    } else {
        run.folder(&collections, &firsts, &cancel, &progress).await
    };
    written.map_err(|e| run.stopped(e))?;
    for e in views {
        check_cancel(&cancel).map_err(|e| run.stopped(e))?;
        run.view(e).await.map_err(|e| run.stopped(e))?;
    }
    Ok(run.done)
}

/// A collection being restored.
struct Restoring {
    target: Target,
    meta: Document,
    time_field: Option<String>,
    /// Made by the restore: its documents go in as they were, also those
    /// its validator would refuse.
    created: bool,
    docs: Imported,
    /// Why the server refused the collection; the rest of its documents
    /// are skipped.
    refused: Option<String>,
}

/// A restore under way.
struct Run<'a> {
    conn: &'a Connection,
    o: &'a RestoreOptions,
    inserts: Inserts,
    done: Restored,
    /// The collections restored, for the message of a restore that stops.
    restored: Vec<String>,
    /// The collections a restore that stopped changed without restoring them.
    partial: Vec<String>,
}

impl Run<'_> {
    /// Whether "drop first" drops `e` before its documents go in, rather
    /// than replacing it by a complete copy: a time series or system.js,
    /// which cannot be renamed, or a name too long for a copy next to it.
    fn dropped_first(&self, e: &Entry) -> bool {
        self.o.drop_first && (time_field(&e.meta).is_some() || e.name.starts_with("system.") || !Target::fits(self.conn, &self.o.db, &e.name))
    }

    /// Notes a collection the restore stops in the middle of.
    fn stopped_in(&mut self, r: &Restoring) {
        if r.target.replace != Replace::Swap && (r.target.replace == Replace::Drop || r.docs.inserted > 0) {
            self.partial.push(r.target.name.clone());
        }
    }

    async fn folder(&mut self, collections: &[&Entry], firsts: &HashMap<String, Vec<Document>>, cancel: &Arc<AtomicBool>, progress: &impl Fn(Progress)) -> anyhow::Result<()> {
        for e in collections {
            check_cancel(cancel)?;
            let Some(mut r) = self.open(e, firsts.get(&e.name)).await? else { continue };
            if let Some(path) = &e.data {
                let file = reader(path)?;
                let mut batches = ReadAhead::spawn(cancel, move |send| read_bson(file, send));
                while let Some(batch) = batches.next().await {
                    let step = async {
                        check_cancel(cancel)?;
                        self.insert(&mut r, batch.with_context(|| format!("{}", path.display()))?).await?;
                        progress(Progress::Written(self.done.docs.inserted + r.docs.inserted));
                        anyhow::Ok(())
                    };
                    if let Err(err) = step.await {
                        r.target.discard(self.conn, &self.o.db).await;
                        self.stopped_in(&r);
                        return Err(err);
                    }
                }
            }
            // A cancel also ends the reading early: the collection may not be complete.
            if let Err(err) = check_cancel(cancel) {
                r.target.discard(self.conn, &self.o.db).await;
                self.stopped_in(&r);
                return Err(err);
            }
            self.finish(r).await?;
        }
        Ok(())
    }

    async fn archive(&mut self, source: &str, collections: &[&Entry], firsts: &HashMap<String, Vec<Document>>, cancel: &Arc<AtomicBool>, progress: &impl Fn(Progress)) -> anyhow::Result<()> {
        let mut open: HashMap<String, Restoring> = HashMap::new();
        // Dropped before they are filled: opened with their first block.
        let mut later: HashMap<String, &Entry> = HashMap::new();
        let result = async {
            for e in collections {
                check_cancel(cancel)?;
                if self.dropped_first(e) {
                    later.insert(e.name.clone(), e);
                } else if let Some(r) = self.open(e, firsts.get(&e.name)).await? {
                    open.insert(e.name.clone(), r);
                }
            }
            let wanted: Vec<String> = open.keys().chain(later.keys()).cloned().collect();
            let (path, source) = (self.o.from.clone(), source.to_string());
            let mut events = ReadAhead::spawn(cancel, move |send| {
                let mut r = reader(&path)?;
                read_prelude(&mut r, &source)?;
                read_archive_blocks(r, &source, &wanted, send)
            });
            while let Some(event) = events.next().await {
                check_cancel(cancel)?;
                let event = event.with_context(|| format!("{}", self.o.from.display()))?;
                let (ArchiveEvent::Docs(name, _) | ArchiveEvent::End(name)) = &event;
                if let Some(e) = later.remove(name)
                    && let Some(r) = self.open(e, firsts.get(name)).await?
                {
                    open.insert(name.clone(), r);
                }
                match event {
                    ArchiveEvent::Docs(name, batch) => {
                        if let Some(r) = open.get_mut(&name) {
                            self.insert(r, batch).await?;
                            let written = self.done.docs.inserted + open.values().map(|r| r.docs.inserted).sum::<u64>();
                            progress(Progress::Written(written));
                        }
                    }
                    ArchiveEvent::End(name) => {
                        if let Some(r) = open.remove(&name) {
                            self.finish(r).await?;
                        }
                    }
                }
            }
            // A cancel also ends the reading early.
            check_cancel(cancel)?;
            if let Some(name) = open.keys().chain(later.keys()).next() {
                bail!("{name}: truncated archive, the documents of the collection do not end");
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            for r in open.values() {
                r.target.discard(self.conn, &self.o.db).await;
                self.stopped_in(r);
            }
        }
        result
    }

    /// Makes the collection the documents of `e` go to; `None` when the
    /// server refuses it, which is reported.
    async fn open(&mut self, e: &Entry, first: Option<&Vec<Document>>) -> anyhow::Result<Option<Restoring>> {
        let (conn, db) = (self.conn, self.o.db.as_str());
        let create = |name: &str| create_command(name, &e.meta);
        let time_field = time_field(&e.meta);
        let made = if !self.o.drop_first {
            // Restored into the collection as it is. MongoDB 7.0+ also takes
            // the create of one that exists with the same options.
            let exists = collect_cursor(conn, db, doc! { "listCollections": 1, "filter": { "name": &e.name }, "cursor": {} }, 1, None).await.map(|l| !l.is_empty());
            match exists {
                Ok(true) => Ok((Target::add_to(&e.name), false)),
                Ok(false) => match conn.run_command(db, create(&e.name), None).await {
                    Ok(_) => Ok((Target::add_to(&e.name), true)),
                    Err(err) if error_code(&err) == Some(48) => Ok((Target::add_to(&e.name), false)),
                    Err(err) => Err(err),
                },
                Err(err) => Err(err),
            }
        } else {
            // Dropped before its documents go in, a collection is first made
            // again on the side with the first of them, to see that the
            // server takes them.
            let renamable = !self.dropped_first(e);
            let tried = match first {
                Some(first) if !renamable => self.try_on_copy(e, first).await,
                _ => Ok(()),
            };
            match tried {
                Ok(()) => Target::replacing(conn, db, &e.name, renamable, create).await.map(|t| (t, true)),
                Err(err) => Err(err),
            }
        };
        match made {
            Ok((target, created)) => Ok(Some(Restoring { target, meta: e.meta.clone(), time_field, created, docs: Imported::default(), refused: None })),
            Err(err) if error_code(&err).is_some() => {
                self.done.problems.push(format!("{}: {}, left as it was", e.name, error_message(&err)));
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    /// Makes a copy of the collection with `first` of its documents, then
    /// drops it: fails as the restore of the collection would.
    async fn try_on_copy(&mut self, e: &Entry, first: &[Document]) -> anyhow::Result<()> {
        let (conn, db) = (self.conn, self.o.db.as_str());
        let Some(copy) = Target::swap(conn, db, &e.name, |name| create_command(name, &e.meta)).await? else { return Ok(()) };
        let mut docs = Imported::default();
        let time_field = time_field(&e.meta);
        let inserted = self.inserts.insert(conn, db, &copy.into, time_field.as_deref(), true, first.to_vec(), &mut docs).await;
        copy.discard(conn, db).await;
        inserted?;
        if docs.failed > 0 {
            bail!("{}", docs.refused());
        }
        Ok(())
    }

    /// Inserts documents of `r`, unless the server refused it; only errors
    /// that stop the whole restore are returned.
    async fn insert(&mut self, r: &mut Restoring, batch: Vec<Document>) -> anyhow::Result<()> {
        if r.refused.is_some() {
            return Ok(());
        }
        match self.inserts.insert(self.conn, &self.o.db, &r.target.into, r.time_field.as_deref(), r.created, batch, &mut r.docs).await {
            Err(err) if error_code(&err).is_some() => r.refused = Some(error_message(&err)),
            // What the server did with the batch is not known.
            Err(err) if is_unreadable_reply(&err) => r.refused = Some(format!("the reply of the server could not be read ({})", error_message(&err))),
            other => other?,
        }
        Ok(())
    }

    /// Creates the indexes of a collection whose documents are all in, and
    /// puts it in place of the old one, unless something was refused.
    async fn finish(&mut self, mut r: Restoring) -> anyhow::Result<()> {
        let (conn, db) = (self.conn, self.o.db.as_str());
        let swap = r.target.replace == Replace::Swap;
        let mut why = r.refused.take();
        if why.is_none() && swap && r.docs.failed > 0 {
            why = Some(r.docs.refused());
        }
        let specs = index_specs(&r.meta);
        if why.is_none() && self.o.indexes && !specs.is_empty() {
            match conn.run_command(db, doc! { "createIndexes": &r.target.into, "indexes": specs }, None).await {
                Err(err) if error_code(&err).is_some() => why = Some(format!("indexes: {}", error_message(&err))),
                Err(err) => {
                    r.target.discard(conn, db).await;
                    return Err(err);
                }
                Ok(_) => {}
            }
        }
        if why.is_none() {
            match r.target.commit(conn, db).await {
                // Its documents are in the new collection, which stays.
                Err(err) if err.is::<Stranded>() => {
                    self.done.docs.add(&r.docs);
                    self.done.problems.push(err.to_string());
                    return Ok(());
                }
                Err(err) if error_code(&err).is_some() => why = Some(error_message(&err)),
                Err(err) => {
                    r.target.discard(conn, db).await;
                    return Err(err);
                }
                Ok(()) => {}
            }
        }
        let name = &r.target.name;
        match why {
            None => {
                self.done.collections += 1;
                self.done.docs.add(&r.docs);
                self.restored.push(name.clone());
            }
            Some(why) if swap => {
                r.target.discard(conn, db).await;
                self.done.problems.push(format!("{name}: {why}, left as it was"));
            }
            Some(why) => {
                self.done.docs.add(&r.docs);
                self.done.problems.push(format!("{name}: {why}"));
            }
        }
        Ok(())
    }

    async fn view(&mut self, e: &Entry) -> anyhow::Result<()> {
        let (conn, db) = (self.conn, self.o.db.as_str());
        let options = e.meta.get_document("options").cloned().unwrap_or_default();
        if self.o.drop_first {
            // A view of that name takes the new definition at once; else it
            // is dropped and made again.
            if options.keys().all(|k| k == "viewOn" || k == "pipeline") {
                let mut cmd = doc! { "collMod": &e.name };
                cmd.extend(options.clone());
                match conn.run_command(db, cmd, None).await {
                    Ok(_) => return self.view_done(e),
                    Err(err) if error_code(&err).is_none() => return Err(err),
                    Err(_) => {}
                }
            }
            ignore_code(conn.run_command(db, doc! { "drop": &e.name }, None).await, 26)?;
        }
        match conn.run_command(db, create_command(&e.name, &e.meta), None).await {
            Ok(_) => self.view_done(e),
            Err(err) if error_code(&err) == Some(48) => {
                self.done.problems.push(format!("{}: exists, left as it was", e.name));
                Ok(())
            }
            Err(err) if error_code(&err).is_some() => {
                self.done.problems.push(format!("{}: {}", e.name, error_message(&err)));
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn view_done(&mut self, e: &Entry) -> anyhow::Result<()> {
        self.done.collections += 1;
        self.restored.push(e.name.clone());
        Ok(())
    }

    /// The error that stopped the restore, with what it restored before.
    fn stopped(&self, e: anyhow::Error) -> anyhow::Error {
        let what = if e.is::<Cancelled>() { "cancelled".to_string() } else { error_message(&e) };
        let mut told = Vec::new();
        if !self.restored.is_empty() {
            let mut names = self.restored.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
            if self.restored.len() > 5 {
                names.push_str(", …");
            }
            told.push(format!("{} collection{} restored ({names})", self.restored.len(), plural(self.restored.len() as u64)));
        }
        if !self.partial.is_empty() {
            told.push(format!("restored in part: {}", self.partial.join(", ")));
        }
        if told.is_empty() {
            return anyhow!("{what}: nothing was restored");
        }
        let others = if self.o.drop_first { "; the other collections were left as they were" } else { "" };
        anyhow!("{what}: {}{others}", told.join("; "))
    }
}

/// What the inserts of a restore may ask of the server, found with the
/// first ones.
#[derive(Default)]
struct Inserts {
    /// Whether documents may go in unchecked by the validator of their
    /// collection (dbAdmin or restore role).
    bypass: Option<bool>,
    /// Whether the server takes buckets with `rawData` (MongoDB 8.2+), else
    /// they go to `system.buckets.<name>` (restore role).
    raw_data: Option<bool>,
}

impl Inserts {
    /// Inserts documents into `into`: the buckets of a time series apart,
    /// and as they were into a collection the restore made.
    #[allow(clippy::too_many_arguments)]
    async fn insert(&mut self, conn: &Connection, db: &str, into: &str, time_field: Option<&str>, created: bool, batch: Vec<Document>, done: &mut Imported) -> anyhow::Result<()> {
        let (buckets, documents): (Vec<Document>, Vec<Document>) = match time_field {
            Some(t) => batch.into_iter().partition(|d| is_bucket(d, t)),
            None => (Vec::new(), batch),
        };
        if !documents.is_empty() {
            if created && time_field.is_none() {
                let with = doc! { "insert": into, "bypassDocumentValidation": true };
                first_try(conn, db, with, doc! { "insert": into }, &mut self.bypass, |code| code == 13, documents, done).await?;
            } else {
                insert_with(conn, db, doc! { "insert": into }, documents, done).await?;
            }
        }
        if !buckets.is_empty() {
            let with = doc! { "insert": into, "rawData": true };
            let without = doc! { "insert": format!("system.buckets.{into}") };
            first_try(conn, db, with, without, &mut self.raw_data, |code| matches!(code, 9 | 72 | 40415), buckets, done).await?;
        }
        Ok(())
    }
}

/// Inserts with the command `with`, or `without` once the server refused
/// it with a code `refused` accepts. `known` keeps what worked.
#[allow(clippy::too_many_arguments)]
async fn first_try(
    conn: &Connection,
    db: &str,
    with: Document,
    without: Document,
    known: &mut Option<bool>,
    refused: impl Fn(i32) -> bool,
    batch: Vec<Document>,
    done: &mut Imported,
) -> anyhow::Result<()> {
    match *known {
        Some(true) => insert_with(conn, db, with, batch, done).await,
        Some(false) => insert_with(conn, db, without, batch, done).await,
        None => match insert_with(conn, db, with, batch.clone(), done).await {
            Err(e) if error_code(&e).is_some_and(&refused) => {
                *known = Some(false);
                insert_with(conn, db, without, batch, done).await
            }
            other => {
                if other.is_ok() {
                    *known = Some(true);
                }
                other
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(path: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(path)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rr-dump-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The documents of each collection of a dump, folder or archive, read
    /// as the restore reads them, checksums included.
    fn read_dump(path: &Path, db: &str) -> anyhow::Result<Vec<(String, Document, Vec<Document>)>> {
        let mut out = Vec::new();
        if path.is_file() {
            let mut r = reader(path)?;
            let (source, list) = read_prelude(&mut r, db)?;
            let wanted: Vec<String> = list.iter().filter(|(_, m)| !is_view(m)).map(|(n, _)| n.clone()).collect();
            let mut docs: HashMap<String, Vec<Document>> = HashMap::new();
            read_archive_blocks(r, &source, &wanted, |e| {
                if let ArchiveEvent::Docs(name, batch) = e {
                    docs.entry(name).or_default().extend(batch);
                }
                true
            })?;
            for (name, meta) in list {
                let d = docs.remove(&name).unwrap_or_default();
                out.push((name, meta, d));
            }
        } else {
            for e in scan_folder(path)? {
                let meta = match &e.metadata {
                    Some(p) => read_metadata(p)?,
                    None => Document::new(),
                };
                let mut docs = Vec::new();
                if let Some(p) = &e.data {
                    read_bson(reader(p)?, |b| {
                        docs.extend(b);
                        true
                    })?;
                }
                out.push((e.name, meta, docs));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    #[test]
    fn names_are_escaped_like_mongodump() {
        assert_eq!(escape("orders"), "orders");
        assert_eq!(escape("a/b c"), "a%2Fb+c", "as written by mongodump 100.x");
        assert_eq!(escape("x.y-z_~$@+"), "x.y-z_~%24%40%2B");
        assert_eq!(escape("café"), "caf%C3%A9");
        for n in ["a/b c", "café", "100%", "x?y", "a+b", "x+y@w&q=1:2;3,4"] {
            assert_eq!(unescape(&escape(n)), n);
        }
    }

    #[test]
    fn names_of_the_files_of_each_mongodump() {
        // mongodump 4.2 and older keep "+", write a space "%20".
        assert_eq!(collection_name("x+y@w&q=1:2%3B3%2C4", None, false).unwrap(), "x+y@w&q=1:2;3,4");
        assert_eq!(collection_name("a%2Fb%20c", None, false).unwrap(), "a/b c");
        // mongodump 100 writes a space "+", and "+" "%2B".
        assert_eq!(collection_name("a%2Fb+c", None, true).unwrap(), "a/b c");
        assert_eq!(collection_name("x%2By%40w%26q%3D1%3A2%3B3%2C4", None, true).unwrap(), "x+y@w&q=1:2;3,4");
        assert_eq!(collection_name("100%25", None, false).unwrap(), "100%");
        assert_eq!(collection_name("100%", None, false).unwrap(), "100%", "not escaped by the oldest tools");
        // The name in the metadata, rustyrobo and mongodump 100, tells.
        let dir = temp_dir("names");
        for (file, name) in [("a+b", "a b"), ("c+d", "c+d"), ("e+f", "unrelated")] {
            std::fs::write(dir.join(format!("{file}.metadata.json")), format!(r#"{{"options":{{}},"collectionName":"{name}"}}"#)).unwrap();
        }
        assert_eq!(collection_name("a+b", Some(&dir.join("a+b.metadata.json")), false).unwrap(), "a b");
        assert_eq!(collection_name("c+d", Some(&dir.join("c+d.metadata.json")), true).unwrap(), "c+d");
        assert_eq!(collection_name("e+f", Some(&dir.join("e+f.metadata.json")), false).unwrap(), "e+f", "a renamed file keeps its name");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The collections of the fixture database, views marked, and their
    /// number of documents.
    fn fixture_collections(ts: bool) -> Vec<(&'static str, bool, usize)> {
        let mut v = vec![("a/b c", false, 1), ("adults", true, 0), ("café", false, 1), ("capped", false, 1), ("empty", false, 0), ("system.js", false, 1), ("users", false, 3), ("x+y", false, 1)];
        if ts {
            // Three measurements in one bucket.
            v.insert(6, ("ts", false, 1));
        }
        v
    }

    #[test]
    fn dumps_of_mongodump_are_read() {
        // mongodump of MongoDB 7 names the buckets of a time series
        // system.buckets.ts, that of MongoDB 8 ts.
        for (tool, ts) in [("mongodb-8", true), ("mongodb-7", true), ("mongodb-4.2", false)] {
            for path in ["rr_fixture", "gzip/rr_fixture", "rr_fixture.archive", "rr_fixture.archive.gz"] {
                let path = fixture(&format!("{tool}/{path}"));
                let what = path.display().to_string();
                let expected = fixture_collections(ts);
                let found = scan(&path, "another_db").unwrap();
                let names: Vec<(&str, bool)> = found.iter().map(|e| (e.name.as_str(), e.view)).collect();
                assert_eq!(names, expected.iter().map(|(n, v, _)| (*n, *v)).collect::<Vec<_>>(), "{what}");
                let dump = read_dump(&path, "another_db").unwrap();
                let counts: Vec<(&str, usize)> = dump.iter().map(|(n, _, d)| (n.as_str(), d.len())).collect();
                assert_eq!(counts, expected.iter().map(|(n, _, c)| (*n, *c)).collect::<Vec<_>>(), "{what}");
                for (name, meta, docs) in &dump {
                    assert_eq!(is_view(meta), *name == "adults", "{what}: {name}");
                    assert_eq!(time_field(meta).as_deref(), (*name == "ts").then_some("t"), "{what}: {name}");
                    if name == "ts" {
                        assert!(docs.iter().all(|d| is_bucket(d, "t")), "mongodump writes buckets: {docs:?}");
                        assert_eq!(create_command("ts", meta).get_document("timeseries").unwrap().get_str("metaField").unwrap(), "m");
                    }
                    if name == "users" {
                        // The shell of 4.2 makes doubles of numbers.
                        let first = if ts { doc! { "_id": 1, "email": "ada@x", "age": 36, "name": "Ada" } } else { doc! { "_id": 1.0, "email": "ada@x", "age": 36.0, "name": "Ada" } };
                        assert_eq!(docs[0], first);
                        let specs = index_specs(meta);
                        assert_eq!(specs.len(), 2, "{what}: {specs:?}");
                        let age_name = specs.iter().find(|s| s.get_str("name") == Ok("age_name")).unwrap();
                        assert_eq!(age_name.get_document("key").unwrap().keys().collect::<Vec<_>>(), ["age", "name"], "key order kept");
                        assert!(specs.iter().all(|s| !s.contains_key("ns")));
                        assert_eq!(specs.iter().find(|s| s.get_str("name") == Ok("email_1")).unwrap().get_bool("unique"), Ok(true));
                    }
                    if name == "capped" {
                        assert_eq!(create_command("capped", meta).get_bool("capped"), Ok(true), "{what}");
                    }
                }
            }
        }
    }

    #[test]
    fn bson_files_are_read_in_batches() {
        let mut file = Vec::new();
        for i in 0..2500 {
            doc! { "_id": i, "s": "x".repeat(10) }.to_writer(&mut file).unwrap();
        }
        let mut sizes = Vec::new();
        read_bson(file.as_slice(), |b| {
            assert_eq!(b[0].get_i32("_id").unwrap() as usize, sizes.iter().sum::<usize>());
            sizes.push(b.len());
            true
        })
        .unwrap();
        assert_eq!(sizes, vec![1000, 1000, 500]);
        // A big document starts a new batch.
        let mut file = Vec::new();
        doc! { "a": 1 }.to_writer(&mut file).unwrap();
        doc! { "big": "x".repeat(crate::transfer::BATCH_BYTES) }.to_writer(&mut file).unwrap();
        let mut sizes = Vec::new();
        read_bson(file.as_slice(), |b| {
            sizes.push(b.len());
            true
        })
        .unwrap();
        assert_eq!(sizes, vec![1, 1]);
        // Empty file, truncated file, a terminator, a size that cannot be.
        read_bson(&[][..], |_| panic!("no batch")).unwrap();
        let e = read_bson(&file[..file.len() - 3], |_| true).unwrap_err();
        assert!(format!("{e:#}").contains("document 2: truncated"), "{e:#}");
        assert!(format!("{:#}", read_bson(&[0xff; 4][..], |_| true).unwrap_err()).contains("invalid size -1"));
        assert!(format!("{:#}", read_bson(&[3, 0, 0, 0][..], |_| true).unwrap_err()).contains("invalid size 3"));
        assert!(format!("{:#}", read_bson(&[2, 0, 0, 0x7f, 0][..], |_| true).unwrap_err()).contains("invalid size"));
        // Garbage of the right size.
        let mut bad = Vec::new();
        doc! { "a": 1 }.to_writer(&mut bad).unwrap();
        bad[4] = 0x7e;
        assert!(format!("{:#}", read_bson(bad.as_slice(), |_| true).unwrap_err()).contains("document 1"));
    }

    #[test]
    fn files_cut_anywhere_are_not_taken_for_complete_ones() {
        let docs: Vec<Document> = (0..40).map(|i: i32| doc! { "_id": i, "s": "x".repeat(i as usize * 7) }).collect();
        let mut plain = Vec::new();
        let mut ends = HashSet::new();
        for d in &docs {
            d.to_writer(&mut plain).unwrap();
            ends.insert(plain.len());
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&plain).unwrap();
        let gz = gz.finish().unwrap();
        let dir = temp_dir("cuts");
        let read = |bytes: &[u8]| {
            let path = dir.join("c.bson");
            std::fs::write(&path, bytes).unwrap();
            let mut n = 0;
            read_bson(reader(&path).unwrap(), |b| {
                n += b.len();
                true
            })
            .map(|()| n)
        };
        assert_eq!(read(&gz).unwrap(), 40);
        // A gzip file checks its end: cut anywhere, even between two documents, it fails.
        for cut in 1..gz.len() {
            assert!(read(&gz[..cut]).is_err(), "gzip cut at {cut} of {}", gz.len());
        }
        // A plain file can only tell a cut inside a document, or inside its size.
        for cut in 1..plain.len() {
            match read(&plain[..cut]) {
                Ok(n) => assert!(ends.contains(&cut) && n < 40, "cut at {cut}"),
                Err(_) => assert!(!ends.contains(&cut), "cut at {cut}"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_are_scanned() {
        let dir = temp_dir("scan");
        for f in ["users.bson", "users.metadata.json", "a%2Fb.bson.gz", "a%2Fb.metadata.json.gz", "v.metadata.json", "system.profile.bson", "system.views.bson", "notes.txt", "raw.bson", "prelude.json.gz"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let found = scan(&dir, "any").unwrap();
        let names: Vec<(&str, bool)> = found.iter().map(|e| (e.name.as_str(), e.view)).collect();
        assert_eq!(names, vec![("a/b", false), ("raw", false), ("users", false), ("v", true)]);
        // The buckets of a time series, as mongodump of MongoDB 7 names them.
        std::fs::write(dir.join("ts.metadata.json"), r#"{"options":{"timeseries":{"timeField":"t"}},"collectionName":"ts","type":"timeseries"}"#).unwrap();
        std::fs::write(dir.join("system.buckets.ts.bson"), "").unwrap();
        let found = scan_folder(&dir).unwrap();
        let ts = found.iter().find(|e| e.name == "ts").unwrap();
        assert_eq!(ts.data.as_deref(), Some(dir.join("system.buckets.ts.bson").as_path()));
        assert!(!found.iter().any(|e| e.name.starts_with("system.buckets.")));
        std::fs::write(dir.join("ts.bson"), "").unwrap();
        let e = scan(&dir, "any").unwrap_err();
        assert!(e.to_string().starts_with("two files for the documents of ts"), "{e}");
        std::fs::remove_file(dir.join("ts.bson")).unwrap();
        // Which of the two is the dump? Neither is guessed.
        std::fs::write(dir.join("users.bson.gz"), "").unwrap();
        let e = scan(&dir, "any").unwrap_err();
        assert!(e.to_string().starts_with("two files for the same collection"), "{e}");
        assert!(scan(&dir.join("missing"), "any").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crc_is_the_one_of_archives() {
        let mut crc = Crc64::default();
        crc.update(b"123456789");
        assert_eq!(crc.0, 0x995D_C9BB_DF19_39FA, "CRC-64/XZ check value");
        // The block of two documents of an archive written by mongodump 100.19.
        let mut crc = Crc64::default();
        crc.update(&bson::to_vec(&doc! { "_id": 1, "a": "x" }).unwrap());
        crc.update(&bson::to_vec(&doc! { "_id": 2, "a": "y" }).unwrap());
        assert_eq!(crc.0 as i64, -283740777170061679);
    }

    /// An archive like mongodump writes them: blocks of collections interleaved.
    fn archive(db: &str, colls: &[(&str, &str, &[Document])]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        doc! { "concurrent_collections": 4, "version": "0.1", "server_version": "8.0.0", "tool_version": "100.19.1" }.to_writer(&mut out).unwrap();
        for (name, kind, _) in colls {
            let meta = format!(r#"{{"indexes":[],"collectionName":"{name}","type":"{kind}"}}"#);
            doc! { "db": db, "collection": *name, "metadata": meta, "size": 0, "type": *kind }.to_writer(&mut out).unwrap();
        }
        out.extend(TERMINATOR);
        let header = |out: &mut Vec<u8>, name: &str, eof: bool, crc: u64| doc! { "db": db, "collection": name, "EOF": eof, "CRC": crc as i64 }.to_writer(out).unwrap();
        // Each collection in two blocks, one after the other collections' first ones.
        let mut crcs = vec![Crc64::default(); colls.len()];
        for half in 0..2 {
            for (i, (name, _, docs)) in colls.iter().enumerate() {
                header(&mut out, name, false, 0);
                let mid = docs.len() / 2;
                for d in if half == 0 { &docs[..mid] } else { &docs[mid..] } {
                    let raw = bson::to_vec(d).unwrap();
                    crcs[i].update(&raw);
                    out.extend(raw);
                }
                out.extend(TERMINATOR);
            }
        }
        for (i, (name, _, _)) in colls.iter().enumerate() {
            header(&mut out, name, true, crcs[i].0);
            out.extend(TERMINATOR);
        }
        out
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn archives_are_read() {
        let users: Vec<Document> = (0..2500).map(|i| doc! { "_id": i }).collect();
        let logs: Vec<Document> = (0..3).map(|i| doc! { "_id": i, "m": "x" }).collect();
        let file = archive("shop", &[("users", "collection", &users), ("logs", "collection", &logs), ("v", "view", &[])]);
        let mut r = file.as_slice();
        let (db, list) = read_prelude(&mut r, "other").unwrap();
        assert_eq!(db, "shop", "the only database of the archive");
        let found: Vec<(&str, bool)> = list.iter().map(|(n, m)| (n.as_str(), is_view(m))).collect();
        assert_eq!(found, vec![("users", false), ("logs", false), ("v", true)]);
        let mut got: HashMap<String, Vec<Document>> = HashMap::new();
        let mut ended = Vec::new();
        read_archive_blocks(r, "shop", &names(&["users", "v"]), |e| {
            match e {
                ArchiveEvent::Docs(c, docs) => {
                    assert!(docs.len() <= BATCH);
                    got.entry(c).or_default().extend(docs);
                }
                ArchiveEvent::End(c) => ended.push(c),
            }
            true
        })
        .unwrap();
        assert_eq!(got.get("users"), Some(&users), "interleaved blocks, in order");
        assert!(!got.contains_key("logs"), "not wanted");
        assert_eq!(ended, vec!["users", "v"]);

        // Damaged: the checksum tells.
        let mut bad = file.clone();
        let raw = bson::to_vec(&doc! { "_id": 7 }).unwrap();
        let at = bad.windows(raw.len()).position(|w| w == raw).unwrap();
        bad[at + raw.len() - 5] = 6; // { _id: 6 }: still a document
        let mut r = bad.as_slice();
        let e = read_prelude(&mut r, "shop").and_then(|_| read_archive_blocks(r, "shop", &names(&["users"]), |_| true)).unwrap_err();
        assert!(e.to_string().contains("users: checksum mismatch"), "{e}");
        // But not in a collection that is not restored.
        let mut r = bad.as_slice();
        read_prelude(&mut r, "shop").and_then(|_| read_archive_blocks(r, "shop", &names(&["logs"]), |_| true)).unwrap();

        // Several databases: the one of the restore.
        let mut out = MAGIC.to_vec();
        doc! { "version": "0.1" }.to_writer(&mut out).unwrap();
        for db in ["b", "a", "b"] {
            doc! { "db": db, "collection": "x", "metadata": "{}", "size": 0, "type": "collection" }.to_writer(&mut out).unwrap();
        }
        out.extend(TERMINATOR);
        assert_eq!(read_prelude(&mut out.as_slice(), "b").unwrap().0, "b");
        let e = read_prelude(&mut out.as_slice(), "c").unwrap_err();
        assert!(e.to_string().contains("several databases (a, b)"), "{e}");
        assert!(read_prelude(&mut &b"not an archive"[..], "a").is_err());
        assert!(read_prelude(&mut &MAGIC[..], "a").is_err(), "truncated in the prelude");
    }

    #[test]
    fn archives_cut_between_two_blocks_are_refused() {
        let users: Vec<Document> = (0..10).map(|i| doc! { "_id": i }).collect();
        let file = archive("shop", &[("users", "collection", &users), ("empty", "collection", &[])]);
        let read = |bytes: &[u8], wanted: &[&str]| {
            let mut r = bytes;
            read_prelude(&mut r, "shop").and_then(|_| read_archive_blocks(r, "shop", &names(wanted), |_| true))
        };
        read(&file, &["users", "empty"]).unwrap();
        // Cut at each end of a document or a block: never read as complete.
        let mut cuts = 0;
        let prelude_end = file.windows(4).position(|w| w == TERMINATOR).unwrap() + 4;
        assert!(read(&file[..prelude_end], &["users"]).unwrap_err().to_string().contains("users: truncated archive"));
        let mut at = prelude_end;
        // Up to the last end of block: without the terminator after it, nothing is missing.
        while at < file.len() - 4 {
            let len = i32::from_le_bytes(file[at..at + 4].try_into().unwrap());
            at += if len == -1 { 4 } else { len as usize };
            if at < file.len() - 4 {
                let e = read(&file[..at], &["users", "empty"]).unwrap_err();
                assert!(e.to_string().contains("truncated archive"), "cut at {at}: {e}");
                cuts += 1;
            }
        }
        assert!(cuts > 10, "{cuts}");
        // The real archives of mongodump, cut before their last end of block.
        for tool in ["mongodb-8", "mongodb-7", "mongodb-4.2"] {
            let file = std::fs::read(fixture(&format!("{tool}/rr_fixture.archive"))).unwrap();
            let mut r = file.as_slice();
            let (_, list) = read_prelude(&mut r, "rr_fixture").unwrap();
            let wanted: Vec<String> = list.iter().filter(|(_, m)| !is_view(m)).map(|(n, _)| n.clone()).collect();
            let eof = bson::to_vec(&doc! { "db": "rr_fixture", "collection": wanted.last().unwrap() }).unwrap();
            let last = file.windows(4).rposition(|w| w == TERMINATOR).unwrap();
            let mut r = &file[..last - 30];
            read_prelude(&mut r, "rr_fixture").unwrap();
            let e = read_archive_blocks(r, "rr_fixture", &wanted, |_| true).unwrap_err();
            assert!(e.to_string().contains("truncated archive") || e.to_string().contains("truncated file"), "{tool}: {e}");
            assert!(!eof.is_empty());
        }
    }

    #[test]
    fn metadata_becomes_commands() {
        let meta = doc! {
            "options": { "capped": true, "size": 4096, "autoIndexId": true },
            "indexes": [
                { "v": 2, "key": { "_id": 1 }, "name": "_id_", "ns": "shop.users" },
                { "v": 2, "key": { "email": 1 }, "name": "email_1", "unique": true, "ns": "shop.users" },
            ],
        };
        assert_eq!(create_command("users", &meta), doc! { "create": "users", "capped": true, "size": 4096 });
        assert_eq!(index_specs(&meta), vec![doc! { "v": 2, "key": { "email": 1 }, "name": "email_1", "unique": true }]);
        assert_eq!(create_command("x", &Document::new()), doc! { "create": "x" });
        assert!(index_specs(&Document::new()).is_empty());
        let clustered = doc! { "options": { "clusteredIndex": { "key": { "_id": 1 }, "unique": true } }, "indexes": [{ "v": 2, "key": { "_id": 1 }, "name": "_id_", "clustered": true }] };
        assert!(index_specs(&clustered).is_empty(), "made by create");
        assert!(create_command("c", &clustered).contains_key("clusteredIndex"));
    }

    #[test]
    fn buckets_are_told_from_measurements() {
        let bucket = doc! { "_id": bson::oid::ObjectId::new(), "control": { "version": 2, "min": { "t": bson::DateTime::now() }, "max": {} }, "meta": "a", "data": {} };
        assert!(is_bucket(&bucket, "t"));
        assert!(!is_bucket(&doc! { "t": bson::DateTime::now(), "control": { "version": 1 }, "data": {} }, "t"), "a measurement with odd fields");
        assert!(!is_bucket(&doc! { "t": bson::DateTime::now(), "v": 1 }, "t"));
        assert!(!is_bucket(&doc! { "control": 1, "data": {} }, "t"));
        let meta = doc! { "options": { "timeseries": { "timeField": "when", "metaField": "m" } }, "type": "timeseries" };
        assert_eq!(time_field(&meta).as_deref(), Some("when"));
        assert_eq!(time_field(&doc! { "options": {} }), None);
    }

    #[test]
    fn restores_tell_what_they_did_not_do() {
        let mut r = Restored { collections: 2, docs: Imported { inserted: 10, failed: 0, first_error: None }, problems: Vec::new() };
        assert_eq!(r.summary(), "2 collections restored: 10 documents imported");
        assert!(!r.incomplete());
        r.problems.push("users: 3 documents refused (E11000), left as it was".into());
        assert_eq!(r.summary(), "2 collections restored: 10 documents imported. Not restored: users: 3 documents refused (E11000), left as it was");
        assert!(r.incomplete());
        assert!(Restored { docs: Imported { failed: 1, ..Default::default() }, ..Default::default() }.incomplete());
    }
}
