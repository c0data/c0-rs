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
        b.reference_to("tags", "001", None);
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

// ---- C0DIFF ----

#[test]
fn diff_parse_simple() {
    let buf = diff::build(|b| {
        b.file("foo.txt", |b| {
            b.replace("Hello ", "world", "universe", "!");
        });
        b.eot();
    });
    assert_eq!(&buf[..12], b"\x16\x01shape\x1fdiff");
    assert_eq!(
        buf,
        parse("␖␁shape␟diff␜foo.txt␞␂Hello ␃␟world␚␟universe␟!␄")
    );
    let edits = diff::parse(&buf).unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].path, "foo.txt");
    let p = &edits[0].records[0];
    assert_eq!(p.search(), b"Hello world!");
    assert_eq!(p.replacement(), b"Hello universe!");
    assert!(!p.is_assertion());
}

#[test]
fn diff_builder_records() {
    let buf = diff::build(|b| {
        b.file("test.txt", |b| {
            b.record(|r| {
                r.anchor("prefix ")
                    .sub("old_value", "new_value")
                    .anchor(" suffix");
            });
            b.note("careful");
            b.insert_after("import os", "\nimport sys");
            b.delete("# TODO", "", "");
            b.assert("def tally(items)");
        });
    });
    assert_eq!(
        buf,
        parse("␖␁shape␟diff␜test.txt␞␂prefix ␃␟old_value␚␟new_value␟␂ suffix␃␇careful␞import os␟␚␟␂\nimport sys␃␞# TODO␚␟␞def tally(items)")
    );
    let edits = diff::parse(&buf).unwrap();
    let recs = &edits[0].records;
    assert_eq!(recs.len(), 4);
    assert_eq!(recs[0].search(), b"prefix old_value suffix");
    assert_eq!(recs[0].replacement(), b"prefix new_value suffix");
    assert_eq!(recs[1].replacement(), b"import os\nimport sys");
    assert_eq!(recs[2].replacement(), b"");
    assert!(recs[3].is_assertion());
}

#[test]
fn diff_apply_sequential_records() {
    let buf = diff::build(|b| {
        b.file("tally.rb", |b| {
            b.assert("def tally(items)");
            b.replace("  ", "count", "total", "");
            b.assert("end");
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

    assert!(matches!(
        diff::parse(&parse("␖␁shape␟stream␜f.txt␞x␚␟y")),
        Err(DiffError::WrongShape(_))
    ));
    assert!(matches!(
        diff::parse(&parse("␖␁shape␟diff␞x␚␟y␜f.txt")),
        Err(DiffError::BeforeAnyFile)
    ));
    assert!(matches!(
        diff::parse(&parse("␖␁shape␟diff␜␞x␚␟y")),
        Err(DiffError::EmptyPath)
    ));
    assert!(matches!(
        diff::parse(&parse("␖␁shape␟diff␜a␞x␚␟y␜a␞p␚␟q")),
        Err(DiffError::DuplicatePath(_))
    ));
    // every slot the diff does not use is refused
    for bad in [
        "␜a␝s␞x␚␟y",
        "␜a␁␞h␞x␚␟y",
        "␜a␞␁k␟x␚␁k␟y",
        "␜a␞␐␞x␘␚␟y",
        "␜a␞␅x␚␟y",
        "␜a␞x␚␟y␚␟z",
        "␜a␞x␚␞y",
        "␜a␞x␚␟y␇␞p␚␟q",
        "␜a␞x␚␟y␇␜b␞p␚␟q",
        "␜a␞x␟␚␟␟y",
    ] {
        let diffbuf = parse(&format!("␖␁shape␟diff{bad}"));
        assert!(
            matches!(diff::parse(&diffbuf), Err(DiffError::Refused(_))),
            "{bad} must be refused"
        );
    }
    // a hunk kept in a note is not applied
    let mut files = HashMap::new();
    files.insert("a.txt".to_string(), "foo bar\n".to_string());
    let out = diff::apply(&parse("␖␁shape␟diff␜a.txt␞foo␚␟FOO␇␂␞bar␚␟BAR␃␄"), &files).unwrap();
    assert_eq!(out["a.txt"], "FOO bar\n");
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
