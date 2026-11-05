use c0::diff::{self, DiffError};
use c0::{csv, format_mode, parse, Body, Builder, FormatMode};
use std::collections::HashMap;

// ---- CSV ----

#[test]
fn csv_from_with_headers() {
    let buf = csv::from_csv("name,amount\nAlice,100\nBob,200\n", "users");
    let body = Body::new(&buf).unwrap();
    let g = body.group(0);
    assert_eq!(g.name(), b"users");
    assert_eq!(g.headers(), vec![b"name".as_ref(), b"amount".as_ref()]);
    assert_eq!(g.record_count(), 2);
    assert_eq!(g.record(1).value(0).unwrap().as_ref(), b"Bob");
}

#[test]
fn csv_empty_and_headers_only() {
    assert!(csv::from_csv("", "data").is_empty());
    let buf = csv::from_csv("a,b,c\n", "t");
    let body = Body::new(&buf).unwrap();
    assert_eq!(body.group(0).header_count(), 3);
    assert_eq!(body.group(0).record_count(), 0);
}

#[test]
fn csv_quoted_fields() {
    let buf = csv::from_csv("\"hello, world\",plain\n\"line1\nline2\",ok\n", "q");
    let body = Body::new(&buf).unwrap();
    let g = body.group(0);
    assert_eq!(g.header(0), b"hello, world");
    assert_eq!(g.header(1), b"plain");
    // The embedded newline makes the value text; value() gives it back.
    assert_eq!(g.record(0).value(0).unwrap().as_ref(), b"line1\nline2");
}

#[test]
fn csv_to_roundtrip_and_references() {
    let buf = Builder::build(|b| {
        b.group("users", Some(&["name", "amount", "tag"]));
        b.begin_record();
        b.field("Alice");
        b.field("x,y");
        b.reference_path(&["tags", "001"]);
        b.record(&["Bob", "200", ""]);
    });
    assert_eq!(
        csv::to_csv(&buf).unwrap(),
        "name,amount,tag\nAlice,\"x,y\",tags/001\nBob,200,\n"
    );
    assert!(csv::to_csv_group(&buf, Some("missing")).is_err());
    let nested = Builder::build(|b| {
        b.group("g", None);
        b.begin_record();
        b.list(&["a"]);
    });
    assert!(csv::to_csv(&nested).is_err());
}

// ---- C0-DIFF ----

#[test]
fn diff_parse_simple() {
    let buf = diff::build(|b| {
        b.file("foo.txt", |b| {
            b.replace("Hello ", "world", "universe", "!");
        });
        b.eot();
    });
    assert_eq!(&buf[..12], b"\x16\x01shape\x1fdiff");
    let edits = diff::parse(&buf).unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].path, "foo.txt");
    let s = &edits[0].sections[0];
    assert_eq!(s.search(), b"Hello world!");
    assert_eq!(s.replacement(), b"Hello universe!");
}

#[test]
fn diff_parse_section_builder_and_replace_all() {
    let buf = diff::build(|b| {
        b.file("test.txt", |b| {
            b.replace_all("tabs", "spaces");
            b.section(|s| {
                s.anchor("prefix ")
                    .sub("old_value", "new_value")
                    .anchor(" suffix");
            });
            b.note("careful");
            b.replace_all("count", "total");
        });
    });
    let edits = diff::parse(&buf).unwrap();
    let e = &edits[0];
    assert_eq!(
        e.leading_replaces,
        vec![(b"tabs".to_vec(), b"spaces".to_vec())]
    );
    let s = &e.sections[0];
    assert_eq!(s.search(), b"prefix old_value suffix");
    assert_eq!(s.replacement(), b"prefix new_value suffix");
    assert_eq!(s.replaces, vec![(b"count".to_vec(), b"total".to_vec())]);
}

#[test]
fn diff_apply_substitution_and_scope() {
    let buf = diff::build(|b| {
        b.file("tally.rb", |b| {
            b.section(|s| {
                s.anchor("def tally(items)");
            });
            b.replace_all("count", "total");
            b.section(|s| {
                s.anchor("end");
            });
        });
    });
    let mut files = HashMap::new();
    files.insert(
        "tally.rb".to_string(),
        "count = 0\ndef tally(items)\n  count\nend\ncount\n".to_string(),
    );
    let out = diff::apply(&buf, &files).unwrap();
    assert_eq!(
        out["tally.rb"],
        "count = 0\ndef tally(items)\n  total\nend\ncount\n"
    );
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
        Err(DiffError::PatternAmbiguous { .. })
    ));

    let wrong_shape = b"\x16\x01shape\x1fstream\x1cf.txt\x1dx\x1ay";
    assert!(matches!(
        diff::parse(wrong_shape),
        Err(DiffError::WrongShape(_))
    ));
}

#[test]
fn diff_apply_files_on_disk() {
    let dir = std::env::temp_dir().join(format!("c0rs-diff-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "foo bar\n").unwrap();
    let buf = diff::build(|b| {
        b.file("a.txt", |b| {
            b.replace("", "foo", "FOO", "");
        });
        b.eot();
    });
    diff::apply_files(&buf, &dir).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "FOO bar\n"
    );
    let escape = diff::build(|b| {
        b.file("../escape.txt", |b| {
            b.replace("", "a", "b", "");
        });
    });
    assert!(diff::apply_files(&escape, &dir).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

// ---- Pretty alignment ----

#[test]
fn pretty_aligned_roundtrips() {
    let buf = Builder::build(|b| {
        b.group("users", Some(&["name", "amount"]));
        b.record(&["Alice", "1502.30"]);
        b.record(&["Bob", "20"]);
        b.group("cfg", None);
        b.map(&[("host", "localhost"), ("port", "5432")]);
    });
    for mode in [FormatMode::Compact, FormatMode::Aligned, FormatMode::Spaced] {
        let pretty = format_mode(&buf, "  ", mode).unwrap();
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
    let spaced = format_mode(&buf, "  ", FormatMode::Spaced).unwrap();
    assert!(spaced.contains("Alice \u{241f} 100"), "{spaced}");
    assert!(spaced.contains("Bob   \u{241f} 200"), "{spaced}");
    assert!(spaced.contains("\u{241d} users"), "{spaced}");
}
