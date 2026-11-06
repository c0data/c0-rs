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
fn builder_body_roundtrip() {
    let buf = Builder::build(|b| {
        b.group("users", Some(&["name", "amount"]));
        b.record(&["Alice", "1502.30"]);
        b.record(&["Bob", "340.00"]);
    });
    let body = Body::new(&buf).unwrap();
    let g = body.group(0);
    assert_eq!(g.name(), b"users");
    assert_eq!(g.depth(), 1);
    assert_eq!(g.headers(), vec![b"name".as_ref(), b"amount".as_ref()]);
    assert_eq!(g.column("amount"), Some(1));
    assert_eq!(g.record_count(), 2);
    assert_eq!(g.record(0).value(0).unwrap().as_ref(), b"Alice");
    assert_eq!(g.record(1).value(1).unwrap().as_ref(), b"340.00");
    assert!(canonical(&buf));
    validate(&buf).unwrap();
}

#[test]
fn document_multi_group_and_files() {
    let buf = Builder::build(|b| {
        b.file("mydb");
        b.group("users", Some(&["name"]));
        b.record(&["Alice"]);
        b.group("products", Some(&["id"]));
        b.record(&["01"]);
        b.file("other");
        b.group("g", None);
        b.record(&["x"]);
    });
    let doc = Document::new(&buf).unwrap();
    assert_eq!(doc.name(), b"mydb");
    assert_eq!(doc.files().len(), 2);
    assert_eq!(doc.group_count(), 2);
    assert_eq!(
        doc.group_names(),
        vec![b"users".as_ref(), b"products".as_ref()]
    );
    let products = doc.group_by_name("products").unwrap();
    assert_eq!(products.record(0).value(0).unwrap().as_ref(), b"01");
    assert!(matches!(doc.group_by_name("missing"), Err(Error::Key(_))));
    assert_eq!(doc.files()[1].name(), b"other");
    assert_eq!(doc.groups().count(), 3);
}

#[test]
fn values_are_plain_or_text() {
    let buf = Builder::build(|b| {
        b.record(&[
            "a\u{1f}b",
            " padded ",
            "line\nbreak",
            "Alice Smith",
            "\u{02}\u{17}",
        ]);
    });
    // US, edge spaces and a line break force text; interior spaces stay plain.
    assert_eq!(&buf[..3], &[RS, STX, b'a']);
    let rec = Body::new(&buf).unwrap().record(0);
    assert_eq!(rec.field_count(), 5);
    assert_eq!(rec.value(0).unwrap().as_ref(), b"a\x1fb");
    assert_eq!(rec.value(1).unwrap().as_ref(), b" padded ");
    assert_eq!(rec.value(2).unwrap().as_ref(), b"line\nbreak");
    assert_eq!(rec.value(3).unwrap().as_ref(), b"Alice Smith");
    assert!(!is_text(rec.field(3)));
    assert_eq!(rec.value(4).unwrap().as_ref(), b"\x02\x17");
    assert!(is_text(rec.field(4)));
    assert!(canonical(&buf));
    // Layout touching a code is not data.
    let rec = Body::new(b"\x1e Alice \x1f Bob ").unwrap().record(0);
    assert_eq!(rec.values().unwrap(), [b"Alice".as_ref(), b"Bob"]);
}

#[test]
fn trailing_empty_field() {
    assert_eq!(
        Body::new(b"\x1eAlice\x1f").unwrap().record(0).field_count(),
        2
    );
    assert_eq!(Body::new(b"\x1eAlice").unwrap().record(0).field_count(), 1);
    assert_eq!(
        Body::new(b"\x1e").unwrap().record(0).values().unwrap(),
        [b"".as_ref()]
    );
}

#[test]
fn keyed_records() {
    let buf = Builder::build(|b| {
        b.group("database", None);
        b.map(&[
            ("host", "localhost"),
            ("port", "5432"),
            ("host", "db.local"),
        ]);
    });
    let body = Body::new(&buf).unwrap();
    let rec = body.group(0).record(0);
    assert!(rec.keyed());
    assert_eq!(rec.field_count(), 0);
    assert_eq!(rec.keys(), vec![b"host".as_ref(), b"port", b"host"]);
    assert_eq!(rec.get("port").unwrap().unwrap().as_ref(), b"5432");
    // duplicate key: last wins
    assert_eq!(rec.get("host").unwrap().unwrap().as_ref(), b"db.local");
    assert!(rec.get("missing").unwrap().is_none());
    assert!(!canonical(&buf)); // duplicate key
    assert!(validate(&buf).is_ok());
}

#[test]
fn nested_levels_and_lists() {
    let buf = Builder::build(|b| {
        b.group("users", Some(&["name", "roles", "address"]));
        b.begin_record();
        b.field("Alice");
        b.list(&["Admin", "Editor"]);
        b.level(|b| {
            b.header(&["street", "city"]);
            b.record(&["1 Main", "Springfield"]);
        });
        b.begin_record();
        b.key("name", "Bob");
        b.key_level("roles", |b| {
            b.record(&["User"]);
        });
    });
    assert!(canonical(&buf));
    let body = Body::new(&buf).unwrap();
    let alice = body.group(0).record(0);
    let roles = alice.list(1).unwrap();
    assert_eq!(roles, [b"Admin".as_ref(), b"Editor"]);
    let address = alice.level(2).unwrap();
    assert_eq!(address.group(0).headers(), [b"street".as_ref(), b"city"]);
    assert_eq!(address.record(0).value(1).unwrap().as_ref(), b"Springfield");
    assert!(alice.list(2).is_err()); // a table is not a list
    assert!(alice.value(1).is_err()); // a level is not a scalar
    let bob = body.group(0).record(1);
    let roles = bob.get_level("roles").unwrap().unwrap();
    assert_eq!(roles.record(0).values().unwrap(), [b"User".as_ref()]);
    // the empty level is the empty list
    let empty = Builder::build(|b| {
        b.list::<&str>(&[]);
    });
    assert_eq!(empty, [RS, DLE, CAN]);
    assert!(Body::new(&empty)
        .unwrap()
        .record(0)
        .list(0)
        .unwrap()
        .is_empty());
}

#[test]
fn references() {
    let buf = Builder::build(|b| {
        b.group("articles", Some(&["title", "tags", "primary"]));
        b.begin_record();
        b.field("My Post");
        b.reference("tags");
        b.reference_path(&["tags", "001", "label"]);
    });
    assert!(canonical(&buf));
    let rec = Body::new(&buf).unwrap().group(0).record(0);
    let r = rec.reference(1).unwrap();
    assert!(r.is_group());
    assert_eq!(r.group(), b"tags");
    let r = rec.reference(2).unwrap();
    assert_eq!(r.group(), b"tags");
    assert_eq!(r.record(), Some(b"001".as_ref()));
    assert_eq!(r.field(), Some(b"label".as_ref()));
    assert!(rec.value(1).is_err());
    assert!(Reference::parse(b"\x05\x02a\x1f\x1fb\x03").is_err());
    assert!(Reference::parse(b"\x05\x02a\x1fb\x1fc\x1fd\x03").is_err());
}

#[test]
fn documents_headings_and_warnings() {
    let buf = Builder::build(|b| {
        b.syn().heading("shape", "tabular").heading("version", "1");
        b.group("g", None);
        b.record(&["a"]);
        b.eot();
    });
    let doc = Document::new(&buf).unwrap();
    assert!(doc.has_syn() && doc.has_eot());
    assert_eq!(doc.shape(), Some("tabular"));
    assert_eq!(doc.heading().version(), 1);
    assert!(doc.warnings().is_empty());

    // A slice without SYN or EOT reads with loud warnings, never an error.
    let doc = Document::new(b"\x1dg\x1ea").unwrap();
    assert_eq!(doc.warnings().len(), 2);
    assert_eq!(doc.group(0).record(0).value(0).unwrap().as_ref(), b"a");

    // Unknown key: a warning.
    let doc = Document::new(b"\x16\x01author\x1fme\x1ea").unwrap();
    assert!(doc.warnings().iter().any(|w| w.contains("author")));
    assert_eq!(doc.heading().get("author"), Some(b"me".as_ref()));
}

#[test]
fn container_documents_and_blob() {
    let photo = b"\x89PNG....";
    let buf = Builder::build(|b| {
        b.syn();
        b.heading_level("blob", |b| {
            b.header(&["name", "offset", "length"]);
            b.record(&["lamp", "0", "4"]);
            b.record(&["desk", "4", "4"]);
        });
        b.eot();
        b.syn();
        b.group("items", Some(&["name", "photo"]));
        b.record(&["Lamp", "lamp"]);
        b.eot();
        b.em(photo);
    });
    let cont = Container::new(&buf).unwrap();
    assert_eq!(cont.documents().len(), 2);
    assert!(cont.has_em());
    assert_eq!(cont.tail(), photo);
    assert_eq!(cont.blob_bytes("lamp").unwrap(), Some(&photo[..4]));
    assert_eq!(cont.blob_bytes("desk").unwrap(), Some(&photo[4..]));
    assert_eq!(cont.blob_bytes("nope").unwrap(), None);
    assert_eq!(
        cont.documents()[1]
            .group(0)
            .record(0)
            .value(1)
            .unwrap()
            .as_ref(),
        b"lamp"
    );
    // Second document without SYN is an error.
    assert!(Container::new(b"\x16\x1ea\x04\x1eb").is_err());
    assert!(Container::new(b"").unwrap().first().is_none());
}

#[test]
fn comments_are_skipped() {
    let buf = Builder::build(|b| {
        b.group("g", None);
        b.note("staging only");
        b.record(&["a"]);
        b.note("has \x1e codes");
    });
    assert!(!canonical(&buf));
    let body = Body::new(&buf).unwrap();
    assert_eq!(body.group(0).record_count(), 1);
    // A dead record is parsed then dropped; a note in a unit leaves it empty.
    let body = Body::new(b"\x1dg\x1ea\x07\x1eb\x1ec\x1f\x07old\x1fd").unwrap();
    let g = body.group(0);
    assert_eq!(g.record_count(), 2);
    assert_eq!(g.record(1).values().unwrap(), [b"c".as_ref(), b"", b"d"]);
}

#[test]
fn validation_errors_name_a_position() {
    let e = validate(b"\x1ea\x0ab").unwrap_err();
    assert!(e.position().is_some(), "{e}");
    assert!(matches!(
        validate(b"\x1ea\x06b"),
        Err(Error::UnassignedCode { byte: 6, .. })
    ));
    assert!(matches!(
        validate(b"\x1e\x02abc"),
        Err(Error::UnexpectedEnd { .. })
    ));
    assert!(validate(b"\x1e\x10\x1ea\x18").is_ok());
}

#[test]
fn pretty_roundtrip() {
    let buf = Builder::build(|b| {
        b.syn().heading("shape", "tabular");
        b.file("db");
        b.group("users", Some(&["name", "amount"]));
        b.record(&["Alice Smith", "1502.30"]);
        b.begin_record();
        b.field("Bob");
        b.level(|b| {
            b.record(&["x", "y\ny"]);
        });
        b.eot();
    });
    let pretty = format(&buf).unwrap();
    assert!(pretty.contains("\u{2401}\u{241e}name\u{241f}amount")); // ␁␞name␟amount
    assert!(pretty.contains("\u{2410}\n")); // ␐ breaks the line
    assert_eq!(parse(&pretty), buf);
}

#[test]
fn pretty_renders_etb_and_refuses_glyphs() {
    let bytes = b"\x1ecreate\x1fa1b2\x17";
    let pretty = format(bytes).unwrap();
    assert!(pretty.contains("\u{241e}create\u{241f}a1b2\u{2417}"));
    assert_eq!(parse(&pretty), bytes);
    assert_eq!(format("\x1ea\u{241e}b".as_bytes()), Err(Error::GlyphInData));
}

#[test]
fn stream_reader_torn_tail_and_damage() {
    let bytes = b"\x1ecreate\x1fa1b2\x17\x1ename\x1fdra";
    let r = StreamReader::new(bytes);
    assert!(r.torn());
    assert_eq!(r.block_count(), 1);
    assert_eq!(r.tail(), b"\x1ename\x1fdra");
    assert_eq!(
        r.records().unwrap(),
        vec![vec![b"create".to_vec(), b"a1b2".to_vec()]]
    );
    // An ETB inside open text is still a commit; the block is damaged.
    let r = StreamReader::new(b"\x1e\x02a\x17\x1eb\x17");
    assert_eq!(r.damaged(), &[0]);
    assert_eq!(r.undamaged().as_ref(), b"\x1eb");
    assert_eq!(r.records().unwrap(), vec![vec![b"b".to_vec()]]);
    assert_eq!(last_commit_end(b"\x1ea\x17\x1eb\x17tag\x1ec"), 9);
}

#[test]
fn stream_writer_append_and_read() {
    let path = temp_path("append");
    {
        let mut log = open_log(&path).unwrap();
        log.start(&[("shape", "stream")]).unwrap();
        log.header(&["op", "arg"]).unwrap();
        log.record(&["create", "a1b2"]).unwrap();
        log.map(&[("op", "name"), ("arg", "draft")]).unwrap();
    }
    let bytes = read_log(&path).unwrap();
    let r = StreamReader::new(&bytes);
    assert!(!r.torn());
    assert_eq!(r.block_count(), 4);
    let committed = r.undamaged();
    let doc = Document::new(&committed).unwrap();
    assert_eq!(doc.shape(), Some("stream"));
    assert!(doc.warnings().is_empty());
    let g = doc.group(0);
    assert_eq!(g.headers(), [b"op".as_ref(), b"arg"]);
    assert_eq!(g.record_count(), 2);
    assert_eq!(g.record(1).get("arg").unwrap().unwrap().as_ref(), b"draft");
    fs::remove_file(&path).ok();
}

#[test]
fn stream_writer_repairs_torn_tail() {
    let path = temp_path("repair");
    {
        let mut log = open_log(&path).unwrap();
        log.record(&["create", "a1b2"]).unwrap();
    }
    // Tear inside an open text: a blind append would fuse with it.
    {
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[RS, STX, b'x']).unwrap();
    }
    {
        let mut log = open_log(&path).unwrap();
        log.record(&["tag", "alpha"]).unwrap();
    }
    let bytes = read_log(&path).unwrap();
    let r = StreamReader::new(&bytes);
    assert!(!r.torn());
    assert_eq!(r.records().unwrap().len(), 2);
    assert_eq!(r.records().unwrap()[1][0], b"tag");
    fs::remove_file(&path).ok();
}

#[test]
fn stream_writer_atomic_batch() {
    let mut w = StreamWriter::new(Vec::new());
    w.batch(|b| {
        b.record(&["name", "draft"]);
        b.record(&["tag", "alpha"]);
    })
    .unwrap();
    let bytes = w.into_inner();
    let r = StreamReader::new(&bytes);
    assert_eq!(r.block_count(), 1);
    assert_eq!(r.records().unwrap().len(), 2);
}

#[test]
fn names_reject_control_bytes_and_edge_space() {
    let r = std::panic::catch_unwind(|| {
        Builder::build(|b| {
            b.group("bad\u{1f}name", None);
        })
    });
    assert!(r.is_err(), "control byte in a name must panic");
    let r = std::panic::catch_unwind(|| {
        Builder::build(|b| {
            b.header(&["name "]);
        })
    });
    assert!(r.is_err(), "edge space in a name must panic");
    assert!(write_name(&mut Vec::new(), b"ok name").is_ok());
}

// ---- builder primitives --------------------------------------------------

#[test]
fn builder_refuses_an_empty_note() {
    let r = std::panic::catch_unwind(|| {
        Builder::build(|b| {
            b.record(&["a"]).note("");
        })
    });
    assert!(r.is_err(), "an empty note must panic");
    let r = std::panic::catch_unwind(|| {
        Builder::build(|b| {
            b.record(&["a"]).bel_note("");
        })
    });
    assert!(r.is_err(), "an empty bel_note must panic");
}

#[test]
fn builder_spells_primitives_as_the_bytes_read() {
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            Builder::build(|b| {
                b.fs("inv")
                    .gs_name("users")
                    .soh()
                    .rs_value("name")
                    .us_value("amount");
            }),
            "␜inv␝users␁␞name␟amount",
        ),
        (
            Builder::build(|b| {
                b.rs_value("Alice").us_value("1502.30");
            }),
            "␞Alice␟1502.30",
        ),
        (
            Builder::build(|b| {
                b.rs()
                    .soh_key("host")
                    .us_value("localhost")
                    .soh_key("port")
                    .us_value("5432");
            }),
            "␞␁host␟localhost␁port␟5432",
        ),
        (
            Builder::build(|b| {
                b.rs_value("cfg")
                    .us()
                    .dle()
                    .rs()
                    .soh_key("b")
                    .us_value("2")
                    .can();
            }),
            "␞cfg␟␐␞␁b␟2␘",
        ),
        (
            Builder::build(|b| {
                b.bel_note("checked").rs_value("x");
            }),
            "␇checked␞x",
        ),
        (
            Builder::build(|b| {
                b.bel()
                    .gs_name("old")
                    .rs_value("1")
                    .gs_name("new")
                    .rs_value("2");
            }),
            "␇␝old␞1␝new␞2",
        ),
        (
            Builder::build(|b| {
                b.gs()
                    .gs_name("sub")
                    .rs()
                    .enq("users")
                    .us()
                    .enq_path(&["users", "1"]);
            }),
            "␝␝sub␞␅users␟␅␂users␟1␃",
        ),
        (
            Builder::build(|b| {
                b.syn().soh_key("version").us_value("1").rs_value("a").eot();
            }),
            "␖␁version␟1␞a␄",
        ),
        (
            Builder::build(|b| {
                b.rs_value("a\nb")
                    .us_value(" x")
                    .bel_note("n\tn")
                    .etb_payload("p");
            }),
            "␞␂a\nb␃␟␂ x␃␇␂n\tn␃␗p",
        ),
    ];
    for (bytes, pretty) in cases {
        assert_eq!(bytes, parse(pretty), "{pretty}");
    }
    let em = Builder::build(|b| {
        b.rs_value("a").etb().em(b"raw");
    });
    assert_eq!(em, b"\x1ea\x17\x19raw");
}

#[test]
fn builder_mixes_primitives_with_composed_methods() {
    let buf = Builder::build(|b| {
        b.rs_value("a");
        b.field("b");
        b.begin_record();
        b.us_value("c");
        b.gs_name("g");
        b.key("k", "v");
        b.group("h", Some(&["h"]));
        b.field("x");
        b.rs();
        b.list(&["1", "2"]);
        b.level(|b| {
            b.rs_value("y");
            b.reference("g");
        });
        b.us();
        b.dle();
        b.record(&["z"]);
        b.can();
        b.field("w");
    });
    assert_eq!(buf, parse("␞a␟b␞␟c␝g␞␁k␟v␝h␁␞h␞x␞␐␞1␟2␘␟␐␞y␟␅g␘␟␐␞z␘␟w"));
    let r = std::panic::catch_unwind(|| {
        Builder::build(|b| {
            b.can();
        })
    });
    assert!(r.is_err(), "CAN with no open level must panic");
}

#[test]
fn builder_writes_the_same_bytes_through_either_layer() {
    let hi = Builder::build(|b| {
        b.syn().heading("shape", "tabular");
        b.group("users", Some(&["name", "amount"]));
        b.record(&["Alice", "1502.30"]);
        b.record(&["Bob", "3 40"]);
        b.group("cfg", None);
        b.map(&[("host", "localhost"), ("port", "5432")]);
        b.begin_record();
        b.key_level("origins", |b| {
            b.record(&["a", "b"]);
        });
        b.eot();
    });
    let lo = Builder::build(|b| {
        b.syn().soh_key("shape").us_value("tabular");
        b.gs_name("users").soh().rs_value("name").us_value("amount");
        b.rs_value("Alice")
            .us_value("1502.30")
            .rs_value("Bob")
            .us_value("3 40");
        b.gs_name("cfg")
            .rs()
            .soh_key("host")
            .us_value("localhost")
            .soh_key("port")
            .us_value("5432");
        b.rs()
            .soh_key("origins")
            .us()
            .dle()
            .rs_value("a")
            .us_value("b")
            .can()
            .eot();
    });
    assert_eq!(lo, hi);
    let doc = Document::new(&lo).unwrap();
    assert!(canonical(doc.body().raw()));
    assert!(doc.warnings().is_empty());
}
