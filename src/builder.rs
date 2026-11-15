//! Writer for canonical compact form.

use crate::*;

/// Writes C0DATA in canonical compact form.
///
/// ```
/// let buf = c0::Builder::build(|b| {
///     b.syn().heading("schema", "ledger");
///     b.group("users", Some(&["name", "amount"]));
///     b.record(&["Alice", "1502.30"]);
///     b.record(&["Bob", "340.00"]);
///     b.eot();
/// });
/// ```
///
/// Two layers. The primitives are one method per code (`fs`, `gs`, `rs`,
/// `us`, `sub`, `soh`, `dle`, `can`, `bel`, `enq`, `syn`, `eot`, `etb`,
/// `em`); each
/// writes its code and, when given, what grammatically follows it, so a
/// chain reads like the bytes. Rust has no optional arguments, so the codes
/// that may stand bare come in two spellings: the bare code (`rs()`, `us()`,
/// `gs()`, `soh()`, `bel()`, `etb()`) and the code with what follows it
/// (`rs_value`, `us_value`, `gs_name`, `soh_key`, `bel_note`,
/// `etb_payload`). `sub` is always bare: the element after the next
/// separator is subordinate to the one before it, before GS a subsection
/// (`␚␝s`), before US a sub-unit (`old␚␟new`). A reference is `enq(name)`
/// or `enq_level(|b| …)`, a pattern:
///
/// ```
/// let buf = c0::Builder::build(|b| {
///     b.rs_value("Alice").us_value("1502.30");          // ␞Alice␟1502.30
///     b.rs().soh_key("host").us_value("localhost");     // ␞␁host␟localhost
///     b.bel().gs_name("old");                           // ␇␝old: a dead group
///     b.sub().gs_name("intro");                         // ␚␝intro: a subsection (depth 2)
///     b.rs_value("old").sub().us_value("new");          // ␞old␚␟new: new is a sub-unit of old
///     b.rs().enq("tags").us().enq_level(|b| {           // ␞␅tags␟␅␐␝tags␞001␘
///         b.gs_name("tags").rs_value("001");
///     });
/// });
/// ```
///
/// The composed methods (`file`, `group`, `header`, `record`, `field`,
/// `map`, `key`, `level`, `list`, `reference`, `sub_unit`, `note`, …) are
/// built on the
/// primitives and cannot write ill-formed output. The primitives write what
/// you spell; well-formedness is then yours to keep. Both layers share the
/// record state, so they mix.
///
/// Values are written in their one spelling: plain when they can be, text
/// (`STX … ETX`, seven codes escaped) otherwise; they accept any
/// `AsRef<[u8]>` and are byte-transparent. Keyed records ([`map`], [`key`])
/// keep the order given. Nested levels ([`level`], [`list`], [`key_level`])
/// open with DLE and close with CAN. Names (file and group labels, keys,
/// header names, pattern steps, commit payloads) are identifiers: a
/// name with a byte below 0x20 or edge whitespace panics.
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

    // ---- primitives: one method per code ---------------------------------

    /// FS and a file name. Ends any open record.
    pub fn fs(&mut self, name: &str) -> &mut Self {
        self.end_record();
        self.buf.push(FS);
        self.name(name);
        self
    }

    /// A bare GS: a group with an empty name (`gs().gs_name("b")` is two
    /// groups, `""` then `b`). Ends any open record.
    pub fn gs(&mut self) -> &mut Self {
        self.end_record();
        self.buf.push(GS);
        self
    }

    /// GS and a group name. Ends any open record.
    pub fn gs_name(&mut self, name: &str) -> &mut Self {
        self.gs();
        self.name(name);
        self
    }

    /// `depth − 1` SUB, GS and a group name: `gs_name_at("s", 2)` is
    /// `␚␝s`, a subsection. Ends any open record.
    pub fn gs_name_at(&mut self, name: &str, depth: usize) -> &mut Self {
        assert!(depth >= 1, "a group is at depth one or deeper");
        self.end_record();
        for _ in 1..depth {
            self.buf.push(SUB);
        }
        self.buf.push(GS);
        self.name(name);
        self
    }

    /// A bare RS: opens a record with no units yet.
    pub fn rs(&mut self) -> &mut Self {
        self.buf.push(RS);
        self.in_record = true;
        self.units = 0;
        self
    }

    /// RS and the record's first value.
    pub fn rs_value(&mut self, value: impl AsRef<[u8]>) -> &mut Self {
        self.rs();
        self.first_value(value.as_ref());
        self
    }

    /// A bare US: the next unit of the record is empty, or is what follows
    /// (a level, a reference).
    pub fn us(&mut self) -> &mut Self {
        self.buf.push(US);
        self.units += 1;
        self
    }

    /// US and the value that follows it.
    pub fn us_value(&mut self, value: impl AsRef<[u8]>) -> &mut Self {
        self.buf.push(US);
        write_value(&mut self.buf, value.as_ref());
        self.units += 1;
        self
    }

    /// SUB: the element after the next separator is subordinate to the one
    /// before it, at that code's grain: before GS a subsection (`␚␝s`, one
    /// SUB per level), before US a sub-unit (`old␚␟new`). Does not change
    /// the unit count.
    pub fn sub(&mut self) -> &mut Self {
        self.buf.push(SUB);
        self
    }

    /// A bare SOH. Before `rs` it opens a header row.
    pub fn soh(&mut self) -> &mut Self {
        self.buf.push(SOH);
        self
    }

    /// SOH and a key. The key's value follows as `us_value`.
    pub fn soh_key(&mut self, name: &str) -> &mut Self {
        self.buf.push(SOH);
        self.name(name);
        self
    }

    /// DLE: open a nested level. Inside, the builder is at the top of a
    /// fresh body; the record state is restored by [`can`].
    ///
    /// [`can`]: Builder::can
    pub fn dle(&mut self) -> &mut Self {
        self.stack.push((self.in_record, self.units));
        self.in_record = false;
        self.units = 0;
        self.buf.push(DLE);
        self
    }

    /// CAN: close the innermost level. Panics when none is open.
    pub fn can(&mut self) -> &mut Self {
        let (in_record, units) = self.stack.pop().expect("CAN with no open level");
        self.buf.push(CAN);
        self.in_record = in_record;
        self.units = units;
        self
    }

    /// A bare BEL: marks the element that follows dead.
    pub fn bel(&mut self) -> &mut Self {
        self.buf.push(BEL);
        self
    }

    /// BEL and a note. A note has content: an empty one panics (a bare BEL
    /// is a dead marker, written with [`bel`]).
    ///
    /// [`bel`]: Builder::bel
    pub fn bel_note(&mut self, text: impl AsRef<[u8]>) -> &mut Self {
        let text = text.as_ref();
        assert!(
            !text.is_empty(),
            "a note needs content: bare BEL marks the next element dead"
        );
        self.buf.push(BEL);
        write_value(&mut self.buf, text);
        self
    }

    /// ENQ and a group name: a group reference. The name is required.
    pub fn enq(&mut self, name: &str) -> &mut Self {
        assert!(!name.is_empty(), "a reference needs a name");
        self.buf.push(ENQ);
        self.name(name);
        self
    }

    /// ENQ and a pattern: a level built in `f`, the target with its values
    /// left out (`enq_level(|b| { b.gs_name("tags").rs_value("001").us_value("label"); })`
    /// writes `␅␐␝tags␞001␟label␘`).
    pub fn enq_level<F: FnOnce(&mut Builder)>(&mut self, f: F) -> &mut Self {
        self.buf.push(ENQ);
        self.dle();
        f(self);
        self.can()
    }

    /// SYN: the start of a document. Heading pairs follow as
    /// `soh_key(k).us_value(v)` or [`heading`]. Ends any open record.
    ///
    /// [`heading`]: Builder::heading
    pub fn syn(&mut self) -> &mut Self {
        self.end_record();
        self.buf.push(SYN);
        self
    }

    /// EOT: the end of the document. Ends any open record.
    pub fn eot(&mut self) -> &mut Self {
        self.end_record();
        self.buf.push(EOT);
        self
    }

    /// A bare ETB: commit what was appended since the last commit. Ends
    /// any open record.
    pub fn etb(&mut self) -> &mut Self {
        self.end_record();
        self.buf.push(ETB);
        self
    }

    /// ETB and an integrity payload (a name).
    pub fn etb_payload(&mut self, payload: &str) -> &mut Self {
        self.etb();
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

    // ---- documents -------------------------------------------------------

    /// A heading key and its value (`SOH key US value`), after [`syn`].
    ///
    /// [`syn`]: Builder::syn
    pub fn heading(&mut self, key: &str, value: impl AsRef<[u8]>) -> &mut Self {
        self.end_record();
        self.soh_key(key).us_value(value)
    }

    /// A heading key whose value is a nested level (e.g. the blob table).
    pub fn heading_level<F: FnOnce(&mut Builder)>(&mut self, key: &str, f: F) -> &mut Self {
        self.end_record();
        self.soh_key(key).us().dle();
        f(self);
        self.can()
    }

    // ---- structure -------------------------------------------------------

    /// FS and a file name.
    pub fn file(&mut self, name: &str) -> &mut Self {
        self.fs(name)
    }

    /// GS, a group name, and a header row when given.
    pub fn group(&mut self, name: &str, headers: Option<&[&str]>) -> &mut Self {
        self.group_at(name, headers, 1)
    }

    /// A group at `depth` (`depth − 1` SUB then GS), a group name, and a
    /// header row when given.
    pub fn group_at(&mut self, name: &str, headers: Option<&[&str]>, depth: usize) -> &mut Self {
        self.gs_name_at(name, depth);
        if let Some(h) = headers {
            self.header(h);
        }
        self
    }

    /// A header row: `SOH RS name US name …`. Ends the record it writes.
    pub fn header(&mut self, names: &[&str]) -> &mut Self {
        for n in names {
            assert!(is_name(n.as_bytes()), "not a legal name: {n:?}");
        }
        self.end_record();
        self.soh().rs();
        for (i, n) in names.iter().enumerate() {
            if i == 0 {
                self.first_value(n.as_bytes());
            } else {
                self.us_value(n);
            }
        }
        self.end_record();
        self
    }

    /// Document mode: a section at `depth` (`depth − 1` SUB, GS and a name).
    pub fn section(&mut self, name: &str, depth: usize) -> &mut Self {
        self.group_at(name, None, depth)
    }

    // ---- records -----------------------------------------------------------

    /// A positional record.
    pub fn record<T: AsRef<[u8]>>(&mut self, fields: &[T]) -> &mut Self {
        self.rs();
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
        self.rs()
    }

    /// Document mode: a paragraph (a record with one unit).
    pub fn block(&mut self, text: impl AsRef<[u8]>) -> &mut Self {
        self.rs_value(text)
    }

    /// Document mode: a list item (a unit of the open block).
    pub fn item(&mut self, text: impl AsRef<[u8]>) -> &mut Self {
        self.field(text)
    }

    /// Append a unit to the open record (opens one if none is open).
    pub fn field(&mut self, value: impl AsRef<[u8]>) -> &mut Self {
        if !self.in_record {
            self.rs_value(value)
        } else if self.units == 0 {
            self.first_value(value.as_ref());
            self
        } else {
            self.us_value(value)
        }
    }

    /// A keyed record: a map, entries in the order given.
    pub fn map<T: AsRef<[u8]>>(&mut self, pairs: &[(&str, T)]) -> &mut Self {
        self.rs();
        for (k, v) in pairs {
            self.key(k, v);
        }
        self
    }

    /// One keyed unit on the open record (opens one if none is open).
    pub fn key(&mut self, name: &str, value: impl AsRef<[u8]>) -> &mut Self {
        if !self.in_record {
            self.rs();
        }
        self.soh_key(name).us_value(value)
    }

    /// One keyed unit whose value is a nested level.
    pub fn key_level<F: FnOnce(&mut Builder)>(&mut self, name: &str, f: F) -> &mut Self {
        if !self.in_record {
            self.rs();
        }
        self.soh_key(name).us().dle();
        f(self);
        self.can()
    }

    // ---- values that are not plain -------------------------------------

    /// A nested level as the next unit. Inside `f` the builder is at the
    /// top of a fresh body.
    pub fn level<F: FnOnce(&mut Builder)>(&mut self, f: F) -> &mut Self {
        self.next_unit();
        self.dle();
        f(self);
        self.can()
    }

    /// A flat list as the next unit: a level holding one record.
    pub fn list<T: AsRef<[u8]>>(&mut self, items: &[T]) -> &mut Self {
        self.next_unit();
        self.dle();
        if !items.is_empty() {
            self.rs();
            for it in items {
                self.field(it);
            }
        }
        self.can()
    }

    /// A group reference (`ENQ name`) as the next unit.
    pub fn reference(&mut self, name: &str) -> &mut Self {
        self.next_unit();
        self.enq(name)
    }

    /// A reference as the next unit, its pattern built in `f` (see
    /// [`enq_level`]).
    ///
    /// [`enq_level`]: Builder::enq_level
    pub fn reference_level<F: FnOnce(&mut Builder)>(&mut self, f: F) -> &mut Self {
        self.next_unit();
        self.enq_level(f)
    }

    /// A reference to a record, or to a field of it, as the next unit: the
    /// pattern `␅␐␝group␞record␟field␘`.
    pub fn reference_to(&mut self, group: &str, record: &str, field: Option<&str>) -> &mut Self {
        self.next_unit();
        self.enq_level(|b| {
            b.gs_name(group).rs_value(record);
            if let Some(f) = field {
                b.us_value(f);
            }
        })
    }

    /// A sub-unit of the unit just written: SUB, US and the value
    /// (`␞a␚␟b`). It is not a unit of the record and does not count as
    /// one. Opens a record if none is open: `␞␚␟b` is an empty first unit
    /// holding `b`.
    pub fn sub_unit(&mut self, value: impl AsRef<[u8]>) -> &mut Self {
        self.sub_unit_at(value, 1)
    }

    /// A sub-unit at `depth`: `depth` SUB, US and the value. `depth` 2 is a
    /// sub-unit of the sub-unit written before (`␞a␚␟b␚␚␟c`).
    pub fn sub_unit_at(&mut self, value: impl AsRef<[u8]>, depth: usize) -> &mut Self {
        assert!(depth >= 1, "a sub-unit is at depth one or deeper");
        if !self.in_record {
            self.rs();
        }
        for _ in 0..depth {
            self.buf.push(SUB);
        }
        self.buf.push(US);
        write_value(&mut self.buf, value.as_ref());
        // the unit it hangs off exists, even when empty
        self.units = self.units.max(1);
        self
    }

    // ---- comments ----------------------------------------------------------

    /// A note at the current position (record level, or as an empty unit).
    /// A note has content: an empty one panics (a bare BEL is a dead
    /// marker, written with [`bel`]).
    ///
    /// [`bel`]: Builder::bel
    pub fn note(&mut self, text: impl AsRef<[u8]>) -> &mut Self {
        self.bel_note(text)
    }

    // ---- internals ---------------------------------------------------------

    fn name(&mut self, name: &str) {
        if let Err(e) = write_name(&mut self.buf, name.as_bytes()) {
            panic!("{e}");
        }
    }

    fn end_record(&mut self) {
        self.in_record = false;
        self.units = 0;
    }

    /// Position for the next unit of the open record (opening one if
    /// needed): US between units, nothing before the first.
    fn next_unit(&mut self) {
        if !self.in_record {
            self.rs();
        } else if self.units > 0 {
            self.buf.push(US);
        }
        self.units += 1;
    }

    /// The first value of a record, straight after RS.
    fn first_value(&mut self, value: &[u8]) {
        write_value(&mut self.buf, value);
        self.units = 1;
    }
}
