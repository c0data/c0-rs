//! Zero-copy index of a document body: files, groups, header rows and
//! records, each a slice into the original buffer.

use crate::*;
use std::borrow::Cow;
use std::fmt;

/// A document body: the bytes between a heading and EOT, or the inside of
/// a nested level. Indexes files, groups, header rows and records once;
/// everything it returns borrows the original buffer.
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
    /// Index a whole buffer as a body (no SYN, no EOT).
    pub fn new(buf: &'a [u8]) -> Result<Self> {
        Self::slice(buf, 0, buf.len())
    }

    /// Index `buf[start..stop]` as a body.
    pub fn slice(buf: &'a [u8], start: usize, stop: usize) -> Result<Self> {
        let mut body = Body {
            buf,
            start,
            stop,
            files: Vec::new(),
        };
        body.index()?;
        Ok(body)
    }

    /// The contents of a nested level: `raw` is a unit holding `DLE … CAN`
    /// (layout at its edges allowed).
    pub fn of_level(raw: &'a [u8]) -> Result<Self> {
        let t = trim(raw);
        if t.first() != Some(&DLE) {
            return Err(Error::other("unit is not a nested level"));
        }
        let stop = scan::level(t, 0, t.len())?;
        Self::slice(t, 1, stop - 1)
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
            && self.files[0].name().is_empty()
            && self.files[0].groups.len() <= 1
            && self.files[0].groups.first().is_none_or(|g| g.depth == 0)
    }

    fn first_file(&self) -> &FileView<'a> {
        &self.files[0]
    }

    // ------------------------------------------------------------------

    fn index(&mut self) -> Result<()> {
        let buf = self.buf;
        let stop = self.stop;
        let mut pos = scan::layout(buf, self.start, stop);
        // leading unnamed file
        let mut file = FileView::new(buf, 0, 0);
        pos = index_part(buf, pos, stop, &mut file, false)?;
        let fs_follows = pos < stop && (buf[pos] == FS || dead_fs(buf, pos, stop));
        let mut unnamed = Some(file);
        if !(unnamed.as_ref().is_some_and(|f| f.groups.is_empty()) && fs_follows) {
            self.files.extend(unnamed.take());
        }
        while pos < stop {
            let mut b = buf[pos];
            let mut dead = false;
            if b == BEL && dead_fs(buf, pos, stop) {
                dead = true;
                pos = scan::layout(buf, pos + 1, stop);
                b = FS;
            }
            if b != FS {
                break;
            }
            pos = scan::layout(buf, pos + 1, stop);
            let ns = pos;
            pos = scan::plain(buf, pos, stop);
            if dead {
                // a dead file line is read as if absent: what follows belongs
                // to the file before it, its leading items to that file's
                // last group
                if self.files.is_empty() {
                    self.files.extend(unnamed.take());
                }
                let i = self.files.len() - 1;
                pos = index_part(buf, pos, stop, &mut self.files[i], true)?;
            } else {
                let mut f = FileView::new(buf, ns, pos);
                pos = index_part(buf, pos, stop, &mut f, false)?;
                self.files.push(f);
            }
        }
        if self.files.is_empty() {
            self.files.push(FileView::new(buf, 0, 0));
        }
        Ok(())
    }
}

fn dead_fs(buf: &[u8], pos: usize, stop: usize) -> bool {
    if !(pos < stop && buf[pos] == BEL) {
        return false;
    }
    let p2 = scan::layout(buf, pos + 1, stop);
    p2 < stop && buf[p2] == FS
}

fn has_content(g: &Group<'_>) -> bool {
    g.header.is_some() || !g.records.is_empty()
}

// Index one part: leading items (the unnamed group, or the file's last
// group when continuing after a dead file line), then groups. Returns the
// position of the next FS (or dead FS), or stop.
fn index_part<'a>(
    buf: &'a [u8],
    mut pos: usize,
    stop: usize,
    file: &mut FileView<'a>,
    cont: bool,
) -> Result<usize> {
    // the group a dead group line's contents attach to: an index into
    // file.groups, or the still-pending unnamed group
    let mut current: Option<usize> = None;
    let mut pending: Option<Group<'a>> = None;
    if cont && !file.groups.is_empty() {
        let i = file.groups.len() - 1;
        pos = index_group_contents(buf, pos, stop, &mut file.groups[i])?;
        current = Some(i);
    } else {
        let mut g = Group::new(buf, 0, 0, 0);
        pos = index_group_contents(buf, pos, stop, &mut g)?;
        if has_content(&g) {
            file.groups.push(g);
            current = Some(file.groups.len() - 1);
        } else {
            pending = Some(g);
        }
    }
    while pos < stop {
        let mut b = buf[pos];
        let mut dead = false;
        if b == BEL {
            let p2 = scan::layout(buf, pos + 1, stop);
            if p2 < stop && buf[p2] == GS {
                dead = true;
                pos = p2;
                b = GS;
            } else {
                break;
            }
        }
        if b != GS {
            break;
        }
        let mut depth = 0;
        while pos < stop && (buf[pos] == GS || is_layout(buf[pos])) {
            if buf[pos] == GS {
                depth += 1;
            }
            pos += 1;
        }
        let ns = pos;
        pos = scan::plain(buf, pos, stop);
        let ne = pos;
        if dead {
            // a dead group line: what follows still belongs to the group before
            match current {
                Some(i) => pos = index_group_contents(buf, pos, stop, &mut file.groups[i])?,
                None => {
                    if let Some(mut g) = pending.take() {
                        pos = index_group_contents(buf, pos, stop, &mut g)?;
                        if has_content(&g) {
                            file.groups.push(g);
                            current = Some(file.groups.len() - 1);
                        } else {
                            pending = Some(g);
                        }
                    }
                }
            }
        } else {
            let mut grp = Group::new(buf, depth, ns, ne);
            pos = index_group_contents(buf, pos, stop, &mut grp)?;
            file.groups.push(grp);
            current = Some(file.groups.len() - 1);
        }
    }
    Ok(pos)
}

// After a group label (or at the start of a part): notes, header row,
// items. Returns the position of the next GS/FS/dead GS/FS or stop.
fn index_group_contents<'a>(
    buf: &'a [u8],
    mut pos: usize,
    stop: usize,
    grp: &mut Group<'a>,
) -> Result<usize> {
    let mut first_item = true;
    loop {
        pos = scan::layout(buf, pos, stop);
        if pos >= stop {
            break;
        }
        let b = buf[pos];
        if b == RS {
            first_item = false;
            let rs = pos + 1;
            pos = scan::record_end(buf, rs, stop)?;
            grp.records
                .push(Record::new(buf, rs, trim_end(buf, rs, pos)));
        } else if b == SOH {
            let p2 = scan::layout(buf, pos + 1, stop);
            if !(p2 < stop && buf[p2] == RS) {
                return Err(Error::syntax("keyed unit outside a record", pos));
            }
            if !first_item {
                return Err(Error::syntax(
                    "header row not at the start of its group",
                    pos,
                ));
            }
            first_item = false;
            let hs = p2 + 1;
            pos = scan::record_end(buf, hs, stop)?;
            grp.header = Some(Record::new(buf, hs, trim_end(buf, hs, pos)));
        } else if b == ETB {
            pos = scan::plain(buf, scan::layout(buf, pos + 1, stop), stop);
        } else if b == BEL {
            let p2 = scan::layout(buf, pos + 1, stop);
            if p2 >= stop || buf[p2] >= 0x20 || buf[p2] == STX {
                pos = scan::comment(buf, pos, stop)?; // a note
            } else if buf[p2] == RS || buf[p2] == SOH {
                pos = scan::comment(buf, pos, stop)?; // dead record / header row
                if buf[p2] == RS {
                    first_item = false;
                }
            } else {
                break; // dead GS or FS: the caller's business
            }
        } else {
            break;
        }
    }
    Ok(pos)
}

#[inline]
fn trim_end(buf: &[u8], start: usize, mut stop: usize) -> usize {
    while stop > start && is_layout(buf[stop - 1]) {
        stop -= 1;
    }
    stop
}

/// A file within a body: its name and groups.
pub struct FileView<'a> {
    buf: &'a [u8],
    name_start: usize,
    name_end: usize,
    groups: Vec<Group<'a>>,
}

impl<'a> FileView<'a> {
    fn new(buf: &'a [u8], name_start: usize, name_end: usize) -> Self {
        FileView {
            buf,
            name_start,
            name_end,
            groups: Vec::new(),
        }
    }

    /// The file's name (empty for the unnamed file).
    pub fn name(&self) -> &'a [u8] {
        trim(&self.buf[self.name_start..self.name_end])
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

/// A group: its depth (GS count; 0 for the unnamed leading group), name,
/// optional header row and records.
pub struct Group<'a> {
    buf: &'a [u8],
    depth: usize,
    name_start: usize,
    name_end: usize,
    header: Option<Record<'a>>,
    records: Vec<Record<'a>>,
}

/// A group read as a table is itself.
pub type Table<'a> = Group<'a>;

impl<'a> Group<'a> {
    fn new(buf: &'a [u8], depth: usize, name_start: usize, name_end: usize) -> Self {
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

    /// GS repetitions: 1 for `␝g`, 2 for `␝␝g`; 0 for the unnamed group.
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
            Some(h) => h.fields().into_iter().map(trim).collect(),
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

/// A key and its logical value, from a keyed record.
pub type Pair<'a> = (&'a [u8], Cow<'a, [u8]>);

/// A record: positional or keyed. Copyable, zero-copy accessors.
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

    /// Whether the record is keyed (its first live or dead unit is an SOH).
    pub fn keyed(&self) -> bool {
        let buf = self.buf;
        let end = self.end;
        let mut p = scan::layout(buf, self.start, end);
        while p < end && buf[p] == BEL {
            let p2 = scan::layout(buf, p + 1, end);
            if p2 < end && buf[p2] == SOH {
                return true;
            }
            if p2 < end && buf[p2] < 0x20 && buf[p2] != STX && buf[p2] != DLE {
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

    /// Raw slice of unit `n` (layout at its edges included); empty when
    /// there is no such unit.
    pub fn field(&self, n: usize) -> &'a [u8] {
        self.unit_ranges()
            .get(n)
            .map_or(&[][..], |&(s, e)| &self.buf[s..e])
    }

    /// Number of positional units (0 for a keyed record).
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
            .map(|(k, _, _)| k)
            .collect()
    }

    /// (key, logical value) pairs in written order.
    pub fn pairs(&self) -> Result<Vec<Pair<'a>>> {
        self.keyed_ranges()?
            .into_iter()
            .map(|(k, vs, ve)| Ok((k, value(&self.buf[vs..ve])?)))
            .collect()
    }

    /// Raw unit for a key (last wins), or `None`.
    pub fn get_raw(&self, key: &str) -> Option<&'a [u8]> {
        let mut found = None;
        for (k, vs, ve) in self.keyed_ranges().unwrap_or_default() {
            if k == key.as_bytes() {
                found = Some(&self.buf[vs..ve]);
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

    // (start, end) of each positional unit. A record comes from an indexed
    // body, whose text and levels were already scanned, so the unit scan
    // cannot fail; should it, the unit runs to the record's end.
    fn unit_ranges(&self) -> Vec<(usize, usize)> {
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
            out.push((unit_start, pos));
            if !(pos < end && buf[pos] == US) {
                break;
            }
            pos += 1;
            unit_start = pos;
        }
        out
    }

    // (key, value start, value end) per live keyed unit.
    fn keyed_ranges(&self) -> Result<Vec<(&'a [u8], usize, usize)>> {
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
                out.push((trim(&buf[ks..ke]), vs, ve));
                pos = ve;
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

/// A reference: a group name, or a path of two or three segments
/// (group, record id, field name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference<'a> {
    path: Vec<Cow<'a, [u8]>>,
}

impl<'a> Reference<'a> {
    /// Parse a raw unit holding `ENQ name` or `ENQ STX a US b [US c] ETX`.
    pub fn parse(raw: &'a [u8]) -> Result<Self> {
        let t = trim(raw);
        if t.first() != Some(&ENQ) {
            return Err(Error::other("unit is not a reference"));
        }
        let p = scan::layout(t, 1, t.len());
        if p < t.len() && t[p] == STX {
            let stop = scan::text(t, p, t.len())?;
            let inside = decode_text(&t[p + 1..stop - 1]);
            let segs: Vec<Cow<'a, [u8]>> = match inside {
                Cow::Borrowed(s) => s.split(|&b| b == US).map(Cow::Borrowed).collect(),
                Cow::Owned(v) => v
                    .split(|&b| b == US)
                    .map(|s| Cow::Owned(s.to_vec()))
                    .collect(),
            };
            if segs.len() > 3 {
                return Err(Error::syntax(
                    "reference path with more than three segments",
                    p,
                ));
            }
            if segs.iter().any(|s| s.is_empty()) {
                return Err(Error::syntax("reference path with an empty segment", p));
            }
            Ok(Reference { path: segs })
        } else {
            let name = trim(&t[p..]);
            if name.is_empty() {
                return Err(Error::syntax("reference without a name", p));
            }
            Ok(Reference {
                path: vec![Cow::Borrowed(name)],
            })
        }
    }

    /// The segments: group, then record id and field name when present.
    pub fn path(&self) -> &[Cow<'a, [u8]>] {
        &self.path
    }

    pub fn group(&self) -> &[u8] {
        &self.path[0]
    }

    pub fn record(&self) -> Option<&[u8]> {
        self.path.get(1).map(|s| s.as_ref())
    }

    pub fn field(&self) -> Option<&[u8]> {
        self.path.get(2).map(|s| s.as_ref())
    }

    /// Whether the reference names a whole group.
    pub fn is_group(&self) -> bool {
        self.path.len() == 1
    }
}
