# c0

A Rust implementation of [C0DATA](https://github.com/c0data) — structured
data built on ASCII C0 control codes. Values are UTF-8 text (any bytes, as
text); structure is sixteen single-byte codes: FS/GS/RS/US separators, SOH
keys and header rows, STX…ETX text, DLE…CAN nesting, ENQ references, BEL
comments, SUB supersession, SYN/EOT document framing, ETB stream commits,
EM trailing blobs.

This crate is a **zero-copy** reader/writer: readers index a buffer once and
hand back slices (`&[u8]`) into it, decoding text only on demand
(`Cow<[u8]>`). It has **no dependencies**.

## Status

Implements the 1.0 redesign of the spec, mirroring the Crystal reference
(`c0-cr`):

- one `Parser` for the grammar (GRAMMAR.md); validator (`validate`,
  `canonical`) on top of it
- `Container` / `Document` / `Body` / `Group` / `Record` / `Reference` readers,
  `Builder` writer
- stream mode: `StreamReader` (commits, damaged blocks, torn tail),
  `StreamWriter`, `FileLog` with tail repair and fsync
- pretty form: compact, aligned and spaced layouts, and the round-tripping
  `parse`
- CSV ⇄ C0DATA conversion
- C0DIFF: parse, build, and atomic multi-file apply (anchored patterns with
  `old␚␟new` substitutions)
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
c0data = { version = "0.3.1", features = ["json"] }   # used in code as `c0`
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
`Builder::list`). A reference is a group name (`␅tags`) or a **pattern**,
a level that is one branch of the target with its values left out
(`␅␐␝tags␞001␟label␘`: group `tags`, record `001`, field `label`). It is
written with `reference(name)`, `reference_to(group, record, field)` or
`reference_level(|b| …)`, and read with `Record::reference`, which gives a
`Reference` with `name()` or `pattern()` (the level as a `Body`) and
`path()` to linearise it.

### The result model

The readers report the **result**. A dead element (`␇␞…`) is parsed and
dropped with everything it holds: `␇␝g` takes the group's header row,
records and subsections, `␇␜f` the file's groups. A superseded element is
dropped in favour of the one that supersedes it: `␚` before a separator or
opener means the element before is replaced by the element after, at that
grain (`old␚␟new` a unit, `␚␁` a keyed unit, `␚␞` a record, `␚␝` a group,
`␚␜` a file, in the heading too). `␞a␟b␚␟c` reads as `["a", "c"]`;
`Record::units` lists the units as written, each with `superseded()`.
Supersession is content: canonical form keeps it and hashes it as
written. Malformed input fails at construction (`Body::new`,
`Document::new`, `Container::new` return `Err`); canonical checking is
separate and never fails construction.

### Primitives

Underneath, the builder has one method per code: `fs`, `gs`, `rs`, `us`,
`sub`, `soh`, `dle`, `can`, `bel`, `enq`, `syn`, `eot`, `etb`, `em`. Each writes
its code and, when given, what follows it, so a chain reads like the bytes,
and anything the grammar allows can be written. Rust has no optional
arguments, so a code that may stand bare has two spellings: the bare code
(`rs()`, `us()`, `gs()`, `soh()`, `bel()`, `etb()`) and the code with what
follows it (`rs_value`, `us_value`, `gs_name`, `soh_key`, `bel_note`,
`etb_payload`); `sub` is always bare; a reference is `enq(name)` or
`enq_level(|b| …)`, the pattern built inside.

```rust
let buf = c0::Builder::build(|b| {
    b.rs_value("Alice").us_value("1502.30");                           // ␞Alice␟1502.30
    b.rs().soh_key("host").us_value("localhost").soh_key("port").us_value("5432");
                                                                       // ␞␁host␟localhost␁port␟5432
    b.gs_name("users").soh().rs_value("name").us_value("amount");      // ␝users␁␞name␟amount
    b.rs_value("cfg").us().dle().rs().soh_key("b").us_value("2").can(); // ␞cfg␟␐␞␁b␟2␘
    b.bel().gs_name("old");                                            // ␇␝old: a dead group
    b.gs().gs_name("sub").rs().enq("users").us().enq_level(|b| {
        b.gs_name("users").rs_value("1");                              // ␝␝sub␞␅users␟␅␐␝users␞1␘
    });
    b.rs_value("old").sub().us_value("new");                           // ␞old␚␟new (old superseded by new)
    b.rs_value("a").sub().rs_value("b");                               // ␞a␚␞b (the record superseded)
});
```

The composed methods above are built on these and cannot write ill-formed
output; with the primitives, well-formedness is yours. The two layers
share the record state and mix freely: `rs_value("a")` then `field("b")`
writes `␞a␟b`, and `gs_name("g")` then `key("k", "v")` writes `␝g␞␁k␟v`.

### Documents

```rust
let buf = c0::Builder::build(|b| {
    b.syn().heading("schema", "ledger");            // ␖␁schema␟ledger
    b.file("mydb");                                 // ␜mydb
    b.group("users", Some(&["name"]));              // ␝users␁␞name
    b.record(&["Alice"]);
    b.eot();                                        // ␄
});
let doc = c0::Document::new(&buf)?;
assert_eq!(doc.heading().schema(), Some(b"ledger".as_ref()));
assert!(doc.warnings().is_empty());                 // a missing SYN or EOT warns, never fails
```

Heading keys are `shape` (`diff` or `stream`), `schema` (a name, or a level
holding one keyed record of `name`, `url`, `hash`), `version` and `blob`;
anything else, an unknown shape, or a duplicate key (last wins) is a loud
warning. `Container::new` reads a buffer of several documents and the raw
region after EM (`blob_bytes(name)` looks a blob up through the first
heading's `blob` table). `validate` rejects malformed bytes; `canonical`
tells whether bytes are a canonical span.

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
let doc = r.document()?;                  // the committed region
```

A committed block that is not well-formed, cut by its ETB inside open text
or holding any other error, is **damaged** and delivers nothing. By default
the reader yields the records before it (`each_record`) and then fails with
`Error::DamagedBlock { index, resume_offset }`, the offset after the
block's ETB and payload; `StreamReader::continuing(&bytes)` reads on past
it, which is what `r.damaged()` lists.

### C0DIFF

A diff is a document of `shape` `diff` whose records are anchored patterns;
`old␚␟new` inside one is the substitution (DIFF.md in the spec):

```
␖␁shape␟diff
␜greeting.txt
␞␂Hello ␃␟world␚␟universe␟!
␄
```

```rust
let diff = c0::diff::build(|d| {
    d.file("greeting.txt", |d| {
        d.record(|r| { r.anchor("Hello ").sub("world", "universe").anchor("!"); });
        d.insert_after("universe!", "\n");
        d.assert("Hello");                 // must be present once; nothing changes
    });
    d.eot();
});
let out = c0::diff::apply(&diff, &files)?;  // validates everything first
c0::diff::apply_files(&diff, "src")?;        // atomic on disk
```

Each record's search text must match once from the previous match to the
end of the file. There is no replace-all mark: a tool expands one into
anchored records against the target. Anything the text diff does not use
(keyed units, levels, references, header rows, GS, dead records or file
lines, supersession at another grain, a chained supersession) is refused
with `DiffError::Refused`; a hunk is kept without applying by writing it
in a note (`␇␂␞…␃`).

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
