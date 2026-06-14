use c0::json::{self, Value};
use c0::{Builder, Table};

// ---- Value conversion (always available, dependency-free) ----

#[test]
fn to_value_tabular() {
    let buf = Builder::build(|b| {
        b.group("users", Some(&["name", "amount"]));
        b.record(&["Alice", "100"]);
        b.record(&["Bob", "200"]);
    });
    // GS-rooted → { "users": [ {name,amount}, ... ] }
    let v = json::to_value(&buf);
    let Value::Object(top) = v else {
        panic!("expected object")
    };
    assert_eq!(top[0].0, "users");
    let Value::Array(rows) = &top[0].1 else {
        panic!("expected array")
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        Value::Object(vec![
            ("name".into(), Value::Str("Alice".into())),
            ("amount".into(), Value::Str("100".into())),
        ])
    );
}

#[test]
fn to_value_key_value() {
    // Two-field records with no header → flat object.
    let buf = Builder::build(|b| {
        b.group("config", None);
        b.record(&["host", "localhost"]);
        b.record(&["port", "5432"]);
    });
    let Value::Object(top) = json::to_value(&buf) else {
        panic!()
    };
    let Value::Object(kv) = &top[0].1 else {
        panic!("expected kv object")
    };
    assert_eq!(kv[0], ("host".into(), Value::Str("localhost".into())));
    assert_eq!(kv[1], ("port".into(), Value::Str("5432".into())));
}

#[test]
fn from_value_tabular_roundtrip() {
    let value = Value::Array(vec![
        Value::Object(vec![
            ("name".into(), Value::Str("Alice".into())),
            ("age".into(), Value::Str("30".into())),
        ]),
        Value::Object(vec![
            ("name".into(), Value::Str("Bob".into())),
            ("age".into(), Value::Str("25".into())),
        ]),
    ]);
    let buf = json::from_value(&value, "people");
    let t = Table::new(&buf);
    assert_eq!(t.name(), b"people");
    assert_eq!(t.headers(), vec![b"name".as_ref(), b"age".as_ref()]);
    assert_eq!(t.record_count(), 2);
    assert_eq!(t.record(1).field(0), b"Bob");
    // And it round-trips back to an equivalent Value tree.
    assert_eq!(
        json::to_value(&buf),
        Value::Object(vec![("people".into(), value)])
    );
}

#[test]
fn nested_field_roundtrip() {
    // A nested array inside a tabular field is STX/ETX-wrapped and round-trips.
    // (A bare array of scalars is intentionally not identity-preserving — the
    // shape heuristics read two 1-field records back as an array of arrays.)
    let value = Value::Object(vec![(
        "data".into(),
        Value::Array(vec![Value::Object(vec![
            ("name".into(), Value::Str("Alice".into())),
            (
                "tags".into(),
                Value::Array(vec![Value::Str("x".into()), Value::Str("y".into())]),
            ),
        ])]),
    )]);
    let buf = json::from_value(&value, "ignored");
    assert_eq!(json::to_value(&buf), value);
}

// ---- JSON text (requires the `json` feature) ----

#[cfg(feature = "json")]
#[test]
fn json_text_roundtrip() {
    let input = r#"{"users":[{"name":"Alice","amount":"100"},{"name":"Bob","amount":"200"}]}"#;
    let buf = json::from_json(input, "data").unwrap();

    let t = Table::new(&buf);
    assert_eq!(t.name(), b"users");
    assert_eq!(t.record_count(), 2);

    // Export back to JSON text; key order is preserved (preserve_order).
    let out = json::to_json(&buf);
    assert!(out.contains("\"name\": \"Alice\""));
    assert!(out.contains("\"amount\": \"200\""));
}

#[cfg(feature = "json")]
#[test]
fn json_scalars_become_strings() {
    // Numbers/bools/null collapse to strings (matching the Crystal Value model).
    let buf = json::from_json(r#"{"a":1,"b":true,"c":null}"#, "cfg").unwrap();
    let t = Table::new(&buf);
    assert_eq!(t.record(0).value(0).as_ref(), b"a");
    assert_eq!(t.record(0).value(1).as_ref(), b"1");
    assert_eq!(t.record(2).value(0).as_ref(), b"c");
    assert_eq!(t.record(2).value(1).as_ref(), b"");
}
