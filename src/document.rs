//! Documents: a SYN heading, a body, EOT; and containers of several
//! documents with an EM tail.

use crate::*;
use std::borrow::Cow;

/// The heading of a document: keyed units after SYN, in written order,
/// dead keys dropped. A duplicate key is reported as written;
/// lookups take the last value.
pub struct Heading<'a> {
    raw: &'a [u8],
    pairs: Vec<(&'a [u8], Cow<'a, [u8]>)>,
    raw_pairs: Vec<(&'a [u8], &'a [u8])>,
}

impl<'a> Heading<'a> {
    pub(crate) fn new(
        raw: &'a [u8],
        pairs: Vec<(&'a [u8], Cow<'a, [u8]>)>,
        raw_pairs: Vec<(&'a [u8], &'a [u8])>,
    ) -> Self {
        Heading {
            raw,
            pairs,
            raw_pairs,
        }
    }

    /// The heading of a document without SYN.
    pub fn empty() -> Self {
        Heading {
            raw: &[],
            pairs: Vec::new(),
            raw_pairs: Vec::new(),
        }
    }

    /// The heading's bytes (after SYN, before the body).
    pub fn raw(&self) -> &'a [u8] {
        self.raw
    }

    /// (key, value) pairs in written order. A nested-level value is its raw
    /// `DLE … CAN` bytes; see [`Heading::blob`].
    pub fn pairs(&self) -> &[(&'a [u8], Cow<'a, [u8]>)] {
        &self.pairs
    }

    /// The value for a key (last wins), or `None`.
    pub fn get(&self, key: &str) -> Option<&[u8]> {
        let mut found = None;
        for (k, v) in &self.pairs {
            if *k == key.as_bytes() {
                found = Some(v.as_ref());
            }
        }
        found
    }

    /// Keys in written order.
    pub fn keys(&self) -> Vec<&'a [u8]> {
        self.pairs.iter().map(|(k, _)| *k).collect()
    }

    /// The `shape` key, when present and UTF-8.
    pub fn shape(&self) -> Option<&str> {
        self.get("shape").and_then(|v| std::str::from_utf8(v).ok())
    }

    /// The `version` key as an integer; 1 when absent.
    pub fn version(&self) -> u32 {
        self.get("version")
            .and_then(|v| std::str::from_utf8(v).ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(1)
    }

    /// The schema the body claims: a name, or the raw level holding the
    /// keyed record of `name`, `url` and `hash`; `None` when absent.
    pub fn schema(&self) -> Option<&[u8]> {
        self.get("schema")
    }

    /// Raw unit of the `blob` key (a level), or `None`.
    pub fn raw_blob(&self) -> Option<&'a [u8]> {
        let mut found = None;
        for (k, v) in &self.raw_pairs {
            if *k == b"blob" {
                found = Some(*v);
            }
        }
        found
    }

    /// The blob table, when declared as a nested level.
    pub fn blob(&self) -> Result<Option<Body<'a>>> {
        match self.raw_blob() {
            Some(raw) if is_level(raw) => Body::of_level(raw).map(Some),
            _ => Ok(None),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

const KNOWN_KEYS: [&[u8]; 4] = [b"shape", b"schema", b"version", b"blob"];
const KNOWN_SHAPES: [&str; 2] = ["diff", "stream"];

/// One C0DATA document: an optional SYN heading, a body, an optional EOT.
/// [`Document::new`] reads the first document in a buffer; use
/// [`Container`] for a buffer holding several documents or an EM tail.
pub struct Document<'a> {
    buf: &'a [u8],
    heading: Heading<'a>,
    body: Body<'a>,
    start: usize,
    stop: usize,
    has_syn: bool,
    has_eot: bool,
    warnings: Vec<String>,
}

impl<'a> Document<'a> {
    /// Read the document at the start of `buf`.
    pub fn new(buf: &'a [u8]) -> Result<Self> {
        Self::at(buf, 0)
    }

    /// Read the document starting at `start`.
    pub fn at(buf: &'a [u8], start: usize) -> Result<Self> {
        let mut parser = Parser::new(buf, false);
        Self::parse(buf, start, buf.len(), &mut parser)
    }

    /// Read the document in `buf[start..stop]` with the given parser.
    pub(crate) fn parse(
        buf: &'a [u8],
        start: usize,
        stop: usize,
        p: &mut Parser<'a>,
    ) -> Result<Self> {
        let mut warnings = Vec::new();
        let mut pos = p.layout(start, stop);
        let has_syn = pos < stop && buf[pos] == SYN;
        let heading = if has_syn {
            p.not_canonical("SYN in a canonical span");
            let (h, after) = p.heading(pos + 1, stop)?;
            pos = after;
            h
        } else {
            warnings.push("no SYN at the start of the document".to_string());
            Heading::empty()
        };
        let body_start = pos;
        let (files, body_end) = p.body(pos, stop, false)?;
        let body = Body::from_parts(buf, body_start, body_end, files);
        pos = p.layout(body_end, stop);
        let has_eot = pos < stop && buf[pos] == EOT;
        if has_eot {
            p.not_canonical("EOT in a canonical span");
            pos += 1;
        } else if heading.shape() != Some("stream") {
            warnings.push("no EOT at the end of the document".to_string());
        }
        let mut seen: Vec<&[u8]> = Vec::new();
        for k in heading.keys() {
            let name = String::from_utf8_lossy(k);
            if !KNOWN_KEYS.contains(&k) {
                warnings.push(format!("unknown heading key '{name}'"));
            }
            if seen.contains(&k) {
                warnings.push(format!("duplicate heading key '{name}': last wins"));
            } else {
                seen.push(k);
            }
        }
        if let Some(shape) = heading.get("shape") {
            let known = std::str::from_utf8(shape).is_ok_and(|s| KNOWN_SHAPES.contains(&s));
            if !known {
                warnings.push(format!(
                    "unknown shape '{}'",
                    String::from_utf8_lossy(shape)
                ));
            }
        }
        if let Some(v) = heading.get("version") {
            if v != b"1" {
                warnings.push(format!("unknown version '{}'", String::from_utf8_lossy(v)));
            }
        }
        Ok(Document {
            buf,
            heading,
            body,
            start,
            stop: pos,
            has_syn,
            has_eot,
            warnings,
        })
    }

    pub fn heading(&self) -> &Heading<'a> {
        &self.heading
    }

    pub fn body(&self) -> &Body<'a> {
        &self.body
    }

    /// Offset where the document begins.
    pub fn start(&self) -> usize {
        self.start
    }

    /// Offset just past EOT (or past the body when there is none).
    pub fn stop(&self) -> usize {
        self.stop
    }

    pub fn has_syn(&self) -> bool {
        self.has_syn
    }

    pub fn has_eot(&self) -> bool {
        self.has_eot
    }

    /// Loud warnings met while reading (DESIGN.md, Warnings); never an error.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The document's bytes.
    pub fn raw(&self) -> &'a [u8] {
        &self.buf[self.start..self.stop]
    }

    pub fn shape(&self) -> Option<&str> {
        self.heading.shape()
    }

    // Convenience accessors over the body.

    pub fn name(&self) -> &'a [u8] {
        self.body.name()
    }

    pub fn files(&self) -> &[FileView<'a>] {
        self.body.files()
    }

    pub fn groups(&self) -> impl Iterator<Item = &Group<'a>> + '_ {
        self.body.groups()
    }

    pub fn group_count(&self) -> usize {
        self.body.group_count()
    }

    pub fn group(&self, i: usize) -> &Group<'a> {
        self.body.group(i)
    }

    pub fn group_by_name(&self, name: &str) -> Result<&Group<'a>> {
        self.body.group_by_name(name)
    }

    pub fn group_names(&self) -> Vec<&'a [u8]> {
        self.body.group_names()
    }

    pub fn records(&self) -> &[Record<'a>] {
        self.body.records()
    }
}

/// A file or message: one or more documents and an optional raw tail
/// after EM.
pub struct Container<'a> {
    documents: Vec<Document<'a>>,
    tail: &'a [u8],
    has_em: bool,
    warnings: Vec<String>,
}

impl<'a> Container<'a> {
    pub fn new(buf: &'a [u8]) -> Result<Self> {
        let mut parser = Parser::new(buf, false);
        Self::parse(buf, buf.len(), &mut parser)
    }

    /// Parse `buf[..stop]` with the given parser. The tail after EM runs to
    /// the end of the buffer.
    pub(crate) fn parse(buf: &'a [u8], stop: usize, p: &mut Parser<'a>) -> Result<Self> {
        let mut documents: Vec<Document<'a>> = Vec::new();
        let mut tail: &'a [u8] = &[];
        let mut has_em = false;
        let mut warnings = Vec::new();
        let mut pos = p.layout(0, stop);
        let mut first = true;
        while pos < stop {
            if buf[pos] == EM {
                p.not_canonical("EM in a canonical span");
                has_em = true;
                tail = &buf[pos + 1..];
                break;
            }
            if !first && buf[pos] != SYN {
                return Err(Error::syntax(
                    "a document after the first must begin with SYN",
                    pos,
                ));
            }
            let doc = Document::parse(buf, pos, stop, p)?;
            first = false;
            pos = p.layout(doc.stop, stop);
            let has_eot = doc.has_eot;
            documents.push(doc);
            if pos < stop && buf[pos] != SYN && buf[pos] != EM {
                return Err(Error::syntax(
                    if has_eot {
                        "unexpected byte after EOT"
                    } else {
                        "unexpected byte after document"
                    },
                    pos,
                ));
            }
        }
        for (i, doc) in documents.iter().enumerate() {
            let Some(raw) = doc.heading.raw_blob() else {
                continue;
            };
            if i > 0 {
                warnings.push("blob table in a later heading is ignored".to_string());
            } else if !is_level(raw) {
                warnings.push("blob is not a level: declares nothing".to_string());
            } else {
                let has_header = doc
                    .heading
                    .blob()
                    .ok()
                    .flatten()
                    .is_some_and(|b| b.groups().next().is_some_and(|g| g.has_header()));
                if !has_header {
                    warnings.push("blob table has no header row: declares nothing".to_string());
                }
            }
        }
        if has_em
            && documents
                .first()
                .is_none_or(|d| d.heading.raw_blob().is_none())
        {
            warnings.push("EM with no blob table: the region is unaddressable".to_string());
        }
        Ok(Container {
            documents,
            tail,
            has_em,
            warnings,
        })
    }

    /// The documents, in order.
    pub fn documents(&self) -> &[Document<'a>] {
        &self.documents
    }

    /// The first document, if the buffer holds one.
    pub fn first(&self) -> Option<&Document<'a>> {
        self.documents.first()
    }

    /// The raw region after EM (empty when there is no EM).
    pub fn tail(&self) -> &'a [u8] {
        self.tail
    }

    pub fn has_em(&self) -> bool {
        self.has_em
    }

    /// Container-level loud warnings (a `blob` key in a later heading, EM
    /// with no blob table); each document carries its own.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The blob table from the first document's heading.
    pub fn blob(&self) -> Result<Option<Body<'a>>> {
        match self.documents.first() {
            Some(d) => d.heading.blob(),
            None => Ok(None),
        }
    }

    /// Bytes of a named blob from the tail, or `None` when the table does
    /// not declare it (or its row is unusable).
    pub fn blob_bytes(&self, name: &str) -> Result<Option<&'a [u8]>> {
        let Some(table) = self.blob()? else {
            return Ok(None);
        };
        let Some(g) = table.groups().next() else {
            return Ok(None);
        };
        let ni = g.column("name").unwrap_or(0);
        let oi = g.column("offset").unwrap_or(1);
        let li = g.column("length").unwrap_or(2);
        let mut found = None;
        for r in g.records() {
            if r.value(ni)?.as_ref() != name.as_bytes() {
                continue;
            }
            let off = parse_usize(&r.value(oi)?);
            let len = parse_usize(&r.value(li)?);
            if let (Some(off), Some(len)) = (off, len) {
                found = self.tail.get(off..off + len);
            }
        }
        Ok(found)
    }
}

fn parse_usize(v: &[u8]) -> Option<usize> {
    std::str::from_utf8(v).ok()?.trim().parse().ok()
}
