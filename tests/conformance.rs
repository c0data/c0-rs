//! Runs the language-agnostic conformance vectors in `conformance/`
//! (vendored from c0-cr, the source of truth). Other implementations
//! (c0-js, …) consume the same JSON files.

use c0::*;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn cases(file: &str) -> Vec<Value> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("c0-spec");
    p.push("vectors");
    p.push(file);
    let text = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"));
    let v: Value = serde_json::from_str(&text).unwrap();
    v["cases"].as_array().unwrap().clone()
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn to_hex(buf: &[u8]) -> String {
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

// A field is a JSON string (UTF-8 bytes) or {"hex": "..."} (raw bytes).
fn field_bytes(f: &Value) -> Vec<u8> {
    match f.as_str() {
        Some(s) => s.as_bytes().to_vec(),
        None => hex_bytes(f["hex"].as_str().unwrap()),
    }
}

fn check_table(t: &Table, g: &Value, case: &str) {
    assert_eq!(
        t.name(),
        g["name"].as_str().unwrap().as_bytes(),
        "{case}: name"
    );
    match g["headers"].as_array() {
        Some(hs) => {
            let want: Vec<&[u8]> = hs.iter().map(|h| h.as_str().unwrap().as_bytes()).collect();
            assert_eq!(t.headers(), want, "{case}: headers");
        }
        None => assert_eq!(t.header_count(), 0, "{case}: header_count"),
    }
    let recs = g["records"].as_array().unwrap();
    assert_eq!(t.record_count(), recs.len(), "{case}: record_count");
    for (i, r) in recs.iter().enumerate() {
        let rec = t.record(i);
        let expected = r.as_array().unwrap();
        assert_eq!(rec.field_count(), expected.len(), "{case}: rec {i} arity");
        for (j, f) in expected.iter().enumerate() {
            assert_eq!(
                rec.value(j).as_ref(),
                field_bytes(f).as_slice(),
                "{case}: rec {i} field {j}"
            );
        }
    }
}

#[test]
fn decode() {
    for c in cases("decode.json") {
        let case = c["name"].as_str().unwrap();
        let bytes = hex_bytes(c["bytes"].as_str().unwrap());
        let file = c["file"].as_str();
        let groups = c["groups"].as_array().unwrap();

        if file.is_none() && groups.len() == 1 && groups[0]["name"].as_str() == Some("") {
            check_table(&Table::new(&bytes), &groups[0], case);
        } else {
            let doc = Document::new(&bytes);
            assert_eq!(
                doc.name(),
                file.unwrap_or("").as_bytes(),
                "{case}: doc name"
            );
            assert_eq!(doc.group_count(), groups.len(), "{case}: group count");
            for (i, g) in groups.iter().enumerate() {
                check_table(&doc.group(i).table(), g, case);
            }
        }
    }
}

#[test]
fn encode() {
    for c in cases("encode.json") {
        let case = c["name"].as_str().unwrap();
        let spec = &c["build"];
        let groups = spec["groups"].as_array().unwrap();

        let mut b = Builder::new();
        if let Some(file) = spec["file"].as_str() {
            b.file(file);
        }
        for g in groups {
            let headers: Option<Vec<String>> = g["headers"]
                .as_array()
                .map(|a| a.iter().map(|h| h.as_str().unwrap().to_string()).collect());
            let hdr_refs: Option<Vec<&str>> = headers
                .as_ref()
                .map(|v| v.iter().map(|s| s.as_str()).collect());
            b.group(g["name"].as_str().unwrap(), hdr_refs.as_deref());
            for r in g["records"].as_array().unwrap() {
                let vals: Vec<String> = r
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| String::from_utf8(field_bytes(f)).unwrap())
                    .collect();
                let refs: Vec<&str> = vals.iter().map(|s| s.as_str()).collect();
                b.record(&refs);
            }
        }
        let buf = b.into_bytes();
        assert_eq!(to_hex(&buf), c["canonical"].as_str().unwrap(), "{case}");
        assert!(canonical(&buf), "{case}: builder output must be canonical");
    }
}

#[test]
fn canonical_classification() {
    for c in cases("canonical.json") {
        let case = c["name"].as_str().unwrap();
        let bytes = hex_bytes(c["bytes"].as_str().unwrap());
        assert_eq!(
            tokenize(&bytes).is_ok(),
            c["wellformed"].as_bool().unwrap(),
            "{case}: wellformed"
        );
        assert_eq!(
            canonical(&bytes),
            c["canonical"].as_bool().unwrap(),
            "{case}: canonical"
        );
    }
}

#[test]
fn invalid() {
    for c in cases("invalid.json") {
        let case = c["name"].as_str().unwrap();
        let bytes = hex_bytes(c["bytes"].as_str().unwrap());
        assert!(tokenize(&bytes).is_err(), "{case}: must be rejected");
    }
}

#[test]
fn stream() {
    for c in cases("stream.json") {
        let case = c["name"].as_str().unwrap();
        let bytes = hex_bytes(c["bytes"].as_str().unwrap());
        let r = StreamReader::new(&bytes);

        assert_eq!(
            r.committed_end(),
            c["committed_end"].as_u64().unwrap() as usize,
            "{case}: committed_end"
        );
        assert_eq!(r.torn(), c["torn"].as_bool().unwrap(), "{case}: torn");

        let blocks = c["blocks"].as_array().unwrap();
        assert_eq!(r.block_count(), blocks.len(), "{case}: block count");
        for (i, hexv) in blocks.iter().enumerate() {
            assert_eq!(
                to_hex(r.block(i)),
                hexv.as_str().unwrap(),
                "{case}: block {i}"
            );
        }

        if let Some(recs) = c.get("records") {
            let t = r.table();
            let expected = recs.as_array().unwrap();
            assert_eq!(t.record_count(), expected.len(), "{case}: record count");
            for (i, rr) in expected.iter().enumerate() {
                let got: Vec<String> = t
                    .record(i)
                    .values()
                    .iter()
                    .map(|v| String::from_utf8(v.to_vec()).unwrap())
                    .collect();
                let want: Vec<&str> = rr
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_str().unwrap())
                    .collect();
                assert_eq!(got, want, "{case}: rec {i}");
            }
        }
    }
}
