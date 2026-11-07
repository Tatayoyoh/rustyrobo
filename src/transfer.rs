//! Import and export of collections, in JSON (one document per line, like
//! mongoexport, or an array) and CSV.
//!
//! Documents are read and written in batches: an export streams the cursor
//! to the file, an import parses the file on a blocking thread while the
//! previous batch is being inserted. Nothing holds the whole collection.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, bail};
use bson::{Bson, Document, doc};

use crate::mongo::{Connection, bson_as_i64};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
const BATCH: usize = 1000;
/// Size of the documents of one insert, under the 16 MB of a command.
const BATCH_BYTES: usize = 8 * 1024 * 1024;

/// Writes the documents of a query to a file. `progress` gets the number of
/// documents written so far. Returns that number.
pub async fn export(conn: &Connection, o: &ExportOptions, cancel: &AtomicBool, progress: impl Fn(u64)) -> anyhow::Result<u64> {
    let file = std::fs::File::create(&o.path).with_context(|| format!("cannot create {}", o.path.display()))?;
    let mut w = Writer::new(BufWriter::new(file), o);
    let mut session = conn.start_session().await.ok();
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
    let reply = conn.run_command(&o.db, cmd, session.as_mut()).await?;
    let cursor = reply.get_document("cursor").context("find did not return a cursor")?;
    let mut id = cursor.get("id").and_then(bson_as_i64).unwrap_or(0);
    let mut batch = batch_of(cursor, "firstBatch");
    let mut written = 0;
    loop {
        w.write(&batch)?;
        written += batch.len() as u64;
        progress(written);
        if id == 0 {
            break;
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = conn.run_command(&o.db, doc! { "killCursors": &o.coll, "cursors": [id] }, None).await;
            bail!("cancelled after {written} documents");
        }
        let more = conn.run_command(&o.db, doc! { "getMore": id, "collection": &o.coll, "batchSize": BATCH as i64 }, session.as_mut()).await?;
        let cursor = more.get_document("cursor").context("getMore did not return a cursor")?;
        id = cursor.get("id").and_then(bson_as_i64).unwrap_or(0);
        batch = batch_of(cursor, "nextBatch");
    }
    w.finish()?;
    Ok(written)
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
        let docs = |n: u64| format!("{n} document{}", if n == 1 { "" } else { "s" });
        match &self.first_error {
            None => format!("{} imported", docs(self.inserted)),
            Some(e) => format!("{} imported, {} failed. First error: {e}", docs(self.inserted), self.failed),
        }
    }
}

/// Inserts the documents of a file. Write errors (duplicate keys…) do not
/// stop the import: they are counted, like with mongoimport.
pub async fn import(conn: &Connection, o: &ImportOptions, cancel: Arc<AtomicBool>, progress: impl Fn(u64)) -> anyhow::Result<Imported> {
    let file = std::fs::File::open(&o.path).with_context(|| format!("cannot open {}", o.path.display()))?;
    if o.drop_first {
        // NamespaceNotFound: nothing to drop.
        if let Err(e) = conn.run_command(&o.db, doc! { "drop": &o.coll }, None).await
            && crate::mongo::command_failure(&e).is_none_or(|f| f.get_i32("code").ok() != Some(26))
        {
            return Err(e);
        }
    }
    // The file is parsed on a blocking thread, one batch ahead.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<anyhow::Result<Vec<Document>>>(2);
    let opts = o.clone();
    let stop = Arc::clone(&cancel);
    let reader = tokio::task::spawn_blocking(move || {
        let send = |batch| tx.blocking_send(Ok(batch)).is_ok() && !stop.load(Ordering::Relaxed);
        if let Err(e) = read_documents(BufReader::new(file), &opts, send) {
            let _ = tx.blocking_send(Err(e));
        }
    });
    let mut done = Imported::default();
    while let Some(batch) = rx.recv().await {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled after {} documents", done.inserted);
        }
        let batch = batch?;
        let count = batch.len() as u64;
        let reply = conn.run_command(&o.db, doc! { "insert": &o.coll, "documents": batch, "ordered": false }, None).await?;
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
        progress(done.inserted);
    }
    let _ = reader.await;
    Ok(done)
}

/// Parses a file into batches of documents, given to `send` until it returns false.
pub fn read_documents(mut r: impl BufRead, o: &ImportOptions, mut send: impl FnMut(Vec<Document>) -> bool) -> anyhow::Result<()> {
    let mut batch = Vec::new();
    let mut bytes = 0;
    let mut push = |d: Document, batch: &mut Vec<Document>| -> bool {
        bytes += bson::to_vec(&d).map(|v| v.len()).unwrap_or(0);
        batch.push(d);
        if batch.len() >= BATCH || bytes >= BATCH_BYTES {
            bytes = 0;
            return send(std::mem::take(batch));
        }
        true
    };
    match o.format {
        Format::Csv => {
            let mut text = String::new();
            r.read_to_string(&mut text).context("the file is not UTF-8 text")?;
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            let mut rows = CsvRows::new(text, o.delimiter.char());
            let Some(header) = rows.next() else { return Ok(()) };
            let header = header?;
            if header.iter().all(|h| h.trim().is_empty()) {
                bail!("the first line must hold the field names");
            }
            for (n, row) in rows.enumerate() {
                let row = row.with_context(|| format!("line {}", n + 2))?;
                if row.len() == 1 && row[0].is_empty() {
                    continue;
                }
                if !push(csv_document(&header, &row, o.detect_types), &mut batch) {
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
                    let v: serde_json::Value = serde_json::from_reader(r).context("invalid JSON")?;
                    let serde_json::Value::Array(items) = v else { bail!("expected an array of documents") };
                    for (i, item) in items.into_iter().enumerate() {
                        let d = json_document(item).with_context(|| format!("document {}", i + 1))?;
                        if !push(d, &mut batch) {
                            return Ok(());
                        }
                    }
                }
                Some(_) => {
                    for (i, item) in serde_json::Deserializer::from_reader(r).into_iter::<serde_json::Value>().enumerate() {
                        let item = item.with_context(|| format!("invalid JSON in document {}", i + 1))?;
                        let d = json_document(item).with_context(|| format!("document {}", i + 1))?;
                        if !push(d, &mut batch) {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    if !batch.is_empty() {
        send(batch);
    }
    Ok(())
}

/// A document from Extended JSON, relaxed or canonical.
fn json_document(v: serde_json::Value) -> anyhow::Result<Document> {
    match Bson::try_from(v).map_err(|e| anyhow::anyhow!("{e}"))? {
        Bson::Document(d) => Ok(d),
        other => bail!("expected a document, got {}", crate::bsonfmt::type_name(&other)),
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
/// writes them, so that an import reads them back with their type.
pub fn csv_cell(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::Null | Bson::Undefined => String::new(),
        Bson::Boolean(b) => b.to_string(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 => format!("{f:.1}"),
        Bson::Double(f) => f.to_string(),
        Bson::ObjectId(o) => format!("ObjectId({o})"),
        Bson::DateTime(d) => d.try_to_rfc3339_string().unwrap_or_else(|_| d.timestamp_millis().to_string()),
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

/// Rows of a CSV text (RFC 4180: quoted fields may hold delimiters, quotes
/// and line breaks).
pub struct CsvRows<'a> {
    text: &'a str,
    pos: usize,
    delimiter: char,
}

impl<'a> CsvRows<'a> {
    pub fn new(text: &'a str, delimiter: char) -> Self {
        CsvRows { text, pos: 0, delimiter }
    }
}

impl Iterator for CsvRows<'_> {
    type Item = anyhow::Result<Vec<String>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.text.len() {
            return None;
        }
        let mut row = Vec::new();
        let mut cell = String::new();
        let mut chars = self.text[self.pos..].char_indices().peekable();
        let mut quoted = false;
        let mut was_quoted = false;
        let start = self.pos;
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
                    let mut end = start + i + 1;
                    if c == '\r' && chars.peek().is_some_and(|(_, n)| *n == '\n') {
                        end += 1;
                    }
                    self.pos = end;
                    row.push(cell);
                    return Some(Ok(row));
                }
                c => cell.push(c),
            }
        }
        self.pos = self.text.len();
        if quoted {
            return Some(Err(anyhow::anyhow!("unterminated quoted field")));
        }
        row.push(cell);
        Some(Ok(row))
    }
}

/// A document from a CSV row: dotted names make sub-documents, empty cells
/// are left out.
pub fn csv_document(header: &[String], row: &[String], detect_types: bool) -> Document {
    let mut d = Document::new();
    for (name, text) in header.iter().zip(row) {
        let name = name.trim();
        if name.is_empty() || text.is_empty() {
            continue;
        }
        let v = if detect_types { csv_value(text) } else { Bson::String(text.clone()) };
        set_path(&mut d, name, v);
    }
    d
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
/// with its type; anything else stays a string.
pub fn csv_value(text: &str) -> Bson {
    match text {
        "true" => return Bson::Boolean(true),
        "false" => return Bson::Boolean(false),
        _ => {}
    }
    let digits = text.strip_prefix('-').unwrap_or(text);
    // No leading zeros: "007" is a code, not a number.
    let plain = !digits.is_empty() && (digits == "0" || !digits.starts_with('0') || digits.starts_with("0."));
    if plain && digits.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(n) = text.parse::<i32>() {
            return Bson::Int32(n);
        }
        if let Ok(n) = text.parse::<i64>() {
            return Bson::Int64(n);
        }
    }
    let float_chars = digits.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'));
    if plain && float_chars && digits.as_bytes()[0].is_ascii_digit()
        && let Ok(f) = text.parse::<f64>()
    {
        return Bson::Double(f);
    }
    if let Some(hex) = text.strip_prefix("ObjectId(").and_then(|t| t.strip_suffix(')'))
        && let Ok(o) = bson::oid::ObjectId::parse_str(hex.trim_matches(['"', '\'']))
    {
        return Bson::ObjectId(o);
    }
    // Full ISO dates only: 2024-01-02T03:04:05Z, with milliseconds or an offset.
    if text.len() >= 20 && text.as_bytes()[4] == b'-' && text.as_bytes()[10] == b'T'
        && let Ok(d) = bson::DateTime::parse_rfc3339_str(text)
    {
        return Bson::DateTime(d);
    }
    let container = (text.starts_with('{') && text.ends_with('}')) || (text.starts_with('[') && text.ends_with(']'));
    if container
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(text)
        && let Ok(b) = Bson::try_from(v)
    {
        return b;
    }
    Bson::String(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut docs = Vec::new();
        read_documents(text.as_bytes(), o, |b| {
            docs.extend(b);
            true
        })?;
        Ok(docs)
    }

    fn sample() -> Vec<Document> {
        let oid = bson::oid::ObjectId::parse_str("5f1d7a0e1c9d440000a1b2c3").unwrap();
        let date = bson::DateTime::parse_rfc3339_str("2024-01-02T03:04:05.006Z").unwrap();
        vec![
            doc! { "_id": oid, "name": "Ada, \"the\" first", "n": 1, "big": 5_000_000_000i64, "f": 1.5, "whole": 2.0, "ok": true, "at": date, "address": { "city": "Paris", "zip": "007" }, "tags": ["a", 1] },
            doc! { "_id": 2, "name": "multi\nline", "extra": null },
        ]
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
        assert!(format!("{e:#}").contains("expected a document"), "{e:#}");
        assert!(read(&import_opts(Format::JsonLines), "  \n").unwrap().is_empty());
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
    fn csv_parsing() {
        let rows: Vec<Vec<String>> = CsvRows::new("a,\"b,c\",\"d\"\"e\"\r\n\"x\ny\",,z\nlast", ',').map(Result::unwrap).collect();
        assert_eq!(rows, vec![vec!["a", "b,c", "d\"e"], vec!["x\ny", "", "z"], vec!["last"]]);
        let rows: Vec<Vec<String>> = CsvRows::new("a\tb\n", '\t').map(Result::unwrap).collect();
        assert_eq!(rows, vec![vec!["a", "b"]]);
        assert!(CsvRows::new("\"open", ',').next().unwrap().is_err());
        let e = read(&import_opts(Format::Csv), "a\n\"x").unwrap_err();
        assert!(format!("{e:#}").contains("line 2"), "{e:#}");
        assert!(read(&import_opts(Format::Csv), "").unwrap().is_empty());
        assert!(read(&import_opts(Format::Csv), ",\n1\n").is_err(), "a header is needed");
    }

    #[test]
    fn csv_values() {
        assert_eq!(csv_value("12"), Bson::Int32(12));
        assert_eq!(csv_value("-12"), Bson::Int32(-12));
        assert_eq!(csv_value("12345678901"), Bson::Int64(12345678901));
        assert_eq!(csv_value("0"), Bson::Int32(0));
        assert_eq!(csv_value("0.5"), Bson::Double(0.5));
        assert_eq!(csv_value("1e3"), Bson::Double(1000.0));
        for s in ["007", "+33 6 12", "1.2.3", "e5", "-", "1-2", "abc", "True", "2024-01-02", "{bad}"] {
            assert_eq!(csv_value(s), Bson::String(s.into()), "{s}");
        }
        assert!(matches!(csv_value("2024-01-02T03:04:05Z"), Bson::DateTime(_)));
        assert!(matches!(csv_value("2024-01-02T03:04:05+02:00"), Bson::DateTime(_)));
        assert!(matches!(csv_value("ObjectId(5f1d7a0e1c9d440000a1b2c3)"), Bson::ObjectId(_)));
        assert_eq!(csv_value("{\"a\":1}"), Bson::Document(doc! { "a": 1 }));
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
    }

    #[test]
    fn large_imports_are_batched() {
        let text: String = (0..2500).map(|i| format!("{{\"i\":{i}}}\n")).collect();
        let mut sizes = Vec::new();
        read_documents(text.as_bytes(), &import_opts(Format::JsonLines), |b| {
            sizes.push(b.len());
            true
        })
        .unwrap();
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
    fn formats_of_file_names() {
        assert_eq!(Format::of_path(Path::new("a/b.CSV")), Format::Csv);
        assert_eq!(Format::of_path(Path::new("b.tsv")), Format::Csv);
        assert_eq!(Delimiter::of_path(Path::new("b.tsv")), Delimiter::Tab);
        assert_eq!(Format::of_path(Path::new("b.json")), Format::JsonLines);
        assert_eq!(Format::of_path(Path::new("b")), Format::JsonLines);
        let i = Imported { inserted: 3, failed: 1, first_error: Some("E11000 duplicate key".into()) };
        assert_eq!(i.summary(), "3 documents imported, 1 failed. First error: E11000 duplicate key");
        assert_eq!(Imported { inserted: 1, ..Default::default() }.summary(), "1 document imported");
    }
}
