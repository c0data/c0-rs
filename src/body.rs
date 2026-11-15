//! Zero-copy navigator for a document body: files, groups, header rows and
//! records, each a slice into the original buffer. The structure is built
//! once by the [`Parser`] with dead elements dropped.

use crate::*;
use std::borrow::Cow;
use std::fmt;

/// A document body: the bytes between a heading and EOT, or the inside of
/// a nested level. Everything it returns borrows the original buffer.
///
/// Records before the first GS of a file form an unnamed group; records
/// and groups before the first FS form an unnamed file.
pub struct Body<'a> {
    buf: &'a [u8],
    start: usize,
    stop: usize,
    files: Vec<FileView<'a>>,
}

impl<'a> Body<'a> {
    /// Parse a whole buffer as a body (no SYN, no EOT).
    pub fn new(buf: &'a [u8]) -> Result<Self> {
        Self::slice(buf, 0, buf.len())
    }

    /// Parse `buf[start..stop]` as a body.
    pub fn slice(buf: &'a [u8], start: usize, stop: usize) -> Result<Self> {
        Self::parse(buf, start, stop, false)
    }

    /// The contents of a nested level: `raw` is a unit holding `DLE … CAN`
    /// (layout at its edges allowed).
    pub fn of_level(raw: &'a [u8]) -> Result<Self> {
        let t = trim(raw);
        if t.first() != Some(&DLE) {
            return Err(Error::other("unit is not a nested level"));
        }
        let stop = scan::level(t, 0, t.len())?;
        Self::parse(t, 1, stop - 1, true)
    }

    fn parse(buf: &'a [u8], start: usize, stop: usize, level: bool) -> Result<Self> {
        let mut parser = Parser::new(buf, false);
        let (files, pos) = parser.body(start, stop, level)?;
        let pos = scan::layout(buf, pos, stop);
        if pos < stop {
            return Err(Error::syntax("unexpected byte in body", pos));
        }
        Ok(Body {
            buf,
            start,
            stop,
            files,
        })
    }

    /// A body already parsed (used by the parser).
    pub(crate) fn from_parts(
        buf: &'a [u8],
        start: usize,
        stop: usize,
        files: Vec<FileView<'a>>,
    ) -> Self {
        Body {
            buf,
            start,
            stop,
            files,
        }
    }

    /// The buffer the body indexes.
    pub fn buf(&self) -> &'a [u8] {
        self.buf
    }

    /// Offset of the body's first byte in the buffer.
    pub fn start(&self) -> usize {
        self.start
    }

    /// Offset just past the body in the buffer.
    pub fn stop(&self) -> usize {
        self.stop
    }

    /// The body's bytes.
    pub fn raw(&self) -> &'a [u8] {
        &self.buf[self.start..self.stop]
    }

    /// The files of the body, in order. There is always at least one (the
    /// unnamed file, when the body has no FS).
    pub fn files(&self) -> &[FileView<'a>] {
        &self.files
    }

    /// All groups of all files, in order.
    pub fn groups(&self) -> impl Iterator<Item = &Group<'a>> + '_ {
        self.files.iter().flat_map(|f| f.groups.iter())
    }

    /// Groups of the first file.
    pub fn group_count(&self) -> usize {
        self.first_file().groups.len()
    }

    /// Group `i` of the first file.
    pub fn group(&self, i: usize) -> &Group<'a> {
        &self.first_file().groups[i]
    }

    /// The first file's group named `name`.
    pub fn group_by_name(&self, name: &str) -> Result<&Group<'a>> {
        self.first_file().group(name)
    }

    /// Names of the first file's groups.
    pub fn group_names(&self) -> Vec<&'a [u8]> {
        self.first_file().groups.iter().map(|g| g.name()).collect()
    }

    /// Name of the first file (empty when the body has no FS).
    pub fn name(&self) -> &'a [u8] {
        self.first_file().name()
    }

    /// Records of the body when it is a bare record stream (no FS, no
    /// GS): the unnamed group's records.
    pub fn records(&self) -> &[Record<'a>] {
        match self.first_file().groups.first() {
            Some(g) => &g.records,
            None => &[],
        }
    }

    pub fn record_count(&self) -> usize {
        self.records().len()
    }

    pub fn record(&self, i: usize) -> Record<'a> {
        self.records()[i]
    }

    /// Whether the body is just records: one unnamed file, at most one
    /// unnamed group.
    pub fn bare(&self) -> bool {
        self.files.len() == 1
            && self.files[0].leading
            && self.files[0].groups.len() <= 1
            && self.files[0].groups.first().is_none_or(|g| g.depth == 0)
    }

    fn first_file(&self) -> &FileView<'a> {
        &self.files[0]
    }
}

/// A file within a body: its name and groups. The leading file is the
/// unnamed one before the first FS.
pub struct FileView<'a> {
    buf: &'a [u8],
    name_start: usize,
    name_end: usize,
    pub(crate) groups: Vec<Group<'a>>,
    pub(crate) leading: bool,
}

impl<'a> FileView<'a> {
    pub(crate) fn new(buf: &'a [u8], name_start: usize, name_end: usize, leading: bool) -> Self {
        FileView {
            buf,
            name_start,
            name_end,
            groups: Vec::new(),
            leading,
        }
    }

    /// The file's name (empty for the unnamed file).
    pub fn name(&self) -> &'a [u8] {
        trim(&self.buf[self.name_start..self.name_end])
    }

    /// Whether this is the unnamed file before the first FS.
    pub fn is_leading(&self) -> bool {
        self.leading
    }

    /// The file's groups, in order.
    pub fn groups(&self) -> &[Group<'a>] {
        &self.groups
    }

    /// The group named `name`.
    pub fn group(&self, name: &str) -> Result<&Group<'a>> {
        self.groups
            .iter()
            .find(|g| g.name() == name.as_bytes())
            .ok_or_else(|| Error::Key(format!("No group named '{name}'")))
    }
}

/// A group: its depth (one plus its SUB prefix; 0 for the unnamed leading
/// group), name, optional header row and records.
pub struct Group<'a> {
    buf: &'a [u8],
    pub(crate) depth: usize,
    name_start: usize,
    name_end: usize,
    pub(crate) header: Option<Record<'a>>,
    pub(crate) records: Vec<Record<'a>>,
}

/// A group read as a table is itself.
pub type Table<'a> = Group<'a>;

impl<'a> Group<'a> {
    pub(crate) fn new(buf: &'a [u8], depth: usize, name_start: usize, name_end: usize) -> Self {
        Group {
            buf,
            depth,
            name_start,
            name_end,
            header: None,
            records: Vec::new(),
        }
    }

    /// The group's name (empty for the unnamed leading group).
    pub fn name(&self) -> &'a [u8] {
        trim(&self.buf[self.name_start..self.name_end])
    }

    /// Depth: 1 for `␝g`, 2 for `␚␝g` (one plus the SUB prefix); 0 for the
    /// unnamed group.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Whether the group has a header row.
    pub fn has_header(&self) -> bool {
        self.header.is_some()
    }

    /// The header row as a record, if any.
    pub fn header_row(&self) -> Option<Record<'a>> {
        self.header
    }

    /// Header names, or an empty vector.
    pub fn headers(&self) -> Vec<&'a [u8]> {
        match &self.header {
            Some(h) => h.fields().into_iter().map(name_value).collect(),
            None => Vec::new(),
        }
    }

    pub fn header_count(&self) -> usize {
        self.header.map_or(0, |h| h.field_count())
    }

    pub fn header(&self, i: usize) -> &'a [u8] {
        self.headers()[i]
    }

    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    pub fn record(&self, i: usize) -> Record<'a> {
        self.records[i]
    }

    /// The records, in order.
    pub fn records(&self) -> &[Record<'a>] {
        &self.records
    }

    /// Index of a header name, or `None`.
    pub fn column(&self, name: &str) -> Option<usize> {
        self.headers().iter().position(|h| *h == name.as_bytes())
    }
}

// The logical value of a unit that holds a name (plain, never text, with
// notes allowed after it), borrowed from the buffer.
fn name_value(raw: &[u8]) -> &[u8] {
    match value(raw) {
        Ok(Cow::Borrowed(b)) => b,
        _ => trim(raw),
    }
}

/// A key and its logical value, from a keyed record.
pub type Pair<'a> = (&'a [u8], Cow<'a, [u8]>);

/// One unit as written: its raw value bytes and its sub-units (`␞a␟b␚␟c`:
/// `b` holds the sub-unit `c`; DESIGN.md, Subordinates), each a `Unit`
/// with sub-units of its own (`␞a␚␟b␚␚␟c`: `a` holds `b` holding `c`).
#[derive(Clone, PartialEq, Eq)]
pub struct Unit<'a> {
    raw: &'a [u8],
    subs: Vec<Unit<'a>>,
}

impl<'a> Unit<'a> {
    /// Raw bytes of the unit's value (layout at its edges included, its
    /// sub-units not).
    pub fn raw(&self) -> &'a [u8] {
        self.raw
    }

    /// The sub-units, in written order; empty for a unit without any.
    pub fn subs(&self) -> &[Unit<'a>] {
        &self.subs
    }

    /// The logical value.
    pub fn value(&self) -> Result<Cow<'a, [u8]>> {
        value(self.raw)
    }
}

impl fmt::Debug for Unit<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Unit")
            .field("raw", &String::from_utf8_lossy(self.raw))
            .field("subs", &self.subs)
            .finish()
    }
}

// (depth, value start, value end) of a sub-unit as written.
type SubSpan = (usize, usize, usize);

/// A record: positional or keyed. Copyable, zero-copy accessors. A
/// sub-unit is not a unit of the record: `fields`, `values` and `pairs`
/// list the units; [`Record::unit`] and [`Record::get_unit`] give a unit
/// with its sub-units.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Record<'a> {
    buf: &'a [u8],
    start: usize,
    end: usize,
}

impl fmt::Debug for Record<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Record")
            .field("raw", &String::from_utf8_lossy(self.raw()))
            .finish()
    }
}

impl<'a> Record<'a> {
    pub(crate) fn new(buf: &'a [u8], start: usize, end: usize) -> Self {
        Record { buf, start, end }
    }

    /// Raw bytes of the record (after its RS).
    #[inline]
    pub fn raw(&self) -> &'a [u8] {
        &self.buf[self.start..self.end]
    }

    /// Offset of the record's first byte in the buffer.
    pub fn offset(&self) -> usize {
        self.start
    }

    /// Whether the record is keyed (its first unit, live or dead, is an
    /// SOH).
    pub fn keyed(&self) -> bool {
        let buf = self.buf;
        let end = self.end;
        let mut p = scan::layout(buf, self.start, end);
        while p < end && buf[p] == BEL {
            let p2 = scan::layout(buf, p + 1, end);
            if p2 < end && buf[p2] == SOH {
                return true;
            }
            if p2 >= end || (buf[p2] < 0x20 && buf[p2] != STX) {
                break;
            }
            match scan::comment(buf, p, end) {
                Ok(n) => p = scan::layout(buf, n, end),
                Err(_) => return false,
            }
        }
        p < end && buf[p] == SOH
    }

    // ---- positional -----------------------------------------------------

    /// Raw slice of unit `n` (layout at its edges included, its sub-units
    /// not); empty when there is no such unit.
    pub fn field(&self, n: usize) -> &'a [u8] {
        self.unit_ranges()
            .get(n)
            .map_or(&[][..], |&(s, e)| &self.buf[s..e])
    }

    /// Number of positional units (0 for a keyed record). A sub-unit is
    /// not counted.
    pub fn field_count(&self) -> usize {
        self.unit_ranges().len()
    }

    /// Raw slices of every unit.
    pub fn fields(&self) -> Vec<&'a [u8]> {
        self.unit_ranges()
            .into_iter()
            .map(|(s, e)| &self.buf[s..e])
            .collect()
    }

    /// Every positional unit with its sub-units.
    pub fn units(&self) -> Vec<Unit<'a>> {
        self.unit_spans()
            .into_iter()
            .map(|(s, e, subs)| Unit {
                raw: &self.buf[s..e],
                subs: sub_tree(self.buf, &subs),
            })
            .collect()
    }

    /// Positional unit `n` with its sub-units, or `None` when there is no
    /// such unit.
    pub fn unit(&self, n: usize) -> Option<Unit<'a>> {
        self.unit_spans()
            .into_iter()
            .nth(n)
            .map(|(s, e, subs)| Unit {
                raw: &self.buf[s..e],
                subs: sub_tree(self.buf, &subs),
            })
    }

    /// Logical value of unit `n`: text decoded, layout trimmed.
    pub fn value(&self, n: usize) -> Result<Cow<'a, [u8]>> {
        value(self.field(n))
    }

    /// Logical values of every unit.
    pub fn values(&self) -> Result<Vec<Cow<'a, [u8]>>> {
        self.fields().into_iter().map(value).collect()
    }

    /// Unit `n` as a nested level.
    pub fn level(&self, n: usize) -> Result<Body<'a>> {
        Body::of_level(self.field(n))
    }

    /// Unit `n` as a flat list: a level holding exactly one positional
    /// record (an empty level is the empty list).
    pub fn list(&self, n: usize) -> Result<Vec<Cow<'a, [u8]>>> {
        let body = self.level(n)?;
        let recs = body.records();
        if recs.is_empty() && body.bare() {
            return Ok(Vec::new());
        }
        if !(recs.len() == 1 && body.bare()) {
            return Err(Error::other(format!(
                "level is not a list ({} records)",
                recs.len()
            )));
        }
        if body.group_count() > 0 && body.group(0).has_header() {
            return Err(Error::other("level is not a list (header row)"));
        }
        if recs[0].keyed() {
            return Err(Error::other("level is not a list (keyed record)"));
        }
        recs[0].values()
    }

    /// Unit `n` as a reference.
    pub fn reference(&self, n: usize) -> Result<Reference<'a>> {
        Reference::parse(self.field(n))
    }

    // ---- keyed ---------------------------------------------------------

    /// Keys in written order (duplicates included).
    pub fn keys(&self) -> Vec<&'a [u8]> {
        self.keyed_ranges()
            .unwrap_or_default()
            .into_iter()
            .map(|(k, _, _, _)| k)
            .collect()
    }

    /// (key, logical value) pairs in written order.
    pub fn pairs(&self) -> Result<Vec<Pair<'a>>> {
        self.keyed_ranges()?
            .into_iter()
            .map(|(k, vs, ve, _)| Ok((k, value(&self.buf[vs..ve])?)))
            .collect()
    }

    /// Raw unit for a key (last wins), or `None`.
    pub fn get_raw(&self, key: &str) -> Option<&'a [u8]> {
        let mut found = None;
        for (k, vs, ve, _) in self.keyed_ranges().unwrap_or_default() {
            if k == key.as_bytes() {
                found = Some(&self.buf[vs..ve]);
            }
        }
        found
    }

    /// The unit under a key with its sub-units (last wins), or `None`.
    pub fn get_unit(&self, key: &str) -> Option<Unit<'a>> {
        let mut found = None;
        for (k, vs, ve, subs) in self.keyed_ranges().unwrap_or_default() {
            if k == key.as_bytes() {
                found = Some(Unit {
                    raw: &self.buf[vs..ve],
                    subs: sub_tree(self.buf, &subs),
                });
            }
        }
        found
    }

    /// Logical value for a key (last wins), or `None`.
    pub fn get(&self, key: &str) -> Result<Option<Cow<'a, [u8]>>> {
        self.get_raw(key).map(value).transpose()
    }

    /// The value for a key as a nested level, or `None`.
    pub fn get_level(&self, key: &str) -> Result<Option<Body<'a>>> {
        self.get_raw(key).map(Body::of_level).transpose()
    }

    pub fn has_key(&self, key: &str) -> bool {
        self.get_raw(key).is_some()
    }

    // ---- internals ------------------------------------------------------

    // Skip the sub-units after a value ending at `pos`, collecting their
    // (depth, start, end) spans. Returns the position after them: the US
    // of the next unit, a SUB prefixing the next line, or the end.
    fn sub_spans(&self, mut pos: usize, spans: &mut Vec<SubSpan>) -> usize {
        let buf = self.buf;
        let end = self.end;
        loop {
            if !(pos < end && buf[pos] == SUB) {
                return pos;
            }
            let mut q = pos;
            let mut run = 0usize;
            while q < end && buf[q] == SUB {
                run += 1;
                q = scan::layout(buf, q + 1, end);
            }
            if !(q < end && buf[q] == US) {
                return pos;
            }
            let ss = q + 1;
            let se = scan::unit(buf, ss, end).unwrap_or(end);
            spans.push((run, ss, se));
            pos = se;
        }
    }

    // (start, end, sub-unit spans) of each positional unit. A record comes
    // from a parsed body, whose text and levels were already checked, so
    // the unit scan cannot fail; should it, the unit runs to the end.
    fn unit_spans(&self) -> Vec<(usize, usize, Vec<SubSpan>)> {
        let buf = self.buf;
        let end = self.end;
        let mut out = Vec::new();
        if self.keyed() {
            return out;
        }
        let mut pos = scan::layout(buf, self.start, end);
        let mut unit_start = self.start;
        loop {
            pos = scan::unit(buf, pos, end).unwrap_or(end);
            let mut spans = Vec::new();
            let next = self.sub_spans(pos, &mut spans);
            out.push((unit_start, pos, spans));
            if !(next < end && buf[next] == US) {
                break;
            }
            pos = next + 1;
            unit_start = pos;
        }
        out
    }

    // (start, end) of each positional unit.
    fn unit_ranges(&self) -> Vec<(usize, usize)> {
        self.unit_spans()
            .into_iter()
            .map(|(s, e, _)| (s, e))
            .collect()
    }

    // (key, value start, value end, sub-unit spans) per keyed unit.
    #[allow(clippy::type_complexity)]
    fn keyed_ranges(&self) -> Result<Vec<(&'a [u8], usize, usize, Vec<SubSpan>)>> {
        let buf = self.buf;
        let end = self.end;
        let mut out = Vec::new();
        let mut pos = scan::layout(buf, self.start, end);
        while pos < end {
            let b = buf[pos];
            if b == SOH {
                let ks = scan::layout(buf, pos + 1, end);
                let ke = scan::plain(buf, ks, end);
                if !(ke < end && buf[ke] == US) {
                    return Err(Error::syntax("key without a value (no US)", ke));
                }
                let vs = ke + 1;
                let ve = scan::unit(buf, vs, end)?;
                let mut spans = Vec::new();
                pos = self.sub_spans(ve, &mut spans);
                out.push((trim(&buf[ks..ke]), vs, ve, spans));
            } else if b == BEL {
                pos = scan::comment(buf, pos, end)?;
            } else if is_layout(b) {
                pos += 1;
            } else {
                break;
            }
        }
        Ok(out)
    }
}

// Build the sub-unit tree from (depth, start, end) spans in written order:
// the spans after one that are deeper than it are its descendants. The
// parser has refused skipped depths.
fn sub_tree<'a>(buf: &'a [u8], spans: &[SubSpan]) -> Vec<Unit<'a>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < spans.len() {
        let (d, s, e) = spans[i];
        let mut j = i + 1;
        while j < spans.len() && spans[j].0 > d {
            j += 1;
        }
        out.push(Unit {
            raw: &buf[s..e],
            subs: sub_tree(buf, &spans[i + 1..j]),
        });
        i = j;
    }
    out
}

/// A reference (DESIGN.md, References): a group name written plain, or a
/// pattern, a level that is one branch of the target with its values left
/// out. The pattern is reported as the level it is written as.
pub struct Reference<'a> {
    name: Option<&'a [u8]>,
    pattern: Option<Body<'a>>,
}

impl fmt::Debug for Reference<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path: Vec<String> = self
            .path()
            .iter()
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        f.debug_struct("Reference")
            .field("group", &self.name.is_some())
            .field("path", &path)
            .finish()
    }
}

impl<'a> Reference<'a> {
    pub(crate) fn new(name: Option<&'a [u8]>, pattern: Option<Body<'a>>) -> Self {
        Reference { name, pattern }
    }

    /// Parse a raw unit holding a reference (notes after it allowed).
    pub fn parse(raw: &'a [u8]) -> Result<Self> {
        let t = trim(raw);
        if t.first() != Some(&ENQ) {
            return Err(Error::other("unit is not a reference"));
        }
        let mut parser = Parser::new(t, false);
        let (r, pos) = parser.reference(0, t.len())?;
        let mut pos = scan::layout(t, pos, t.len());
        while pos < t.len() {
            if t[pos] != BEL {
                return Err(Error::syntax("unexpected byte after a reference", pos));
            }
            pos = scan::layout(t, scan::comment(t, pos, t.len())?, t.len());
        }
        Ok(r)
    }

    /// The group name of a plain reference, or `None` for a pattern.
    pub fn name(&self) -> Option<&'a [u8]> {
        self.name
    }

    /// The pattern of `ENQ DLE … CAN`, or `None` for a plain reference.
    pub fn pattern(&self) -> Option<&Body<'a>> {
        self.pattern.as_ref()
    }

    /// Whether the reference names a group as a whole: a plain name, or a
    /// pattern that is a bare group line.
    pub fn is_group(&self) -> bool {
        if self.name.is_some() {
            return true;
        }
        let p = self
            .pattern
            .as_ref()
            .expect("a reference is a name or a pattern");
        let groups: Vec<&Group<'a>> = p.groups().collect();
        p.files()[0].is_leading() && groups.len() == 1 && groups[0].records().is_empty()
    }

    /// The group named, when the reference names a group as a whole.
    pub fn group_name(&self) -> Option<&'a [u8]> {
        if let Some(n) = self.name {
            return Some(n);
        }
        if self.is_group() {
            self.pattern
                .as_ref()
                .and_then(|p| p.groups().next().map(|g| g.name()))
        } else {
            None
        }
    }

    /// The steps of the pattern in walking order (file, groups, record id,
    /// field name, then the inner pattern's steps), or the group name
    /// alone. Informal: a keyed step contributes its key.
    pub fn path(&self) -> Vec<&'a [u8]> {
        if let Some(n) = self.name {
            return vec![n];
        }
        let mut out = Vec::new();
        let Some(p) = self.pattern.as_ref() else {
            return out;
        };
        let f = &p.files()[0];
        if !f.is_leading() {
            out.push(f.name());
        }
        for g in f.groups() {
            if g.depth() > 0 {
                out.push(g.name());
            }
        }
        let Some(rec) = f.groups().last().and_then(|g| g.records().first()) else {
            return out;
        };
        if rec.keyed() {
            if let Some(k) = rec.keys().first() {
                out.push(k);
            }
        } else {
            for (i, raw) in rec.fields().into_iter().enumerate() {
                if i < 2 {
                    out.push(name_value(raw));
                } else if let Ok(inner) = Body::of_level(raw) {
                    out.extend(Reference::new(None, Some(inner)).path());
                }
            }
        }
        out
    }
}
