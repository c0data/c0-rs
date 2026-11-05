use c0::json::{self, Value};
use c0::{Body, Builder};

fn s(x: &str) -> Value {
    Value::Str(x.into())
}

// ---- Value conversion (always available, dependency-free) ----

#[test]
fn to_value_tabular() {
    let buf = Builder::build(|b| {
        b.group("users", Some(&["name", "amount"]));
        b.record(&["Alice", "100"]);
        b.record(&["Bob", "200"]);
    });
    let v = json::to_value(&buf).unwrap();
    assert_eq!(
        v,
        Value::Object(vec![(
            "users".into(),
            Value::Array(vec![
                Value::Object(vec![
                    ("name".into(), s("Alice")),
                    ("amount".into(), s("100"))
                ]),
                Value::Object(vec![("name".into(), s("Bob")), ("amount".into(), s("200"))]),
            ])
        )])
    );
}

#[test]
fn to_value_keyed_and_positional_never_guess() {
    let buf = Builder::build(|b| {
        b.group("config", None);
        b.map(&[("host", "localhost"), ("port", "5432")]);
        b.group("pairs", None);
        b.record(&["host", "localhost"]);
    });
    let v = json::to_value(&buf).unwrap();
    assert_eq!(
        v,
        Value::Object(vec![
            (
                "config".into(),
                Value::Object(vec![
                    ("host".into(), s("localhost")),
                    ("port".into(), s("5432"))
                ])
            ),
            (
                "pairs".into(),
                Value::Array(vec![Value::Array(vec![s("host"), s("localhost")])])
            ),
        ])
    );
}

#[test]
fn to_value_levels_and_references() {
    let buf = Builder::build(|b| {
        b.group("articles", Some(&["title", "tags", "primary", "meta"]));
        b.begin_record();
        b.field("Post");
        b.list(&["x", "y"]);
        b.reference_path(&["tags", "001"]);
        b.level(|b| {
            b.map(&[("k", "v")]);
        });
    });
    let v = json::to_value(&buf).unwrap();
    let Value::Object(top) = v else { panic!() };
    let Value::Array(rows) = &top[0].1 else {
        panic!()
    };
    assert_eq!(
        rows[0],
        Value::Object(vec![
            ("title".into(), s("Post")),
            ("tags".into(), Value::Array(vec![s("x"), s("y")])),
            (
                "primary".into(),
                Value::Object(vec![("$ref".into(), s("tags/001"))])
            ),
            ("meta".into(), Value::Object(vec![("k".into(), s("v"))])),
        ])
    );
}

#[test]
fn from_value_tabular_roundtrip() {
    let value = Value::Array(vec![
        Value::Object(vec![("name".into(), s("Alice")), ("age".into(), s("30"))]),
        Value::Object(vec![("name".into(), s("Bob")), ("age".into(), s("25"))]),
    ]);
    let buf = json::from_value(&value, "people");
    let body = Body::new(&buf).unwrap();
    let g = body.group(0);
    assert_eq!(g.name(), b"people");
    assert_eq!(g.headers(), vec![b"name".as_ref(), b"age".as_ref()]);
    assert_eq!(g.record(1).value(0).unwrap().as_ref(), b"Bob");
    assert_eq!(
        json::to_value(&buf).unwrap(),
        Value::Object(vec![("people".into(), value)])
    );
}

#[test]
fn from_value_nested_roundtrip() {
    let value = Value::Object(vec![(
        "data".into(),
        Value::Array(vec![Value::Object(vec![
            ("name".into(), s("Alice")),
            ("tags".into(), Value::Array(vec![s("x"), s("y")])),
            (
                "home".into(),
                Value::Object(vec![("city".into(), s("Springfield"))]),
            ),
        ])]),
    )]);
    let buf = json::from_value(&value, "ignored");
    assert!(c0::canonical(&buf));
    assert_eq!(json::to_value(&buf).unwrap(), value);
}

#[test]
fn from_value_object_of_scalars_is_a_map() {
    let value = Value::Object(vec![("a".into(), s("1")), ("b".into(), s("2"))]);
    let buf = json::from_value(&value, "cfg");
    let body = Body::new(&buf).unwrap();
    let rec = body.group(0).record(0);
    assert!(rec.keyed());
    assert_eq!(rec.keys(), vec![b"a".as_ref(), b"b"]);
    assert_eq!(
        json::to_value(&buf).unwrap(),
        Value::Object(vec![("cfg".into(), value)])
    );
}

// ---- JSON text (requires the `json` feature) ----

#[cfg(feature = "json")]
#[test]
fn json_text_roundtrip() {
    let input = r#"{"users":[{"name":"Alice","amount":"100"},{"name":"Bob","amount":"200"}]}"#;
    let buf = json::from_json(input, "data").unwrap();
    let body = Body::new(&buf).unwrap();
    assert_eq!(body.group(0).name(), b"users");
    assert_eq!(body.group(0).record_count(), 2);
    let out = json::to_json(&buf).unwrap();
    assert!(out.contains("\"name\": \"Alice\""));
    assert!(out.contains("\"amount\": \"200\""));
}

#[cfg(feature = "json")]
#[test]
fn json_scalars_become_strings() {
    let buf = json::from_json(r#"{"a":1,"b":true,"c":null}"#, "cfg").unwrap();
    let body = Body::new(&buf).unwrap();
    let rec = body.group(0).record(0);
    assert_eq!(rec.get("a").unwrap().unwrap().as_ref(), b"1");
    assert_eq!(rec.get("b").unwrap().unwrap().as_ref(), b"true");
    assert_eq!(rec.get("c").unwrap().unwrap().as_ref(), b"");
}
