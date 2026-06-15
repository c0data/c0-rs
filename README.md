# c0

A Rust implementation of [C0DATA](https://github.com/c0data) — structured
data built on ASCII C0 control codes. Values are plain UTF-8 text; structure is
expressed through single-byte control codes (FS/GS/RS/US separators, SOH
headers, STX/ETX nesting, DLE escape, ETB stream commits).

This crate is a **zero-copy** reader/writer: accessors borrow the input buffer
and return slices (`&[u8]`) into it, decoding escapes only on demand
(`Cow<[u8]>`). It has **no dependencies**.

## Status

Port of the Crystal reference (`c0-cr`):

- tokenizer, table/record and document/group navigation, builder
- canonical-form helpers, ETB stream mode
- pretty form: compact, aligned, and spaced layouts + round-tripping parse
- CSV ⇄ C0DATA conversion
- C0DIFF: parse, build, and atomic multi-file apply
- JSON: the `Value` tree and `to_value`/`from_value` (dependency-free), with
  `to_json`/`from_json` text helpers behind the optional `json` feature

It passes the shared language-agnostic conformance vectors from
[c0-spec](https://github.com/c0data/c0-spec), included here as a git submodule
at `c0-spec/`. After cloning:

```sh
git submodule update --init
```

Not yet ported: a YAML converter and a serde-style derive (the `Serializable`
equivalent).

## Features

The library is dependency-free by default. The optional `json` feature adds
JSON *text* conversion and pulls in `serde_json`:

```toml
c0 = { version = "0.1", features = ["json"] }
```

`json::to_value`/`from_value` (C0DATA ⇄ an in-memory tree) need no feature and
no dependency; only the JSON-text helpers do.

## Usage

```rust
use c0::{Builder, Table, Document, canonical};

// Build compact bytes
let buf = Builder::build(|b| {
    b.group("users", Some(&["name", "amount"]));
    b.record(&["Alice", "1502.30"]);
    b.record(&["Bob", "340.00"]);
});

// Read them back, zero-copy
let t = Table::new(&buf);
assert_eq!(t.record(0).field(0), b"Alice");

// Compact form is canonical — hashable for content addressing
assert!(canonical(&buf));
```

### Stream logs (ETB commits)

```rust
use c0::{open_log, read_log, StreamReader};

let mut log = open_log("claims.c0")?;   // repairs a torn tail first
log.record(&["create", "a1b2", "1718208000"])?;
log.batch(|b| {                          // atomic multi-record commit
    b.record(&["name", "draft"]);
    b.record(&["tag", "alpha"]);
})?;

let bytes = read_log("claims.c0")?;
let r = StreamReader::new(&bytes);
assert!(!r.torn());                       // any torn trailing append is skipped
for rec in r.table().records() { /* ... */ }
```

## Development

```sh
cargo test     # unit, integration, and conformance vectors
cargo clippy
cargo fmt
```

## License

MIT
