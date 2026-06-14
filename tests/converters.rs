use c0::diff::{self, DiffError};
use c0::{csv, format_mode, parse, Builder, FormatMode, Table};
use std::collections::HashMap;

// ---- CSV ----

#[test]
fn csv_from_with_headers() {
    let buf = csv::from_csv("name,amount\nAlice,100\nBob,200\n", "users");
    let t = Table::new(&buf);
    assert_eq!(t.name(), b"users");
    assert_eq!(t.headers(), vec![b"name".as_ref(), b"amount".as_ref()]);
    assert_eq!(t.record_count(), 2);
    assert_eq!(t.record(1).field(0), b"Bob");
}

#[test]
fn csv_empty_and_headers_only() {
    assert!(csv::from_csv("", "data").is_empty());
    let t_buf = csv::from_csv("a,b,c\n", "t");
    let t = Table::new(&t_buf);
    assert_eq!(t.header_count(), 3);
    assert_eq!(t.record_count(), 0);
}

#[test]
fn csv_quoted_fields() {
    let buf = csv::from_csv("\"hello, world\",plain\n\"line1\nline2\",ok\n", "q");
    let t = Table::new(&buf);
    assert_eq!(t.header(0), b"hello, world");
    assert_eq!(t.header(1), b"plain");
    // The embedded newline is a control byte, so it is stored DLE-escaped;
    // value() decodes it back.
    assert_eq!(t.record(0).value(0).as_ref(), b"line1\nline2");
}

#[test]
fn csv_to_roundtrip() {
    let buf = Builder::build(|b| {
        b.group("users", Some(&["name", "amount"]));
        b.record(&["Alice", "100"]);
        b.record(&["Bob", "200"]);
    });
    assert_eq!(csv::to_csv(&buf), "name,amount\nAlice,100\nBob,200\n");
}

#[test]
fn csv_to_quotes_when_needed() {
    let buf = Builder::build(|b| {
        b.group("g", Some(&["a", "b"]));
        b.record(&["x,y", "z"]);
    });
    assert_eq!(csv::to_csv(&buf), "a,b\n\"x,y\",z\n");
}

// ---- C0DIFF ----

#[test]
fn diff_parse_simple() {
    let buf = diff::build(|b| {
        b.file("foo.txt", |b| {
            b.replace("Hello ", "world", "universe", "!");
        });
    });
    let edits = diff::parse(&buf);
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].path, b"foo.txt");
    let s = &edits[0].sections[0];
    assert_eq!(s.search_pattern(), b"Hello world!");
    assert_eq!(s.replacement(), b"Hello universe!");
}

#[test]
fn diff_parse_section_builder() {
    let buf = diff::build(|b| {
        b.file("test.txt", |b| {
            b.section(|s| {
                s.anchor("prefix ");
                s.sub("old_value", "new_value");
                s.anchor(" suffix");
            });
        });
    });
    let edits = diff::parse(&buf);
    let s = &edits[0].sections[0];
    assert_eq!(s.search_pattern(), b"prefix old_value suffix");
    assert_eq!(s.replacement(), b"prefix new_value suffix");
}

#[test]
fn diff_apply_substitution() {
    let buf = diff::build(|b| {
        b.file("foo.txt", |b| {
            b.replace("Hello ", "world", "universe", "!");
        });
    });
    let mut files = HashMap::new();
    files.insert("foo.txt".to_string(), "Hello world!".to_string());
    let out = diff::apply(&buf, &files).unwrap();
    assert_eq!(out["foo.txt"], "Hello universe!");
}

#[test]
fn diff_apply_errors() {
    let buf = diff::build(|b| {
        b.file("f.txt", |b| {
            b.replace("", "x", "y", "");
        });
    });

    let mut missing = HashMap::new();
    missing.insert("other.txt".to_string(), "x".to_string());
    assert!(matches!(
        diff::apply(&buf, &missing),
        Err(DiffError::FileNotFound(_))
    ));

    let mut absent = HashMap::new();
    absent.insert("f.txt".to_string(), "no match here".to_string());
    assert!(matches!(
        diff::apply(&buf, &absent),
        Err(DiffError::PatternNotFound { .. })
    ));

    let mut ambiguous = HashMap::new();
    ambiguous.insert("f.txt".to_string(), "x and x".to_string());
    assert!(matches!(
        diff::apply(&buf, &ambiguous),
        Err(DiffError::PatternAmbiguous { count: 2, .. })
    ));
}

// ---- Pretty alignment ----

#[test]
fn pretty_aligned_roundtrips() {
    let buf = Builder::build(|b| {
        b.group("users", Some(&["name", "amount"]));
        b.record(&["Alice", "1502.30"]);
        b.record(&["Bob", "20"]);
    });
    for mode in [FormatMode::Aligned, FormatMode::Spaced] {
        let pretty = format_mode(&buf, "  ", mode);
        assert_eq!(parse(&pretty), buf, "{mode:?} must round-trip");
    }
}

#[test]
fn pretty_spaced_inserts_spaces() {
    let buf = Builder::build(|b| {
        b.group("users", Some(&["name", "amount"]));
        b.record(&["Alice", "100"]);
        b.record(&["Bob", "200"]);
    });
    let spaced = format_mode(&buf, "  ", FormatMode::Spaced);
    // Columns aligned: "Alice" (5) and "Bob  " (padded) before the ␟.
    assert!(spaced.contains("Alice \u{241f} 100"));
    assert!(spaced.contains("Bob   \u{241f} 200"));
}
