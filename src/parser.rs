//! The one parser of the base grammar (GRAMMAR.md). It walks a buffer,
//! fails on the first structural error, builds the document model with
//! supersession applied (the superseded element dropped), notes every
//! canonical violation when asked, and counts what tools above the format
//! need to know: dead elements met in bodies and the grain of every
//! supersession. [`Body`], [`Document`], [`Container`], [`Validator`] and
//! the stream reader are all built on it.
//!
//! Body level is a flat state machine over lines (file line, group line,
//! header row, record, note, commit); records and their values are
//! recursive, since a level holds a body.

use crate::*;
use std::borrow::Cow;
use std::collections::HashMap;

pub struct Parser<'a> {
    buf: &'a [u8],
    canonical: bool,
    ok: bool,
    warnings: Vec<&'static str>,
    dead_elements: usize,
    supersessions: Vec<u8>,
    // Regions to jump over at a block boundary, start => end: the damaged
    // blocks of a stream (see StreamReader).
    skip: Option<HashMap<usize, usize>>,
}

impl<'a> Parser<'a> {
    /// A parser over `buf`. With `canonical` it also records every
    /// canonical violation (see [`Parser::warnings`]).
    pub fn new(buf: &'a [u8], canonical: bool) -> Self {
        Parser {
            buf,
            canonical,
            ok: true,
            warnings: Vec::new(),
            dead_elements: 0,
            supersessions: Vec::new(),
            skip: None,
        }
    }

    /// A parser that jumps over the given regions (`start => end`).
    pub(crate) fn with_skip(buf: &'a [u8], skip: HashMap<usize, usize>) -> Self {
        let mut p = Parser::new(buf, false);
        p.skip = Some(skip);
        p
    }

    /// Whether nothing non-canonical has been met.
    pub fn ok(&self) -> bool {
        self.ok
    }

    /// Canonical violations met, one entry per finding.
    pub fn warnings(&self) -> &[&'static str] {
        &self.warnings
    }

    /// Dead elements met in bodies (the heading's dead keys are not counted).
    pub fn dead_elements(&self) -> usize {
        self.dead_elements
    }

    /// The code after each SUB met: US, SOH, RS, GS or FS.
    pub fn supersessions(&self) -> &[u8] {
        &self.supersessions
    }

    pub(crate) fn not_canonical(&mut self, why: &'static str) {
        self.ok = false;
        self.warnings.push(why);
    }

    // Jump over skipped regions beginning at pos.
    #[inline]
    fn skip(&self, mut pos: usize) -> usize {
        if let Some(s) = &self.skip {
            while let Some(&e) = s.get(&pos) {
                pos = e;
            }
        }
        pos
    }

    /// Skip layout (and skipped regions). Layout is a canonical violation.
    pub(crate) fn layout(&mut self, pos: usize, stop: usize) -> usize {
        let pos = self.skip(pos);
        let p2 = scan::layout(self.buf, pos, stop);
        if p2 != pos {
            self.not_canonical("layout bytes");
        }
        self.skip(p2)
    }

    // ---- heading --------------------------------------------------------

    /// After SYN. Returns the heading (superseded keys dropped, duplicates
    /// kept) and the position of the first body code.
    pub fn heading(&mut self, mut pos: usize, stop: usize) -> Result<(Heading<'a>, usize)> {
        let buf = self.buf;
        let hs = pos;
        let mut pairs: Vec<(&'a [u8], Cow<'a, [u8]>)> = Vec::new();
        let mut raw_pairs: Vec<(&'a [u8], &'a [u8])> = Vec::new();
        loop {
            pos = self.layout(pos, stop);
            if pos >= stop {
                break;
            }
            let b = buf[pos];
            if b == SOH {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 < stop && buf[p2] == RS {
                    break; // a header row begins the body
                }
                let (key, vs, p) = self.keyed_unit(pos + 1, stop, true)?;
                pos = p;
                self.heading_pair(&mut pairs, &mut raw_pairs, key, trim(&buf[vs..pos]), vs)?;
            } else if b == BEL {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 >= stop {
                    return Err(Error::end("BEL at end of input", p2));
                }
                let c = buf[p2];
                if c == SOH {
                    let p3 = scan::layout(buf, p2 + 1, stop);
                    if p3 < stop && buf[p3] == RS {
                        break; // a dead header row begins the body
                    }
                    self.not_canonical("dead element");
                    let (_key, _vs, p) = self.keyed_unit(p2 + 1, stop, true)?;
                    pos = p;
                } else if c >= 0x20 || c == STX {
                    self.not_canonical("comment");
                    pos = self.note_content(p2, stop)?;
                } else if matches!(c, RS | GS | FS | DLE) {
                    break; // a dead body element
                } else {
                    return Err(Error::syntax(
                        "BEL before a code that is not an element",
                        p2,
                    ));
                }
            } else if b == SUB {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 >= stop {
                    return Err(Error::syntax("SUB at the end of input", pos));
                }
                let c = buf[p2];
                if c == SOH {
                    let p3 = scan::layout(buf, p2 + 1, stop);
                    if p3 < stop && buf[p3] == RS {
                        break; // the body's business: an error there
                    }
                    if pairs.is_empty() {
                        return Err(Error::syntax(
                            "nothing to supersede: no keyed unit before",
                            pos,
                        ));
                    }
                    self.supersessions.push(SOH);
                    pairs.pop();
                    raw_pairs.pop();
                    let (key, vs, p) = self.keyed_unit(p2 + 1, stop, true)?;
                    pos = p;
                    self.heading_pair(&mut pairs, &mut raw_pairs, key, trim(&buf[vs..pos]), vs)?;
                } else {
                    break; // SUB before a body code: the body's business
                }
            } else if b >= 0x20 {
                return Err(Error::syntax("plain text directly after SYN", pos));
            } else {
                break;
            }
        }
        Ok((Heading::new(&buf[hs..pos], pairs, raw_pairs), pos))
    }

    fn heading_pair(
        &mut self,
        pairs: &mut Vec<(&'a [u8], Cow<'a, [u8]>)>,
        raw_pairs: &mut Vec<(&'a [u8], &'a [u8])>,
        key: &'a [u8],
        rawv: &'a [u8],
        at: usize,
    ) -> Result<()> {
        raw_pairs.push((key, rawv));
        let v = if rawv.first() == Some(&DLE) {
            Cow::Borrowed(rawv)
        } else {
            value(rawv)?
        };
        pairs.push((key, v));
        if key == b"schema" {
            self.check_schema(rawv, at)?;
        }
        Ok(())
    }

    // The `schema` key: a plain name, or a level holding one keyed record
    // whose keys are drawn from name, url and hash (DESIGN.md, Start).
    fn check_schema(&mut self, rawv: &'a [u8], at: usize) -> Result<()> {
        let t = trim(rawv);
        let Some(&first) = t.first() else {
            return Err(Error::syntax("schema needs a value", at));
        };
        if first == DLE {
            let body = Body::of_level(t)?;
            if !(body.bare() && body.records().len() == 1) {
                return Err(Error::syntax("a schema level holds one keyed record", at));
            }
            let rec = body.record(0);
            if !rec.keyed() {
                return Err(Error::syntax(
                    "a schema level holds a keyed record, not a positional one",
                    at,
                ));
            }
            let pairs = rec.pairs()?;
            if pairs.is_empty() {
                return Err(Error::syntax("a schema record needs name, url or hash", at));
            }
            for (k, v) in &pairs {
                if !matches!(*k, b"name" | b"url" | b"hash") {
                    return Err(Error::syntax(
                        format!("unknown schema key '{}'", String::from_utf8_lossy(k)),
                        at,
                    ));
                }
                if v.is_empty() || !is_name(v) {
                    return Err(Error::syntax(
                        format!("a schema {} is a name", String::from_utf8_lossy(k)),
                        at,
                    ));
                }
            }
        } else if first == STX {
            return Err(Error::syntax("a schema name cannot be text", at));
        } else {
            let v = value(t)?;
            if v.is_empty() || !is_name(&v) {
                return Err(Error::syntax("a schema name is a name", at));
            }
        }
        Ok(())
    }

    // ---- body -----------------------------------------------------------

    /// A body from pos to stop. At depth zero (`level` false) commits are
    /// legal and the body ends at EOT, SYN, EM or stop; in a level it ends
    /// at CAN. Returns the files (dead and superseded elements dropped) and
    /// the position of the code that ended the body.
    pub fn body(
        &mut self,
        mut pos: usize,
        stop: usize,
        level: bool,
    ) -> Result<(Vec<FileView<'a>>, usize)> {
        let buf = self.buf;
        let mut files: Vec<FileView<'a>> = Vec::new();
        let mut file = FileView::new(buf, 0, 0, true);
        let mut group = Group::new(buf, 0, 0, 0); // the unnamed group
        let mut group_live = true; // whether `group` joins its file when it ends
        let mut header_ok = true; // a header row may still begin this group
        let mut dead_file = false; // inside a dead file: everything to the next file line is discarded
        let mut dead_depth: Option<usize> = None; // inside a dead group of this depth
        let mut dead = false; // BEL seen: the next element is dead
        let mut sup = false; // SUB seen: the next element supersedes the one before it
        let mut mark_at = pos; // position of that BEL or SUB, for errors

        // A group joins its file when it ends: a named one always, the
        // unnamed one once it has content.
        fn flush<'b>(file: &mut FileView<'b>, group: Group<'b>, live: bool) {
            if live && (group.depth > 0 || group.header.is_some() || !group.records.is_empty()) {
                file.groups.push(group);
            }
        }

        loop {
            pos = self.layout(pos, stop);
            if pos >= stop {
                break;
            }
            let b = buf[pos];
            match b {
                RS => {
                    let (rec, p) = self.record(pos + 1, stop)?;
                    pos = p;
                    if dead {
                        self.dead_elements += 1;
                    } else {
                        if sup {
                            if group.records.is_empty() {
                                return Err(Error::syntax(
                                    "nothing to supersede: no record before it in the group",
                                    mark_at,
                                ));
                            }
                            group.records.pop();
                        }
                        group.records.push(rec);
                        header_ok = false;
                    }
                    dead = false;
                    sup = false;
                }
                SOH => {
                    let p2 = scan::layout(buf, pos + 1, stop);
                    if !(p2 < stop && buf[p2] == RS) {
                        return Err(Error::syntax("keyed unit outside a record", pos));
                    }
                    if sup {
                        return Err(Error::syntax(
                            "a header row admits no supersession",
                            mark_at,
                        ));
                    }
                    if !header_ok {
                        return Err(Error::syntax(
                            "header row not at the start of its group",
                            pos,
                        ));
                    }
                    let (hdr, p) = self.header_row(p2 + 1, stop)?;
                    pos = p;
                    if dead {
                        self.dead_elements += 1;
                    } else {
                        group.header = Some(hdr);
                        header_ok = false;
                    }
                    dead = false;
                }
                GS => {
                    let mut depth = 0;
                    while pos < stop && buf[pos] == GS {
                        depth += 1;
                        pos = self.layout(pos + 1, stop);
                    }
                    let (ns, ne, p) = self.name(pos, stop)?;
                    pos = p;
                    let done = std::mem::replace(&mut group, Group::new(buf, depth, ns, ne));
                    flush(&mut file, done, group_live);
                    if dead_depth.is_some_and(|dd| depth <= dd) {
                        dead_depth = None; // the dead group's span ends here
                    }
                    if dead {
                        self.dead_elements += 1;
                        if !dead_file && dead_depth.is_none() {
                            dead_depth = Some(depth);
                        }
                        group_live = false; // a dead group is parsed, never attached
                    } else if dead_file || dead_depth.is_some() {
                        group_live = false;
                    } else {
                        if sup {
                            supersede_group(&mut file.groups, depth, mark_at)?;
                        }
                        group_live = true;
                    }
                    header_ok = true;
                    dead = false;
                    sup = false;
                }
                FS => {
                    let (ns, ne, p) = self.name(pos + 1, stop)?;
                    pos = p;
                    let done = std::mem::replace(&mut group, Group::new(buf, 0, 0, 0));
                    flush(&mut file, done, group_live);
                    // close the current file: an empty leading file is
                    // dropped when a file line follows it
                    let done = std::mem::replace(&mut file, FileView::new(buf, ns, ne, false));
                    if !(dead_file || (done.leading && done.groups.is_empty())) {
                        files.push(done);
                    }
                    if dead {
                        self.dead_elements += 1;
                        dead_file = true;
                    } else {
                        dead_file = false;
                        if sup {
                            if files.is_empty() {
                                return Err(Error::syntax(
                                    "nothing to supersede: no file before it",
                                    mark_at,
                                ));
                            }
                            files.pop();
                        }
                    }
                    group_live = !dead_file;
                    dead_depth = None;
                    header_ok = true;
                    dead = false;
                    sup = false;
                }
                BEL => {
                    if sup {
                        return Err(Error::syntax("SUB before BEL", mark_at));
                    }
                    if dead {
                        return Err(Error::syntax(
                            "BEL before a code that is not an element",
                            pos,
                        ));
                    }
                    let p2 = scan::layout(buf, pos + 1, stop);
                    if p2 >= stop {
                        return Err(Error::end("BEL at end of input", p2));
                    }
                    let c = buf[p2];
                    if c >= 0x20 || c == STX {
                        self.not_canonical("comment");
                        pos = self.note_content(p2, stop)?;
                    } else if matches!(c, RS | SOH | GS | FS) {
                        self.not_canonical("dead element");
                        dead = true;
                        mark_at = pos;
                        pos = p2;
                    } else if c == DLE {
                        return Err(Error::syntax("a nested level is not an item", p2));
                    } else {
                        return Err(Error::syntax(
                            "BEL before a code that is not an element",
                            p2,
                        ));
                    }
                }
                SUB => {
                    if dead {
                        return Err(Error::syntax(
                            "BEL before SUB: SUB is not an element",
                            mark_at,
                        ));
                    }
                    if sup {
                        return Err(Error::syntax("SUB before SUB", pos));
                    }
                    let p2 = scan::layout(buf, pos + 1, stop);
                    if p2 >= stop {
                        return Err(Error::syntax("SUB at the end of input", pos));
                    }
                    let c = buf[p2];
                    if matches!(c, RS | GS | FS) {
                        self.supersessions.push(c);
                        sup = true;
                        mark_at = pos;
                        pos = p2;
                    } else if c == SOH {
                        let p3 = scan::layout(buf, p2 + 1, stop);
                        if p3 < stop && buf[p3] == RS {
                            return Err(Error::syntax("a header row admits no supersession", pos));
                        }
                        return Err(Error::syntax("keyed unit outside a record", p2));
                    } else if c == US {
                        return Err(Error::syntax("US outside a record", p2));
                    } else if c == BEL {
                        return Err(Error::syntax("SUB before BEL", pos));
                    } else {
                        return Err(Error::syntax(
                            "SUB before a byte that is not a separator or opener",
                            pos,
                        ));
                    }
                }
                ETB => {
                    if level {
                        return Err(Error::syntax("commit inside a nested level", pos));
                    }
                    if dead {
                        return Err(Error::syntax(
                            "BEL before ETB: framing cannot be commented out",
                            mark_at,
                        ));
                    }
                    if sup {
                        return Err(Error::syntax("SUB before ETB", mark_at));
                    }
                    self.not_canonical("commit");
                    let p = self.layout(pos + 1, stop);
                    pos = scan::plain(buf, p, stop);
                }
                EOT | SYN | EM => {
                    if level {
                        return Err(Error::syntax("framing inside open level", pos));
                    }
                    if dead {
                        return Err(Error::syntax("BEL before framing", mark_at));
                    }
                    if sup {
                        return Err(Error::syntax("SUB before framing", mark_at));
                    }
                    break;
                }
                CAN => {
                    if !level {
                        return Err(Error::syntax("CAN with no open level", pos));
                    }
                    if dead {
                        return Err(Error::syntax("BEL before CAN", mark_at));
                    }
                    if sup {
                        return Err(Error::syntax("SUB before CAN", mark_at));
                    }
                    break;
                }
                US => return Err(Error::syntax("US outside a record", pos)),
                STX => return Err(Error::syntax("text outside a record", pos)),
                DLE => return Err(Error::syntax("a nested level is not an item", pos)),
                ENQ => return Err(Error::syntax("a reference outside a record", pos)),
                _ => {
                    if b >= 0x20 {
                        return Err(Error::syntax("plain text outside a record", pos));
                    }
                    return Err(Error::UnassignedCode {
                        byte: b,
                        position: pos,
                    });
                }
            }
        }
        flush(&mut file, group, group_live);
        if !(dead_file || (file.leading && file.groups.is_empty() && !files.is_empty())) {
            files.push(file);
        }
        // a body whose every element is dead is an empty body: one empty
        // leading file, so there is always a file to report
        if files.is_empty() {
            files.push(FileView::new(buf, 0, 0, true));
        }
        Ok((files, pos))
    }

    // A label or path: plain, never text. Returns (name start, name end, pos).
    fn name(&mut self, pos: usize, stop: usize) -> Result<(usize, usize, usize)> {
        let buf = self.buf;
        let pos = self.layout(pos, stop);
        if pos < stop && buf[pos] == STX {
            return Err(Error::syntax("a name cannot be text", pos));
        }
        let ns = pos;
        let pos = scan::plain(buf, pos, stop);
        let mut ne = pos;
        while ne > ns && is_layout(buf[ne - 1]) {
            ne -= 1;
        }
        if ne != pos {
            self.not_canonical("layout bytes");
        }
        Ok((ns, ne, pos))
    }

    // After SOH RS: names, each followed by notes, separated by US.
    fn header_row(&mut self, mut pos: usize, stop: usize) -> Result<(Record<'a>, usize)> {
        let buf = self.buf;
        let start = pos;
        loop {
            let (_ns, _ne, p) = self.name(pos, stop)?;
            let p = self.layout(p, stop);
            pos = self.trailing_notes(p, stop)?;
            if !(pos < stop && buf[pos] == US) {
                break;
            }
            pos += 1;
        }
        if pos < stop && buf[pos] == SUB {
            let p2 = scan::layout(buf, pos + 1, stop);
            if p2 < stop && buf[p2] == US {
                return Err(Error::syntax("a header row admits no supersession", pos));
            }
        }
        Ok((Record::new(buf, start, trim_end(buf, start, pos)), pos))
    }

    // ---- records --------------------------------------------------------

    /// After RS. Returns the record and the position after it.
    pub fn record(&mut self, pos: usize, stop: usize) -> Result<(Record<'a>, usize)> {
        let start = pos;
        let pos = if self.keyed_start(pos, stop)? {
            self.keyed(pos, stop)?
        } else {
            self.positional(pos, stop)?
        };
        Ok((
            Record::new(self.buf, start, trim_end(self.buf, start, pos)),
            pos,
        ))
    }

    // Does the record starting here begin with a keyed unit, live, dead or
    // superseding?
    fn keyed_start(&self, mut pos: usize, stop: usize) -> Result<bool> {
        let buf = self.buf;
        loop {
            pos = scan::layout(buf, pos, stop);
            if pos >= stop {
                return Ok(false);
            }
            let b = buf[pos];
            if b == SOH {
                let p2 = scan::layout(buf, pos + 1, stop);
                return Ok(!(p2 < stop && buf[p2] == RS));
            } else if b == BEL || b == SUB {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 >= stop {
                    return Ok(false);
                }
                let c = buf[p2];
                if c == SOH {
                    let p3 = scan::layout(buf, p2 + 1, stop);
                    return Ok(!(p3 < stop && buf[p3] == RS));
                }
                if !(b == BEL && (c >= 0x20 || c == STX)) {
                    return Ok(false);
                }
                pos = self.note_content(p2, stop)?; // a note: look past it
            } else {
                return Ok(false);
            }
        }
    }

    fn positional(&mut self, mut pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        loop {
            pos = self.value(pos, stop, false)?;
            pos = self.layout(pos, stop);
            if pos >= stop {
                break;
            }
            let b = buf[pos];
            if b == US {
                pos += 1;
            } else if b == SUB {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 >= stop {
                    return Err(Error::syntax("SUB at the end of input", pos));
                }
                let c = buf[p2];
                if c == US {
                    self.supersessions.push(US);
                    pos = p2 + 1;
                } else if matches!(c, RS | GS | FS) {
                    break; // qualifies the next line: the body's business
                } else if c == SOH {
                    let p3 = scan::layout(buf, p2 + 1, stop);
                    if p3 < stop && buf[p3] == RS {
                        return Err(Error::syntax("a header row admits no supersession", pos));
                    }
                    return Err(Error::syntax(
                        "keyed unit in a positional record (mixed record)",
                        p2,
                    ));
                } else if c == BEL {
                    return Err(Error::syntax("SUB before BEL", pos));
                } else {
                    return Err(Error::syntax(
                        "SUB before a byte that is not a separator or opener",
                        pos,
                    ));
                }
            } else if b == SOH {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 < stop && buf[p2] == RS {
                    break; // a header row ends the record
                }
                return Err(Error::syntax(
                    "keyed unit in a positional record (mixed record)",
                    pos,
                ));
            } else {
                break;
            }
        }
        Ok(pos)
    }

    fn keyed(&mut self, mut pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        let mut keys: Vec<&'a [u8]> = Vec::new(); // the result's keys
        loop {
            pos = self.layout(pos, stop);
            if pos >= stop {
                break;
            }
            let b = buf[pos];
            if b == SOH {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 < stop && buf[p2] == RS {
                    break;
                }
                let (key, _vs, p) = self.keyed_unit(pos + 1, stop, false)?;
                pos = p;
                keys.push(key);
            } else if b == BEL {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 >= stop {
                    return Err(Error::end("BEL at end of input", p2));
                }
                let c = buf[p2];
                if c == SOH {
                    let p3 = scan::layout(buf, p2 + 1, stop);
                    if p3 < stop && buf[p3] == RS {
                        break;
                    }
                    self.not_canonical("dead element");
                    let (_key, _vs, p) = self.keyed_unit(p2 + 1, stop, false)?;
                    pos = p;
                } else if c >= 0x20 || c == STX {
                    pos = self.trailing_notes(pos, stop)?;
                } else if c == SUB {
                    return Err(Error::syntax("BEL before SUB: SUB is not an element", p2));
                } else {
                    break;
                }
            } else if b == SUB {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 >= stop {
                    return Err(Error::syntax("SUB at the end of input", pos));
                }
                let c = buf[p2];
                if c == SOH {
                    let p3 = scan::layout(buf, p2 + 1, stop);
                    if p3 < stop && buf[p3] == RS {
                        return Err(Error::syntax("a header row admits no supersession", pos));
                    }
                    if keys.is_empty() {
                        return Err(Error::syntax(
                            "nothing to supersede: no keyed unit before it",
                            pos,
                        ));
                    }
                    self.supersessions.push(SOH);
                    keys.pop();
                    let (key, _vs, p) = self.keyed_unit(p2 + 1, stop, false)?;
                    pos = p;
                    keys.push(key);
                } else if c == US {
                    return Err(Error::syntax("US after a keyed value (mixed record)", p2));
                } else if matches!(c, RS | GS | FS) {
                    break;
                } else if c == BEL {
                    return Err(Error::syntax("SUB before BEL", pos));
                } else {
                    return Err(Error::syntax(
                        "SUB before a byte that is not a separator or opener",
                        pos,
                    ));
                }
            } else {
                break;
            }
        }
        if pos < stop && buf[pos] == US {
            return Err(Error::syntax("US after a keyed value (mixed record)", pos));
        }
        for (i, k) in keys.iter().enumerate() {
            if keys[..i].contains(k) {
                self.not_canonical("duplicate key");
                break;
            }
        }
        Ok(pos)
    }

    // After SOH (not a header row). Returns (key, value start, pos after the value).
    fn keyed_unit(
        &mut self,
        pos: usize,
        stop: usize,
        heading: bool,
    ) -> Result<(&'a [u8], usize, usize)> {
        let buf = self.buf;
        let (ks, ke, pos) = self.name(pos, stop)?;
        if !(pos < stop && buf[pos] == US) {
            return Err(Error::syntax("key without a value (no US)", pos));
        }
        let vs = pos + 1;
        let pos = self.value(vs, stop, heading)?;
        Ok((&buf[ks..ke], vs, pos))
    }

    // ---- values ---------------------------------------------------------

    // One unit's value, possibly empty, with the notes after it. Returns
    // the position of the next code at this depth.
    fn value(&mut self, pos: usize, stop: usize, heading: bool) -> Result<usize> {
        let buf = self.buf;
        let pos = self.layout(pos, stop);
        if pos >= stop {
            return Ok(pos);
        }
        let b = buf[pos];
        let pos = if b >= 0x20 {
            let p = scan::plain(buf, pos, stop);
            if buf[p - 1] == SP {
                self.not_canonical("layout bytes");
            }
            p
        } else if b == STX {
            let start = pos;
            let p = scan::text(buf, pos, stop)?;
            if self.canonical {
                let inside = &buf[start + 1..p - 1];
                if inside.is_empty() {
                    self.not_canonical("empty text");
                } else if is_plain(&decode_text(inside)) {
                    self.not_canonical("gratuitous text");
                }
            }
            p
        } else if b == DLE {
            let (_files, p) = self.level(pos, stop)?;
            p
        } else if b == ENQ {
            if heading {
                return Err(Error::syntax("reference in a heading", pos));
            }
            let (_r, p) = self.reference(pos, stop)?;
            p
        } else if b == BEL {
            let p2 = scan::layout(buf, pos + 1, stop);
            if p2 >= stop {
                return Err(Error::end("BEL at end of input", p2));
            }
            let c = buf[p2];
            if c == DLE {
                self.not_canonical("dead element"); // a dead level: the unit is empty
                let (_files, p) = self.level(p2, stop)?;
                p
            } else if c >= 0x20 || c == STX {
                let p = self.trailing_notes(pos, stop)?;
                if self.value_start(p, stop) {
                    return Err(Error::syntax("value after a note in the same unit", p));
                }
                return Ok(p);
            } else {
                return Ok(pos); // a dead element after this empty unit: the caller's business
            }
        } else {
            return Ok(pos); // an empty unit
        };
        let p = self.layout(pos, stop);
        let after = self.trailing_notes(p, stop)?;
        if self.value_start(after, stop) {
            return Err(Error::syntax("two values in one unit", after));
        }
        Ok(after)
    }

    #[inline]
    fn value_start(&self, pos: usize, stop: usize) -> bool {
        if pos >= stop {
            return false;
        }
        let b = self.buf[pos];
        b >= 0x20 || b == STX || b == DLE || b == ENQ
    }

    // The content of a note, from the byte after BEL and layout.
    fn note_content(&self, pos: usize, stop: usize) -> Result<usize> {
        if self.buf[pos] == STX {
            scan::text(self.buf, pos, stop)
        } else {
            Ok(scan::plain(self.buf, pos, stop))
        }
    }

    // Notes after a value. A BEL before a code is left for the caller.
    fn trailing_notes(&mut self, mut pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        while pos < stop && buf[pos] == BEL {
            let p2 = scan::layout(buf, pos + 1, stop);
            if p2 >= stop {
                return Err(Error::end("BEL at end of input", p2));
            }
            let c = buf[p2];
            if !(c >= 0x20 || c == STX) {
                break;
            }
            self.not_canonical("comment");
            let p = self.note_content(p2, stop)?;
            pos = self.layout(p, stop);
        }
        Ok(pos)
    }

    /// DLE at pos: a nested level. Returns its files and the position after CAN.
    pub fn level(&mut self, pos: usize, stop: usize) -> Result<(Vec<FileView<'a>>, usize)> {
        let buf = self.buf;
        let start = pos;
        let pos = self.layout(pos + 1, stop);
        if pos >= stop {
            return Err(Error::end(
                format!("level opened at {start} is not closed"),
                pos,
            ));
        }
        let b = buf[pos];
        if b == CAN {
            return Ok((vec![FileView::new(buf, 0, 0, true)], pos + 1));
        }
        if !matches!(b, FS | GS | RS | SOH | BEL | SUB) {
            return Err(Error::syntax(
                "DLE must be followed by a structural code",
                pos,
            ));
        }
        let (files, pos) = self.body(pos, stop, true)?;
        let pos = self.layout(pos, stop);
        if pos >= stop {
            return Err(Error::end(
                format!("level opened at {start} is not closed"),
                pos,
            ));
        }
        if buf[pos] != CAN {
            return Err(Error::syntax("unexpected byte inside level", pos));
        }
        Ok((files, pos + 1))
    }

    /// ENQ at pos: a group name, or a pattern (DESIGN.md, References).
    /// Returns the reference and the position after it.
    pub fn reference(&mut self, pos: usize, stop: usize) -> Result<(Reference<'a>, usize)> {
        let buf = self.buf;
        let at = pos;
        let pos = self.layout(pos + 1, stop);
        if pos >= stop {
            return Err(Error::syntax("reference without a name", at));
        }
        let b = buf[pos];
        if b == DLE {
            let dead0 = self.dead_elements;
            let sup0 = self.supersessions.len();
            let (files, after) = self.level(pos, stop)?;
            if self.dead_elements != dead0 {
                return Err(Error::syntax(
                    "a dead element is not a step of a pattern",
                    pos,
                ));
            }
            if self.supersessions.len() != sup0 {
                return Err(Error::syntax(
                    "supersession is not a step of a pattern",
                    pos,
                ));
            }
            let body = Body::from_parts(buf, pos + 1, after - 1, files);
            self.check_pattern(&body, pos, false)?;
            Ok((Reference::new(None, Some(body)), after))
        } else if b == STX {
            Err(Error::syntax(
                "a reference is a name or a pattern, not text",
                pos,
            ))
        } else if b >= 0x20 {
            let (ns, ne, p) = self.name(pos, stop)?;
            if ne == ns {
                return Err(Error::syntax("reference without a name", at));
            }
            Ok((Reference::new(Some(&buf[ns..ne]), None), p))
        } else {
            Err(Error::syntax("reference without a name", at))
        }
    }

    // A pattern is one branch of the target with its values left out.
    // `inner` is a pattern continued into a level, whose records may stand
    // in the unnamed group.
    fn check_pattern(&mut self, body: &Body<'a>, at: usize, inner: bool) -> Result<()> {
        let files = body.files();
        if files.len() > 1 {
            return Err(Error::syntax("a pattern is one branch: two files", at));
        }
        let groups = files[0].groups();
        if groups.is_empty() {
            return Err(Error::syntax(
                if inner {
                    "an empty level continues nothing"
                } else {
                    "an empty level is not a reference"
                },
                at,
            ));
        }
        if !inner && groups[0].depth() == 0 {
            return Err(Error::syntax(
                "a pattern begins with a file or group line",
                at,
            ));
        }
        let mut prev: Option<usize> = None;
        for (i, g) in groups.iter().enumerate() {
            if g.has_header() {
                return Err(Error::syntax("a header row is not a step of a pattern", at));
            }
            if prev.is_some_and(|p| g.depth() <= p) {
                return Err(Error::syntax(
                    "a pattern is one branch: two groups at one depth",
                    at,
                ));
            }
            prev = Some(g.depth());
            if i < groups.len() - 1 && !g.records().is_empty() {
                return Err(Error::syntax("nothing follows the record in a pattern", at));
            }
        }
        let recs = groups[groups.len() - 1].records();
        if recs.len() > 1 {
            return Err(Error::syntax("a pattern is one branch: two records", at));
        }
        let Some(rec) = recs.first() else {
            if !inner {
                self.not_canonical("group reference written as a pattern");
            }
            return Ok(());
        };
        if rec.keyed() {
            let pairs = rec.pairs()?;
            if pairs.len() != 1 {
                return Err(Error::syntax(
                    "a keyed record in a pattern holds one keyed unit",
                    at,
                ));
            }
            if !pairs[0].1.is_empty() {
                return Err(Error::syntax("a keyed unit in a pattern has no value", at));
            }
            if pairs[0].0.is_empty() || !is_name(pairs[0].0) {
                return Err(Error::syntax("a step of a pattern is a name", at));
            }
            return Ok(());
        }
        let units = rec.fields();
        if units.len() > 3 {
            return Err(Error::syntax(
                "a pattern record holds at most a record id, a field name and a level",
                at,
            ));
        }
        for (i, raw) in units.iter().enumerate() {
            let t = trim(raw);
            if i == 2 {
                if t.first() != Some(&DLE) {
                    return Err(Error::syntax(
                        "the third unit of a pattern record is a level",
                        at,
                    ));
                }
                let inner_body = Body::of_level(t)?;
                self.check_pattern(&inner_body, at, true)?;
            } else {
                if t.first().is_some_and(|&c| c == DLE || c == ENQ || c == STX) {
                    return Err(Error::syntax("a step of a pattern is a name", at));
                }
                let v = value(t)?;
                if v.is_empty() || !is_name(&v) {
                    return Err(Error::syntax("a step of a pattern is a name", at));
                }
            }
        }
        Ok(())
    }
}

// Remove the last group at `depth` and the deeper groups that follow it
// (its span), so the superseding group can take its place.
fn supersede_group(groups: &mut Vec<Group<'_>>, depth: usize, at: usize) -> Result<()> {
    let Some(i) = groups.iter().rposition(|g| g.depth == depth) else {
        return Err(Error::syntax(
            "nothing to supersede: no earlier group at that depth",
            at,
        ));
    };
    let mut j = i + 1;
    while j < groups.len() && groups[j].depth > depth {
        j += 1;
    }
    groups.drain(i..j);
    Ok(())
}

#[inline]
pub(crate) fn trim_end(buf: &[u8], start: usize, mut stop: usize) -> usize {
    while stop > start && is_layout(buf[stop - 1]) {
        stop -= 1;
    }
    stop
}
