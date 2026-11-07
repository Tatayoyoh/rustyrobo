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
//! archives interleave the blocks of several collections.
//!
//! Like the import and export, documents go by batches: nothing holds a
//! whole collection.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, bail};
use bson::{Bson, Document, doc};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::mongo::{Connection, collect_cursor};
use crate::transfer::{BATCH, BATCH_BYTES, Imported, for_each_batch, insert_batch};

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
}

impl Restored {
    pub fn summary(&self) -> String {
        format!("{} collection{} restored: {}", self.collections, plural(self.collections), self.docs.summary())
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn docs(n: u64) -> String {
    format!("{n} document{}", plural(n))
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

/// The collections dumped: system collections are left out, but for stored functions.
fn dumped(name: &str) -> bool {
    !name.starts_with("system.") || name == "system.js"
}

fn is_view(meta: &Document) -> bool {
    meta.get_str("type").ok() == Some("view") || meta.get_document("options").is_ok_and(|o| o.contains_key("viewOn"))
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

/// Writes the collections of a database. `progress` gets the number of
/// documents written so far.
pub async fn dump(conn: &Connection, o: &DumpOptions, cancel: &AtomicBool, progress: impl Fn(u64)) -> anyhow::Result<Dumped> {
    let list = collected(conn, o).await?;
    let mut done = Dumped::default();
    // The documents of a collection, with what they are written to.
    let mut documents = async |name: &str, w: &mut Out, crc: &mut Crc64| -> anyhow::Result<()> {
        let before = done.documents;
        let cmd = doc! { "find": name, "filter": {}, "batchSize": BATCH as i64 };
        for_each_batch(conn, &o.db, name, cmd, cancel, done.documents, |batch| {
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
        .with_context(|| format!("{name}: {} written", docs(done.documents - before)))
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
            let block = |eof: bool, crc: u64| doc! { "db": &o.db, "collection": &c.name, "EOF": eof, "CRC": crc as i64 };
            block(false, 0).to_writer(&mut w)?;
            let mut crc = Crc64::default();
            if !is_view(&c.meta) {
                documents(&c.name, &mut w, &mut crc).await?;
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
            let file = |ext: &str| target.join(format!("{}.{ext}{}", escape(&c.name), if o.gzip { ".gz" } else { "" }));
            let mut w = writer(&file("metadata.json"), o.gzip)?;
            w.write_all(metadata_json(&c.meta)?.as_bytes())?;
            w.finish()?;
            if !is_view(&c.meta) {
                let mut w = writer(&file("bson"), o.gzip)?;
                documents(&c.name, &mut w, &mut Crc64::default()).await?;
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
        let (name, data) = if let Some(n) = file.strip_suffix(".metadata.json") {
            (n, false)
        } else if let Some(n) = file.strip_suffix(".bson") {
            (n, true)
        } else {
            continue;
        };
        let name = unescape(name);
        if !dumped(&name) {
            continue;
        }
        let entry = match found.iter_mut().find(|f| f.name == name) {
            Some(f) => f,
            None => {
                found.push(FolderEntry { name, data: None, metadata: None });
                found.last_mut().unwrap()
            }
        };
        if data { entry.data = Some(path) } else { entry.metadata = Some(path) };
    }
    Ok(found)
}

/// Options, indexes and type of a collection, from its metadata file.
pub fn read_metadata(path: &Path) -> anyhow::Result<Document> {
    let mut text = String::new();
    reader(path)?.read_to_string(&mut text)?;
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
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
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

/// Documents waiting to be inserted, sent when a round trip is full.
#[derive(Default)]
struct Batcher {
    docs: Vec<Document>,
    bytes: usize,
}

impl Batcher {
    /// Adds a document; returns the batch to send first, if full.
    fn push(&mut self, d: Document, size: usize) -> Option<Vec<Document>> {
        // Under the size of a command, also with a big document.
        let full = !self.docs.is_empty() && (self.docs.len() >= BATCH || self.bytes + size > BATCH_BYTES);
        let out = full.then(|| self.take());
        self.bytes += size;
        self.docs.push(d);
        out
    }

    fn take(&mut self) -> Vec<Document> {
        self.bytes = 0;
        std::mem::take(&mut self.docs)
    }
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
    if !batch.docs.is_empty() {
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
/// of `db` that `wanted` accepts. Checks the checksum of each collection.
fn read_archive_blocks(mut r: impl Read, db: &str, wanted: impl Fn(&str) -> bool, mut send: impl FnMut(ArchiveEvent) -> bool) -> anyhow::Result<()> {
    // The collection of the block being read; `None` in a block to skip.
    let mut block: Option<Option<String>> = None;
    let mut pending: HashMap<String, (Batcher, Crc64)> = HashMap::new();
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
                let coll = h.get_str("collection").unwrap_or_default().to_string();
                let ours = h.get_str("db").ok() == Some(db) && dumped(&coll) && wanted(&coll);
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
                    if !batch.docs.is_empty() && !send(ArchiveEvent::Docs(coll.clone(), batch.take())) {
                        return Ok(());
                    }
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
    if let Some(coll) = pending.keys().next() {
        bail!("{coll}: truncated archive");
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

/// Restores the collections of a dump, folder or archive: collections
/// first, then the views, which may read them. Write errors (duplicate
/// keys…) are counted, like with mongorestore.
pub async fn restore(conn: &Connection, o: &RestoreOptions, cancel: Arc<AtomicBool>, progress: impl Fn(u64)) -> anyhow::Result<Restored> {
    let selected = |name: &str| o.collections.is_empty() || o.collections.iter().any(|c| c == name);
    let archive = o.from.is_file();
    // Name, metadata and, in a folder, the file of the documents.
    let (source, mut list): (String, Vec<(String, Document, Option<PathBuf>)>) = if archive {
        let (path, db) = (o.from.clone(), o.db.clone());
        let (source, list) = tokio::task::spawn_blocking(move || archive_prelude(&path, &db)).await??;
        (source, list.into_iter().map(|(n, m)| (n, m, None)).collect())
    } else {
        let mut list = Vec::new();
        for e in scan_folder(&o.from)? {
            let meta = match &e.metadata {
                Some(p) => read_metadata(p)?,
                None => Document::new(),
            };
            list.push((e.name, meta, e.data));
        }
        (o.db.clone(), list)
    };
    list.retain(|(name, _, _)| selected(name));
    if list.is_empty() {
        bail!("no collection to restore in {}", o.from.display());
    }
    list.sort_by_key(|(_, meta, _)| is_view(meta));
    let mut done = Restored::default();
    let create = async |name: &str, meta: &Document| -> anyhow::Result<()> {
        if o.drop_first {
            ignore_code(conn.run_command(&o.db, doc! { "drop": name }, None).await, 26)?; // NamespaceNotFound
        }
        // NamespaceExists: restored into the collection as it is.
        ignore_code(conn.run_command(&o.db, create_command(name, meta), None).await, 48).with_context(|| format!("cannot create {name}"))
    };
    let indexes = async |name: &str, meta: &Document| -> anyhow::Result<()> {
        let specs = index_specs(meta);
        if o.indexes && !specs.is_empty() {
            conn.run_command(&o.db, doc! { "createIndexes": name, "indexes": specs }, None).await.with_context(|| format!("indexes of {name}"))?;
        }
        Ok(())
    };
    let cancelled = |done: &Restored| -> anyhow::Result<()> {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled after {}", docs(done.docs.inserted));
        }
        Ok(())
    };
    if archive {
        let collections: Vec<&(String, Document, Option<PathBuf>)> = list.iter().filter(|(_, m, _)| !is_view(m)).collect();
        for (name, meta, _) in &collections {
            cancelled(&done)?;
            create(name, meta).await?;
        }
        let names: Vec<String> = collections.iter().map(|(n, _, _)| n.clone()).collect();
        let mut r = reader(&o.from)?;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<anyhow::Result<ArchiveEvent>>(2);
        let stop = Arc::clone(&cancel);
        let read = tokio::task::spawn_blocking(move || {
            let send = |e| tx.blocking_send(Ok(e)).is_ok() && !stop.load(Ordering::Relaxed);
            let blocks = read_prelude(&mut r, &source).and_then(|_| read_archive_blocks(r, &source, |c| names.iter().any(|n| n == c), send));
            if let Err(e) = blocks {
                let _ = tx.blocking_send(Err(e));
            }
        });
        while let Some(event) = rx.recv().await {
            cancelled(&done)?;
            match event.with_context(|| format!("{}", o.from.display()))? {
                ArchiveEvent::Docs(name, batch) => {
                    insert_batch(conn, &o.db, &name, batch, &mut done.docs).await?;
                    progress(done.docs.inserted);
                }
                ArchiveEvent::End(name) => {
                    if let Some((_, meta, _)) = collections.iter().find(|(n, _, _)| *n == name) {
                        indexes(&name, meta).await?;
                        done.collections += 1;
                    }
                }
            }
        }
        let _ = read.await;
        cancelled(&done)?;
        for (name, meta, _) in list.iter().filter(|(_, m, _)| is_view(m)) {
            create(name, meta).await?;
            done.collections += 1;
        }
        return Ok(done);
    }
    for (name, meta, data) in &list {
        cancelled(&done)?;
        create(name, meta).await?;
        if let Some(path) = data {
            let r = reader(path)?;
            let (tx, mut rx) = tokio::sync::mpsc::channel::<anyhow::Result<Vec<Document>>>(2);
            let stop = Arc::clone(&cancel);
            let read = tokio::task::spawn_blocking(move || {
                let send = |batch| tx.blocking_send(Ok(batch)).is_ok() && !stop.load(Ordering::Relaxed);
                if let Err(e) = read_bson(r, send) {
                    let _ = tx.blocking_send(Err(e));
                }
            });
            while let Some(batch) = rx.recv().await {
                cancelled(&done)?;
                let batch = batch.with_context(|| format!("{}", path.display()))?;
                insert_batch(conn, &o.db, name, batch, &mut done.docs).await?;
                progress(done.docs.inserted);
            }
            let _ = read.await;
        }
        indexes(name, meta).await?;
        done.collections += 1;
    }
    Ok(done)
}

/// `r`, with the server error of code `code` taken as a success.
fn ignore_code(r: anyhow::Result<Document>, code: i32) -> anyhow::Result<()> {
    match r {
        Err(e) if crate::mongo::command_failure(&e).is_none_or(|f| f.get_i32("code").ok() != Some(code)) => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_escaped_like_mongodump() {
        assert_eq!(escape("orders"), "orders");
        assert_eq!(escape("a/b c"), "a%2Fb+c", "as written by mongodump 100.x");
        assert_eq!(escape("x.y-z_~$@+"), "x.y-z_~%24%40%2B");
        assert_eq!(escape("café"), "caf%C3%A9");
        for n in ["a/b c", "café", "100%", "x?y", "a+b"] {
            assert_eq!(unescape(&escape(n)), n);
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
        doc! { "big": "x".repeat(BATCH_BYTES) }.to_writer(&mut file).unwrap();
        let mut sizes = Vec::new();
        read_bson(file.as_slice(), |b| {
            sizes.push(b.len());
            true
        })
        .unwrap();
        assert_eq!(sizes, vec![1, 1]);
        // Empty file, truncated file.
        read_bson(&[][..], |_| panic!("no batch")).unwrap();
        let e = read_bson(&file[..file.len() - 3], |_| true).unwrap_err();
        assert!(format!("{e:#}").contains("document 2: truncated"), "{e:#}");
    }

    #[test]
    fn folders_are_scanned() {
        let dir = std::env::temp_dir().join(format!("rr-dump-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["users.bson", "users.metadata.json", "a%2Fb.bson.gz", "a%2Fb.metadata.json.gz", "v.metadata.json", "system.profile.bson", "notes.txt", "raw.bson"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let found = scan(&dir, "any").unwrap();
        let names: Vec<(&str, bool)> = found.iter().map(|e| (e.name.as_str(), e.view)).collect();
        assert_eq!(names, vec![("a/b", false), ("raw", false), ("users", false), ("v", true)]);
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

    #[test]
    fn archives_are_read() {
        let users: Vec<Document> = (0..2500).map(|i| doc! { "_id": i }).collect();
        let logs: Vec<Document> = (0..3).map(|i| doc! { "_id": i, "m": "x" }).collect();
        let file = archive("shop", &[("users", "collection", &users), ("logs", "collection", &logs), ("v", "view", &[])]);
        let mut r = file.as_slice();
        let (db, list) = read_prelude(&mut r, "other").unwrap();
        assert_eq!(db, "shop", "the only database of the archive");
        let names: Vec<(&str, bool)> = list.iter().map(|(n, m)| (n.as_str(), is_view(m))).collect();
        assert_eq!(names, vec![("users", false), ("logs", false), ("v", true)]);
        let mut got: HashMap<String, Vec<Document>> = HashMap::new();
        let mut ended = Vec::new();
        read_archive_blocks(r, "shop", |c| c != "logs", |e| {
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
        let e = read_prelude(&mut r, "shop").and_then(|_| read_archive_blocks(r, "shop", |_| true, |_| true)).unwrap_err();
        assert!(e.to_string().contains("users: checksum mismatch"), "{e}");

        // Several databases: the one of the restore.
        let mut two = archive("a", &[("x", "collection", &[])]);
        two.truncate(4);
        let mut out = two;
        doc! { "version": "0.1" }.to_writer(&mut out).unwrap();
        for db in ["a", "b"] {
            doc! { "db": db, "collection": "x", "metadata": "{}", "size": 0, "type": "collection" }.to_writer(&mut out).unwrap();
        }
        out.extend(TERMINATOR);
        assert_eq!(read_prelude(&mut out.as_slice(), "b").unwrap().0, "b");
        let e = read_prelude(&mut out.as_slice(), "c").unwrap_err();
        assert!(e.to_string().contains("several databases (a, b)"), "{e}");
        assert!(read_prelude(&mut &b"not an archive"[..], "a").is_err());
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
    }
}
