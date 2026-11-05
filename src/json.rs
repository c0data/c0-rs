//! JSON ⇄ C0DATA conversion.
//!
//! The intermediate [`Value`] tree and the [`to_value`]/[`from_value`]
//! conversions are dependency-free and always available. The JSON *text*
//! helpers ([`to_json`]/[`from_json`]) need the `json` feature, which pulls
//! in `serde_json`.
//!
//! Export never guesses: a positional record is an array, a keyed record
//! an object, a group with a header row an array of objects, a reference
//! `{"$ref": "group/record/field"}`.

use crate::*;

/// An intermediate data tree. All scalars are strings; objects keep
/// insertion order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

// --- Export: C0DATA → Value ---

/// Build a [`Value`] tree from C0DATA bytes (the first document's body).
pub fn to_value(buf: &[u8]) -> Result<Value> {
    let doc = Document::new(buf)?;
    export_body(doc.body(), false)
}

/// A body: `{file => groups}` per named file, or the groups directly; a
/// bare body (one unnamed group) exports as that group.
pub fn export_body(body: &Body<'_>, level: bool) -> Result<Value> {
    let files = body.files();
    if files.len() == 1 {
        let f = &files[0];
        if f.name().is_empty() {
            return export_groups(f.groups(), level);
        }
        return Ok(Value::Object(vec![(
            s(f.name()),
            export_groups(f.groups(), level)?,
        )]));
    }
    let mut out = Vec::new();
    for f in files {
        out.push((s(f.name()), export_groups(f.groups(), level)?));
    }
    Ok(Value::Object(out))
}

fn export_groups(groups: &[Group<'_>], level: bool) -> Result<Value> {
    if groups.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    if groups.len() == 1 && groups[0].name().is_empty() && groups[0].depth() == 0 {
        return export_group(&groups[0], level);
    }
    let mut out = Vec::new();
    for g in groups {
        out.push((s(g.name()), export_group(g, false)?));
    }
    Ok(Value::Object(out))
}

/// A group: with a header row, an array of objects; a single keyed record
/// is an object; otherwise an array of its records.
pub fn export_group(g: &Group<'_>, level: bool) -> Result<Value> {
    if g.has_header() {
        let headers: Vec<String> = g.headers().iter().map(|h| s(h)).collect();
        let mut rows = Vec::new();
        for rec in g.records() {
            if rec.keyed() {
                rows.push(export_record(rec)?);
            } else {
                let n = rec.field_count();
                let mut row = Vec::new();
                for (i, h) in headers.iter().enumerate() {
                    let v = if i < n {
                        export_raw(rec.field(i))?
                    } else {
                        Value::Str(String::new())
                    };
                    row.push((h.clone(), v));
                }
                rows.push(Value::Object(row));
            }
        }
        return Ok(Value::Array(rows));
    }
    let recs = g.records();
    if recs.len() == 1 && (recs[0].keyed() || level) {
        return export_record(&recs[0]);
    }
    recs.iter()
        .map(export_record)
        .collect::<Result<Vec<_>>>()
        .map(Value::Array)
}

/// A record: a keyed record is an object (last value wins for a duplicate
/// key), a positional one an array.
pub fn export_record(rec: &Record<'_>) -> Result<Value> {
    if rec.keyed() {
        let mut obj: Vec<(String, Value)> = Vec::new();
        for (k, _) in rec.pairs()? {
            let key = s(k);
            let v = export_raw(rec.get_raw(&key).unwrap_or(&[]))?;
            match obj.iter_mut().find(|(ok, _)| *ok == key) {
                Some(slot) => slot.1 = v,
                None => obj.push((key, v)),
            }
        }
        Ok(Value::Object(obj))
    } else {
        rec.fields()
            .into_iter()
            .map(export_raw)
            .collect::<Result<Vec<_>>>()
            .map(Value::Array)
    }
}

fn export_raw(raw: &[u8]) -> Result<Value> {
    if is_level(raw) {
        export_body(&Body::of_level(raw)?, true)
    } else if is_reference(raw) {
        let r = Reference::parse(raw)?;
        let path: Vec<String> = r.path().iter().map(|seg| s(seg)).collect();
        Ok(Value::Object(vec![(
            "$ref".to_string(),
            Value::Str(path.join("/")),
        )]))
    } else {
        Ok(Value::Str(s(&value(raw)?)))
    }
}

// --- Import: Value → C0DATA ---

/// Emit a [`Value`] tree as canonical C0DATA bytes. An object of groups
/// becomes groups (one key wrapping an object of groups becomes a file);
/// an array becomes one group named `group_name`; an object of scalars one
/// group holding one map; a scalar one record.
pub fn from_value(value: &Value, group_name: &str) -> Vec<u8> {
    Builder::build(|b| match value {
        Value::Object(pairs) => {
            if let [(name, Value::Object(inner))] = pairs.as_slice() {
                if all_groupable(inner) {
                    b.file(name);
                    emit_groups(b, inner);
                    return;
                }
            }
            if all_groupable(pairs) {
                emit_groups(b, pairs);
            } else {
                b.group(group_name, None);
                emit_map(b, pairs);
            }
        }
        Value::Array(items) => emit_array_as_group(b, items, group_name),
        Value::Str(text) => {
            b.group(group_name, None);
            b.record(&[text]);
        }
    })
}

fn emit_groups(b: &mut Builder, pairs: &[(String, Value)]) {
    for (name, value) in pairs {
        match value {
            Value::Object(inner) => {
                b.group(name, None);
                emit_map(b, inner);
            }
            Value::Array(items) => emit_array_as_group(b, items, name),
            Value::Str(text) => {
                b.group(name, None);
                b.record(&[text]);
            }
        }
    }
}

// An object → one keyed record, entries in object order.
fn emit_map(b: &mut Builder, pairs: &[(String, Value)]) {
    b.begin_record();
    for (k, v) in pairs {
        match v {
            Value::Str(text) => {
                b.key(k, text);
            }
            other => {
                b.key_level(k, |b| emit_level_body(b, other));
            }
        }
    }
}

fn emit_array_as_group(b: &mut Builder, arr: &[Value], name: &str) {
    if let Some(headers) = tabular(arr) {
        let refs: Vec<&str> = headers.iter().map(String::as_str).collect();
        b.group(name, Some(&refs));
        for row in arr {
            if let Value::Object(p) = row {
                b.begin_record();
                for k in &headers {
                    emit_unit(b, lookup(p, k));
                }
            }
        }
    } else {
        b.group(name, None);
        for item in arr {
            emit_item_as_record(b, item);
        }
    }
}

fn emit_item_as_record(b: &mut Builder, item: &Value) {
    match item {
        Value::Str(text) => {
            b.record(&[text]);
        }
        Value::Array(items) => {
            b.begin_record();
            for v in items {
                emit_unit(b, v);
            }
        }
        Value::Object(pairs) => emit_map(b, pairs),
    }
}

// A value in unit position: a scalar, or a level.
fn emit_unit(b: &mut Builder, value: &Value) {
    match value {
        Value::Str(text) => {
            b.field(text);
        }
        other => {
            b.level(|b| emit_level_body(b, other));
        }
    }
}

// Inside a level: an array of scalars is one record (a list); an array of
// uniform objects a table with a header row; an array of arrays a table; a
// mixed array one record per item; an object a map.
fn emit_level_body(b: &mut Builder, value: &Value) {
    match value {
        Value::Object(pairs) => emit_map(b, pairs),
        Value::Array(items) => {
            if items.iter().all(|v| matches!(v, Value::Str(_))) {
                b.begin_record();
                for v in items {
                    if let Value::Str(text) = v {
                        b.field(text);
                    }
                }
            } else if let Some(headers) = tabular(items) {
                let refs: Vec<&str> = headers.iter().map(String::as_str).collect();
                b.header(&refs);
                for row in items {
                    if let Value::Object(p) = row {
                        b.begin_record();
                        for k in &headers {
                            emit_unit(b, lookup(p, k));
                        }
                    }
                }
            } else if items.iter().all(|v| matches!(v, Value::Array(_))) {
                for row in items {
                    if let Value::Array(cells) = row {
                        b.begin_record();
                        for v in cells {
                            emit_unit(b, v);
                        }
                    }
                }
            } else {
                for item in items {
                    emit_item_as_record(b, item);
                }
            }
        }
        Value::Str(text) => {
            b.record(&[text]);
        }
    }
}

static EMPTY: Value = Value::Str(String::new());

fn lookup<'v>(pairs: &'v [(String, Value)], key: &str) -> &'v Value {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map_or(&EMPTY, |(_, v)| v)
}

fn all_groupable(pairs: &[(String, Value)]) -> bool {
    !pairs.is_empty()
        && pairs
            .iter()
            .all(|(_, v)| matches!(v, Value::Object(_) | Value::Array(_)))
}

// The shared keys of an array of uniform objects, or None.
fn tabular(arr: &[Value]) -> Option<Vec<String>> {
    let keys: Vec<String> = match arr.first()? {
        Value::Object(p) => p.iter().map(|(k, _)| k.clone()).collect(),
        _ => return None,
    };
    let uniform = arr.iter().all(|item| match item {
        Value::Object(p) => p.iter().map(|(k, _)| k).eq(keys.iter()),
        _ => false,
    });
    uniform.then_some(keys)
}

// --- JSON text (requires the `json` feature) ---

/// C0DATA bytes → pretty JSON text. Requires the `json` feature.
#[cfg(feature = "json")]
pub fn to_json(buf: &[u8]) -> Result<String> {
    let v = to_value(buf)?;
    Ok(serde_json::to_string_pretty(&to_serde(&v)).expect("a Value tree serializes"))
}

/// JSON text → canonical C0DATA bytes. Requires the `json` feature.
#[cfg(feature = "json")]
pub fn from_json(input: &str, group_name: &str) -> std::result::Result<Vec<u8>, serde_json::Error> {
    let sv: serde_json::Value = serde_json::from_str(input)?;
    Ok(from_value(&from_serde(&sv), group_name))
}

#[cfg(feature = "json")]
fn to_serde(v: &Value) -> serde_json::Value {
    match v {
        Value::Str(text) => serde_json::Value::String(text.clone()),
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
        J::String(text) => Value::Str(text.clone()),
        J::Null => Value::Str(String::new()),
        other => Value::Str(other.to_string()),
    }
}
