//! Import and export of collections, in JSON (one document per line, like
//! mongoexport, or an array) and CSV.
//!
//! Documents are read and written in batches: an export streams the cursor
//! to the file, an import parses the file on a blocking thread while the
//! previous batch is being inserted. Nothing holds the whole collection, nor
//! the whole file.
//!
//! An import reads its file to the end before writing anything: a file that
//! cannot be read changes nothing. With "drop first", the documents go to a
//! new collection that replaces the old one once complete ([`Target`]):
//! until then, a failure or a cancel leaves the old one as it was.

use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, anyhow, bail};
use bson::{Bson, Document, doc};

use crate::mongo::{Connection, bson_as_i64, error_code, error_message};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Format {
    /// One document per line (mongoexport's default).
    #[default]
    JsonLines,
    JsonArray,
    Csv,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::JsonLines, Format::JsonArray, Format::Csv];

    pub fn label(self) -> &'static str {
        match self {
            Format::JsonLines => "JSON (one document per line)",
            Format::JsonArray => "JSON array",
            Format::Csv => "CSV",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::JsonLines | Format::JsonArray => "json",
            Format::Csv => "csv",
        }
    }

    /// JSON, unless the file name ends in `.csv` or `.tsv`.
    pub fn of_path(path: &Path) -> Format {
        match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
            Some("csv" | "tsv") => Format::Csv,
            _ => Format::JsonLines,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Delimiter {
    #[default]
    Comma,
    Semicolon,
    Tab,
}

impl Delimiter {
    pub const ALL: [Delimiter; 3] = [Delimiter::Comma, Delimiter::Semicolon, Delimiter::Tab];

    pub fn char(self) -> char {
        match self {
            Delimiter::Comma => ',',
            Delimiter::Semicolon => ';',
            Delimiter::Tab => '\t',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Delimiter::Comma => "Comma",
            Delimiter::Semicolon => "Semicolon",
            Delimiter::Tab => "Tab",
        }
    }

    pub fn of_path(path: &Path) -> Delimiter {
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tsv")) { Delimiter::Tab } else { Delimiter::Comma }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportOptions {
    pub db: String,
    pub coll: String,
    pub filter: Document,
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub skip: u64,
    /// 0 = all the documents.
    pub limit: u64,
    pub format: Format,
    /// Canonical Extended JSON (`{"$numberInt": "1"}`) instead of relaxed.
    pub canonical: bool,
    /// CSV columns, as dotted paths. Empty: the fields of the first batch.
    pub fields: Vec<String>,
    pub delimiter: Delimiter,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportOptions {
    pub db: String,
    pub coll: String,
    pub format: Format,
    pub delimiter: Delimiter,
    /// CSV: numbers, booleans, dates, ObjectIds… instead of strings only.
    pub detect_types: bool,
    pub drop_first: bool,
    pub path: PathBuf,
}

/// Documents of one round trip to the server.
pub(crate) const BATCH: usize = 1000;
/// Size of the documents of one insert, under the 16 MB of a command.
pub(crate) const BATCH_BYTES: usize = 8 * 1024 * 1024;
/// Largest document MongoDB stores.
pub(crate) const MAX_DOCUMENT: usize = 16 * 1024 * 1024;

/// How far an import or a restore is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Documents read, before anything is written.
    Checked(u64),
    /// Documents written.
    Written(u64),
}

/// A transfer stopped by the user.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// A replacement stopped once the old collection could be gone: the new
/// one is kept, under its temporary name.
#[derive(Debug)]
pub struct Stranded {
    pub name: String,
    pub into: String,
    pub cause: String,
}

impl std::fmt::Display for Stranded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: the new documents of {} are kept in {}", self.cause, self.name, self.into)
    }
}

impl std::error::Error for Stranded {}

pub(crate) fn check_cancel(cancel: &AtomicBool) -> anyhow::Result<()> {
    if cancel.load(Ordering::Relaxed) { Err(Cancelled.into()) } else { Ok(()) }
}

pub(crate) fn docs(n: u64) -> String {
    format!("{n} document{}", if n == 1 { "" } else { "s" })
}

/// Writes the documents of a query to a file. `progress` gets the number of
/// documents written so far. Returns that number.
pub async fn export(conn: &Connection, o: &ExportOptions, cancel: &AtomicBool, progress: impl Fn(u64)) -> anyhow::Result<u64> {
    let file = std::fs::File::create(&o.path).with_context(|| format!("cannot create {}", o.path.display()))?;
    let mut w = Writer::new(BufWriter::new(file), o);
    let mut cmd = doc! { "find": &o.coll, "filter": o.filter.clone(), "batchSize": BATCH as i64 };
    if let Some(p) = &o.projection {
        cmd.insert("projection", p.clone());
    }
    if let Some(s) = &o.sort {
        cmd.insert("sort", s.clone());
    }
    if o.skip > 0 {
        cmd.insert("skip", o.skip as i64);
    }
    if o.limit > 0 {
        cmd.insert("limit", o.limit as i64);
    }
    let mut written = 0;
    for_each_batch(conn, &o.db, &o.coll, cmd, cancel, 0, |batch| {
        w.write(&batch)?;
        written += batch.len() as u64;
        progress(written);
        Ok(())
    })
    .await?;
    w.finish()?;
    Ok(written)
}

/// Runs a `find` on `coll` and gives each batch of its cursor to `f`.
/// Cancelled between two batches; `before` is the number of documents
/// already done, for the message.
pub(crate) async fn for_each_batch(
    conn: &Connection,
    db: &str,
    coll: &str,
    cmd: Document,
    cancel: &AtomicBool,
    before: u64,
    mut f: impl FnMut(Vec<Document>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut session = conn.start_session().await.ok();
    let reply = conn.run_command(db, cmd, session.as_mut()).await?;
    let cursor = reply.get_document("cursor").context("find did not return a cursor")?;
    // The namespace of the cursor, which getMore must name.
    let coll = cursor.get_str("ns").ok().and_then(|ns| ns.split_once('.')).map_or(coll, |(_, c)| c).to_string();
    let mut id = cursor.get("id").and_then(bson_as_i64).unwrap_or(0);
    let mut batch = batch_of(cursor, "firstBatch");
    let mut done = before;
    loop {
        done += batch.len() as u64;
        f(batch)?;
        if id == 0 {
            return Ok(());
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = conn.run_command(db, doc! { "killCursors": &coll, "cursors": [id] }, None).await;
            bail!("cancelled after {}", docs(done));
        }
        let more = conn.run_command(db, doc! { "getMore": id, "collection": &coll, "batchSize": BATCH as i64 }, session.as_mut()).await?;
        let cursor = more.get_document("cursor").context("getMore did not return a cursor")?;
        id = cursor.get("id").and_then(bson_as_i64).unwrap_or(0);
        batch = batch_of(cursor, "nextBatch");
    }
}

fn batch_of(cursor: &Document, key: &str) -> Vec<Document> {
    cursor.get_array(key).map(|a| a.iter().filter_map(|d| d.as_document().cloned()).collect()).unwrap_or_default()
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Imported {
    pub inserted: u64,
    pub failed: u64,
    /// The first write error, if any.
    pub first_error: Option<String>,
}

impl Imported {
    pub fn summary(&self) -> String {
        match &self.first_error {
            None => format!("{} imported", docs(self.inserted)),
            Some(e) => format!("{} imported, {} failed. First error: {e}", docs(self.inserted), self.failed),
        }
    }

    pub(crate) fn add(&mut self, other: &Imported) {
        self.inserted += other.inserted;
        self.failed += other.failed;
        if self.first_error.is_none() {
            self.first_error.clone_from(&other.first_error);
        }
    }

    /// The documents refused, as a reason not to replace a collection.
    pub(crate) fn refused(&self) -> String {
        format!("{} refused ({})", docs(self.failed), self.first_error.as_deref().unwrap_or("write error"))
    }
}

/// Reads a file on a blocking thread, one batch ahead of what is done with
/// it. The reader gives its batches to the function it gets, which returns
/// false once the transfer is cancelled.
pub(crate) struct ReadAhead<T> {
    rx: tokio::sync::mpsc::Receiver<anyhow::Result<T>>,
    thread: Option<tokio::task::JoinHandle<()>>,
}

impl<T: Send + 'static> ReadAhead<T> {
    pub(crate) fn spawn(cancel: &Arc<AtomicBool>, read: impl FnOnce(&mut dyn FnMut(T) -> bool) -> anyhow::Result<()> + Send + 'static) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let stop = Arc::clone(cancel);
        let thread = tokio::task::spawn_blocking(move || {
            let mut send = |item| tx.blocking_send(Ok(item)).is_ok() && !stop.load(Ordering::Relaxed);
            if let Err(e) = read(&mut send) {
                let _ = tx.blocking_send(Err(e));
            }
        });
        ReadAhead { rx, thread: Some(thread) }
    }

    /// The next batch, `None` at the end. A reader that panicked is an
    /// error, not the end of the file.
    pub(crate) async fn next(&mut self) -> Option<anyhow::Result<T>> {
        if let Some(item) = self.rx.recv().await {
            return Some(item);
        }
        match self.thread.take()?.await {
            Ok(()) => None,
            Err(e) => Some(Err(anyhow!("the file could not be read to the end: {e}"))),
        }
    }
}

/// Documents waiting to be inserted, sent when a round trip is full.
#[derive(Default)]
pub(crate) struct Batcher {
    docs: Vec<Document>,
    bytes: usize,
}

impl Batcher {
    /// Adds a document of `size` bytes; returns the batch to send first, if full.
    pub(crate) fn push(&mut self, d: Document, size: usize) -> Option<Vec<Document>> {
        // Under the size of a command, also with a big document.
        let full = !self.docs.is_empty() && (self.docs.len() >= BATCH || self.bytes + size > BATCH_BYTES);
        let out = full.then(|| self.take());
        self.bytes += size;
        self.docs.push(d);
        out
    }

    pub(crate) fn take(&mut self) -> Vec<Document> {
        self.bytes = 0;
        std::mem::take(&mut self.docs)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

/// `r`, with the server error of code `code` taken as a success.
pub(crate) fn ignore_code(r: anyhow::Result<Document>, code: i32) -> anyhow::Result<()> {
    match r {
        Err(e) if error_code(&e) != Some(code) => Err(e),
        _ => Ok(()),
    }
}

/// How "drop first" replaces a collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Replace {
    /// Not replaced: the documents are added to it.
    No,
    /// The documents go to a new collection, renamed over the old one once
    /// complete: until then, a failure or a cancel leaves it as it was.
    Swap,
    /// Dropped, then filled: what cannot be renamed (time series,
    /// system.js), or whose name leaves no room for a temporary one.
    Drop,
}

/// The collection the documents of `name` are written to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Target {
    pub name: String,
    /// `name`, or the new collection that replaces it.
    pub into: String,
    pub replace: Replace,
}

impl Target {
    pub(crate) fn add_to(name: &str) -> Target {
        Target { name: name.to_string(), into: name.to_string(), replace: Replace::No }
    }

    /// A collection made by `create` to replace `name`. Only when it cannot
    /// be renamed is `name` itself dropped and made again.
    pub(crate) async fn replacing(conn: &Connection, db: &str, name: &str, renamable: bool, create: impl Fn(&str) -> Document) -> anyhow::Result<Target> {
        if renamable && let Some(t) = Target::swap(conn, db, name, &create).await? {
            return Ok(t);
        }
        match conn.run_command(db, doc! { "drop": name }, None).await {
            // MongoDB 4.2 and older do not drop system.js: emptied.
            Err(e) if error_code(&e) == Some(20) => {
                conn.run_command(db, doc! { "delete": name, "deletes": [{ "q": {}, "limit": 0 }] }, None).await?;
            }
            r => ignore_code(r, 26)?, // NamespaceNotFound
        }
        ignore_code(conn.run_command(db, create(name), None).await, 48)?; // NamespaceExists: emptied
        Ok(Target { name: name.to_string(), into: name.to_string(), replace: Replace::Drop })
    }

    /// A new collection made by `create` next to `name`, to replace it;
    /// `None` when the name leaves no room for another.
    pub(crate) async fn swap(conn: &Connection, db: &str, name: &str, create: impl Fn(&str) -> Document) -> anyhow::Result<Option<Target>> {
        if !Target::fits(conn, db, name) {
            return Ok(None);
        }
        let into = format!("{name}.rr-tmp-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        conn.run_command(db, create(&into), None).await?;
        Ok(Some(Target { name: name.to_string(), into, replace: Replace::Swap }))
    }

    /// Whether the name of a collection leaves room for a temporary one
    /// next to it: 120 bytes for a namespace up to MongoDB 4.2, 255 since 4.4.
    pub(crate) fn fits(conn: &Connection, db: &str, name: &str) -> bool {
        let max = if conn.is_legacy() { 120 } else { 255 };
        db.len() + 1 + name.len() + ".rr-tmp-12345678".len() <= max
    }

    /// Puts the new collection in place of the old one. Fails with
    /// [`Stranded`] once the old one may be gone: the new one must stay.
    pub(crate) async fn commit(&self, conn: &Connection, db: &str) -> anyhow::Result<()> {
        if self.replace != Replace::Swap {
            return Ok(());
        }
        let rename = |drop_target: bool| doc! { "renameCollection": format!("{db}.{}", self.into), "to": format!("{db}.{}", self.name), "dropTarget": drop_target };
        match conn.run_command("admin", rename(true), None).await {
            // A view (or a time series) is not replaced by a rename: dropped first.
            Err(e) if error_code(&e) == Some(48) => {
                let moved = async {
                    conn.run_command(db, doc! { "drop": &self.name }, None).await?;
                    conn.run_command("admin", rename(false), None).await
                };
                if let Err(e) = moved.await {
                    return Err(Stranded { name: self.name.clone(), into: self.into.clone(), cause: error_message(&e) }.into());
                }
            }
            r => {
                r?;
            }
        }
        Ok(())
    }

    /// Drops the new collection of a replacement that did not complete,
    /// unless it holds the only copy left.
    pub(crate) async fn discard(&self, conn: &Connection, db: &str) {
        if self.replace == Replace::Swap {
            let _ = conn.run_command(db, doc! { "drop": &self.into }, None).await;
        }
    }
}

/// Inserts the documents of a file. Write errors (duplicate keys…) do not
/// stop the import: they are counted, like with mongoimport. With "drop
/// first", the collection is replaced only by all the documents of the file.
pub async fn import(conn: &Connection, o: &ImportOptions, cancel: Arc<AtomicBool>, progress: impl Fn(Progress)) -> anyhow::Result<Imported> {
    let mut checked = 0;
    let mut counts = read_file(o, &cancel, |batch| batch.len() as u64)?;
    while let Some(n) = counts.next().await {
        checked += n?;
        progress(Progress::Checked(checked));
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled: nothing was imported");
    }
    let mut done = Imported::default();
    if !o.drop_first {
        return match fill(conn, o, &o.coll, &cancel, &progress, &mut done).await {
            Ok(()) => Ok(done),
            Err(e) => Err(stopped(e, done.inserted)),
        };
    }
    let target = Target::replacing(conn, &o.db, &o.coll, true, |name| doc! { "create": name }).await?;
    let left = |why: String| anyhow!("{why}: {} was left as it was", o.coll);
    let filled = fill(conn, o, &target.into, &cancel, &progress, &mut done).await;
    if target.replace == Replace::Drop {
        return match filled {
            Ok(()) => Ok(done),
            Err(e) => Err(anyhow!("{}: {} was dropped first", stopped(e, done.inserted), o.coll)),
        };
    }
    let result = match filled {
        Err(e) => Err(left(error_message(&e))),
        Ok(()) if done.failed > 0 => Err(left(done.refused())),
        Ok(()) => target.commit(conn, &o.db).await.map_err(|e| if e.is::<Stranded>() { e } else { left(error_message(&e)) }),
    };
    if let Err(e) = &result
        && !e.is::<Stranded>()
    {
        target.discard(conn, &o.db).await;
    }
    result.map(|_| done)
}

/// `e`, saying how many documents were written before it.
fn stopped(e: anyhow::Error, inserted: u64) -> anyhow::Error {
    match (e.is::<Cancelled>(), inserted) {
        (true, 0) => anyhow!("cancelled: nothing was imported"),
        (true, n) => anyhow!("cancelled after {} were imported", docs(n)),
        (false, 0) => e,
        (false, n) => anyhow!("stopped after {} were imported: {}", docs(n), error_message(&e)),
    }
}

/// Reads the file of an import on a blocking thread, handing what `f`
/// makes of each batch.
fn read_file<T: Send + 'static>(o: &ImportOptions, cancel: &Arc<AtomicBool>, f: impl Fn(Vec<Document>) -> T + Send + 'static) -> anyhow::Result<ReadAhead<T>> {
    let file = std::fs::File::open(&o.path).with_context(|| format!("cannot open {}", o.path.display()))?;
    let opts = o.clone();
    Ok(ReadAhead::spawn(cancel, move |send| read_documents(BufReader::new(file), &opts, |batch| send(f(batch)))))
}

/// Inserts the documents of the file of an import into `into`.
async fn fill(conn: &Connection, o: &ImportOptions, into: &str, cancel: &Arc<AtomicBool>, progress: &impl Fn(Progress), done: &mut Imported) -> anyhow::Result<()> {
    let mut batches = read_file(o, cancel, |batch| batch)?;
    while let Some(batch) = batches.next().await {
        check_cancel(cancel)?;
        insert_batch(conn, &o.db, into, batch?, done).await?;
        progress(Progress::Written(done.inserted));
    }
    check_cancel(cancel)
}

/// Inserts documents, counting the ones refused (duplicate keys…) in `done`.
pub(crate) async fn insert_batch(conn: &Connection, db: &str, coll: &str, batch: Vec<Document>, done: &mut Imported) -> anyhow::Result<()> {
    insert_with(conn, db, doc! { "insert": coll }, batch, done).await
}

/// Like [`insert_batch`], with the fields of `cmd`, whose first one names
/// the collection.
pub(crate) async fn insert_with(conn: &Connection, db: &str, mut cmd: Document, batch: Vec<Document>, done: &mut Imported) -> anyhow::Result<()> {
    let count = batch.len() as u64;
    cmd.insert("documents", batch);
    cmd.insert("ordered", false);
    let reply = conn.run_command(db, cmd, None).await?;
    let errors = reply.get_array("writeErrors").map(|a| a.as_slice()).unwrap_or_default();
    let inserted = reply.get("n").and_then(bson_as_i64).map_or(count - errors.len() as u64, |n| n as u64);
    done.inserted += inserted;
    done.failed += count.saturating_sub(inserted);
    if done.first_error.is_none()
        && let Some(e) = errors.first().and_then(|e| e.as_document())
    {
        done.first_error = Some(e.get_str("errmsg").unwrap_or("write error").to_string());
    }
    if let Ok(wce) = reply.get_document("writeConcernError") {
        bail!("{}", wce.get_str("errmsg").unwrap_or("write concern error"));
    }
    Ok(())
}

/// Documents read from a file, given to `send` by batches.
struct Batches<F> {
    batch: Batcher,
    send: F,
}

impl<F: FnMut(Vec<Document>) -> bool> Batches<F> {
    /// Adds a document; false once `send` asked to stop.
    fn push(&mut self, d: Document) -> anyhow::Result<bool> {
        let size = bson::to_vec(&d)?.len();
        if size > MAX_DOCUMENT {
            bail!("{size} bytes, more than the 16 MB of a MongoDB document");
        }
        Ok(self.batch.push(d, size).is_none_or(&mut self.send))
    }

    fn finish(mut self) {
        if !self.batch.is_empty() {
            (self.send)(self.batch.take());
        }
    }
}

/// Parses a file into batches of documents, given to `send` until it returns false.
pub fn read_documents(mut r: impl BufRead, o: &ImportOptions, send: impl FnMut(Vec<Document>) -> bool) -> anyhow::Result<()> {
    let mut out = Batches { batch: Batcher::default(), send };
    match o.format {
        Format::Csv => {
            let mut rows = CsvRows::new(r, o.delimiter.char());
            let Some(first) = rows.next() else { return Ok(()) };
            let header = first.and_then(|cells| CsvHeader::new(&cells)).context("line 1")?;
            while let Some(row) = rows.next() {
                let line = rows.row_line;
                let row = row.with_context(|| format!("line {line}"))?;
                if row.len() == 1 && row[0].is_empty() {
                    continue;
                }
                let d = header.document(&row, o.detect_types).with_context(|| format!("line {line}"))?;
                if !out.push(d).with_context(|| format!("line {line}"))? {
                    return Ok(());
                }
            }
        }
        Format::JsonLines | Format::JsonArray => {
            // An array, or documents one after the other (one per line,
            // or pretty-printed).
            let first = loop {
                let buf = r.fill_buf()?;
                match buf.iter().position(|b| !b.is_ascii_whitespace() && *b != 0xef && *b != 0xbb && *b != 0xbf) {
                    Some(i) => {
                        let c = buf[i];
                        r.consume(i);
                        break Some(c);
                    }
                    None if buf.is_empty() => break None,
                    None => {
                        let n = buf.len();
                        r.consume(n);
                    }
                }
            };
            match first {
                None => {}
                Some(b'[') => {
                    let mut n = 0;
                    let mut stopped = false;
                    each_item(r, &mut |item| {
                        n += 1;
                        let d = json_document(item).and_then(|d| out.push(d)).with_context(|| format!("document {n}"))?;
                        stopped = !d;
                        Ok(d)
                    })?;
                    if stopped {
                        return Ok(());
                    }
                }
                Some(_) => {
                    for (i, item) in serde_json::Deserializer::from_reader(r).into_iter::<serde_json::Value>().enumerate() {
                        let n = i + 1;
                        let item = item.with_context(|| format!("invalid JSON in document {n}"))?;
                        if !json_document(item).and_then(|d| out.push(d)).with_context(|| format!("document {n}"))? {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    out.finish();
    Ok(())
}

/// Gives the items of the JSON array `r` holds to `f`, one by one, without
/// holding the array. `f` returns false to stop.
fn each_item(r: impl Read, f: &mut dyn FnMut(serde_json::Value) -> anyhow::Result<bool>) -> anyhow::Result<()> {
    struct Items<'a> {
        f: &'a mut dyn FnMut(serde_json::Value) -> anyhow::Result<bool>,
        given: &'a mut usize,
        /// Why `f` stopped the reading, if it did.
        stop: &'a mut Option<anyhow::Result<()>>,
    }
    impl<'de> serde::de::Visitor<'de> for Items<'_> {
        type Value = ();

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an array of documents")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            while let Some(item) = seq.next_element::<serde_json::Value>()? {
                *self.given += 1;
                match (self.f)(item) {
                    Ok(true) => {}
                    other => {
                        *self.stop = Some(other.map(|_| ()));
                        return Err(serde::de::Error::custom("stopped"));
                    }
                }
            }
            Ok(())
        }
    }
    let (mut given, mut stop) = (0, None);
    let mut de = serde_json::Deserializer::from_reader(r);
    let read = serde::Deserializer::deserialize_seq(&mut de, Items { f, given: &mut given, stop: &mut stop });
    if let Some(stopped) = stop {
        return stopped;
    }
    read.with_context(|| format!("invalid JSON in document {}", given + 1))?;
    de.end().context("invalid JSON after the array")
}

/// A document from Extended JSON, relaxed or canonical, or in the legacy
/// forms of mongoexport 4.0 and older.
fn json_document(v: serde_json::Value) -> anyhow::Result<Document> {
    match json_value(v)? {
        Bson::Document(d) => Ok(d),
        other => bail!("expected a document, got {}", crate::bsonfmt::type_name(&other)),
    }
}

/// The keys that make an object a value of Extended JSON v2.
const EXTJSON_KEYS: &[&str] = &[
    "$oid", "$symbol", "$regularExpression", "$numberInt", "$numberLong", "$numberDouble", "$numberDecimal", "$binary", "$uuid",
    "$code", "$timestamp", "$date", "$minKey", "$maxKey", "$dbPointer", "$undefined",
];

/// A value of Extended JSON. Unlike the conversion of the bson crate, a
/// number too big for 64 bits is an error rather than a rounded double,
/// and the legacy forms are read as what they stand for, not as documents.
pub(crate) fn json_value(v: serde_json::Value) -> anyhow::Result<Bson> {
    use serde_json::Value;
    Ok(match v {
        Value::Number(n) if n.is_u64() && !n.is_i64() => bail!("{n} does not fit in a 64-bit integer"),
        Value::Array(items) => Bson::Array(items.into_iter().map(json_value).collect::<anyhow::Result<_>>()?),
        Value::Object(map) => {
            if let Some(b) = legacy_value(&map)? {
                return Ok(b);
            }
            if map.keys().any(|k| EXTJSON_KEYS.contains(&k.as_str())) {
                Bson::try_from(Value::Object(map)).map_err(|e| anyhow!("{e}"))?
            } else {
                let fields = map.into_iter().map(|(k, v)| Ok((k, json_value(v)?))).collect::<anyhow::Result<Vec<(String, Bson)>>>()?;
                Bson::Document(fields.into_iter().collect())
            }
        }
        other => Bson::try_from(other).map_err(|e| anyhow!("{e}"))?,
    })
}

/// The legacy forms of Extended JSON (mongoexport 4.0 and older):
/// `{"$binary": "<base64>", "$type": "<hex>"}`, `{"$regex": "…", "$options": "…"}`
/// and dates with an offset like `+0100`.
fn legacy_value(map: &serde_json::Map<String, serde_json::Value>) -> anyhow::Result<Option<Bson>> {
    use base64::Engine;
    use serde_json::Value;
    let only = |keys: &[&str]| map.keys().all(|k| keys.contains(&k.as_str()));
    match (map.get("$binary"), map.get("$type"), map.get("$regex"), map.get("$options"), map.get("$date")) {
        (Some(Value::String(b64)), Some(Value::String(t)), _, _, _) if map.len() == 2 => {
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64).context("$binary: invalid base64")?;
            let subtype = u8::from_str_radix(t, 16).context("$type: invalid binary subtype")?;
            Ok(Some(Bson::Binary(bson::Binary { subtype: subtype.into(), bytes })))
        }
        (_, _, Some(Value::String(pattern)), options, _) if only(&["$regex", "$options"]) => {
            let options = match options {
                None => "",
                Some(Value::String(o)) => o.as_str(),
                Some(_) => bail!("$options: expected a string"),
            };
            // In alphabetical order, like the bson crate reads them.
            let mut options: Vec<char> = options.chars().collect();
            options.sort_unstable();
            Ok(Some(Bson::RegularExpression(bson::Regex { pattern: pattern.clone(), options: options.into_iter().collect() })))
        }
        (_, _, _, _, Some(Value::String(date))) if map.len() == 1 && bson::DateTime::parse_rfc3339_str(date).is_err() => {
            let d = chrono::DateTime::parse_from_str(date, "%Y-%m-%dT%H:%M:%S%.f%z").with_context(|| format!("$date: invalid date {date}"))?;
            Ok(Some(Bson::DateTime(bson::DateTime::from_millis(d.timestamp_millis()))))
        }
        _ => Ok(None),
    }
}

// ------------------------------------------------------------------ export

struct Writer<W: Write> {
    out: W,
    format: Format,
    canonical: bool,
    delimiter: char,
    fields: Vec<String>,
    count: u64,
}

impl<W: Write> Writer<W> {
    fn new(out: W, o: &ExportOptions) -> Self {
        let fields = o.fields.iter().map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect();
        Writer { out, format: o.format, canonical: o.canonical, delimiter: o.delimiter.char(), fields, count: 0 }
    }

    fn json(&self, d: &Document) -> String {
        let b = Bson::Document(d.clone());
        let v = if self.canonical { b.into_canonical_extjson() } else { b.into_relaxed_extjson() };
        v.to_string()
    }

    fn write(&mut self, docs: &[Document]) -> std::io::Result<()> {
        for d in docs {
            match self.format {
                Format::JsonLines => writeln!(self.out, "{}", self.json(d))?,
                Format::JsonArray => {
                    let sep = if self.count == 0 { "[\n" } else { ",\n" };
                    write!(self.out, "{sep}{}", self.json(d))?;
                }
                Format::Csv => {
                    if self.count == 0 {
                        if self.fields.is_empty() {
                            self.fields = csv_fields(docs);
                        }
                        let header: Vec<&str> = self.fields.iter().map(String::as_str).collect();
                        csv_line(&mut self.out, &header, self.delimiter)?;
                    }
                    let cells: Vec<String> = self.fields.iter().map(|f| get_path(d, f).map(csv_cell).unwrap_or_default()).collect();
                    let cells: Vec<&str> = cells.iter().map(String::as_str).collect();
                    csv_line(&mut self.out, &cells, self.delimiter)?;
                }
            }
            self.count += 1;
        }
        Ok(())
    }

    fn finish(mut self) -> std::io::Result<()> {
        match self.format {
            Format::JsonArray if self.count == 0 => writeln!(self.out, "[]")?,
            Format::JsonArray => writeln!(self.out, "\n]")?,
            // Nothing to export: the header of the columns asked for.
            Format::Csv if self.count == 0 && !self.fields.is_empty() => {
                let header: Vec<&str> = self.fields.iter().map(String::as_str).collect();
                csv_line(&mut self.out, &header, self.delimiter)?;
            }
            _ => {}
        }
        self.out.flush()
    }
}

/// Dotted paths of the values of `docs` that are not documents, in the order
/// they first appear. Arrays are one column.
pub fn csv_fields(docs: &[Document]) -> Vec<String> {
    fn walk(d: &Document, prefix: &str, out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>) {
        for (k, v) in d {
            let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            match v {
                Bson::Document(sub) if !sub.is_empty() => walk(sub, &path, out, seen),
                _ => {
                    if seen.insert(path.clone()) {
                        out.push(path);
                    }
                }
            }
        }
    }
    let (mut out, mut seen) = (Vec::new(), Default::default());
    for d in docs {
        walk(d, "", &mut out, &mut seen);
    }
    out
}

/// The value at a dotted path.
pub fn get_path<'a>(d: &'a Document, path: &str) -> Option<&'a Bson> {
    if let Some(v) = d.get(path) {
        return Some(v);
    }
    let (head, rest) = path.split_once('.')?;
    match d.get(head)? {
        Bson::Document(sub) => get_path(sub, rest),
        _ => None,
    }
}

/// Text of a CSV cell. Strings as they are; other values as mongoexport
/// writes them, or in Extended JSON, so that an import reads them back with
/// their type.
pub fn csv_cell(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::Null | Bson::Undefined => String::new(),
        Bson::Boolean(b) => b.to_string(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        // NaN and the infinities as {"$numberDouble": "NaN"}.
        Bson::Double(f) if !f.is_finite() => v.clone().into_relaxed_extjson().to_string(),
        // A point or an exponent: read back as a double, not an integer.
        Bson::Double(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{f:.1}"),
        Bson::Double(f) if f.fract() == 0.0 => format!("{f:e}"),
        Bson::Double(f) => f.to_string(),
        Bson::ObjectId(o) => format!("ObjectId({o})"),
        // Before year 0 or after 9999: {"$date": {"$numberLong": "…"}}.
        Bson::DateTime(d) => d.try_to_rfc3339_string().unwrap_or_else(|_| v.clone().into_relaxed_extjson().to_string()),
        Bson::Decimal128(d) => d.to_string(),
        other => other.clone().into_relaxed_extjson().to_string(),
    }
}

fn csv_line(out: &mut impl Write, cells: &[&str], delimiter: char) -> std::io::Result<()> {
    let mut line = String::new();
    for (i, c) in cells.iter().enumerate() {
        if i > 0 {
            line.push(delimiter);
        }
        let quote = c.contains([delimiter, '"', '\n', '\r']) || c.starts_with(' ') || c.ends_with(' ');
        if quote {
            line.push('"');
            line.push_str(&c.replace('"', "\"\""));
            line.push('"');
        } else {
            line.push_str(c);
        }
    }
    line.push_str("\r\n");
    out.write_all(line.as_bytes())
}

// ------------------------------------------------------------------ import

/// Rows of a CSV file (RFC 4180: quoted fields may hold delimiters, quotes
/// and line breaks), read line by line.
pub struct CsvRows<R> {
    r: R,
    delimiter: char,
    /// The line being read, from `pos`.
    buf: String,
    pos: usize,
    /// Number of the line being read.
    line: u64,
    /// Line where the last row given starts.
    pub row_line: u64,
}

impl<R: BufRead> CsvRows<R> {
    pub fn new(r: R, delimiter: char) -> Self {
        CsvRows { r, delimiter, buf: String::new(), pos: 0, line: 0, row_line: 0 }
    }

    /// Reads the next line, if any.
    fn read_line(&mut self) -> anyhow::Result<bool> {
        self.buf.clear();
        self.pos = 0;
        // A file of another kind may have no line break: not held whole.
        match (&mut self.r).take(MAX_DOCUMENT as u64 + 1).read_line(&mut self.buf) {
            Ok(0) => return Ok(false),
            Ok(n) if n > MAX_DOCUMENT => bail!("a line of more than 16 MB: is this a CSV file?"),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => bail!("the file is not UTF-8 text"),
            Err(e) => return Err(e.into()),
        }
        self.line += 1;
        if self.line == 1 && self.buf.starts_with('\u{feff}') {
            self.pos = '\u{feff}'.len_utf8();
        }
        Ok(true)
    }
}

impl<R: BufRead> Iterator for CsvRows<R> {
    type Item = anyhow::Result<Vec<String>>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut row = Vec::new();
        let mut cell = String::new();
        let (mut quoted, mut was_quoted, mut started) = (false, false, false);
        // The rest of the line after a lone \r, or the next line.
        self.row_line = if self.pos < self.buf.len() { self.line } else { self.line + 1 };
        loop {
            if self.pos >= self.buf.len() {
                match self.read_line() {
                    Err(e) => return Some(Err(e)),
                    Ok(true) => {}
                    Ok(false) if !started => return None,
                    Ok(false) if quoted => return Some(Err(anyhow!("unterminated quoted field"))),
                    Ok(false) => {
                        row.push(cell);
                        return Some(Ok(row));
                    }
                }
            }
            started = true;
            let mut chars = self.buf[self.pos..].char_indices().peekable();
            while let Some((i, c)) = chars.next() {
                if quoted {
                    if c == '"' {
                        if chars.peek().is_some_and(|(_, n)| *n == '"') {
                            chars.next();
                            cell.push('"');
                        } else {
                            quoted = false;
                        }
                    } else {
                        cell.push(c);
                    }
                    continue;
                }
                match c {
                    '"' if cell.is_empty() && !was_quoted => {
                        quoted = true;
                        was_quoted = true;
                    }
                    c if c == self.delimiter => {
                        row.push(std::mem::take(&mut cell));
                        was_quoted = false;
                    }
                    '\r' | '\n' => {
                        let mut end = i + 1;
                        if c == '\r' && chars.peek().is_some_and(|(_, n)| *n == '\n') {
                            end += 1;
                        }
                        self.pos += end;
                        row.push(cell);
                        return Some(Ok(row));
                    }
                    c => cell.push(c),
                }
            }
            if cell.len() > MAX_DOCUMENT {
                return Some(Err(anyhow!("a field of more than 16 MB: a quote may be missing")));
            }
            self.pos = self.buf.len();
        }
    }
}

/// The field names of a CSV file, from its first line.
pub struct CsvHeader {
    names: Vec<String>,
}

impl CsvHeader {
    /// Fails when two names write the same field: one would be lost.
    pub fn new(cells: &[String]) -> anyhow::Result<CsvHeader> {
        if cells.iter().all(|c| c.trim().is_empty()) {
            bail!("the first line must hold the field names");
        }
        // Named like mongoimport names the cells past the last name.
        let names: Vec<String> = cells.iter().enumerate().map(|(i, c)| if c.trim().is_empty() { format!("field{i}") } else { c.trim().to_string() }).collect();
        for (i, name) in names.iter().enumerate() {
            for before in &names[..i] {
                same_field(before, name)?;
            }
        }
        Ok(CsvHeader { names })
    }

    /// A document from a row: dotted names make sub-documents, empty cells
    /// are left out.
    pub fn document(&self, row: &[String], detect_types: bool) -> anyhow::Result<Document> {
        let mut d = Document::new();
        for (i, text) in row.iter().enumerate() {
            if text.is_empty() {
                continue;
            }
            let extra;
            let name = match self.names.get(i) {
                Some(name) => name.as_str(),
                None => {
                    extra = format!("field{i}");
                    for name in &self.names {
                        same_field(name, &extra)?;
                    }
                    &extra
                }
            };
            let v = if detect_types { csv_value(text) } else { Bson::String(text.clone()) };
            set_path(&mut d, name, v);
        }
        Ok(d)
    }
}

/// Fails when `a` and `b` write the same field: `a` twice, or `a` and `a.b`.
fn same_field(a: &str, b: &str) -> anyhow::Result<()> {
    let (pa, pb) = (path_keys(a), path_keys(b));
    let n = pa.len().min(pb.len());
    if pa[..n] != pb[..n] {
        Ok(())
    } else if a == b {
        bail!("the field name \"{a}\" is given twice")
    } else {
        bail!("the fields \"{a}\" and \"{b}\" write the same field")
    }
}

/// The keys [`set_path`] goes through.
fn path_keys(path: &str) -> Vec<&str> {
    let mut keys = Vec::new();
    let mut rest = path;
    while let Some((head, tail)) = rest.split_once('.').filter(|(h, t)| !h.is_empty() && !t.is_empty()) {
        keys.push(head);
        rest = tail;
    }
    keys.push(rest);
    keys
}

fn set_path(d: &mut Document, path: &str, v: Bson) {
    match path.split_once('.') {
        Some((head, rest)) if !head.is_empty() && !rest.is_empty() => {
            if !matches!(d.get(head), Some(Bson::Document(_))) {
                d.insert(head, Document::new());
            }
            if let Some(Bson::Document(sub)) = d.get_mut(head) {
                set_path(sub, rest, v);
            }
        }
        _ => {
            d.insert(path, v);
        }
    }
}

/// The value a CSV cell stands for: what [`csv_cell`] writes is read back
/// with its type; anything else stays a string, and so does what a type
/// would change: "007", a 20-digit number, the 6 decimals of a time.
pub fn csv_value(text: &str) -> Bson {
    match text {
        "true" => return Bson::Boolean(true),
        "false" => return Bson::Boolean(false),
        _ => {}
    }
    if let Some(n) = csv_number(text) {
        return n;
    }
    if let Some(hex) = text.strip_prefix("ObjectId(").and_then(|t| t.strip_suffix(')'))
        && let Ok(o) = bson::oid::ObjectId::parse_str(hex.trim_matches(['"', '\'']))
    {
        return Bson::ObjectId(o);
    }
    // Full ISO dates only: 2024-01-02T03:04:05Z, with milliseconds or an offset.
    if text.len() >= 20 && text.as_bytes()[4] == b'-' && text.as_bytes()[10] == b'T'
        && !finer_than_millis(text)
        && let Ok(d) = bson::DateTime::parse_rfc3339_str(text)
    {
        return Bson::DateTime(d);
    }
    let container = (text.starts_with('{') && text.ends_with('}')) || (text.starts_with('[') && text.ends_with(']'));
    if container
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(text)
        && let Ok(b) = json_value(v)
    {
        return b;
    }
    Bson::String(text.to_string())
}

/// A number its type holds as written: an integer of 32 or 64 bits, or a
/// double with all its digits. `None` for "007" (a code), "-0", a number
/// too big for 64 bits, or with more digits than a double keeps.
fn csv_number(text: &str) -> Option<Bson> {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let int = unsigned.bytes().take_while(u8::is_ascii_digit).count();
    if int == 0 || (int > 1 && unsigned.starts_with('0')) {
        return None;
    }
    let rest = &unsigned[int..];
    if rest.is_empty() {
        if text == "-0" {
            return None;
        }
        return text.parse::<i32>().map(Bson::Int32).or_else(|_| text.parse::<i64>().map(Bson::Int64)).ok();
    }
    if !rest.starts_with(['.', 'e', 'E']) || !rest.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')) {
        return None;
    }
    let f: f64 = text.parse().ok()?;
    (f.is_finite() && significant_digits(text) == significant_digits(&format!("{f:e}"))).then_some(Bson::Double(f))
}

/// The digits of a number that count, without leading or trailing zeros.
fn significant_digits(number: &str) -> String {
    let mantissa = number.split(['e', 'E']).next().unwrap_or_default();
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    digits.trim_start_matches('0').trim_end_matches('0').to_string()
}

/// Whether an ISO date has digits past the milliseconds, which a BSON date
/// does not keep.
fn finer_than_millis(text: &str) -> bool {
    text[11..].split_once('.').is_some_and(|(_, fraction)| fraction.bytes().take_while(u8::is_ascii_digit).skip(3).any(|b| b != b'0'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::spec::BinarySubtype;
    use bson::{Binary, DateTime, Decimal128, Timestamp, oid::ObjectId};

    fn export_opts(format: Format) -> ExportOptions {
        ExportOptions {
            db: "db".into(),
            coll: "c".into(),
            filter: Document::new(),
            projection: None,
            sort: None,
            skip: 0,
            limit: 0,
            format,
            canonical: false,
            fields: Vec::new(),
            delimiter: Delimiter::Comma,
            path: PathBuf::new(),
        }
    }

    fn import_opts(format: Format) -> ImportOptions {
        ImportOptions { db: "db".into(), coll: "c".into(), format, delimiter: Delimiter::Comma, detect_types: true, drop_first: false, path: PathBuf::new() }
    }

    fn written(o: &ExportOptions, batches: &[Vec<Document>]) -> String {
        let mut out = Vec::new();
        let mut w = Writer::new(&mut out, o);
        for b in batches {
            w.write(b).unwrap();
        }
        w.finish().unwrap();
        String::from_utf8(out).unwrap()
    }

    fn read(o: &ImportOptions, text: &str) -> anyhow::Result<Vec<Document>> {
        read_from(o, text.as_bytes())
    }

    fn read_from(o: &ImportOptions, r: impl BufRead) -> anyhow::Result<Vec<Document>> {
        let mut docs = Vec::new();
        read_documents(r, o, |b| {
            docs.extend(b);
            true
        })?;
        Ok(docs)
    }

    /// The sizes of the batches a file is read in, until it fails.
    fn batches(o: &ImportOptions, r: impl BufRead) -> (Vec<usize>, anyhow::Result<()>) {
        let mut sizes = Vec::new();
        let result = read_documents(r, o, |b| {
            sizes.push(b.len());
            true
        });
        (sizes, result)
    }

    /// Byte for byte: types, the sign of zero and NaN included, which `==`
    /// on documents does not see.
    fn assert_same(got: &[Document], expected: &[Document], what: &str) {
        assert_eq!(got.len(), expected.len(), "{what}: number of documents");
        for (g, e) in got.iter().zip(expected) {
            if bson::to_vec(g).unwrap() == bson::to_vec(e).unwrap() {
                continue;
            }
            for (k, v) in e {
                let other = g.get(k);
                let same = other.is_some_and(|o| bson::to_vec(&doc! { "v": o.clone() }).unwrap() == bson::to_vec(&doc! { "v": v.clone() }).unwrap());
                assert!(same, "{what}: field {k}: {other:?} instead of {v:?}");
            }
            panic!("{what}: fields in another order or added: {g:?}");
        }
    }

    fn sample() -> Vec<Document> {
        let oid = ObjectId::parse_str("5f1d7a0e1c9d440000a1b2c3").unwrap();
        let date = DateTime::parse_rfc3339_str("2024-01-02T03:04:05.006Z").unwrap();
        vec![
            doc! { "_id": oid, "name": "Ada, \"the\" first", "n": 1, "big": 5_000_000_000i64, "f": 1.5, "whole": 2.0, "ok": true, "at": date, "address": { "city": "Paris", "zip": "007" }, "tags": ["a", 1] },
            doc! { "_id": 2, "name": "multi\nline", "extra": null },
        ]
    }

    /// A value of each BSON type, with the edge values conversions lose
    /// most: the sign of zero, NaN, the limits of each number and date,
    /// every binary subtype, odd keys, deep nesting.
    fn every_type() -> Document {
        let bin = |subtype: u8, bytes: &[u8]| Bson::Binary(Binary { subtype: BinarySubtype::from(subtype), bytes: bytes.to_vec() });
        let dec = |s: &str| Bson::Decimal128(s.parse::<Decimal128>().unwrap());
        let date = DateTime::from_millis;
        let pointer = Bson::try_from(serde_json::json!({ "$dbPointer": { "$ref": "c", "$id": { "$oid": "5f1d7a0e1c9d440000a1b2c4" } } })).unwrap();
        let mut deep = doc! { "bottom": 1 };
        for _ in 0..90 {
            deep = doc! { "d": deep };
        }
        let mut d = doc! {
            "_id": ObjectId::parse_str("5f1d7a0e1c9d440000a1b2c3").unwrap(),
            "double": 1.5, "whole": 2.0, "negZero": -0.0, "nan": f64::NAN, "inf": f64::INFINITY, "ninf": f64::NEG_INFINITY,
            "tiny": 5e-324, "huge": f64::MAX, "third": 1.0 / 3.0, "big": 1e300,
            "int": 7, "intMin": i32::MIN, "intMax": i32::MAX,
            "long": 9_007_199_254_740_993i64, "smallLong": 5i64, "longMin": i64::MIN, "longMax": i64::MAX,
        };
        d.extend(doc! {
            "dec": dec("123.456"), "decNaN": dec("NaN"), "decInf": dec("-Infinity"), "decTiny": dec("1E-6176"), "decNegZero": dec("-0"), "decLong": dec("1234567890123456789012345678901234"),
            "string": "héllo \0 nul \n \"q\" \\ \u{1F600} שלום مرحبا 你好", "empty": "",
            "date": date(1_706_933_106_789), "before1970": date(-1), "year1": date(-62_135_596_800_000), "after9999": date(253_402_300_800_000), "dateMin": DateTime::MIN, "dateMax": DateTime::MAX,
        });
        d.extend(doc! {
            "binary": bin(0, &[1, 2, 3]), "function": bin(1, &[1]), "old": bin(2, &[1, 2]), "legacyUuid": bin(3, &[7; 16]), "uuid": bin(4, &[8; 16]),
            "md5": bin(5, &[9; 16]), "encrypted": bin(6, &[1]), "column": bin(7, &[1]), "sensitive": bin(8, &[1]), "user": bin(0x80, &[0xff]), "emptyBinary": bin(0, &[]),
            "regex": Bson::RegularExpression(bson::Regex { pattern: "ab+c".into(), options: "im".into() }), "emptyRegex": Bson::RegularExpression(bson::Regex { pattern: String::new(), options: String::new() }),
            "ts": Timestamp { time: 10, increment: 2 }, "tsMax": Timestamp { time: u32::MAX, increment: u32::MAX },
        });
        d.extend(doc! {
            "min": Bson::MinKey, "max": Bson::MaxKey, "null": Bson::Null, "undefined": Bson::Undefined, "yes": true, "no": false,
            "symbol": Bson::Symbol("sym".into()), "code": Bson::JavaScriptCode("function () { return 1 }".into()),
            "codeScope": Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope { code: "function () { return x }".into(), scope: doc! { "x": 1i64 } }),
            "pointer": pointer,
            "array": [1, "a", { "x": true }, [], [[1i64]]], "emptyArray": [], "emptyDoc": {},
            "keys": { "": 1, "a.b": 2, "$dollar": 3, "ünï": 4, "with space": 5, "_id": 6 },
            "deep": deep,
        });
        d
    }

    /// `v` as relaxed Extended JSON gives it back: a long that fits in 32
    /// bits is read as an int, like with mongoimport.
    fn relaxed(v: &Bson) -> Bson {
        match v {
            Bson::Int64(n) if i32::try_from(*n).is_ok() => Bson::Int32(*n as i32),
            Bson::Document(d) => Bson::Document(d.iter().map(|(k, v)| (k.clone(), relaxed(v))).collect()),
            Bson::Array(a) => Bson::Array(a.iter().map(relaxed).collect()),
            Bson::JavaScriptCodeWithScope(c) => {
                let Bson::Document(scope) = relaxed(&Bson::Document(c.scope.clone())) else { unreachable!() };
                Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope { code: c.code.clone(), scope })
            }
            other => other.clone(),
        }
    }

    #[test]
    fn json_lines_round_trip() {
        let docs = sample();
        let text = written(&export_opts(Format::JsonLines), std::slice::from_ref(&docs));
        assert_eq!(text.lines().count(), 2);
        assert!(text.starts_with(r#"{"_id":{"$oid":"5f1d7a0e1c9d440000a1b2c3"},"name":"#), "{text}");
        assert_eq!(read(&import_opts(Format::JsonLines), &text).unwrap(), docs, "types kept");
        let canonical = written(&ExportOptions { canonical: true, ..export_opts(Format::JsonLines) }, std::slice::from_ref(&docs));
        assert!(canonical.contains(r#""n":{"$numberInt":"1"}"#));
        assert_eq!(read(&import_opts(Format::JsonLines), &canonical).unwrap(), docs);
    }

    #[test]
    fn json_array_round_trip() {
        let docs = sample();
        let text = written(&export_opts(Format::JsonArray), &[docs[..1].to_vec(), docs[1..].to_vec()]);
        assert!(text.starts_with("[\n{") && text.ends_with("}\n]\n"), "{text}");
        assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok(), "valid JSON");
        assert_eq!(read(&import_opts(Format::JsonLines), &text).unwrap(), docs, "arrays are recognised");
        assert_eq!(written(&export_opts(Format::JsonArray), &[]), "[]\n");
        assert!(read(&import_opts(Format::JsonLines), "[]").unwrap().is_empty());
        assert!(read(&import_opts(Format::JsonLines), " [ ] ").unwrap().is_empty());
    }

    #[test]
    fn canonical_json_keeps_every_type_and_value() {
        let d = every_type();
        for format in [Format::JsonLines, Format::JsonArray] {
            let text = written(&ExportOptions { canonical: true, ..export_opts(format) }, &[vec![d.clone(), d.clone()]]);
            assert_same(&read(&import_opts(format), &text).unwrap(), &[d.clone(), d.clone()], &format!("{format:?}"));
        }
    }

    #[test]
    fn relaxed_json_changes_only_small_longs() {
        let d = every_type();
        let text = written(&export_opts(Format::JsonLines), std::slice::from_ref(&vec![d.clone()]));
        assert!(text.contains(r#""smallLong":5,"#) && text.contains(r#""long":9007199254740993,"#), "{text}");
        let Bson::Document(expected) = relaxed(&Bson::Document(d)) else { unreachable!() };
        assert_same(&read(&import_opts(Format::JsonLines), &text).unwrap(), &[expected], "relaxed");
    }

    #[test]
    fn json_import_reads_pretty_documents_and_reports_errors() {
        let text = "\u{feff}{\n  \"a\": 1\n}\n\n{\"b\": {\"$date\": \"2020-01-01T00:00:00Z\"}}";
        let docs = read(&import_opts(Format::JsonLines), text).unwrap();
        assert_eq!(docs[0], doc! { "a": 1 });
        assert!(matches!(docs[1].get("b"), Some(Bson::DateTime(_))));
        let e = read(&import_opts(Format::JsonLines), "{\"a\":1}\n{\"a\":").unwrap_err();
        assert!(format!("{e:#}").contains("document 2"), "{e:#}");
        let e = read(&import_opts(Format::JsonLines), "[1]").unwrap_err();
        assert!(format!("{e:#}").contains("document 1: expected a document, got Int32"), "{e:#}");
        let e = read(&import_opts(Format::JsonLines), "{\"a\":1} 7").unwrap_err();
        assert!(format!("{e:#}").contains("document 2: expected a document"), "{e:#}");
        let e = read(&import_opts(Format::JsonLines), "[{\"a\":1}, {\"a\": {\"$numberLong\": 5}}]").unwrap_err();
        assert!(format!("{e:#}").contains("document 2"), "a wrong Extended JSON value: {e:#}");
        let e = read(&import_opts(Format::JsonLines), "[{\"a\":1}] {\"b\":2}").unwrap_err();
        assert!(format!("{e:#}").contains("after the array"), "{e:#}");
        assert!(read(&import_opts(Format::JsonLines), "  \n").unwrap().is_empty());
        assert!(read(&import_opts(Format::JsonLines), "").unwrap().is_empty());
        // A key order kept, the _id where it is.
        let docs = read(&import_opts(Format::JsonLines), r#"{"z":1,"a":2,"_id":3,"m":{"y":1,"b":2}}"#).unwrap();
        assert_eq!(docs[0].keys().collect::<Vec<_>>(), ["z", "a", "_id", "m"]);
        assert_eq!(docs[0].get_document("m").unwrap().keys().collect::<Vec<_>>(), ["y", "b"]);
    }

    #[test]
    fn json_numbers_are_never_rounded() {
        let docs = read(&import_opts(Format::JsonLines), r#"{"max":9223372036854775807,"min":-9223372036854775808,"i":2147483647,"l":2147483648,"f":1.0,"e":1e2}"#).unwrap();
        assert_eq!(docs[0], doc! { "max": i64::MAX, "min": i64::MIN, "i": i32::MAX, "l": 2_147_483_648i64, "f": 1.0, "e": 100.0 });
        let e = read(&import_opts(Format::JsonLines), r#"{"a":1}{"id":18446744073709551615}"#).unwrap_err();
        assert!(format!("{e:#}").contains("document 2: 18446744073709551615 does not fit in a 64-bit integer"), "{e:#}");
        let e = read(&import_opts(Format::JsonLines), r#"{"deep":[{"id":9223372036854775808}]}"#).unwrap_err();
        assert!(format!("{e:#}").contains("does not fit"), "also inside arrays: {e:#}");
        let e = read(&import_opts(Format::JsonLines), r#"{"a":1e400}"#).unwrap_err();
        assert!(format!("{e:#}").contains("document 1"), "out of range: {e:#}");
    }

    #[test]
    fn legacy_extended_json_is_read_as_what_it_stands_for() {
        // mongoexport 4.0 and older, and other tools.
        let text = r#"{"bin":{"$binary":"AQID","$type":"80"},"re":{"$regex":"^a","$options":"mi"},"re2":{"$regex":"b"},
            "date":{"$date":"2014-01-01T01:00:00.000+0100"},"ms":{"$date":1388534400000},"n":{"$numberLong":"5"},
            "ref":{"$ref":"c","$id":{"$oid":"5f1d7a0e1c9d440000a1b2c3"}},"query":{"$regex":{"$regularExpression":{"pattern":"x","options":""}}}}"#;
        let d = &read(&import_opts(Format::JsonLines), text).unwrap()[0];
        assert_eq!(d.get("bin"), Some(&Bson::Binary(Binary { subtype: BinarySubtype::UserDefined(0x80), bytes: vec![1, 2, 3] })));
        assert_eq!(d.get("re"), Some(&Bson::RegularExpression(bson::Regex { pattern: "^a".into(), options: "im".into() })));
        assert_eq!(d.get("re2"), Some(&Bson::RegularExpression(bson::Regex { pattern: "b".into(), options: String::new() })));
        let at = DateTime::from_millis(1_388_534_400_000);
        assert_eq!((d.get("date"), d.get("ms")), (Some(&Bson::DateTime(at)), Some(&Bson::DateTime(at))));
        assert_eq!(d.get("n"), Some(&Bson::Int64(5)));
        // A DBRef is a document; a $regex holding a value is too.
        assert_eq!(d.get_document("ref").unwrap(), &doc! { "$ref": "c", "$id": ObjectId::parse_str("5f1d7a0e1c9d440000a1b2c3").unwrap() });
        assert!(matches!(d.get_document("query").unwrap().get("$regex"), Some(Bson::RegularExpression(_))));
        for bad in [r#"{"b":{"$binary":"%%","$type":"00"}}"#, r#"{"b":{"$binary":"AQ==","$type":"zz"}}"#, r#"{"d":{"$date":"yesterday"}}"#, r#"{"r":{"$regex":"a","$options":1}}"#] {
            assert!(read(&import_opts(Format::JsonLines), bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn json_arrays_are_read_one_document_at_a_time() {
        // The first batches are given before the error further on: the
        // array is not held whole.
        let mut text = String::from("[");
        for i in 0..2500 {
            text.push_str(&format!("{{\"i\":{i}}},"));
        }
        text.push_str("oops]");
        let (sizes, result) = batches(&import_opts(Format::JsonArray), text.as_bytes());
        assert_eq!(sizes, vec![1000, 1000]);
        assert!(format!("{:#}", result.unwrap_err()).contains("invalid JSON in document 2501"));
        // And stops when asked.
        let mut calls = 0;
        read_documents(&b"[{\"a\":1},{\"a\":2}]"[..], &import_opts(Format::JsonArray), |_| {
            calls += 1;
            false
        })
        .unwrap();
        assert_eq!(calls, 1);
    }

    /// A file that cannot be read past its first `ok` bytes.
    fn failing_after(ok: Vec<u8>) -> impl BufRead {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk error"))
            }
        }
        BufReader::new(std::io::Cursor::new(ok).chain(Broken))
    }

    #[test]
    fn csv_files_are_read_line_by_line() {
        let mut text = String::from("i\n");
        for i in 0..1500 {
            text.push_str(&format!("{i}\n"));
        }
        let (sizes, result) = batches(&import_opts(Format::Csv), failing_after(text.into_bytes()));
        assert_eq!(sizes, vec![1000], "the first batch is given before the end of the file is read");
        assert!(format!("{:#}", result.unwrap_err()).contains("disk error"));
        let (sizes, result) = batches(&import_opts(Format::JsonLines), failing_after(b"{\"a\":1}\n".repeat(1500)));
        assert_eq!(sizes, vec![1000]);
        assert!(result.is_err());
    }

    #[test]
    fn large_imports_are_batched() {
        let text: String = (0..2500).map(|i| format!("{{\"i\":{i}}}\n")).collect();
        let (sizes, result) = batches(&import_opts(Format::JsonLines), text.as_bytes());
        result.unwrap();
        assert_eq!(sizes, vec![1000, 1000, 500]);
        let mut calls = 0;
        read_documents(text.as_bytes(), &import_opts(Format::JsonLines), |_| {
            calls += 1;
            false
        })
        .unwrap();
        assert_eq!(calls, 1, "stops when asked");
    }

    #[test]
    fn batches_stay_under_the_size_of_a_command() {
        // 7.5 MB of documents, then one of 9 MB: together they would be more
        // than the 16 MB of an insert command.
        let mut text = String::new();
        for i in 0..15 {
            text.push_str(&format!("{{\"i\":{i},\"s\":\"{}\"}}\n", "x".repeat(500 * 1024)));
        }
        text.push_str(&format!("{{\"big\":\"{}\"}}\n{{\"after\":1}}\n", "y".repeat(9 * 1024 * 1024)));
        let mut sizes = Vec::new();
        read_documents(text.as_bytes(), &import_opts(Format::JsonLines), |b| {
            sizes.push((b.len(), b.iter().map(|d| bson::to_vec(d).unwrap().len()).sum::<usize>()));
            true
        })
        .unwrap();
        assert_eq!(sizes.iter().map(|(n, _)| n).sum::<usize>(), 17);
        for (n, bytes) in &sizes {
            assert!(*n == 1 || *bytes <= BATCH_BYTES, "{sizes:?}");
        }
        // A document over 16 MB is refused, not sent.
        let text = format!("{{\"a\":1}}\n{{\"big\":\"{}\"}}\n", "z".repeat(MAX_DOCUMENT));
        let e = read(&import_opts(Format::JsonLines), &text).unwrap_err();
        let size = bson::to_vec(&doc! { "big": "z".repeat(MAX_DOCUMENT) }).unwrap().len();
        assert!(format!("{e:#}").contains(&format!("document 2: {size} bytes, more than the 16 MB of a MongoDB document")), "{e:#}");
        let text = format!("a\n1\n\"{}\"\n", "z".repeat(MAX_DOCUMENT));
        let e = read(&import_opts(Format::Csv), &text).unwrap_err();
        assert!(format!("{e:#}").contains("line 3: "), "{e:#}");
        // As does a key BSON cannot hold.
        assert!(read(&import_opts(Format::JsonLines), "{\"a\\u0000b\":1}").is_err());
    }

    #[test]
    fn csv_export() {
        let text = written(&export_opts(Format::Csv), &[sample()]);
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(lines[0], "_id,name,n,big,f,whole,ok,at,address.city,address.zip,tags,extra", "fields of the first batch, nested ones dotted");
        assert_eq!(
            lines[1],
            r#"ObjectId(5f1d7a0e1c9d440000a1b2c3),"Ada, ""the"" first",1,5000000000,1.5,2.0,true,2024-01-02T03:04:05.006Z,Paris,007,"[""a"",1]","#
        );
        assert!(text.contains("2,\"multi\nline\""), "line breaks quoted: {text}");
        let chosen = ExportOptions { fields: vec!["name".into(), " address.city ".into(), "missing".into()], delimiter: Delimiter::Semicolon, ..export_opts(Format::Csv) };
        let text = written(&chosen, &[sample()]);
        assert!(text.starts_with("name;address.city;missing\r\n\"Ada, \"\"the\"\" first\";Paris;\r\n"), "{text}");
        assert_eq!(written(&chosen, &[]), "name;address.city;missing\r\n", "header even without documents");
    }

    #[test]
    fn csv_round_trip_keeps_the_types() {
        let docs = sample();
        let text = written(&export_opts(Format::Csv), std::slice::from_ref(&docs));
        let back = read(&import_opts(Format::Csv), &text).unwrap();
        let mut expected = docs[0].clone();
        expected.insert("tags", Bson::Array(vec![Bson::String("a".into()), Bson::Int32(1)]));
        assert_eq!(back[0], expected);
        assert_eq!(back[1], doc! { "_id": 2, "name": "multi\nline" }, "empty cells left out");
        let strings = read(&ImportOptions { detect_types: false, ..import_opts(Format::Csv) }, "a,b\n1,true\n").unwrap();
        assert_eq!(strings[0], doc! { "a": "1", "b": "true" });
    }

    #[test]
    fn csv_cells_give_back_their_value() {
        let oid = ObjectId::parse_str("5f1d7a0e1c9d440000a1b2c3").unwrap();
        let same = |v: Bson| (v.clone(), v);
        let cases = [
            same(Bson::Int32(-7)),
            same(Bson::Int64(5_000_000_000)),
            same(Bson::Int64(i64::MIN)),
            same(Bson::Double(2.0)),
            same(Bson::Double(-0.0)),
            same(Bson::Double(0.1 + 0.2)),
            same(Bson::Double(1e15)),
            same(Bson::Double(1e300)),
            same(Bson::Double(-1.5e-300)),
            same(Bson::Double(f64::MAX)),
            same(Bson::Double(5e-324)),
            same(Bson::Double(f64::NAN)),
            same(Bson::Double(f64::INFINITY)),
            same(Bson::Double(f64::NEG_INFINITY)),
            same(Bson::Boolean(false)),
            same(Bson::ObjectId(oid)),
            same(Bson::DateTime(DateTime::from_millis(1_706_933_106_789))),
            same(Bson::DateTime(DateTime::from_millis(-1))),
            same(Bson::DateTime(DateTime::MIN)),
            same(Bson::DateTime(DateTime::MAX)),
            same(Bson::String("007".into())),
            same(Bson::String(" padded, \"quoted\"\r\n".into())),
            same(Bson::String("12345678901234567890".into())),
            same(Bson::String("3.14159265358979323846".into())),
            same(Bson::Binary(Binary { subtype: BinarySubtype::Uuid, bytes: vec![1; 16] })),
            same(Bson::RegularExpression(bson::Regex { pattern: "a,b".into(), options: "i".into() })),
            same(Bson::Timestamp(Timestamp { time: 1, increment: 2 })),
            same(Bson::MinKey),
            same(Bson::Symbol("s".into())),
            same(Bson::Document(Document::new())),
            same(Bson::Array(vec![Bson::Int32(1), Bson::String("a".into())])),
            // CSV has no types: what reads as another type comes back as it.
            (Bson::Int64(5), Bson::Int32(5)),
            (Bson::String("123".into()), Bson::Int32(123)),
            (Bson::String("true".into()), Bson::Boolean(true)),
            (Bson::Decimal128("123.456".parse().unwrap()), Bson::Double(123.456)),
        ];
        for (value, back) in cases {
            let text = written(&export_opts(Format::Csv), &[vec![doc! { "v": value.clone() }]]);
            assert_same(&read(&import_opts(Format::Csv), &text).unwrap(), &[doc! { "v": back }], &format!("{value:?} written as {text:?}"));
        }
        // Null, undefined and the empty string are empty cells: left out.
        for value in [Bson::Null, Bson::Undefined, Bson::String(String::new())] {
            let text = written(&ExportOptions { fields: vec!["v".into(), "w".into()], ..export_opts(Format::Csv) }, &[vec![doc! { "v": value, "w": 1 }]]);
            assert_eq!(read(&import_opts(Format::Csv), &text).unwrap(), vec![doc! { "w": 1 }]);
        }
    }

    #[test]
    fn csv_parsing() {
        let rows = |text: &str, delimiter: char| -> Vec<Vec<String>> { CsvRows::new(text.as_bytes(), delimiter).map(Result::unwrap).collect() };
        assert_eq!(rows("a,\"b,c\",\"d\"\"e\"\r\n\"x\ny\",,z\nlast", ','), vec![vec!["a", "b,c", "d\"e"], vec!["x\ny", "", "z"], vec!["last"]]);
        assert_eq!(rows("a\tb\n", '\t'), vec![vec!["a", "b"]]);
        assert_eq!(rows("a;b\rc;d\r\ne\n", ';'), vec![vec!["a", "b"], vec!["c", "d"], vec!["e"]], "old Mac line ends");
        assert_eq!(rows("\u{feff}a,b\n", ','), vec![vec!["a", "b"]], "byte order mark");
        assert_eq!(rows("\"a\"b,c\"d\n", ','), vec![vec!["ab", "c\"d"]], "stray quotes kept");
        assert_eq!(rows("a,\n,\n", ','), vec![vec!["a", ""], vec!["", ""]]);
        assert!(CsvRows::new(&b"\"open"[..], ',').next().unwrap().is_err());
        // Line numbers count the lines of quoted fields.
        let mut r = CsvRows::new(&b"h\n\"a\nb\nc\"\nnext\n"[..], ',');
        assert_eq!((r.next().unwrap().unwrap(), r.row_line), (vec!["h".to_string()], 1));
        assert_eq!((r.next().unwrap().unwrap(), r.row_line), (vec!["a\nb\nc".to_string()], 2));
        assert_eq!((r.next().unwrap().unwrap(), r.row_line), (vec!["next".to_string()], 5));
        assert!(r.next().is_none());
        let e = read(&import_opts(Format::Csv), "a\n1\n\"x\n2\n").unwrap_err();
        assert!(format!("{e:#}").contains("line 3: unterminated quoted field"), "{e:#}");
        assert!(read(&import_opts(Format::Csv), "").unwrap().is_empty());
        assert!(read(&import_opts(Format::Csv), ",\n1\n").is_err(), "a header is needed");
        // A file of another kind, without line breaks, is not read whole.
        let e = read(&import_opts(Format::Csv), &"x".repeat(MAX_DOCUMENT + 10)).unwrap_err();
        assert!(format!("{e:#}").contains("line 1: a line of more than 16 MB"), "{e:#}");
        // Excel on Windows writes Latin-1: refused, not mangled.
        let e = read_from(&import_opts(Format::Csv), &b"name\nAda\nCaf\xe9\n"[..]).unwrap_err();
        assert!(format!("{e:#}").contains("line 3: the file is not UTF-8 text"), "{e:#}");
        let docs = read(&import_opts(Format::Csv), "a,b\r\n\r\n1,2\r\n\r\n").unwrap();
        assert_eq!(docs, vec![doc! { "a": 1, "b": 2 }], "empty lines skipped");
    }

    #[test]
    fn csv_columns_never_overwrite_each_other() {
        let header = |line: &str| CsvHeader::new(&line.split(',').map(String::from).collect::<Vec<_>>());
        for bad in ["a,b,a", "a, a ", "a,a.b", "a.b,a", "a.b,a.b.c", "x.y.z,x.y", "a..b,a"] {
            let e = header(bad).err().unwrap_or_else(|| panic!("{bad}"));
            assert!(e.to_string().contains("given twice") || e.to_string().contains("write the same field"), "{bad}: {e}");
        }
        for good in ["a.b,a.c", "a,ab", "a.,a", "a..b,a.c"] {
            assert!(header(good).is_ok(), "{good}");
        }
        let e = read(&import_opts(Format::Csv), "a,b,a\n1,2,3\n").unwrap_err();
        assert!(format!("{e:#}").starts_with("line 1: the field name \"a\" is given twice"), "{e:#}");
        // Unnamed columns and cells past the last name: named like mongoimport does.
        let docs = read(&import_opts(Format::Csv), "a,,c\n1,2,3,4,5\n").unwrap();
        assert_eq!(docs, vec![doc! { "a": 1, "field1": 2, "c": 3, "field3": 4, "field4": 5 }]);
        let e = read(&import_opts(Format::Csv), "a,field2\n1,2\n1,2,3\n").unwrap_err();
        assert!(format!("{e:#}").starts_with("line 3: the field name \"field2\" is given twice"), "{e:#}");
        let docs = read(&import_opts(Format::Csv), "a.b,a.c\n1,2\n").unwrap();
        assert_eq!(docs, vec![doc! { "a": { "b": 1, "c": 2 } }]);
    }

    #[test]
    fn csv_values() {
        assert_eq!(csv_value("12"), Bson::Int32(12));
        assert_eq!(csv_value("-12"), Bson::Int32(-12));
        assert_eq!(csv_value("12345678901"), Bson::Int64(12345678901));
        assert_eq!(csv_value("-9223372036854775808"), Bson::Int64(i64::MIN));
        assert_eq!(csv_value("0"), Bson::Int32(0));
        assert_eq!(csv_value("0.5"), Bson::Double(0.5));
        assert_eq!(csv_value("1e3"), Bson::Double(1000.0));
        assert_eq!(csv_value("1.50"), Bson::Double(1.5));
        assert_eq!(csv_value("0.30000000000000004"), Bson::Double(0.1 + 0.2));
        assert!(matches!(csv_value("-0.0"), Bson::Double(f) if f == 0.0 && f.is_sign_negative()));
        // What a type would change stays text.
        for s in [
            "007", "+33 6 12", "1.2.3", "e5", "-", "1-2", "abc", "True", "2024-01-02", "{bad}", "-0", " 12", "12 ", "0x1F", "1_000",
            "9223372036854775808", "12345678901234567890", "3.14159265358979323846", "9007199254740993.0", "1e400", "-1e400", "1e-400", "NaN", "inf", "Infinity",
            "2024-01-02T03:04:05.123456Z", "2024-01-02T03:04:05.1234+02:00",
        ] {
            assert_eq!(csv_value(s), Bson::String(s.into()), "{s}");
        }
        assert!(matches!(csv_value("2024-01-02T03:04:05Z"), Bson::DateTime(_)));
        assert!(matches!(csv_value("2024-01-02T03:04:05+02:00"), Bson::DateTime(_)));
        assert_eq!(csv_value("2024-01-02T03:04:05.123000Z"), Bson::DateTime(DateTime::parse_rfc3339_str("2024-01-02T03:04:05.123Z").unwrap()), "zeros past the milliseconds lose nothing");
        assert!(matches!(csv_value("ObjectId(5f1d7a0e1c9d440000a1b2c3)"), Bson::ObjectId(_)));
        assert_eq!(csv_value("{\"a\":1}"), Bson::Document(doc! { "a": 1 }));
        assert_eq!(csv_value("[18446744073709551615]"), Bson::String("[18446744073709551615]".into()), "not rounded in arrays either");
    }

    #[test]
    fn dotted_paths() {
        let d = doc! { "a": { "b": { "c": 1 } }, "x.y": 2, "e": {} };
        assert_eq!(get_path(&d, "a.b.c"), Some(&Bson::Int32(1)));
        assert_eq!(get_path(&d, "x.y"), Some(&Bson::Int32(2)), "a key with a dot");
        assert_eq!(get_path(&d, "a.z"), None);
        assert_eq!(csv_fields(&[d]), vec!["a.b.c", "x.y", "e"]);
        let mut d = Document::new();
        set_path(&mut d, "a.b", Bson::Int32(1));
        set_path(&mut d, "a.c", Bson::Int32(2));
        set_path(&mut d, "a.", Bson::Int32(3));
        assert_eq!(d, doc! { "a": { "b": 1, "c": 2 }, "a.": 3 });
        assert_eq!(path_keys("a.b.c"), ["a", "b", "c"]);
        assert_eq!(path_keys("a..b"), ["a", ".b"]);
        assert_eq!(path_keys(".a"), [".a"]);
    }

    #[test]
    fn formats_of_file_names() {
        assert_eq!(Format::of_path(Path::new("a/b.CSV")), Format::Csv);
        assert_eq!(Format::of_path(Path::new("b.tsv")), Format::Csv);
        assert_eq!(Delimiter::of_path(Path::new("b.tsv")), Delimiter::Tab);
        assert_eq!(Format::of_path(Path::new("b.json")), Format::JsonLines);
        assert_eq!(Format::of_path(Path::new("b")), Format::JsonLines);
        let i = Imported { inserted: 3, failed: 1, first_error: Some("E11000 duplicate key".into()) };
        assert_eq!(i.summary(), "3 documents imported, 1 failed. First error: E11000 duplicate key");
        assert_eq!(i.refused(), "1 document refused (E11000 duplicate key)");
        assert_eq!(Imported { inserted: 1, ..Default::default() }.summary(), "1 document imported");
        let mut all = Imported::default();
        all.add(&Imported { inserted: 2, failed: 0, first_error: None });
        all.add(&i);
        all.add(&Imported { inserted: 1, failed: 1, first_error: Some("other".into()) });
        assert_eq!(all, Imported { inserted: 6, failed: 2, first_error: Some("E11000 duplicate key".into()) });
    }

    #[test]
    fn stops_are_told_with_what_was_done() {
        assert_eq!(stopped(Cancelled.into(), 0).to_string(), "cancelled: nothing was imported");
        assert_eq!(stopped(Cancelled.into(), 1).to_string(), "cancelled after 1 document were imported");
        assert_eq!(stopped(anyhow!("network"), 0).to_string(), "network");
        assert_eq!(stopped(anyhow!("network"), 2000).to_string(), "stopped after 2000 documents were imported: network");
        assert_eq!(stopped(anyhow!(Cancelled).context("reading"), 3).to_string(), "cancelled after 3 documents were imported");
    }

    #[test]
    fn a_replacement_stopped_halfway_keeps_the_new_collection() {
        let e: anyhow::Error = Stranded { name: "orders".into(), into: "orders.rr-tmp-1a2b3c4d".into(), cause: "NetworkTimeout".into() }.into();
        assert!(e.is::<Stranded>());
        assert_eq!(e.to_string(), "NetworkTimeout: the new documents of orders are kept in orders.rr-tmp-1a2b3c4d");
    }

    #[test]
    fn a_reader_that_panics_is_an_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let cancel = Arc::new(AtomicBool::new(false));
            let mut r = ReadAhead::spawn(&cancel, |send: &mut dyn FnMut(u64) -> bool| {
                send(1);
                panic!("bug in the parser");
            });
            assert_eq!(r.next().await.unwrap().unwrap(), 1);
            let e = r.next().await.unwrap().unwrap_err();
            assert!(e.to_string().contains("could not be read to the end"), "{e}");
            assert!(r.next().await.is_none());
            // A cancel ends the reading, but not as an end of file: the
            // flag tells.
            let mut r = ReadAhead::spawn(&cancel, |send: &mut dyn FnMut(u64) -> bool| {
                for i in 0.. {
                    if !send(i) {
                        return Ok(());
                    }
                }
                unreachable!()
            });
            assert_eq!(r.next().await.unwrap().unwrap(), 0);
            cancel.store(true, Ordering::Relaxed);
            while r.next().await.is_some() {}
            assert!(check_cancel(&cancel).unwrap_err().is::<Cancelled>());
        });
    }
}
