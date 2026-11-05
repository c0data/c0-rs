//! Writer for canonical compact form.

use crate::*;

/// Writes C0DATA in canonical compact form.
///
/// ```
/// let buf = c0::Builder::build(|b| {
///     b.syn().heading("shape", "tabular");
///     b.group("users", Some(&["name", "amount"]));
///     b.record(&["Alice", "1502.30"]);
///     b.record(&["Bob", "340.00"]);
///     b.eot();
/// });
/// ```
///
/// Values are written in their one spelling: plain when they can be, text
/// (`STX … ETX`, seven codes escaped) otherwise; they accept any
/// `AsRef<[u8]>` and are byte-transparent. Keyed records ([`map`], [`key`])
/// keep the order given. Nested levels ([`level`], [`list`], [`key_level`])
/// open with DLE and close with CAN. Names (file and group labels, keys,
/// header names, reference segments) are identifiers: a name with a byte
/// below 0x20 or edge whitespace panics.
///
/// [`map`]: Builder::map
/// [`key`]: Builder::key
/// [`level`]: Builder::level
/// [`list`]: Builder::list
/// [`key_level`]: Builder::key_level
#[derive(Default)]
pub struct Builder {
    buf: Vec<u8>,
    in_record: bool,
    units: usize,
    stack: Vec<(bool, usize)>,
}

impl Builder {
    pub fn new() -> Self {
        Builder::default()
    }

    /// Build a buffer by driving a fresh builder, returning its bytes.
    pub fn build<F: FnOnce(&mut Builder)>(f: F) -> Vec<u8> {
        let mut b = Builder::new();
        f(&mut b);
        b.into_bytes()
    }

    /// The bytes built so far.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Consume the builder, returning the built buffer.
    #[inline]
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    // ---- documents -------------------------------------------------------

    /// SYN: the start of a document. Follow it with [`heading`] keys.
    ///
    /// [`heading`]: Builder::heading
    pub fn syn(&mut self) -> &mut Self {
        self.end_record();
        self.buf.push(SYN);
        self
    }

    /// A heading key and its value (`SOH key US value`), after [`syn`].
    ///
    /// [`syn`]: Builder::syn
    pub fn heading(&mut self, key: &str, value: impl AsRef<[u8]>) -> &mut Self {
        self.end_record();
        self.buf.push(SOH);
        self.name(key);
        self.buf.push(US);
        write_value(&mut self.buf, value.as_ref());
        self
    }

    /// A heading key whose value is a nested level (e.g. the blob table).
    pub fn heading_level<F: FnOnce(&mut Builder)>(&mut self, key: &str, f: F) -> &mut Self {
        self.end_record();
        self.buf.push(SOH);
        self.name(key);
        self.buf.push(US);
        self.open_level(f)
    }

    /// EOT: the end of the document.
    pub fn eot(&mut self) -> &mut Self {
        self.end_record();
        self.buf.push(EOT);
        self
    }

    /// ETB: commit what was appended since the last commit.
    pub fn etb(&mut self) -> &mut Self {
        self.end_record();
        self.buf.push(ETB);
        self
    }

    /// ETB with an integrity payload (a plain value).
    pub fn etb_payload(&mut self, payload: &str) -> &mut Self {
        self.end_record();
        self.buf.push(ETB);
        self.name(payload);
        self
    }

    /// EM and the raw tail. Nothing may follow.
    pub fn em(&mut self, tail: &[u8]) -> &mut Self {
        self.end_record();
        self.buf.push(EM);
        self.buf.extend_from_slice(tail);
        self
    }

    // ---- structure -------------------------------------------------------

    /// FS and a file name.
    pub fn file(&mut self, name: &str) -> &mut Self {
        self.end_record();
        self.buf.push(FS);
        self.name(name);
        self
    }

    /// GS, a group name, and a header row when given.
    pub fn group(&mut self, name: &str, headers: Option<&[&str]>) -> &mut Self {
        self.group_at(name, headers, 1)
    }

    /// GS×`depth`, a group name, and a header row when given.
    pub fn group_at(&mut self, name: &str, headers: Option<&[&str]>, depth: usize) -> &mut Self {
        self.end_record();
        for _ in 0..depth {
            self.buf.push(GS);
        }
        self.name(name);
        if let Some(h) = headers {
            self.header(h);
        }
        self
    }

    /// A header row: `SOH RS name US name …`.
    pub fn header(&mut self, names: &[&str]) -> &mut Self {
        self.end_record();
        self.buf.push(SOH);
        self.buf.push(RS);
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                self.buf.push(US);
            }
            self.name(n);
        }
        self
    }

    /// Document mode: a section at `depth` (GS×depth and a name).
    pub fn section(&mut self, name: &str, depth: usize) -> &mut Self {
        self.group_at(name, None, depth)
    }

    // ---- records -----------------------------------------------------------

    /// A positional record.
    pub fn record<T: AsRef<[u8]>>(&mut self, fields: &[T]) -> &mut Self {
        self.start_record();
        for f in fields {
            self.field(f);
        }
        self
    }

    /// Begin a record and leave it open for [`field`], [`level`], [`list`],
    /// [`reference`] or [`key`] calls.
    ///
    /// [`field`]: Builder::field
    /// [`level`]: Builder::level
    /// [`list`]: Builder::list
    /// [`reference`]: Builder::reference
    /// [`key`]: Builder::key
    pub fn begin_record(&mut self) -> &mut Self {
        self.start_record();
        self
    }

    /// Document mode: a paragraph (a record with one unit).
    pub fn block(&mut self, text: impl AsRef<[u8]>) -> &mut Self {
        self.start_record();
        self.field(text)
    }

    /// Document mode: a list item (a unit of the open block).
    pub fn item(&mut self, text: impl AsRef<[u8]>) -> &mut Self {
        self.field(text)
    }

    /// Append a unit to the open record (opens one if none is open).
    pub fn field(&mut self, value: impl AsRef<[u8]>) -> &mut Self {
        self.next_unit();
        write_value(&mut self.buf, value.as_ref());
        self
    }

    /// A keyed record: a map, entries in the order given.
    pub fn map<T: AsRef<[u8]>>(&mut self, pairs: &[(&str, T)]) -> &mut Self {
        self.start_record();
        for (k, v) in pairs {
            self.key(k, v);
        }
        self
    }

    /// One keyed unit on the open record (opens one if none is open).
    pub fn key(&mut self, name: &str, value: impl AsRef<[u8]>) -> &mut Self {
        if !self.in_record {
            self.start_record();
        }
        self.buf.push(SOH);
        self.name(name);
        self.buf.push(US);
        write_value(&mut self.buf, value.as_ref());
        self.units += 1;
        self
    }

    /// One keyed unit whose value is a nested level.
    pub fn key_level<F: FnOnce(&mut Builder)>(&mut self, name: &str, f: F) -> &mut Self {
        if !self.in_record {
            self.start_record();
        }
        self.buf.push(SOH);
        self.name(name);
        self.buf.push(US);
        self.units += 1;
        self.open_level(f)
    }

    // ---- values that are not plain -------------------------------------

    /// A nested level as the next unit. Inside `f` the builder is at the
    /// top of a fresh body.
    pub fn level<F: FnOnce(&mut Builder)>(&mut self, f: F) -> &mut Self {
        self.next_unit();
        self.open_level(f)
    }

    /// A flat list as the next unit: a level holding one record.
    pub fn list<T: AsRef<[u8]>>(&mut self, items: &[T]) -> &mut Self {
        self.next_unit();
        self.buf.push(DLE);
        if !items.is_empty() {
            self.buf.push(RS);
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    self.buf.push(US);
                }
                write_value(&mut self.buf, it.as_ref());
            }
        }
        self.buf.push(CAN);
        self
    }

    /// A group reference (`ENQ name`) as the next unit.
    pub fn reference(&mut self, name: &str) -> &mut Self {
        self.next_unit();
        self.buf.push(ENQ);
        self.name(name);
        self
    }

    /// A reference by path as the next unit: one segment is a group
    /// reference, two or three are a path written as text.
    pub fn reference_path(&mut self, segments: &[&str]) -> &mut Self {
        assert!(
            (1..=3).contains(&segments.len()),
            "a reference path has one to three segments"
        );
        if segments.len() == 1 {
            return self.reference(segments[0]);
        }
        self.next_unit();
        self.buf.push(ENQ);
        self.buf.push(STX);
        for (i, seg) in segments.iter().enumerate() {
            if i > 0 {
                self.buf.push(US);
            }
            assert!(
                !seg.is_empty() && is_name(seg.as_bytes()),
                "reference segment is not a name: {seg:?}"
            );
            self.buf.extend_from_slice(seg.as_bytes());
        }
        self.buf.push(ETX);
        self
    }

    // ---- comments ----------------------------------------------------------

    /// A note at the current position (record level, or as an empty unit).
    /// A note has content: an empty one panics (a bare BEL is malformed).
    pub fn note(&mut self, text: impl AsRef<[u8]>) -> &mut Self {
        let text = text.as_ref();
        assert!(!text.is_empty(), "a note needs content");
        self.buf.push(BEL);
        if is_plain(text) {
            self.buf.extend_from_slice(text);
        } else {
            write_value(&mut self.buf, text);
        }
        self
    }

    // ---- internals ---------------------------------------------------------

    fn name(&mut self, name: &str) {
        if let Err(e) = write_name(&mut self.buf, name.as_bytes()) {
            panic!("{e}");
        }
    }

    fn start_record(&mut self) {
        self.buf.push(RS);
        self.in_record = true;
        self.units = 0;
    }

    fn end_record(&mut self) {
        self.in_record = false;
        self.units = 0;
    }

    fn next_unit(&mut self) {
        if !self.in_record {
            self.start_record();
        }
        if self.units > 0 {
            self.buf.push(US);
        }
        self.units += 1;
    }

    fn open_level<F: FnOnce(&mut Builder)>(&mut self, f: F) -> &mut Self {
        self.stack.push((self.in_record, self.units));
        self.in_record = false;
        self.units = 0;
        self.buf.push(DLE);
        f(self);
        self.buf.push(CAN);
        let (in_record, units) = self.stack.pop().unwrap_or((false, 0));
        self.in_record = in_record;
        self.units = units;
        self
    }
}
