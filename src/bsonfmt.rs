//! Display helpers for BSON values: shell-syntax JSON (text view and editor),
//! short values (tree/table cells) and type names.

use bson::spec::BinarySubtype;
use bson::{Bson, Document};
use chrono::{Local, TimeZone, Utc};

use crate::settings::{TimeZoneMode, UuidEncoding};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FormatOptions {
    pub tz: TimeZoneMode,
    pub uuid: UuidEncoding,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self { tz: TimeZoneMode::Utc, uuid: UuidEncoding::Default }
    }
}

pub fn type_name(b: &Bson) -> &'static str {
    match b {
        Bson::Double(_) => "Double",
        Bson::String(_) => "String",
        Bson::Array(_) => "Array",
        Bson::Document(_) => "Object",
        Bson::Boolean(_) => "Boolean",
        Bson::Null => "Null",
        Bson::RegularExpression(_) => "Regular Expression",
        Bson::JavaScriptCode(_) => "JavaScript",
        Bson::JavaScriptCodeWithScope(_) => "JavaScript with scope",
        Bson::Int32(_) => "Int32",
        Bson::Int64(_) => "Int64",
        Bson::Timestamp(_) => "Timestamp",
        Bson::Binary(b) if b.subtype == BinarySubtype::Uuid => "UUID",
        Bson::Binary(b) if b.subtype == BinarySubtype::UuidOld => "Legacy UUID",
        Bson::Binary(_) => "Binary",
        Bson::ObjectId(_) => "ObjectId",
        Bson::DateTime(_) => "Date",
        Bson::Symbol(_) => "Symbol",
        Bson::Decimal128(_) => "Decimal128",
        Bson::Undefined => "Undefined",
        Bson::MaxKey => "MaxKey",
        Bson::MinKey => "MinKey",
        Bson::DbPointer(_) => "DBPointer",
    }
}

pub fn format_date(ms: i64, tz: TimeZoneMode, iso: bool) -> String {
    let Some(utc) = Utc.timestamp_millis_opt(ms).single() else {
        return format!("Date({ms})");
    };
    match (tz, iso) {
        (TimeZoneMode::Utc, true) => utc.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        (TimeZoneMode::Utc, false) => utc.format("%Y-%m-%d %H:%M:%S.%3fZ").to_string(),
        (TimeZoneMode::Local, true) => utc.with_timezone(&Local).format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string(),
        (TimeZoneMode::Local, false) => utc.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S.%3f%:z").to_string(),
    }
}

fn hex_uuid(bytes: &[u8]) -> String {
    let h = hex::encode(bytes);
    if h.len() != 32 {
        return h;
    }
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Decodes a legacy (subtype 3) UUID according to the driver convention.
fn legacy_uuid(bytes: &[u8], enc: UuidEncoding) -> Option<(String, &'static str)> {
    if bytes.len() != 16 {
        return None;
    }
    let mut b = bytes.to_vec();
    let name = match enc {
        UuidEncoding::Default => return None,
        UuidEncoding::PythonLegacy => "PYUUID",
        UuidEncoding::JavaLegacy => {
            b[0..8].reverse();
            b[8..16].reverse();
            "JUUID"
        }
        UuidEncoding::CSharpLegacy => {
            b[0..4].reverse();
            b[4..6].reverse();
            b[6..8].reverse();
            "CSUUID"
        }
    };
    Some((hex_uuid(&b), name))
}

fn fmt_double(d: f64) -> String {
    if d.is_nan() {
        "NaN".into()
    } else if d.is_infinite() {
        if d > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else if d.fract() == 0.0 && d.abs() < 1e16 {
        format!("{d:.1}")
    } else {
        format!("{d}")
    }
}

fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}

/// Scalar value in shell syntax (also used for objects/arrays when inline).
pub fn scalar_shell(b: &Bson, o: FormatOptions) -> String {
    match b {
        Bson::Double(d) => fmt_double(*d),
        Bson::String(s) => js_string(s),
        Bson::Boolean(v) => v.to_string(),
        Bson::Null => "null".into(),
        Bson::Undefined => "undefined".into(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => format!("NumberLong({i})"),
        Bson::Decimal128(d) => format!("NumberDecimal(\"{d}\")"),
        Bson::ObjectId(id) => format!("ObjectId(\"{}\")", id.to_hex()),
        Bson::DateTime(dt) => format!("ISODate(\"{}\")", format_date(dt.timestamp_millis(), TimeZoneMode::Utc, true)),
        Bson::Timestamp(ts) => format!("Timestamp({}, {})", ts.time, ts.increment),
        Bson::RegularExpression(r) => format!("/{}/{}", r.pattern, r.options),
        Bson::JavaScriptCode(c) => c.clone(),
        Bson::JavaScriptCodeWithScope(c) => c.code.clone(),
        Bson::Symbol(s) => js_string(s),
        Bson::MinKey => "MinKey".into(),
        Bson::MaxKey => "MaxKey".into(),
        Bson::DbPointer(_) => "DBPointer(…)".into(),
        Bson::Binary(bin) => match bin.subtype {
            BinarySubtype::Uuid if bin.bytes.len() == 16 => format!("UUID(\"{}\")", hex_uuid(&bin.bytes)),
            BinarySubtype::UuidOld => match legacy_uuid(&bin.bytes, o.uuid) {
                Some((u, name)) => format!("{name}(\"{u}\")"),
                None => format!("BinData(3,\"{}\")", b64(&bin.bytes)),
            },
            st => format!("BinData({},\"{}\")", u8::from(st), b64(&bin.bytes)),
        },
        Bson::Array(_) | Bson::Document(_) => to_shell_json(b, 0, o),
    }
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Multi-line shell JSON with 4-space indentation, as the Robo 3T text view.
pub fn to_shell_json(b: &Bson, indent: usize, o: FormatOptions) -> String {
    let mut s = String::new();
    write_shell(&mut s, b, indent, o);
    s
}

fn write_shell(out: &mut String, b: &Bson, indent: usize, o: FormatOptions) {
    let pad = |n: usize| "    ".repeat(n);
    match b {
        Bson::Document(d) => {
            if d.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            let n = d.len();
            for (i, (k, v)) in d.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                out.push_str(&js_string(k));
                out.push_str(" : ");
                write_shell(out, v, indent + 1, o);
                if i + 1 < n {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(indent));
            out.push('}');
        }
        Bson::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, v) in a.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                write_shell(out, v, indent + 1, o);
                if i + 1 < a.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(indent));
            out.push(']');
        }
        other => out.push_str(&scalar_shell(other, o)),
    }
}

pub fn doc_to_shell(d: &Document, o: FormatOptions) -> String {
    to_shell_json(&Bson::Document(d.clone()), 0, o)
}

/// Value shown in tree/table cells.
pub fn short_value(b: &Bson, o: FormatOptions) -> String {
    match b {
        Bson::Document(d) => format!("{{ {} field{} }}", d.len(), if d.len() == 1 { "" } else { "s" }),
        Bson::Array(a) => format!("[ {} element{} ]", a.len(), if a.len() == 1 { "" } else { "s" }),
        Bson::String(s) => {
            let one_line: String = s.chars().take(500).map(|c| if c == '\n' { '⏎' } else { c }).collect();
            one_line
        }
        Bson::DateTime(dt) => format_date(dt.timestamp_millis(), o.tz, false),
        Bson::Int64(i) => i.to_string(),
        Bson::Decimal128(d) => d.to_string(),
        Bson::JavaScriptCode(c) => c.lines().next().unwrap_or_default().to_string(),
        other => scalar_shell(other, o),
    }
}

/// Root label used by the tree view: `(1) ObjectId("…")`, like Robo 3T.
pub fn root_label(index: usize, b: &Bson, o: FormatOptions) -> String {
    match b {
        Bson::Document(d) => match d.get("_id") {
            Some(id @ (Bson::Document(_) | Bson::Array(_))) => format!("({index}) {}", short_value(id, o)),
            Some(id) => format!("({index}) {}", scalar_shell(id, o)),
            None => format!("({index})"),
        },
        other => format!("({index}) {}", short_value(other, o)),
    }
}

pub fn is_container(b: &Bson) -> bool {
    matches!(b, Bson::Document(_) | Bson::Array(_))
}

/// Human-readable byte size (for stats tooltips).
pub fn human_bytes(n: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{v:.0} {}", UNITS[u]) } else { format!("{v:.2} {}", UNITS[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn shell_json() {
        let d = doc! { "_id": bson::oid::ObjectId::parse_str("5f1d7a0e1c9d440000a1b2c3").unwrap(), "n": 1, "f": 2.0, "l": 3i64, "a": [1, "x"], "e": {} };
        let s = doc_to_shell(&d, FormatOptions::default());
        assert_eq!(
            s,
            "{\n    \"_id\" : ObjectId(\"5f1d7a0e1c9d440000a1b2c3\"),\n    \"n\" : 1,\n    \"f\" : 2.0,\n    \"l\" : NumberLong(3),\n    \"a\" : [\n        1,\n        \"x\"\n    ],\n    \"e\" : {}\n}"
        );
    }

    #[test]
    fn java_legacy_uuid() {
        let bytes: Vec<u8> = (0u8..16).collect();
        let (u, n) = legacy_uuid(&bytes, UuidEncoding::JavaLegacy).unwrap();
        assert_eq!(n, "JUUID");
        assert_eq!(u, "07060504-0302-0100-0f0e-0d0c0b0a0908");
    }
}
