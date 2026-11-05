//! Runs the language-agnostic conformance vectors from the `c0-spec`
//! submodule (schema v2, see `c0-spec/vectors/README.md`). Mirrors the
//! Crystal harness in `c0-cr/spec/conformance_spec.cr`.

use c0::*;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn cases(file: &str) -> Vec<Value> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("c0-spec");
    p.push("vectors");
    p.push(file);
    let text = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"));
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["version"].as_u64(), Some(2), "{file}: vector schema");
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

// A scalar expected value: a JSON string or {"hex": "..."}.
fn scalar_bytes(f: &Value) -> Vec<u8> {
    match f.as_str() {
        Some(s) => s.as_bytes().to_vec(),
        None => hex_bytes(f["hex"].as_str().unwrap()),
    }
}

fn is_scalar(f: &Value) -> bool {
    f.is_string() || f.get("hex").is_some()
}

fn strs(v: &Value) -> Vec<&str> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect()
}

// Flatten a group list with "sections" into (depth, group) pairs.
fn flat_groups(groups: &[Value], depth: usize) -> Vec<(usize, &Value)> {
    let mut out = Vec::new();
    for g in groups {
        out.push((depth, g));
        if let Some(secs) = g.get("sections").and_then(Value::as_array) {
            out.extend(flat_groups(secs, depth + 1));
        }
    }
    out
}

// ---------------------------------------------------------------- checking

fn check_value(rec: &Record, i: usize, f: &Value, case: &str) {
    if is_scalar(f) {
        assert_eq!(
            rec.value(i).unwrap().as_ref(),
            scalar_bytes(f).as_slice(),
            "{case}: field {i}"
        );
    } else if let Some(lvl) = f.get("level") {
        check_body(&rec.level(i).unwrap(), lvl, case);
    } else if let Some(r) = f.get("ref") {
        let got = rec.reference(i).unwrap();
        let want: Vec<Vec<u8>> = match r.as_str() {
            Some(s) => vec![s.as_bytes().to_vec()],
            None => strs(r).iter().map(|s| s.as_bytes().to_vec()).collect(),
        };
        let path: Vec<Vec<u8>> = got.path().iter().map(|s| s.to_vec()).collect();
        assert_eq!(path, want, "{case}: field {i} ref");
    } else {
        panic!("{case}: unknown value form {f}");
    }
}

fn check_record(rec: &Record, r: &Value, case: &str) {
    if let Some(arr) = r.as_array() {
        assert!(!rec.keyed(), "{case}: record should be positional");
        assert_eq!(rec.field_count(), arr.len(), "{case}: arity");
        for (j, f) in arr.iter().enumerate() {
            check_value(rec, j, f, case);
        }
    } else {
        assert!(rec.keyed(), "{case}: record should be keyed");
        let pairs = r["keys"].as_array().unwrap();
        let keys: Vec<&[u8]> = pairs
            .iter()
            .map(|p| p[0].as_str().unwrap().as_bytes())
            .collect();
        assert_eq!(rec.keys(), keys, "{case}: keys");
        let got = rec.pairs().unwrap();
        for (j, p) in pairs.iter().enumerate() {
            assert_eq!(
                got[j].1.as_ref(),
                scalar_bytes(&p[1]).as_slice(),
                "{case}: key {j} value"
            );
        }
    }
}

fn check_group(g: &Group, e: &Value, case: &str) {
    assert_eq!(
        g.name(),
        e["name"].as_str().unwrap().as_bytes(),
        "{case}: group name"
    );
    match e["headers"].as_array() {
        Some(_) => {
            let want: Vec<&[u8]> = strs(&e["headers"]).iter().map(|s| s.as_bytes()).collect();
            assert_eq!(g.headers(), want, "{case}: headers");
        }
        None => assert!(!g.has_header(), "{case}: no header expected"),
    }
    let records = e["records"].as_array().unwrap();
    assert_eq!(g.record_count(), records.len(), "{case}: record count");
    for (i, r) in records.iter().enumerate() {
        check_record(&g.record(i), r, case);
    }
}

fn check_groups(groups: &[&Group], expected: &[Value], case: &str) {
    let flat = flat_groups(expected, 1);
    assert_eq!(groups.len(), flat.len(), "{case}: group count");
    for (i, (depth, e)) in flat.iter().enumerate() {
        check_group(groups[i], e, case);
        if !(e["name"].as_str() == Some("") && i == 0) {
            assert_eq!(groups[i].depth(), *depth, "{case}: depth of group {i}");
        }
    }
}

fn check_body(body: &Body, e: &Value, case: &str) {
    if let Some(files) = e.get("files") {
        check_files(body, files.as_array().unwrap(), case);
    } else if let Some(records) = e.get("records") {
        // a level described as a single table
        assert_eq!(body.files().len(), 1, "{case}: level files");
        let gs = body.files()[0].groups();
        let no_headers = e.get("headers").and_then(Value::as_array).is_none();
        if records.as_array().unwrap().is_empty() && no_headers {
            assert_eq!(gs.len(), 0, "{case}: empty level");
        } else {
            assert_eq!(gs.len(), 1, "{case}: level groups");
            let as_group = serde_json::json!({
                "name": "", "headers": e["headers"], "records": records
            });
            check_group(&gs[0], &as_group, case);
        }
    } else {
        let groups: Vec<&Group> = body.groups().collect();
        check_groups(&groups, e["groups"].as_array().unwrap(), case);
    }
}

fn check_files(body: &Body, files: &[Value], case: &str) {
    assert_eq!(body.files().len(), files.len(), "{case}: file count");
    for (i, f) in files.iter().enumerate() {
        let fv = &body.files()[i];
        assert_eq!(
            fv.name(),
            f["name"].as_str().unwrap().as_bytes(),
            "{case}: file name"
        );
        let groups: Vec<&Group> = fv.groups().iter().collect();
        check_groups(&groups, f["groups"].as_array().unwrap(), case);
    }
}

fn check_heading(h: &Heading, e: &Value, case: &str) {
    let pairs = e.as_array().unwrap();
    let keys: Vec<&[u8]> = pairs
        .iter()
        .map(|p| p[0].as_str().unwrap().as_bytes())
        .collect();
    assert_eq!(h.keys(), keys, "{case}: heading keys");
    for (i, p) in pairs.iter().enumerate() {
        let v = &p[1];
        if is_scalar(v) {
            assert_eq!(
                h.pairs()[i].1.as_ref(),
                scalar_bytes(v).as_slice(),
                "{case}: heading value {i}"
            );
        } else if let Some(lvl) = v.get("level") {
            let body = Body::of_level(&h.pairs()[i].1).unwrap();
            check_body(&body, lvl, case);
        }
    }
}

// ---------------------------------------------------------------- building

fn build_value(b: &mut Builder, f: &Value) {
    if is_scalar(f) {
        b.field(scalar_bytes(f));
    } else if let Some(lvl) = f.get("level") {
        b.level(|b| build_body(b, lvl));
    } else if let Some(r) = f.get("ref") {
        match r.as_str() {
            Some(s) => b.reference(s),
            None => b.reference_path(&strs(r)),
        };
    } else if let Some(m) = f.get("map") {
        b.level(|b| build_record(b, &serde_json::json!({ "keys": m })));
    } else {
        panic!("unknown value form {f}");
    }
}

fn build_record(b: &mut Builder, r: &Value) {
    b.begin_record();
    if let Some(arr) = r.as_array() {
        for f in arr {
            build_value(b, f);
        }
    } else {
        for p in r["keys"].as_array().unwrap() {
            let k = p[0].as_str().unwrap();
            let v = &p[1];
            if is_scalar(v) {
                b.key(k, scalar_bytes(v));
            } else if let Some(m) = v.get("map") {
                b.key_level(k, |b| build_record(b, &serde_json::json!({ "keys": m })));
            } else if let Some(lvl) = v.get("level") {
                b.key_level(k, |b| build_body(b, lvl));
            }
        }
    }
}

fn build_groups(b: &mut Builder, groups: &[Value]) {
    for (i, g) in groups.iter().enumerate() {
        let headers: Option<Vec<&str>> = g["headers"].as_array().map(|_| strs(&g["headers"]));
        if g["name"].as_str() == Some("") && i == 0 {
            if let Some(h) = &headers {
                b.header(h); // the unnamed leading group: no GS
            }
        } else {
            b.group(g["name"].as_str().unwrap(), headers.as_deref());
        }
        for r in g["records"].as_array().unwrap() {
            build_record(b, r);
        }
    }
}

fn build_body(b: &mut Builder, e: &Value) {
    if let Some(records) = e.get("records") {
        if e["headers"].is_array() {
            b.header(&strs(&e["headers"]));
        }
        for r in records.as_array().unwrap() {
            build_record(b, r);
        }
    } else if let Some(groups) = e.get("groups") {
        build_groups(b, groups.as_array().unwrap());
    }
}

// ================================================================== tests

#[test]
fn decode() {
    for c in cases("decode.json") {
        let case = c["name"].as_str().unwrap();
        let bytes = hex_bytes(c["bytes"].as_str().unwrap());
        let cont = Container::new(&bytes).unwrap_or_else(|e| panic!("{case}: {e}"));
        if let Some(docs) = c.get("documents") {
            let docs = docs.as_array().unwrap();
            assert_eq!(cont.documents().len(), docs.len(), "{case}: document count");
            for (i, d) in docs.iter().enumerate() {
                let doc = &cont.documents()[i];
                if let Some(h) = d.get("heading") {
                    check_heading(doc.heading(), h, case);
                }
                assert_eq!(
                    doc.name(),
                    d["file"].as_str().unwrap_or("").as_bytes(),
                    "{case}: file"
                );
                let groups: Vec<&Group> = doc.groups().collect();
                check_groups(&groups, d["groups"].as_array().unwrap(), case);
            }
        } else {
            let doc = cont.first().unwrap();
            if let Some(h) = c.get("heading") {
                assert!(doc.has_syn(), "{case}: SYN expected");
                check_heading(doc.heading(), h, case);
            }
            if let Some(files) = c.get("files") {
                check_files(doc.body(), files.as_array().unwrap(), case);
            } else {
                assert_eq!(
                    doc.name(),
                    c["file"].as_str().unwrap_or("").as_bytes(),
                    "{case}: file"
                );
                let groups: Vec<&Group> = doc.groups().collect();
                check_groups(&groups, c["groups"].as_array().unwrap(), case);
            }
            if let Some(tail) = c.get("tail") {
                assert!(cont.has_em(), "{case}: EM expected");
                assert_eq!(to_hex(cont.tail()), tail.as_str().unwrap(), "{case}: tail");
            }
        }
    }
}

#[test]
fn encode() {
    for c in cases("encode.json") {
        let case = c["name"].as_str().unwrap();
        let build = &c["build"];
        let emit = |b: &mut Builder| {
            if let Some(file) = build["file"].as_str() {
                b.file(file);
            }
            build_groups(b, build["groups"].as_array().unwrap());
        };
        let canon = Builder::build(emit);
        assert_eq!(
            to_hex(&canon),
            c["canonical"].as_str().unwrap(),
            "{case}: canonical"
        );
        assert!(
            canonical(&canon),
            "{case}: builder output must be canonical"
        );
        if let Some(h) = build.get("heading") {
            let whole = Builder::build(|b| {
                b.syn();
                for p in h.as_array().unwrap() {
                    b.heading(p[0].as_str().unwrap(), p[1].as_str().unwrap());
                }
                emit(b);
                b.eot();
            });
            assert_eq!(
                to_hex(&whole),
                c["bytes"].as_str().unwrap(),
                "{case}: bytes"
            );
        }
    }
}

#[test]
fn canonical_classification() {
    for c in cases("canonical.json") {
        let case = c["name"].as_str().unwrap();
        let bytes = hex_bytes(c["bytes"].as_str().unwrap());
        assert_eq!(
            validate(&bytes).is_ok(),
            c["wellformed"].as_bool().unwrap(),
            "{case}: wellformed ({:?})",
            validate(&bytes)
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
        assert!(validate(&bytes).is_err(), "{case}: must be rejected");
    }
}

#[test]
fn nested() {
    for c in cases("nested.json") {
        let case = c["name"].as_str().unwrap();
        let bytes = hex_bytes(c["bytes"].as_str().unwrap());
        let expected = c["record"].as_array().unwrap();
        let body = Body::new(&bytes).unwrap_or_else(|e| panic!("{case}: {e}"));
        let rec = body.record(0);
        assert_eq!(rec.field_count(), expected.len(), "{case}: arity");
        let all_scalar = |items: &Value| items.as_array().unwrap().iter().all(is_scalar);
        let list_bytes = |items: &Value| -> Vec<Vec<u8>> {
            items.as_array().unwrap().iter().map(scalar_bytes).collect()
        };
        for (i, f) in expected.iter().enumerate() {
            if is_scalar(f) {
                assert_eq!(
                    rec.value(i).unwrap().as_ref(),
                    scalar_bytes(f).as_slice(),
                    "{case}: field {i}"
                );
            } else if let Some(items) = f.get("list") {
                if all_scalar(items) {
                    let got: Vec<Vec<u8>> =
                        rec.list(i).unwrap().iter().map(|v| v.to_vec()).collect();
                    assert_eq!(got, list_bytes(items), "{case}: list {i}");
                } else {
                    let level = rec.level(i).unwrap();
                    let inner = level.record(0);
                    let items = items.as_array().unwrap();
                    assert_eq!(inner.field_count(), items.len(), "{case}: inner arity");
                    for (j, x) in items.iter().enumerate() {
                        if is_scalar(x) {
                            assert_eq!(
                                inner.value(j).unwrap().as_ref(),
                                scalar_bytes(x).as_slice()
                            );
                        } else {
                            let got: Vec<Vec<u8>> =
                                inner.list(j).unwrap().iter().map(|v| v.to_vec()).collect();
                            assert_eq!(got, list_bytes(&x["list"]), "{case}: inner list {j}");
                        }
                    }
                }
            } else if let Some(lvl) = f.get("level") {
                check_body(&rec.level(i).unwrap(), lvl, case);
                assert!(
                    rec.list(i).is_err(),
                    "{case}: list accessor must reject field {i}"
                );
            }
        }
        if c["canonical"].as_bool().unwrap() {
            let buf = Builder::build(|b| {
                b.begin_record();
                for f in expected {
                    match f.get("list") {
                        Some(items) if all_scalar(items) => {
                            b.list(&list_bytes(items));
                        }
                        Some(items) => {
                            b.level(|b| {
                                b.begin_record();
                                for x in items.as_array().unwrap() {
                                    if is_scalar(x) {
                                        b.field(scalar_bytes(x));
                                    } else {
                                        b.list(&list_bytes(&x["list"]));
                                    }
                                }
                            });
                        }
                        None => build_value(b, f),
                    }
                }
            });
            assert_eq!(to_hex(&buf), c["bytes"].as_str().unwrap(), "{case}: build");
            assert!(canonical(&buf), "{case}: builder output must be canonical");
        }
    }
}

#[test]
fn diff_vectors() {
    for c in cases("diff.json") {
        let case = c["name"].as_str().unwrap();
        let bytes = hex_bytes(c["diff"].as_str().unwrap());
        let files: HashMap<String, String> = c["files"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();
        let got = diff::apply(&bytes, &files);
        if c.get("error").is_some() {
            assert!(got.is_err(), "{case}: must refuse");
        } else {
            let got = got.unwrap_or_else(|e| panic!("{case}: {e}"));
            let want: HashMap<String, String> = c["result"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                .collect();
            assert_eq!(got, want, "{case}: result");
        }
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
        let blocks = strs(&c["blocks"]);
        assert_eq!(r.block_count(), blocks.len(), "{case}: block count");
        for (i, hexv) in blocks.iter().enumerate() {
            assert_eq!(to_hex(r.block(i)), *hexv, "{case}: block {i}");
        }
        let damaged: Vec<usize> = c
            .get("damaged")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|x| x.as_u64().unwrap() as usize).collect())
            .unwrap_or_default();
        assert_eq!(r.damaged(), damaged.as_slice(), "{case}: damaged");
        if let Some(recs) = c.get("records") {
            let got = r.records().unwrap_or_else(|e| panic!("{case}: {e}"));
            let want: Vec<Vec<Vec<u8>>> = recs
                .as_array()
                .unwrap()
                .iter()
                .map(|rr| strs(rr).iter().map(|s| s.as_bytes().to_vec()).collect())
                .collect();
            assert_eq!(got, want, "{case}: records");
        }
    }
}
