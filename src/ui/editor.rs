//! JavaScript code editor: syntax highlighting, line numbers and the
//! shell autocompletion popup (collections, db/collection/cursor methods).

use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Color32, FontId, Galley, Id, Key, Modifiers, TextBuffer, Ui};

use crate::theme::Theme;

const KEYWORDS: &[&str] = &[
    "var", "let", "const", "function", "return", "if", "else", "for", "while", "do", "break",
    "continue", "new", "delete", "typeof", "instanceof", "in", "of", "try", "catch", "finally",
    "throw", "switch", "case", "default", "this", "class", "extends", "super", "async", "await",
    "yield", "void", "with",
];
const CONSTANTS: &[&str] = &["true", "false", "null", "undefined", "NaN", "Infinity"];
const BSON_CTORS: &[&str] = &[
    "ObjectId", "ISODate", "Date", "NumberLong", "NumberInt", "NumberDecimal", "BinData", "UUID",
    "Timestamp", "MinKey", "MaxKey", "DBRef", "Code", "HexData", "MD5", "Long", "Int32", "Double",
    "Decimal128", "BSONRegExp",
];

#[derive(Clone, Copy, PartialEq)]
enum Tok {
    Plain,
    Keyword,
    String,
    Number,
    Comment,
    Function,
    Property,
    Punct,
    Regex,
    Constant,
    Bson,
}

fn color(t: &Theme, tok: Tok) -> Color32 {
    let s = &t.syntax;
    match tok {
        Tok::Plain => t.text,
        Tok::Keyword => s.keyword,
        Tok::String => s.string,
        Tok::Number => s.number,
        Tok::Comment => s.comment,
        Tok::Function => s.function,
        Tok::Property => s.property,
        Tok::Punct => s.punctuation,
        Tok::Regex => s.regex,
        Tok::Constant => s.constant,
        Tok::Bson => s.bson_type,
    }
}

/// Tokenises JavaScript into coloured spans (byte ranges).
fn tokenize(src: &str) -> Vec<(usize, usize, Tok)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut last_sig: u8 = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        let tok = if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            Tok::Comment
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(b.len());
            Tok::Comment
        } else if c == b'"' || c == b'\'' || c == b'`' {
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' {
                    i += 1;
                }
                if b[i.min(b.len() - 1)] == b'\n' && c != b'`' {
                    break;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            Tok::String
        } else if c == b'/' && !(last_sig.is_ascii_alphanumeric() || matches!(last_sig, b')' | b']' | b'}' | b'_' | b'$')) {
            i += 1;
            let mut class = false;
            while i < b.len() && b[i] != b'\n' {
                match b[i] {
                    b'\\' => i += 1,
                    b'[' => class = true,
                    b']' => class = false,
                    b'/' if !class => break,
                    _ => {}
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            while i < b.len() && b[i].is_ascii_alphabetic() {
                i += 1;
            }
            Tok::Regex
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
            Tok::Number
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80 {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] >= 0x80) {
                i += 1;
            }
            let word = &src[start..i];
            let mut j = i;
            while j < b.len() && b[j] == b' ' {
                j += 1;
            }
            let is_call = b.get(j) == Some(&b'(');
            let after_dot = start > 0 && b[start - 1] == b'.';
            let is_key = !after_dot && b.get(j) == Some(&b':') && b.get(j + 1) != Some(&b':');
            if KEYWORDS.contains(&word) {
                Tok::Keyword
            } else if CONSTANTS.contains(&word) {
                Tok::Constant
            } else if BSON_CTORS.contains(&word) {
                Tok::Bson
            } else if is_call {
                Tok::Function
            } else if after_dot || is_key || word.starts_with('$') {
                Tok::Property
            } else {
                Tok::Plain
            }
        } else if c.is_ascii_whitespace() {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push((start, i, Tok::Plain));
            continue;
        } else {
            i += 1;
            while i < b.len() && !src.is_char_boundary(i) {
                i += 1;
            }
            Tok::Punct
        };
        last_sig = b[i - 1];
        out.push((start, i, tok));
    }
    out
}

#[derive(Default)]
struct Highlighter;

impl egui::cache::ComputerMut<(&FontId, &str, &str), LayoutJob> for Highlighter {
    fn compute(&mut self, (font, theme_name, code): (&FontId, &str, &str)) -> LayoutJob {
        let theme = crate::theme::THEMES.iter().find(|t| t.name == theme_name).unwrap_or(&crate::theme::THEMES[0]);
        highlight(code, theme, font.clone())
    }
}

/// Highlighting memoised across frames: the text is only re-tokenised
/// when it (or the theme/font) changes.
pub fn highlight_cached(ctx: &egui::Context, code: &str, theme: &Theme, font: &FontId) -> LayoutJob {
    type Cache = egui::cache::FrameCache<LayoutJob, Highlighter>;
    ctx.memory_mut(|m| m.caches.cache::<Cache>().get((font, theme.name, code)).clone())
}

pub fn highlight(src: &str, theme: &Theme, font: FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (s, e, tok) in tokenize(src) {
        job.append(&src[s..e], 0.0, TextFormat::simple(font.clone(), color(theme, tok)));
    }
    if src.is_empty() {
        job.append("", 0.0, TextFormat::simple(font, theme.text));
    }
    job
}

// ---------------------------------------------------------------- completion

const DB_METHODS: &[&str] = &[
    "getCollection", "getCollectionNames", "getCollectionInfos", "getSiblingDB", "getName",
    "runCommand", "adminCommand", "stats", "version", "serverStatus", "serverBuildInfo", "hostInfo",
    "currentOp", "killOp", "createCollection", "createView", "dropDatabase", "getUsers", "getUser",
    "createUser", "updateUser", "dropUser", "changeUserPassword", "grantRolesToUser",
    "revokeRolesFromUser", "getRoles", "createRole", "dropRole", "printCollectionStats", "isMaster",
    "hello", "getProfilingLevel", "getProfilingStatus", "setProfilingLevel", "fsyncLock",
    "fsyncUnlock", "aggregate", "listCommands", "serverCmdLineOpts", "getMongo",
];
const COLLECTION_METHODS: &[&str] = &[
    "find", "findOne", "insert", "insertOne", "insertMany", "update", "updateOne", "updateMany",
    "replaceOne", "remove", "deleteOne", "deleteMany", "save", "findOneAndUpdate",
    "findOneAndReplace", "findOneAndDelete", "findAndModify", "bulkWrite", "count",
    "countDocuments", "estimatedDocumentCount", "distinct", "aggregate", "mapReduce",
    "createIndex", "createIndexes", "ensureIndex", "getIndexes", "dropIndex", "dropIndexes",
    "hideIndex", "unhideIndex", "reIndex", "drop", "renameCollection", "stats", "dataSize",
    "storageSize", "totalIndexSize", "totalSize", "isCapped", "validate", "exists", "getName",
    "getFullName", "getDB", "getShardVersion", "getShardDistribution", "latencyStats",
];
const CURSOR_METHODS: &[&str] = &[
    "sort", "skip", "limit", "batchSize", "projection", "hint", "collation", "maxTimeMS",
    "comment", "min", "max", "returnKey", "showRecordId", "allowDiskUse", "noCursorTimeout",
    "readPref", "readConcern", "pretty", "toArray", "forEach", "map", "hasNext", "next",
    "tryNext", "itcount", "count", "size", "explain", "close", "isExhausted",
];
const GLOBALS: &[&str] = &[
    "db", "rs", "sh", "print", "printjson", "tojson", "tojsononeline", "sleep", "ObjectId",
    "ISODate", "NumberLong", "NumberInt", "NumberDecimal", "BinData", "UUID", "Timestamp",
    "MinKey", "MaxKey", "DBRef", "Code", "HexData", "Date", "Math", "JSON", "Object", "Array",
    "String", "Number", "RegExp", "function", "var", "let", "const", "return",
];
const RS_METHODS: &[&str] = &[
    "status", "conf", "config", "isMaster", "hello", "initiate", "reconfig", "add", "remove",
    "stepDown", "freeze", "syncFrom", "printReplicationInfo", "printSecondaryReplicationInfo",
];
const SH_METHODS: &[&str] = &[
    "status", "enableSharding", "shardCollection", "addShard", "getBalancerState",
    "isBalancerRunning", "startBalancer", "stopBalancer",
];

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionMode {
    #[default]
    All,
    NoCollectionNames,
    Off,
}

const QUERY_OPERATORS: &[&str] = &[
    "$eq", "$ne", "$gt", "$gte", "$lt", "$lte", "$in", "$nin", "$and", "$or", "$nor", "$not",
    "$exists", "$type", "$regex", "$options", "$expr", "$jsonSchema", "$mod", "$text", "$search",
    "$where", "$all", "$elemMatch", "$size", "$near", "$nearSphere", "$geoWithin",
    "$geoIntersects", "$box", "$center", "$centerSphere", "$geometry", "$maxDistance", "$comment",
];
const UPDATE_OPERATORS: &[&str] = &[
    "$set", "$unset", "$inc", "$mul", "$rename", "$setOnInsert", "$min", "$max", "$currentDate",
    "$push", "$pull", "$pullAll", "$addToSet", "$pop", "$each", "$position", "$slice", "$sort",
    "$bit",
];
const AGGREGATION_STAGES: &[&str] = &[
    "$match", "$group", "$project", "$sort", "$limit", "$skip", "$unwind", "$lookup", "$addFields",
    "$set", "$unset", "$count", "$facet", "$bucket", "$bucketAuto", "$sortByCount", "$replaceRoot",
    "$replaceWith", "$sample", "$out", "$merge", "$graphLookup", "$unionWith", "$densify", "$fill",
    "$setWindowFields", "$geoNear", "$redact", "$collStats", "$indexStats", "$documents",
];
const ACCUMULATORS: &[&str] = &[
    "$sum", "$avg", "$min", "$max", "$first", "$last", "$push", "$addToSet", "$count", "$top",
    "$bottom", "$median", "$percentile", "$stdDevPop", "$stdDevSamp", "$mergeObjects",
    "$cond", "$ifNull", "$switch", "$concat", "$toUpper", "$toLower", "$dateToString",
    "$arrayElemAt", "$size", "$filter", "$map", "$reduce", "$toString", "$toInt", "$toDate",
];

/// Statement around `cursor`: from the previous `;` or blank line.
fn current_statement(chars: &[char], cursor: usize) -> usize {
    let mut i = cursor;
    while i > 0 {
        let c = chars[i - 1];
        if c == ';' || (c == '\n' && i >= 2 && chars[..i - 1].iter().rev().take_while(|c| **c != '\n').all(|c| c.is_whitespace())) {
            break;
        }
        i -= 1;
    }
    i
}

/// Collection targeted by the statement: `getCollection('x')` or `db.x.`.
pub fn statement_collection(stmt: &str) -> Option<String> {
    if let Some(i) = stmt.rfind("getCollection(") {
        let rest = stmt[i + "getCollection(".len()..].trim_start();
        let q = rest.chars().next()?;
        if q == '\'' || q == '"' || q == '`' {
            let body = &rest[1..];
            let end = body.find(q)?;
            return Some(body[..end].to_string());
        }
    }
    let i = stmt.rfind("db.")?;
    let name: String = stmt[i + 3..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    let after = stmt[i + 3 + name.len()..].chars().next();
    (after == Some('.') && !name.is_empty() && !DB_METHODS.contains(&name.as_str())).then_some(name)
}

/// Kind of object being typed into, judged from the method that opened it.
#[derive(Debug, PartialEq, Eq)]
enum KeyContext {
    Query,
    Update,
    Pipeline,
}

/// If `pos` is at a key position inside an object literal, returns the
/// context of the enclosing call.
fn key_context(chars: &[char], pos: usize, from: usize) -> Option<KeyContext> {
    // Character before the key (skipping whitespace and an opening quote).
    let mut i = pos;
    while i > from && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    if i > from && matches!(chars[i - 1], '"' | '\'') {
        i -= 1;
        while i > from && chars[i - 1].is_whitespace() {
            i -= 1;
        }
    }
    if i == from || !matches!(chars[i - 1], '{' | ',') {
        return None;
    }
    // Walk back to the unmatched '(' of the enclosing call, ignoring strings.
    let mut depth_paren = 0i32;
    let mut depth_brace = 0i32;
    let mut arg_index = 0;
    let mut min_brace = 0i32;
    let mut in_str: Option<char> = None;
    let mut j = i;
    while j > from {
        j -= 1;
        let c = chars[j];
        if let Some(q) = in_str {
            if c == q && (j == 0 || chars[j - 1] != '\\') {
                in_str = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => in_str = Some(c),
            '}' | ']' => depth_brace += 1,
            '{' | '[' => {
                depth_brace -= 1;
                // Leaving an enclosing object: commas seen so far were inside it.
                if depth_brace < min_brace {
                    min_brace = depth_brace;
                    arg_index = 0;
                }
            }
            ')' => depth_paren += 1,
            ',' if depth_paren == 0 && depth_brace == min_brace => arg_index += 1,
            '(' => {
                if depth_paren == 0 {
                    let name: String = chars[from..j]
                        .iter()
                        .rev()
                        .take_while(|c| c.is_alphanumeric() || **c == '_')
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    return Some(match name.as_str() {
                        "aggregate" => KeyContext::Pipeline,
                        "update" | "updateOne" | "updateMany" | "findOneAndUpdate" if arg_index >= 1 => KeyContext::Update,
                        _ => KeyContext::Query,
                    });
                }
                depth_paren -= 1;
            }
            _ => {}
        }
    }
    None
}

pub struct CompletionCtx<'a> {
    pub collections: &'a [String],
    /// Known field names per collection of the current database.
    pub fields: &'a std::collections::HashMap<String, Vec<String>>,
    pub mode: CompletionMode,
}

#[derive(Default, Debug)]
pub struct Completion {
    pub result: Option<(usize, Vec<String>)>,
    /// Field names of this collection are needed but not loaded yet.
    pub want_fields: Option<String>,
}

pub fn complete(text: &str, cursor: usize, cx: &CompletionCtx<'_>) -> Completion {
    let mut out = Completion::default();
    if cx.mode == CompletionMode::Off {
        return out;
    }
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let mut start = cursor;
    while start > 0 && (chars[start - 1].is_alphanumeric() || matches!(chars[start - 1], '_' | '$' | '.')) {
        start -= 1;
    }
    let stmt_start = current_statement(&chars, start);
    let quoted = start > 0 && matches!(chars[start - 1], '"' | '\'');
    if let Some(ctx) = key_context(&chars, start, stmt_start) {
        let prefix: String = chars[start..cursor].iter().collect();
        if !quoted && prefix.contains('.') {
            // `a.b` unquoted is not a valid key: let the member completion run.
        } else {
            let mut pool: Vec<String> = Vec::new();
            if prefix.starts_with('$') {
                let ops: &[&str] = match ctx {
                    KeyContext::Pipeline => AGGREGATION_STAGES,
                    KeyContext::Update => UPDATE_OPERATORS,
                    KeyContext::Query => QUERY_OPERATORS,
                };
                pool.extend(ops.iter().map(|s| s.to_string()));
                if ctx == KeyContext::Pipeline {
                    pool.extend(ACCUMULATORS.iter().map(|s| s.to_string()));
                    pool.extend(QUERY_OPERATORS.iter().map(|s| s.to_string()));
                }
            } else {
                let stmt: String = chars[stmt_start..start].iter().collect();
                if let Some(coll) = statement_collection(&stmt) {
                    match cx.fields.get(&coll) {
                        Some(f) => pool.extend(f.iter().filter(|f| quoted || !f.contains('.')).cloned()),
                        None => out.want_fields = Some(coll),
                    }
                }
            }
            let lower = prefix.to_lowercase();
            let mut items: Vec<String> = pool.into_iter().filter(|c| c.to_lowercase().starts_with(&lower) && *c != prefix).collect();
            let mut seen = std::collections::HashSet::new();
            items.retain(|i| seen.insert(i.clone()));
            if !items.is_empty() {
                out.result = Some((start, items));
            }
            return out;
        }
    }
    out.result = completions(text, cursor, cx.collections, cx.mode);
    out
}

/// Candidates for the identifier being typed at `cursor` (char index).
/// Returns `(prefix_start_char_index, candidates)`.
pub fn completions(text: &str, cursor: usize, collections: &[String], mode: CompletionMode) -> Option<(usize, Vec<String>)> {
    if mode == CompletionMode::Off {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let mut start = cursor;
    while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_' || chars[start - 1] == '$') {
        start -= 1;
    }
    let prefix: String = chars[start..cursor].iter().collect();
    let before: String = chars[..start].iter().collect();
    let trimmed = before.trim_end();

    let mut pool: Vec<String> = Vec::new();
    if let Some(obj) = trimmed.strip_suffix('.') {
        let obj = obj.trim_end();
        let ident_end: String = obj
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$' || *c == '.')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if ident_end == "db" {
            if mode == CompletionMode::All {
                pool.extend(
                    collections
                        .iter()
                        .filter(|c| c.chars().all(|ch| ch.is_alphanumeric() || ch == '_') && !c.starts_with(char::is_numeric))
                        .cloned(),
                );
            }
            pool.extend(DB_METHODS.iter().map(|s| s.to_string()));
        } else if ident_end == "rs" {
            pool.extend(RS_METHODS.iter().map(|s| s.to_string()));
        } else if ident_end == "sh" {
            pool.extend(SH_METHODS.iter().map(|s| s.to_string()));
        } else if ident_end.starts_with("db.") && !ident_end[3..].contains('.') {
            pool.extend(COLLECTION_METHODS.iter().map(|s| s.to_string()));
        } else if obj.ends_with(')') {
            let call = obj.rfind(".getCollection(").map(|i| obj[i..].matches('(').count() == 1);
            if call == Some(true) || obj.ends_with("getSiblingDB(") {
                pool.extend(COLLECTION_METHODS.iter().map(|s| s.to_string()));
            } else {
                pool.extend(CURSOR_METHODS.iter().map(|s| s.to_string()));
            }
        } else {
            return None;
        }
    } else {
        if prefix.chars().count() < 2 {
            return None;
        }
        pool.extend(GLOBALS.iter().map(|s| s.to_string()));
    }
    let lower = prefix.to_lowercase();
    let mut items: Vec<String> = pool
        .into_iter()
        .filter(|c| c.to_lowercase().starts_with(&lower) && *c != prefix)
        .collect();
    items.dedup();
    if items.is_empty() {
        return None;
    }
    Some((start, items))
}

#[derive(Clone, Default)]
struct CompletionState {
    open: bool,
    start: usize,
    items: Vec<String>,
    selected: usize,
    /// Set when an item is clicked; applied on the next frame.
    pending_accept: bool,
}

pub struct EditorOptions<'a> {
    pub line_numbers: bool,
    pub font: FontId,
    pub collections: &'a [String],
    pub fields: &'a std::collections::HashMap<String, Vec<String>>,
    pub completion: CompletionMode,
    pub focus: bool,
    pub read_only: bool,
}

pub struct EditorResponse {
    pub changed: bool,
    /// Selected text (for "execute selection").
    pub selection: Option<String>,
    pub has_focus: bool,
    pub want_fields: Option<String>,
}

pub fn code_editor(ui: &mut Ui, id: Id, text: &mut String, theme: &Theme, opts: EditorOptions<'_>) -> EditorResponse {
    let comp_id = id.with("completion");
    let mut comp: CompletionState = ui.data(|d| d.get_temp(comp_id)).unwrap_or_default();
    let edit_id = id.with("text");
    let focused = ui.memory(|m| m.has_focus(edit_id));

    // Keys for the popup must be consumed before the TextEdit sees them.
    let mut accept = std::mem::take(&mut comp.pending_accept);
    if comp.open && focused {
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                comp.selected = (comp.selected + 1) % comp.items.len().max(1);
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                comp.selected = (comp.selected + comp.items.len().max(1) - 1) % comp.items.len().max(1);
            }
            if i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab) {
                accept = true;
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                comp.open = false;
            }
        });
    }
    let manual_trigger = focused && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Space));

    let theme_copy = *theme;
    let font = opts.font.clone();
    let mut layouter = move |ui: &Ui, buf: &dyn TextBuffer, _wrap: f32| -> Arc<Galley> {
        let job = highlight_cached(ui.ctx(), buf.as_str(), &theme_copy, &font);
        ui.fonts_mut(|f| f.layout_job(job))
    };

    let row_height = ui.fonts_mut(|f| f.row_height(&opts.font));
    let mut out_changed = false;
    let mut selection = None;
    let mut has_focus = false;
    let mut want_fields = None;

    egui::ScrollArea::both()
        .id_salt(id.with("scroll"))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                if opts.line_numbers {
                    let lines = text.lines().count().max(1) + usize::from(text.ends_with('\n'));
                    let width = (lines.to_string().len() as f32 + 1.0) * row_height * 0.6;
                    let mut numbers = String::with_capacity(lines * 4);
                    for n in 1..=lines {
                        numbers.push_str(&format!("{n}\n"));
                    }
                    ui.add_sized(
                        [width, row_height * lines as f32],
                        egui::Label::new(egui::RichText::new(numbers).font(opts.font.clone()).color(theme.text_weak))
                            .halign(egui::Align::RIGHT)
                            .selectable(false),
                    );
                }
                let available = ui.available_size();
                let output = egui::TextEdit::multiline(text)
                    .id(edit_id)
                    .font(opts.font.clone())
                    .code_editor()
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .desired_rows(((available.y / row_height) as usize).max(4))
                    .frame(egui::Frame::NONE)
                    .interactive(!opts.read_only)
                    .layouter(&mut layouter)
                    .show(ui);
                if opts.focus {
                    output.response.request_focus();
                }
                has_focus = output.response.has_focus();
                out_changed = output.response.changed();

                let cursor_range = output.cursor_range;
                if let Some(r) = cursor_range {
                    let (a, b) = (r.primary.index.0, r.secondary.index.0);
                    if a != b {
                        let (lo, hi) = (a.min(b), a.max(b));
                        selection = Some(text.chars().skip(lo).take(hi - lo).collect());
                    }
                }

                let cursor = cursor_range.map(|r| r.primary.index.0);
                if accept && comp.open {
                    if let (Some(cur), Some(item)) = (cursor, comp.items.get(comp.selected).cloned()) {
                        let byte_start = text.char_indices().nth(comp.start).map(|(b, _)| b).unwrap_or(text.len());
                        let byte_end = text.char_indices().nth(cur).map(|(b, _)| b).unwrap_or(text.len());
                        text.replace_range(byte_start..byte_end, &item);
                        let new_cursor = comp.start + item.chars().count();
                        let mut state = output.state.clone();
                        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(new_cursor))));
                        state.store(ui.ctx(), edit_id);
                        out_changed = true;
                    }
                    comp.open = false;
                } else if (out_changed || manual_trigger) && has_focus {
                    let cx = CompletionCtx { collections: opts.collections, fields: opts.fields, mode: opts.completion };
                    let comp_res = cursor.map(|c| complete(text, c, &cx)).unwrap_or_default();
                    want_fields = comp_res.want_fields;
                    match comp_res.result {
                        Some((start, items)) => {
                            if !comp.open || comp.start != start {
                                comp.selected = 0;
                            }
                            comp.open = true;
                            comp.start = start;
                            comp.selected = comp.selected.min(items.len() - 1);
                            comp.items = items;
                        }
                        None => comp.open = false,
                    }
                } else if !has_focus {
                    comp.open = false;
                }

                if comp.open
                    && let Some(cur) = cursor
                {
                    let pos = output.galley.pos_from_cursor(CCursor::new(cur));
                    let anchor = output.galley_pos + pos.left_bottom().to_vec2();
                    egui::Area::new(id.with("completion_popup"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(anchor + egui::vec2(0.0, 2.0))
                        .show(ui.ctx(), |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.set_min_width(180.0);
                                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                                    for (i, item) in comp.items.clone().iter().enumerate() {
                                        let sel = i == comp.selected;
                                        let r = ui.selectable_label(sel, egui::RichText::new(item).font(opts.font.clone()));
                                        if sel {
                                            r.scroll_to_me(None);
                                        }
                                        if r.clicked() {
                                            comp.selected = i;
                                            comp.pending_accept = true;
                                        }
                                    }
                                });
                            });
                        });
                    if comp.pending_accept {
                        output.response.request_focus();
                        ui.ctx().request_repaint();
                    }
                }
            });
        });

    ui.data_mut(|d| d.insert_temp(comp_id, comp));
    EditorResponse { changed: out_changed, selection, has_focus, want_fields }
}

/// Read-only highlighted text block (results text view, function viewer).
pub fn code_view(ui: &mut Ui, id: Id, text: &str, theme: &Theme, font: FontId) {
    egui::ScrollArea::both().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
        read_only_text(ui, id, text, theme, font);
    });
}

fn read_only_text(ui: &mut Ui, id: Id, text: &str, theme: &Theme, font: FontId) -> egui::text_edit::TextEditOutput {
    // `&str` is a read-only TextBuffer: no copy of the (possibly large) text.
    let mut view: &str = text;
    let theme_copy = *theme;
    let mut layouter = move |ui: &Ui, buf: &dyn TextBuffer, _wrap: f32| -> Arc<Galley> {
        let job = highlight_cached(ui.ctx(), buf.as_str(), &theme_copy, &font);
        ui.fonts_mut(|f| f.layout_job(job))
    };
    egui::TextEdit::multiline(&mut view)
        .id(id.with("t"))
        .code_editor()
        .desired_width(f32::INFINITY)
        .frame(egui::Frame::NONE)
        .layouter(&mut layouter)
        .show(ui)
}

/// What the user did in the fold gutter of [`folding_code_view`].
pub enum FoldClick {
    Toggle(usize),
    CollapseAll,
    ExpandAll,
}

/// [`code_view`] with a fold gutter: `marks[i]` is `Some(folded)` when line
/// `i` opens a block. Only the visible markers are painted.
pub fn folding_code_view(ui: &mut Ui, id: Id, text: &str, marks: &[Option<bool>], theme: &Theme, font: FontId) -> Option<FoldClick> {
    let mut click = None;
    let row_height = ui.fonts_mut(|f| f.row_height(&font));
    egui::ScrollArea::both().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let size = egui::vec2(row_height, row_height * marks.len().max(1) as f32);
            let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
            let out = read_only_text(ui, id, text, theme, font.clone());
            // Rows of the galley are the lines (no wrapping): align on them.
            let rows = out.galley.rows.len().max(1);
            let row_h = out.galley.size().y / rows as f32;
            let top = out.galley_pos.y;
            let row_at = |y: f32| ((y - top) / row_h).floor().max(0.0) as usize;
            let clip = ui.clip_rect();
            let hovered = resp.hover_pos().map(|p| row_at(p.y)).filter(|&r| marks.get(r).is_some_and(|m| m.is_some()));
            if hovered.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let painter = ui.painter().with_clip_rect(egui::Rect::from_x_y_ranges(rect.x_range(), clip.y_range()));
            let (first, last) = (row_at(clip.top()), row_at(clip.bottom()) + 1);
            for (r, mark) in marks.iter().enumerate().take(last).skip(first) {
                let Some(folded) = *mark else { continue };
                let color = if hovered == Some(r) { theme.text } else { theme.text_weak };
                let center = egui::pos2(rect.center().x, top + (r as f32 + 0.5) * row_h);
                painter.text(center, egui::Align2::CENTER_CENTER, if folded { "⏵" } else { "⏷" }, font.clone(), color);
            }
            if resp.clicked()
                && let Some(r) = hovered
            {
                click = Some(FoldClick::Toggle(r));
            }
            resp.context_menu(|ui| {
                if ui.button("Collapse All").clicked() {
                    click = Some(FoldClick::CollapseAll);
                    ui.close();
                }
                if ui.button("Expand All").clicked() {
                    click = Some(FoldClick::ExpandAll);
                    ui.close();
                }
            });
        });
    });
    click
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_collections_and_methods() {
        let cols = vec!["users".to_string(), "orders".to_string()];
        let (start, items) = completions("db.us", 5, &cols, CompletionMode::All).unwrap();
        assert_eq!(start, 3);
        assert_eq!(items, vec!["users"]);
        let (_, items) = completions("db.users.fin", 12, &cols, CompletionMode::All).unwrap();
        assert!(items.contains(&"find".to_string()) && items.contains(&"findOne".to_string()));
        let (_, items) = completions("db.getCollection('a b').find({}).so", 35, &cols, CompletionMode::All).unwrap();
        assert_eq!(items, vec!["sort"]);
        let (_, items) = completions("db.getCollection('a').ins", 25, &cols, CompletionMode::All).unwrap();
        assert!(items.contains(&"insertOne".to_string()));
        assert!(completions("db.us", 5, &cols, CompletionMode::Off).is_none());
    }

    #[test]
    fn completes_fields_and_operators() {
        let cols = vec!["items".to_string()];
        let mut fields = std::collections::HashMap::new();
        let cx_empty = CompletionCtx { collections: &cols, fields: &fields, mode: CompletionMode::All };
        let src = "db.getCollection('items').find({ ta";
        let c = complete(src, src.chars().count(), &cx_empty);
        assert_eq!(c.want_fields.as_deref(), Some("items"));
        fields.insert("items".to_string(), vec!["tag".into(), "total".into(), "address.city".into()]);
        let cx = CompletionCtx { collections: &cols, fields: &fields, mode: CompletionMode::All };
        let (start, items) = complete(src, src.chars().count(), &cx).result.unwrap();
        assert_eq!(start, src.chars().count() - 2);
        assert_eq!(items, vec!["tag"]);
        let src = "db.items.find({ n: 1, \"addr";
        let (_, items) = complete(src, src.chars().count(), &cx).result.unwrap();
        assert_eq!(items, vec!["address.city"]);
        let src = "db.items.find({ total: { $g";
        let (_, items) = complete(src, src.chars().count(), &cx).result.unwrap();
        assert_eq!(items, vec!["$gt", "$gte", "$geoWithin", "$geoIntersects", "$geometry"]);
        let src = "db.items.updateOne({ a: 1 }, { $se";
        let (_, items) = complete(src, src.chars().count(), &cx).result.unwrap();
        assert_eq!(items, vec!["$set", "$setOnInsert"]);
        let src = "db.items.aggregate([{ $gr";
        let (_, items) = complete(src, src.chars().count(), &cx).result.unwrap();
        assert_eq!(items, vec!["$group", "$graphLookup"]);
        // Values are not keys.
        let src = "db.items.find({ tag: ta";
        assert!(complete(src, src.chars().count(), &cx).result.is_none());
    }

    #[test]
    fn statement_collection_detection() {
        assert_eq!(statement_collection("db.getCollection('a b').find(").as_deref(), Some("a b"));
        assert_eq!(statement_collection("db.users.find(").as_deref(), Some("users"));
        assert_eq!(statement_collection("db.getCollectionNames(").as_deref(), None);
        assert_eq!(statement_collection("print(1)"), None);
    }

    #[test]
    fn tokenizer_spans_cover_text() {
        let src = "db.c.find({ a: /x/i, b: \"s\", n: 1.5 }) // done\nvar é = ObjectId()";
        let toks = tokenize(src);
        let total: usize = toks.iter().map(|(s, e, _)| e - s).sum();
        assert_eq!(total, src.len());
    }

    #[test]
    fn token_kinds() {
        let src = "var x = db.c.find({ a: \"s\", n: 1 }) // c\nObjectId(true)";
        let kinds: Vec<(String, Tok)> = tokenize(src).into_iter().map(|(s, e, t)| (src[s..e].to_string(), t)).collect();
        let find = |w: &str| kinds.iter().find(|(s, _)| s == w).map(|(_, t)| *t);
        assert!(find("var") == Some(Tok::Keyword));
        assert!(find("\"s\"") == Some(Tok::String));
        assert!(find("1") == Some(Tok::Number));
        assert!(find("// c") == Some(Tok::Comment));
        assert!(find("find") == Some(Tok::Function));
        assert!(find("a") == Some(Tok::Property));
        assert!(find("ObjectId") == Some(Tok::Bson));
        assert!(find("true") == Some(Tok::Constant));
    }

    #[test]
    fn highlight_job_covers_text() {
        let t = &crate::theme::THEMES[0];
        let job = highlight("db.x.find({})", t, FontId::monospace(12.0));
        assert_eq!(job.text, "db.x.find({})");
        assert!(job.sections.len() > 3);
        let empty = highlight("", t, FontId::monospace(12.0));
        assert_eq!(empty.text, "");
    }

    #[test]
    fn completion_misc() {
        let cols = vec!["a".to_string()];
        assert!(completions("x", 1, &cols, CompletionMode::All).is_none(), "one letter is too short");
        let (_, items) = completions("pri", 3, &cols, CompletionMode::All).unwrap();
        assert!(items.contains(&"print".to_string()) && items.contains(&"printjson".to_string()));
        let (_, items) = completions("rs.st", 5, &cols, CompletionMode::All).unwrap();
        assert!(items.contains(&"status".to_string()) && items.contains(&"stepDown".to_string()));
        let (_, items) = completions("db.", 3, &cols, CompletionMode::NoCollectionNames).unwrap();
        assert!(!items.contains(&"a".to_string()));
        assert!(completions("foo.ba", 6, &cols, CompletionMode::All).is_none());
    }
}
