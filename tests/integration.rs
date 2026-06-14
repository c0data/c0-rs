use c0::*;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("c0rs-test-{}-{}", std::process::id(), name));
    let _ = fs::remove_file(&p);
    p
}

#[test]
fn builder_table_roundtrip() {
    let buf = Builder::build(|b| {
        b.group("users", Some(&["name", "amount"]));
        b.record(&["Alice", "1502.30"]);
        b.record(&["Bob", "340.00"]);
    });
    let t = Table::new(&buf);
    assert_eq!(t.name(), b"users");
    assert_eq!(t.headers(), vec![b"name".as_ref(), b"amount".as_ref()]);
    assert_eq!(t.record_count(), 2);
    assert_eq!(t.record(0).field(0), b"Alice");
    assert_eq!(t.record(1).field(1), b"340.00");
    assert!(canonical(&buf));
}

#[test]
fn document_multi_group() {
    let buf = Builder::build(|b| {
        b.file("mydb");
        b.group("users", Some(&["name"]));
        b.record(&["Alice"]);
        b.group("products", Some(&["id"]));
        b.record(&["01"]);
    });
    let doc = Document::new(&buf);
    assert_eq!(doc.name(), b"mydb");
    assert_eq!(doc.group_count(), 2);
    assert_eq!(
        doc.group_by_name("products").unwrap().record(0).field(0),
        b"01"
    );
    assert!(doc.group_by_name("missing").is_none());
}

#[test]
fn escaping_and_unescape() {
    // A value containing a US byte must be escaped, then decode back.
    let buf = Builder::build(|b| {
        b.group("g", None);
        b.record(&["a\u{1f}b", "c"]);
    });
    let t = Table::new(&buf);
    let rec = t.record(0);
    assert_eq!(rec.field_count(), 2);
    assert_eq!(rec.value(0).as_ref(), b"a\x1fb");
    assert_eq!(rec.value(1).as_ref(), b"c");
}

#[test]
fn trailing_empty_field() {
    // N separators => N+1 fields.
    let with = b"\x1eAlice\x1f"; // RS Alice US
    let without = b"\x1eAlice"; // RS Alice
    assert_eq!(Table::new(with).record(0).field_count(), 2);
    assert_eq!(Table::new(without).record(0).field_count(), 1);
}

#[test]
fn pretty_roundtrip() {
    let buf = Builder::build(|b| {
        b.group("users", Some(&["name", "amount"]));
        b.record(&["Alice", "1502.30"]);
    });
    let pretty = format(&buf);
    assert!(pretty.contains('\u{241e}')); // ␞ record glyph
    assert_eq!(parse(&pretty), buf);
}

#[test]
fn pretty_renders_etb() {
    let bytes = b"\x1ecreate\x1fa1b2\x17"; // RS create US a1b2 ETB
    let pretty = format(bytes);
    assert!(pretty.contains("\u{241e}create\u{241f}a1b2\u{2417}"));
    assert_eq!(parse(&pretty), bytes);
}

#[test]
fn stream_reader_torn_tail() {
    let bytes = b"\x1ecreate\x1fa1b2\x17\x1ename\x1fdra"; // committed + torn
    let r = StreamReader::new(bytes);
    assert!(r.torn());
    assert_eq!(r.block_count(), 1);
    assert_eq!(r.tail(), b"\x1ename\x1fdra");
    assert_eq!(r.table().record_count(), 1);
}

#[test]
fn stream_writer_append_and_read() {
    let path = temp_path("append");
    {
        let mut log = open_log(&path).unwrap();
        log.header(&["op", "arg"]).unwrap();
        log.record(&["create", "a1b2"]).unwrap();
        log.record(&["name", "draft"]).unwrap();
    }
    let bytes = read_log(&path).unwrap();
    let r = StreamReader::new(&bytes);
    assert!(!r.torn());
    assert_eq!(r.block_count(), 3);
    let t = r.table();
    assert_eq!(t.headers(), vec![b"op".as_ref(), b"arg".as_ref()]);
    assert_eq!(t.record_count(), 2);
    fs::remove_file(&path).ok();
}

#[test]
fn stream_writer_repairs_bare_dle_tail() {
    let path = temp_path("repair");
    {
        let mut log = open_log(&path).unwrap();
        log.record(&["create", "a1b2"]).unwrap();
    }
    // Tear between DLE and its escaped byte — a blind append's RS would be
    // swallowed by this trailing DLE.
    {
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[RS, 0x78, DLE]).unwrap();
    }
    {
        let mut log = open_log(&path).unwrap();
        log.record(&["tag", "alpha"]).unwrap();
    }
    let bytes = read_log(&path).unwrap();
    let r = StreamReader::new(&bytes);
    assert!(!r.torn());
    assert_eq!(r.table().record_count(), 2);
    assert_eq!(r.table().record(1).field(0), b"tag");
    fs::remove_file(&path).ok();
}

#[test]
fn stream_writer_atomic_batch() {
    let path = temp_path("batch");
    {
        let mut log = open_log(&path).unwrap();
        log.batch(|b| {
            b.record(&["name", "draft"]);
            b.record(&["tag", "alpha"]);
        })
        .unwrap();
    }
    let bytes = read_log(&path).unwrap();
    let r = StreamReader::new(&bytes);
    assert_eq!(r.block_count(), 1);
    assert_eq!(r.table().record_count(), 2);
    fs::remove_file(&path).ok();
}

#[test]
fn names_reject_control_bytes() {
    let r = std::panic::catch_unwind(|| {
        Builder::build(|b| {
            b.group("bad\u{1f}name", None);
        })
    });
    assert!(r.is_err(), "control byte in a name must panic");
}
