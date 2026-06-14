//! JSON ⇄ C0DATA conversion.
//!
//! The intermediate [`Value`] tree and the [`to_value`]/[`from_value`]
//! conversions are dependency-free and always available. The JSON *text*
//! helpers ([`to_json`]/[`from_json`]) require the `json` feature, which pulls
//! in `serde_json` for parsing and serializing — everyone else pays nothing.

use crate::*;

/// An intermediate data tree (mirrors the Crystal `Value = String | Array |
/// Hash`). All scalars are strings; objects preserve insertion order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

fn bytes_to_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn unescape_string(b: &[u8]) -> String {
    String::from_utf8_lossy(&unescape(b)).into_owned()
}

// --- Export: C0DATA → Value ---

/// Build an intermediate [`Value`] tree from C0DATA bytes, detecting whether
/// the data is tabular, key-value, nested, or document-shaped.
pub fn to_value(buf: &[u8]) -> Value {
    match buf.first() {
        Some(&FS) => export_document(&Document::new(buf)),
        Some(&GS) => {
            let t = Table::new(buf);
            Value::Object(vec![(bytes_to_string(t.name()), export_group_data(&t))])
        }
        _ => Value::Object(vec![]),
    }
}

fn export_document(doc: &Document) -> Value {
    let name = bytes_to_string(doc.name());
    let mut groups = Vec::new();
    for g in doc.groups() {
        groups.push((bytes_to_string(g.name()), export_group_data(&g.table())));
    }
    if name.is_empty() {
        Value::Object(groups)
    } else {
        Value::Object(vec![(name, Value::Object(groups))])
    }
}

fn export_group_data(t: &Table) -> Value {
    if t.header_count() > 0 {
        export_table(t)
    } else if t.record_count() > 0 && t.record(0).field_count() == 2 {
        export_kv(t)
    } else if t.record_count() > 0 {
        export_records(t)
    } else {
        Value::Array(vec![])
    }
}

// Tabular: array of objects with header keys.
fn export_table(t: &Table) -> Value {
    let headers: Vec<String> = (0..t.header_count())
        .map(|i| bytes_to_string(t.header(i)))
        .collect();
    let mut rows = Vec::new();
    for rec in t.records() {
        let mut row = Vec::new();
        for (i, h) in headers.iter().enumerate() {
            row.push((h.clone(), field_to_value(rec.field(i))));
        }
        rows.push(Value::Object(row));
    }
    Value::Array(rows)
}

// Key-value: flat object (records of exactly two fields).
fn export_kv(t: &Table) -> Value {
    let mut obj = Vec::new();
    for rec in t.records() {
        obj.push((unescape_string(rec.field(0)), field_to_value(rec.field(1))));
    }
    Value::Object(obj)
}

// Raw records: array of arrays.
fn export_records(t: &Table) -> Value {
    let mut rows = Vec::new();
    for rec in t.records() {
        let fields = (0..rec.field_count())
            .map(|i| field_to_value(rec.field(i)))
            .collect();
        rows.push(Value::Array(fields));
    }
    Value::Array(rows)
}

// A field's bytes → Value. A field opening with STX is a nested structure.
fn field_to_value(field: &[u8]) -> Value {
    if field.first() == Some(&STX) {
        parse_nested_field(field)
    } else {
        Value::Str(unescape_string(field))
    }
}

// Parse a nested field (STX … ETX): RS inside ⇒ key-value object, else array.
fn parse_nested_field(field: &[u8]) -> Value {
    let mut stop = field.len();
    if stop > 0 && field[stop - 1] == ETX {
        stop -= 1;
    }
    let start = 1.min(field.len()); // skip STX

    let mut has_rs = false;
    let mut scan = start;
    while scan < stop {
        match field[scan] {
            RS => {
                has_rs = true;
                break;
            }
            STX => scan = skip_nested_bytes(field, scan, stop),
            DLE => scan += 2,
            _ => scan += 1,
        }
    }

    if has_rs {
        parse_nested_kv(field, start, stop)
    } else {
        parse_nested_array(field, start, stop)
    }
}

fn parse_nested_kv(field: &[u8], mut pos: usize, stop: usize) -> Value {
    let mut obj: Vec<(String, Value)> = Vec::new();
    while pos < stop {
        if field[pos] == RS {
            pos += 1;
            let key_start = pos;
            while pos < stop && field[pos] != US {
                if field[pos] == DLE {
                    pos += 2;
                } else {
                    pos += 1;
                }
            }
            let key = unescape_string(&field[key_start..pos.min(stop)]);
            if pos < stop && field[pos] == US {
                pos += 1;
                let val_start = pos;
                while pos < stop && field[pos] != RS {
                    if field[pos] == STX {
                        pos = skip_nested_bytes(field, pos, stop);
                    } else if field[pos] == DLE {
                        pos += 2;
                    } else {
                        pos += 1;
                    }
                }
                obj.push((key, field_to_value(&field[val_start..pos.min(stop)])));
            } else {
                obj.push((key, Value::Str(String::new())));
            }
        } else {
            pos += 1;
        }
    }
    Value::Object(obj)
}

fn parse_nested_array(field: &[u8], mut pos: usize, stop: usize) -> Value {
    let mut items = Vec::new();
    while pos < stop {
        if field[pos] == US {
            pos += 1;
            let item_start = pos;
            while pos < stop && field[pos] != US {
                if field[pos] == STX {
                    pos = skip_nested_bytes(field, pos, stop);
                } else if field[pos] == DLE {
                    pos += 2;
                } else {
                    pos += 1;
                }
            }
            items.push(field_to_value(&field[item_start..pos.min(stop)]));
        } else {
            pos += 1;
        }
    }
    Value::Array(items)
}

fn skip_nested_bytes(buf: &[u8], mut pos: usize, stop: usize) -> usize {
    pos += 1; // skip STX
    let mut depth = 1;
    while pos < stop && depth > 0 {
        match buf[pos] {
            STX => depth += 1,
            ETX => depth -= 1,
            DLE => pos += 1,
            _ => {}
        }
        pos += 1;
    }
    pos
}

// --- Import: Value → C0DATA ---

/// Emit a [`Value`] tree as C0DATA compact bytes.
pub fn from_value(value: &Value, group_name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    emit_root(value, group_name, &mut out);
    out
}

fn emit_root(value: &Value, group_name: &str, out: &mut Vec<u8>) {
    match value {
        Value::Object(pairs) => {
            if all_scalar(pairs) {
                write_group(group_name, out);
                for (k, v) in pairs {
                    out.push(RS);
                    write_escaped(k, out);
                    out.push(US);
                    write_escaped(as_str(v), out);
                }
            } else if pairs.len() == 1 {
                let (key, inner) = &pairs[0];
                if let Value::Object(ip) = inner {
                    if all_groupable(ip) {
                        out.push(FS);
                        out.extend_from_slice(key.as_bytes());
                        emit_hash_as_groups(ip, out);
                        return;
                    }
                }
                emit_hash_as_groups(pairs, out);
            } else {
                emit_hash_as_groups(pairs, out);
            }
        }
        Value::Array(items) => emit_array_as_group(items, group_name, out),
        Value::Str(s) => {
            write_group(group_name, out);
            out.push(RS);
            write_escaped(s, out);
        }
    }
}

fn emit_hash_as_groups(pairs: &[(String, Value)], out: &mut Vec<u8>) {
    for (name, value) in pairs {
        match value {
            Value::Object(inner) => {
                write_group(name, out);
                let scalar = all_scalar(inner);
                for (k, v) in inner {
                    out.push(RS);
                    write_escaped(k, out);
                    out.push(US);
                    if scalar {
                        write_escaped(as_str(v), out);
                    } else {
                        emit_field_value(v, out);
                    }
                }
            }
            Value::Array(arr) => emit_array_as_group(arr, name, out),
            Value::Str(s) => {
                write_group(name, out);
                out.push(RS);
                write_escaped(s, out);
            }
        }
    }
}

fn emit_array_as_group(arr: &[Value], name: &str, out: &mut Vec<u8>) {
    if tabular(arr) {
        let headers: Vec<&String> = match &arr[0] {
            Value::Object(p) => p.iter().map(|(k, _)| k).collect(),
            _ => Vec::new(),
        };
        write_group(name, out);
        out.push(SOH);
        for (i, h) in headers.iter().enumerate() {
            if i > 0 {
                out.push(US);
            }
            out.extend_from_slice(h.as_bytes());
        }
        for row in arr {
            if let Value::Object(p) = row {
                out.push(RS);
                for (i, key) in headers.iter().enumerate() {
                    if i > 0 {
                        out.push(US);
                    }
                    if let Some((_, v)) = p.iter().find(|(k, _)| &k == key) {
                        emit_field_value(v, out);
                    }
                }
            }
        }
    } else {
        write_group(name, out);
        for item in arr {
            match item {
                Value::Str(s) => {
                    out.push(RS);
                    write_escaped(s, out);
                }
                Value::Array(a) => {
                    out.push(RS);
                    for (i, v) in a.iter().enumerate() {
                        if i > 0 {
                            out.push(US);
                        }
                        emit_field_value(v, out);
                    }
                }
                Value::Object(p) => {
                    for (k, v) in p {
                        out.push(RS);
                        write_escaped(k, out);
                        out.push(US);
                        emit_field_value(v, out);
                    }
                }
            }
        }
    }
}

// Emit a value as a record field: scalars directly; nested values in STX/ETX.
fn emit_field_value(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Str(s) => write_escaped(s, out),
        Value::Object(p) => {
            out.push(STX);
            for (k, v) in p {
                out.push(RS);
                write_escaped(k, out);
                out.push(US);
                emit_field_value(v, out);
            }
            out.push(ETX);
        }
        Value::Array(a) => {
            out.push(STX);
            for item in a {
                out.push(US);
                emit_field_value(item, out);
            }
            out.push(ETX);
        }
    }
}

fn write_group(name: &str, out: &mut Vec<u8>) {
    out.push(GS);
    out.extend_from_slice(name.as_bytes());
}

fn write_escaped(s: &str, out: &mut Vec<u8>) {
    for &byte in s.as_bytes() {
        if byte < 0x20 {
            out.push(DLE);
        }
        out.push(byte);
    }
}

fn as_str(v: &Value) -> &str {
    match v {
        Value::Str(s) => s,
        _ => "",
    }
}

fn all_scalar(pairs: &[(String, Value)]) -> bool {
    pairs.iter().all(|(_, v)| matches!(v, Value::Str(_)))
}

fn all_groupable(pairs: &[(String, Value)]) -> bool {
    pairs
        .iter()
        .all(|(_, v)| matches!(v, Value::Object(_) | Value::Array(_)))
}

fn tabular(arr: &[Value]) -> bool {
    let keys: Vec<&String> = match arr.first() {
        Some(Value::Object(p)) => p.iter().map(|(k, _)| k).collect(),
        _ => return false,
    };
    arr.iter().all(|item| match item {
        Value::Object(p) => p.iter().map(|(k, _)| k).collect::<Vec<_>>() == keys,
        _ => false,
    })
}

// --- JSON text (requires the `json` feature) ---

/// Convert C0DATA bytes to a pretty JSON string. Requires the `json` feature.
#[cfg(feature = "json")]
pub fn to_json(buf: &[u8]) -> String {
    serde_json::to_string_pretty(&to_serde(&to_value(buf))).unwrap()
}

/// Convert a JSON string to C0DATA compact bytes. Requires the `json` feature.
#[cfg(feature = "json")]
pub fn from_json(input: &str, group_name: &str) -> std::result::Result<Vec<u8>, serde_json::Error> {
    let sv: serde_json::Value = serde_json::from_str(input)?;
    Ok(from_value(&from_serde(&sv), group_name))
}

#[cfg(feature = "json")]
fn to_serde(v: &Value) -> serde_json::Value {
    match v {
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Array(a) => serde_json::Value::Array(a.iter().map(to_serde).collect()),
        Value::Object(p) => {
            let mut m = serde_json::Map::new();
            for (k, val) in p {
                m.insert(k.clone(), to_serde(val));
            }
            serde_json::Value::Object(m)
        }
    }
}

#[cfg(feature = "json")]
fn from_serde(v: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    match v {
        J::Object(m) => Value::Object(
            m.iter()
                .map(|(k, val)| (k.clone(), from_serde(val)))
                .collect(),
        ),
        J::Array(a) => Value::Array(a.iter().map(from_serde).collect()),
        J::String(s) => Value::Str(s.clone()),
        J::Null => Value::Str(String::new()),
        other => Value::Str(other.to_string()),
    }
}
