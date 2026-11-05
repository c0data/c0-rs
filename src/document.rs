//! Documents: a SYN heading, a body, EOT; and containers of several
//! documents with an EM tail.

use crate::*;
use std::borrow::Cow;

/// The heading of a document: keyed units after SYN, in written order.
/// Lookups take the last value for a duplicate key.
pub struct Heading<'a> {
    raw: &'a [u8],
    pairs: Vec<(&'a [u8], Cow<'a, [u8]>)>,
    raw_pairs: Vec<(&'a [u8], &'a [u8])>,
}

impl<'a> Heading<'a> {
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

/// Parse keyed units after SYN. Returns the heading and the position of
/// the first body code.
pub(crate) fn parse_heading(
    buf: &[u8],
    mut pos: usize,
    len: usize,
) -> Result<(Heading<'_>, usize)> {
    let hs = pos;
    let mut pairs = Vec::new();
    let mut raw_pairs = Vec::new();
    loop {
        pos = scan::layout(buf, pos, len);
        if pos >= len {
            break;
        }
        let b = buf[pos];
        if b == SOH {
            let p2 = scan::layout(buf, pos + 1, len);
            if p2 < len && buf[p2] == RS {
                break; // a header row begins the body
            }
            let ks = p2;
            let ke = scan::plain(buf, ks, len);
            if !(ke < len && buf[ke] == US) {
                return Err(Error::syntax("key without a value (no US)", ke));
            }
            let vs = ke + 1;
            let ve = scan::unit(buf, vs, len)?;
            let key = trim(&buf[ks..ke]);
            let rawv = trim(&buf[vs..ve]);
            if rawv.first() == Some(&ENQ) {
                return Err(Error::syntax("reference in a heading", vs));
            }
            raw_pairs.push((key, rawv));
            let v = if rawv.first() == Some(&DLE) {
                Cow::Borrowed(rawv)
            } else {
                value(rawv)?
            };
            pairs.push((key, v));
            pos = ve;
        } else if b == BEL {
            let p2 = scan::layout(buf, pos + 1, len);
            if p2 >= len {
                return Err(Error::end("BEL at end of input", p2));
            }
            let b2 = buf[p2];
            let header_row = b2 == SOH && {
                let p3 = scan::layout(buf, p2 + 1, len);
                p3 < len && buf[p3] == RS
            };
            if b2 >= 0x20 || b2 == STX || (b2 == SOH && !header_row) {
                pos = scan::comment(buf, pos, len)?; // a note or a dead key
            } else {
                break; // a dead body element begins the body
            }
        } else if b >= 0x20 {
            return Err(Error::syntax("plain text directly after SYN", pos));
        } else {
            break;
        }
    }
    Ok((
        Heading {
            raw: &buf[hs..pos],
            pairs,
            raw_pairs,
        },
        pos,
    ))
}

// From the start of a body to its end: EOT, SYN, EM or end of input at
// depth zero, skipping text and levels.
fn find_body_end(buf: &[u8], mut pos: usize, len: usize) -> Result<usize> {
    while pos < len {
        let b = buf[pos];
        if b >= 0x20 {
            pos += 1;
        } else if b == STX {
            pos = scan::text(buf, pos, len)?;
        } else if b == DLE {
            pos = scan::level(buf, pos, len)?;
        } else if b == EOT || b == SYN || b == EM {
            return Ok(pos);
        } else if b == CAN {
            return Err(Error::syntax("CAN with no open level", pos));
        } else {
            pos += 1;
        }
    }
    Ok(pos)
}

const KNOWN_KEYS: [&[u8]; 3] = [b"shape", b"version", b"blob"];
const KNOWN_SHAPES: [&str; 5] = ["diff", "stream", "tabular", "document", "key-value"];

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
        let len = buf.len();
        let mut warnings = Vec::new();
        let mut pos = scan::layout(buf, start, len);
        let has_syn = pos < len && buf[pos] == SYN;
        let heading = if has_syn {
            let (h, p) = parse_heading(buf, pos + 1, len)?;
            pos = p;
            h
        } else {
            warnings.push("no SYN at the start of the document".to_string());
            Heading::empty()
        };
        let body_start = pos;
        let body_end = find_body_end(buf, pos, len)?;
        let body = Body::slice(buf, body_start, body_end)?;
        pos = scan::layout(buf, body_end, len);
        let has_eot = pos < len && buf[pos] == EOT;
        if has_eot {
            pos += 1;
        } else if heading.shape() != Some("stream") {
            warnings.push("no EOT at the end of the document".to_string());
        }
        for k in heading.keys() {
            if !KNOWN_KEYS.contains(&k) {
                warnings.push(format!(
                    "unknown heading key '{}'",
                    String::from_utf8_lossy(k)
                ));
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

    /// Loud and ordinary warnings met while reading; never an error.
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
        let len = buf.len();
        let mut documents: Vec<Document<'a>> = Vec::new();
        let mut tail: &'a [u8] = &[];
        let mut has_em = false;
        let mut warnings = Vec::new();
        let mut pos = scan::layout(buf, 0, len);
        while pos < len {
            if buf[pos] == EM {
                has_em = true;
                tail = &buf[pos + 1..];
                break;
            }
            let doc = Document::at(buf, pos)?;
            pos = scan::layout(buf, doc.stop, len);
            if !documents.is_empty() && doc.heading.raw_blob().is_some() {
                warnings.push(format!(
                    "blob table in heading of document {} ignored",
                    documents.len() + 1
                ));
            }
            documents.push(doc);
            if pos < len && buf[pos] != SYN && buf[pos] != EM {
                return Err(Error::syntax("unexpected byte after EOT", pos));
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
    /// with no blob table).
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
