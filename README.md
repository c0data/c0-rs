# c0

A Rust implementation of [C0DATA](https://github.com/c0data) — structured
data built on ASCII C0 control codes. Values are UTF-8 text (any bytes, as
text); structure is sixteen single-byte codes: FS/GS/RS/US separators, SOH
keys and header rows, STX…ETX text, DLE…CAN nesting, ENQ references, BEL
comments, SYN/EOT document framing, ETB stream commits, EM trailing blobs.

This crate is a **zero-copy** reader/writer: readers index a buffer once and
hand back slices (`&[u8]`) into it, decoding text only on demand
(`Cow<[u8]>`). It has **no dependencies**.

## Status

Implements the 1.0 redesign of the spec, mirroring the Crystal reference
(`c0-cr`):

- scanning primitives, validator (`validate`, `canonical`)
- `Container` / `Document` / `Body` / `Group` / `Record` / `Reference` readers,
  `Builder` writer
- stream mode: `StreamReader` (commits, damaged blocks, torn tail),
  `StreamWriter`, `FileLog` with tail repair and fsync
- pretty form: compact, aligned and spaced layouts, and the round-tripping
  `parse`
- CSV ⇄ C0DATA conversion
- C0-DIFF: parse, build, and atomic multi-file apply (sections and
  replace-all)
- JSON: the `Value` tree and `to_value`/`from_value` (dependency-free), with
  `to_json`/`from_json` text helpers behind the optional `json` feature

It passes every case of the shared conformance vectors from
[c0-spec](https://github.com/c0data/c0-spec), read from the git submodule at
`c0-spec/`. After cloning:

```sh
git submodule update --init
```

Not ported: a YAML converter and a serde-style derive (the `Serializable`
equivalent).

## Features

The library is dependency-free by default. The optional `json` feature adds
JSON *text* conversion and pulls in `serde_json`:

```toml
c0 = { version = "1.0.0-dev", features = ["json"] }
```

## Usage

```rust
use c0::{Builder, Body, canonical};

// Build canonical compact bytes: ␝users␁␞name␟amount␞Alice␟1502.30␞Bob␟340.00
let buf = Builder::build(|b| {
    b.group("users", Some(&["name", "amount"]));
    b.record(&["Alice", "1502.30"]);
    b.record(&["Bob", "340.00"]);
});

// Read them back, zero-copy
let body = Body::new(&buf)?;
let users = body.group(0);
assert_eq!(users.headers(), [b"name".as_slice(), b"amount"]);
assert_eq!(users.record(0).value(0)?.as_ref(), b"Alice");

// The builder's output is canonical — hashable for content addressing
assert!(canonical(&buf));
```

Values (`record`, `field`, `list`, `key`, `map`) take any `AsRef<[u8]>` and are
written in their one spelling: plain when they have no byte below 0x20 and no
edge whitespace, otherwise text `␂…␃` with only STX, ETX, DLE, ETB, EOT, SYN
and EM escaped. Names (labels, keys, header names) are identifiers and panic
when they are not.

### Keyed records and nesting

```rust
let buf = c0::Builder::build(|b| {
    b.group("server", None);
    b.begin_record();
    b.key("host", "0.0.0.0");                       // ␞␁host␟0.0.0.0
    b.key("port", "8080");                          // ␁port␟8080
    b.key_level("origins", |b| {                    // ␁origins␟␐␞localhost␟example.com␘
        b.record(&["localhost", "example.com"]);
    });
});
let body = c0::Body::new(&buf)?;
let server = body.group(0).record(0);
assert_eq!(server.get("port")?.unwrap().as_ref(), b"8080");
let origins = server.get_level("origins")?.unwrap();
assert_eq!(origins.record(0).values()?.len(), 2);
```

A positional unit holding a level is read with `Record::level` (any body) or
`Record::list` (a level holding one positional record, written with
`Builder::list`). A reference (`␅tags`, `␅␂tags␟001␃`) is written with
`reference`/`reference_path` and read with `Record::reference`.

### Documents

```rust
let buf = c0::Builder::build(|b| {
    b.syn().heading("shape", "tabular");            // ␖␁shape␟tabular
    b.file("mydb");                                 // ␜mydb
    b.group("users", Some(&["name"]));              // ␝users␁␞name
    b.record(&["Alice"]);
    b.eot();                                        // ␄
});
let doc = c0::Document::new(&buf)?;
assert_eq!(doc.shape(), Some("tabular"));
assert!(doc.warnings().is_empty());                 // a missing SYN or EOT warns, never fails
```

`Container::new` reads a buffer of several documents and the raw region after
EM (`blob_bytes(name)` looks a blob up through the first heading's `blob`
table). `validate` rejects malformed bytes; `canonical` tells whether bytes
are a canonical unit.

### Stream logs (ETB commits)

```rust
use c0::{open_log, read_log, StreamReader, Document};

let mut log = open_log("events.c0")?;   // truncates a torn tail first
log.start(&[("shape", "stream")])?;     // ␖␁shape␟stream␗
log.record(&["create", "a1b2", "1718208000"])?;
log.batch(|b| {                          // one commit for several records
    b.record(&["name", "draft"]);
    b.record(&["tag", "alpha"]);
})?;

let bytes = read_log("events.c0")?;
let r = StreamReader::new(&bytes);
assert!(!r.torn());                       // an uncommitted tail is skipped
let committed = r.undamaged();            // blocks cut by a stray ETB are dropped
let doc = Document::new(&committed)?;
```

### Pretty form

`format`/`format_mode` render compact bytes with Control Picture glyphs,
line breaks and indentation (`FormatMode::{Compact, Aligned, Spaced}`);
`parse` reads them back. A value containing a glyph character is refused.

## Development

```sh
cargo test                                # unit, integration, and conformance vectors
cargo test --all-features
cargo clippy --all-targets --all-features
cargo fmt
```

## License

MIT
