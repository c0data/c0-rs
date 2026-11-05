//! Recursive-descent check of a buffer against GRAMMAR.md. [`validate`]
//! fails on the first structural error; [`canonical`] additionally applies
//! the canonical constraints and answers `false` instead of failing.

use crate::*;

/// The well-formedness and canonicality checker. Use [`validate`] and
/// [`canonical`] unless you want the reasons a unit is not canonical.
pub struct Validator<'a> {
    buf: &'a [u8],
    len: usize,
    canonical: bool,
    ok: bool,
    diff: bool,
    warnings: Vec<&'static str>,
}

/// Fail on the first structural error.
pub fn validate(buf: &[u8]) -> Result<()> {
    Validator::new(buf, false).run()
}

/// Well-formed and canonical: no layout, one spelling per value, keys
/// unique within a keyed record, no framing, no comments.
pub fn canonical(buf: &[u8]) -> bool {
    let mut v = Validator::new(buf, true);
    v.run().is_ok() && v.ok()
}

impl<'a> Validator<'a> {
    pub fn new(buf: &'a [u8], canonical: bool) -> Self {
        Validator {
            buf,
            len: buf.len(),
            canonical,
            ok: true,
            diff: false,
            warnings: Vec::new(),
        }
    }

    /// Whether every canonical constraint held (meaningful after `run`).
    pub fn ok(&self) -> bool {
        self.ok
    }

    /// Why the unit is not canonical, one entry per finding.
    pub fn warnings(&self) -> &[&'static str] {
        &self.warnings
    }

    fn not_canonical(&mut self, why: &'static str) {
        self.ok = false;
        self.warnings.push(why);
    }

    /// Check the whole buffer: documents, headings, bodies, EM.
    pub fn run(&mut self) -> Result<()> {
        let buf = self.buf;
        let len = self.len;
        let mut pos = self.layout(0, len);
        let mut first = true;
        while pos < len {
            let byte = buf[pos];
            if byte == SYN {
                self.not_canonical("SYN in a canonical unit");
                pos = self.heading(pos + 1)?;
                first = true; // the body that follows belongs to this document
            } else if byte == EM {
                self.not_canonical("EM in a canonical unit");
                return Ok(()); // raw region follows
            } else {
                if !first {
                    return Err(Error::syntax(
                        "a document after the first must begin with SYN",
                        pos,
                    ));
                }
                pos = self.body(pos, len, true)?;
                // after a body only EOT, SYN, EM or end
                pos = self.layout(pos, len);
                first = false;
                if pos < len {
                    let b = buf[pos];
                    if b == EOT {
                        self.not_canonical("EOT in a canonical unit");
                        pos = self.layout(pos + 1, len);
                        if pos < len && buf[pos] != SYN && buf[pos] != EM {
                            return Err(Error::syntax("unexpected byte after EOT", pos));
                        }
                    } else if b == SYN || b == EM {
                        continue;
                    } else if b < 0x20 && !is_assigned(b) {
                        return Err(Error::UnassignedCode {
                            byte: b,
                            position: pos,
                        });
                    } else {
                        return Err(Error::syntax("unexpected byte after document", pos));
                    }
                }
            }
        }
        Ok(())
    }

    #[inline]
    fn layout(&mut self, pos: usize, stop: usize) -> usize {
        let p2 = scan::layout(self.buf, pos, stop);
        if p2 != pos {
            self.not_canonical("layout bytes");
        }
        p2
    }

    // Heading: keyed units and notes after SYN. Returns the position of the
    // first body code. Also decides diff vs data body.
    fn heading(&mut self, pos: usize) -> Result<usize> {
        let buf = self.buf;
        let len = self.len;
        let mut keys: Vec<&[u8]> = Vec::new();
        let mut pos = self.layout(pos, len);
        while pos < len {
            let b = buf[pos];
            if b == SOH {
                let p2 = self.layout(pos + 1, len);
                if p2 < len && buf[p2] == RS {
                    break; // header row ends the heading
                }
                let (k, v, p) = self.keyed_unit(pos + 1, len, true)?;
                pos = p;
                keys.push(k);
                if k == b"shape" && v == b"diff" {
                    self.diff = true;
                }
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
                    self.not_canonical("comment");
                    pos = scan::comment(buf, pos, len)?;
                } else {
                    break; // a dead body element ends the heading
                }
            } else if b >= 0x20 {
                return Err(Error::syntax("plain text directly after SYN", pos));
            } else {
                break;
            }
            pos = self.layout(pos, len);
        }
        self.check_keys(&keys);
        Ok(pos)
    }

    fn check_keys(&mut self, keys: &[&[u8]]) {
        for (i, k) in keys.iter().enumerate() {
            if keys[..i].contains(k) {
                self.not_canonical("duplicate key");
                return;
            }
        }
    }

    // A body (data or diff) from pos to stop. depth0 allows commits.
    fn body(&mut self, pos: usize, stop: usize, depth0: bool) -> Result<usize> {
        if self.diff && depth0 {
            return self.diff_body(pos, stop);
        }
        let buf = self.buf;
        let mut pos = self.layout(pos, stop);
        pos = self.part(pos, stop, depth0)?;
        while pos < stop {
            let b = buf[pos];
            if b == FS {
                pos = self.plain_name(pos + 1, stop)?;
                pos = self.part(pos, stop, depth0)?;
            } else if b == BEL {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 < stop && buf[p2] == FS {
                    self.not_canonical("dead element");
                    pos = self.plain_name(p2 + 1, stop)?;
                    pos = self.part(pos, stop, depth0)?;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(pos)
    }

    // part = *pre [header-row] *item *group
    fn part(&mut self, pos: usize, stop: usize, depth0: bool) -> Result<usize> {
        let buf = self.buf;
        let mut pos = self.pre(pos, stop, depth0)?;
        pos = self.header_row(pos, stop)?;
        pos = self.items(pos, stop, depth0)?;
        while pos < stop {
            let b = buf[pos];
            if b == GS {
                pos = self.group(pos, stop, depth0)?;
            } else if b == BEL {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 < stop && buf[p2] == GS {
                    self.not_canonical("dead element");
                    pos = self.group(p2, stop, depth0)?;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(pos)
    }

    fn group(&mut self, mut pos: usize, stop: usize, depth0: bool) -> Result<usize> {
        let buf = self.buf;
        while pos < stop && buf[pos] == GS {
            pos = self.layout(pos + 1, stop);
        }
        pos = self.plain_name(pos, stop)?;
        pos = self.pre(pos, stop, depth0)?;
        pos = self.header_row(pos, stop)?;
        self.items(pos, stop, depth0)
    }

    // Before a header row: notes, commits (at depth zero), dead records and
    // dead header rows.
    fn pre(&mut self, mut pos: usize, stop: usize, depth0: bool) -> Result<usize> {
        let buf = self.buf;
        loop {
            pos = self.layout(pos, stop);
            if pos >= stop {
                break;
            }
            let b = buf[pos];
            if b == ETB {
                if !depth0 {
                    return Err(Error::syntax("commit inside a nested level", pos));
                }
                self.not_canonical("commit");
                let p = self.layout(pos + 1, stop);
                pos = scan::plain(buf, p, stop);
            } else if b == BEL {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 >= stop {
                    return Err(Error::end("BEL at end of input", p2));
                }
                if buf[p2] >= 0x20 || buf[p2] == STX {
                    pos = self.notes(pos, stop)?;
                } else if buf[p2] == RS {
                    self.not_canonical("dead element");
                    pos = self.record(p2 + 1, stop)?;
                } else if buf[p2] == SOH {
                    let p3 = scan::layout(buf, p2 + 1, stop);
                    if p3 < stop && buf[p3] == RS {
                        self.not_canonical("dead element");
                        pos = self.header_names(p3 + 1, stop)?;
                    } else {
                        return Err(Error::syntax("keyed unit outside a record", p2));
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(pos)
    }

    fn header_names(&mut self, pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        let mut pos = self.layout(pos, stop);
        loop {
            pos = self.plain_name(pos, stop)?;
            if !(pos < stop && buf[pos] == US) {
                break;
            }
            pos = self.layout(pos + 1, stop);
        }
        Ok(pos)
    }

    // Notes only (BEL followed by plain/text); dead elements are handled by
    // their element rules.
    fn notes(&mut self, mut pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        loop {
            pos = self.layout(pos, stop);
            if !(pos < stop && buf[pos] == BEL) {
                break;
            }
            let p2 = scan::layout(buf, pos + 1, stop);
            if p2 >= stop {
                return Err(Error::end("BEL at end of input", p2));
            }
            if buf[p2] < 0x20 && buf[p2] != STX {
                break; // a dead element, not a note
            }
            self.not_canonical("comment");
            pos = if buf[p2] == STX {
                scan::text(buf, p2, stop)?
            } else {
                scan::plain(buf, p2, stop)
            };
        }
        Ok(pos)
    }

    fn header_row(&mut self, pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        let pos = self.layout(pos, stop);
        if !(pos < stop && buf[pos] == SOH) {
            return Ok(pos);
        }
        let p2 = self.layout(pos + 1, stop);
        if !(p2 < stop && buf[p2] == RS) {
            return Ok(pos);
        }
        self.header_names(p2 + 1, stop)
    }

    fn items(&mut self, mut pos: usize, stop: usize, depth0: bool) -> Result<usize> {
        let buf = self.buf;
        loop {
            pos = self.layout(pos, stop);
            if pos >= stop {
                break;
            }
            let b = buf[pos];
            if b == RS {
                pos = self.record(pos + 1, stop)?;
            } else if b == ETB {
                if !depth0 {
                    return Err(Error::syntax("commit inside a nested level", pos));
                }
                self.not_canonical("commit");
                let p = self.layout(pos + 1, stop);
                pos = scan::plain(buf, p, stop);
            } else if b == BEL {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 >= stop || buf[p2] >= 0x20 || buf[p2] == STX {
                    pos = self.notes(pos, stop)?;
                } else if buf[p2] == RS {
                    self.not_canonical("dead element");
                    pos = self.record(p2 + 1, stop)?;
                } else if buf[p2] == SOH {
                    let p3 = scan::layout(buf, p2 + 1, stop);
                    if p3 < stop && buf[p3] == RS {
                        self.not_canonical("dead element");
                        pos = self.header_names(p3 + 1, stop)?;
                    } else {
                        return Err(Error::syntax("keyed unit outside a record", p2));
                    }
                } else if buf[p2] == DLE {
                    return Err(Error::syntax("a nested level is not an item", p2));
                } else {
                    break; // dead GS/FS handled by the caller; others are errors there
                }
            } else if b == SOH {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 < stop && buf[p2] == RS {
                    return Err(Error::syntax(
                        "header row not at the start of its group",
                        pos,
                    ));
                }
                return Err(Error::syntax("keyed unit outside a record", pos));
            } else {
                break;
            }
        }
        Ok(pos)
    }

    // After RS. Returns the position after the record.
    fn record(&mut self, pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        let mut pos = self.layout(pos, stop);
        if self.keyed_start(pos, stop)? {
            let mut keys: Vec<&[u8]> = Vec::new();
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
                    let (k, _v, p) = self.keyed_unit(pos + 1, stop, false)?;
                    keys.push(k);
                    pos = p;
                } else if b == BEL {
                    let p2 = scan::layout(buf, pos + 1, stop);
                    if p2 >= stop {
                        return Err(Error::end("BEL at end of input", p2));
                    }
                    if buf[p2] == SOH {
                        let p3 = scan::layout(buf, p2 + 1, stop);
                        if p3 < stop && buf[p3] == RS {
                            break;
                        }
                        self.not_canonical("dead element");
                        let (_k, _v, p) = self.keyed_unit(p2 + 1, stop, false)?;
                        pos = p;
                    } else if buf[p2] >= 0x20 || buf[p2] == STX {
                        pos = self.notes(pos, stop)?;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            self.check_keys(&keys);
            if pos < stop && buf[pos] == US {
                return Err(Error::syntax("US after a keyed value (mixed record)", pos));
            }
            return Ok(pos);
        }
        // positional
        loop {
            pos = self.value(pos, stop, false)?;
            pos = self.layout(pos, stop);
            if pos >= stop {
                break;
            }
            let b = buf[pos];
            if b == US {
                pos += 1;
            } else if b == SOH {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 < stop && buf[p2] == RS {
                    break;
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

    // Does a record starting here begin with a (live or dead) keyed unit?
    fn keyed_start(&self, mut pos: usize, stop: usize) -> Result<bool> {
        let buf = self.buf;
        while pos < stop {
            let b = buf[pos];
            if b == SOH {
                return Ok(true);
            }
            if b != BEL {
                return Ok(false);
            }
            let p2 = scan::layout(buf, pos + 1, stop);
            if p2 >= stop {
                return Ok(false);
            }
            if buf[p2] == SOH {
                return Ok(true);
            }
            if buf[p2] < 0x20 && buf[p2] != STX {
                return Ok(false);
            }
            pos = scan::layout(buf, scan::comment(buf, pos, stop)?, stop);
        }
        Ok(false)
    }

    // After SOH. Returns (key, trimmed raw value, pos).
    fn keyed_unit(
        &mut self,
        pos: usize,
        stop: usize,
        heading: bool,
    ) -> Result<(&'a [u8], &'a [u8], usize)> {
        let buf = self.buf;
        let ks = self.layout(pos, stop);
        let pos = scan::plain(buf, ks, stop);
        let key = trim(&buf[ks..pos]);
        if !(pos < stop && buf[pos] == US) {
            return Err(Error::syntax("key without a value (no US)", pos));
        }
        let vs = pos + 1;
        let pos = self.value(vs, stop, heading)?;
        Ok((key, trim(&buf[vs..pos]), pos))
    }

    // One value (possibly empty) starting at pos. Returns the position after
    // it (before the next code at this depth).
    fn value(&mut self, pos: usize, stop: usize, heading: bool) -> Result<usize> {
        let buf = self.buf;
        let pos = self.layout(pos, stop);
        if pos >= stop {
            return Ok(pos);
        }
        let b = buf[pos];
        let two_values = |p: usize| -> bool {
            p < stop && (buf[p] >= 0x20 || buf[p] == STX || buf[p] == DLE || buf[p] == ENQ)
        };
        if b >= 0x20 {
            let start = pos;
            let pos = scan::plain(buf, pos, stop);
            if is_layout(buf[pos - 1]) {
                self.not_canonical("layout bytes");
            }
            let _ = start;
            let p = self.layout(pos, stop);
            let after = self.trailing_notes(p, stop)?;
            if two_values(after) {
                return Err(Error::syntax("two values in one unit", after));
            }
            Ok(after)
        } else if b == STX {
            let start = pos;
            let pos = scan::text(buf, pos, stop)?;
            let inside = &buf[start + 1..pos - 1];
            if self.canonical {
                if inside.is_empty() {
                    self.not_canonical("empty text");
                } else if is_plain(&decode_text(inside)) {
                    self.not_canonical("gratuitous text");
                }
            }
            let p = self.layout(pos, stop);
            let after = self.trailing_notes(p, stop)?;
            if two_values(after) {
                return Err(Error::syntax("two values in one unit", after));
            }
            Ok(after)
        } else if b == DLE {
            let pos = self.level(pos, stop)?;
            let p = self.layout(pos, stop);
            let after = self.trailing_notes(p, stop)?;
            if two_values(after) {
                return Err(Error::syntax("two values in one unit", after));
            }
            Ok(after)
        } else if b == ENQ {
            if heading {
                return Err(Error::syntax("reference in a heading", pos));
            }
            let pos = self.layout(pos + 1, stop);
            if pos >= stop || (buf[pos] < 0x20 && buf[pos] != STX) {
                return Err(Error::syntax("reference without a name", pos));
            }
            let pos = if buf[pos] == STX {
                let start = pos;
                let pos = scan::text(buf, pos, stop)?;
                let inside = decode_text(&buf[start + 1..pos - 1]);
                let segs: Vec<&[u8]> = inside.split(|&b| b == US).collect();
                if segs.iter().any(|s| s.is_empty()) {
                    return Err(Error::syntax("reference path with an empty segment", start));
                }
                if segs.len() > 3 {
                    return Err(Error::syntax(
                        "reference path with more than three segments",
                        start,
                    ));
                }
                if segs.len() == 1 {
                    self.not_canonical("group reference written as text");
                }
                pos
            } else {
                scan::plain(buf, pos, stop)
            };
            let p = self.layout(pos, stop);
            self.trailing_notes(p, stop)
        } else if b == BEL {
            // a note or dead level in unit position: the unit is empty
            let p2 = scan::layout(buf, pos + 1, stop);
            if p2 >= stop {
                return Err(Error::end("BEL at end of input", p2));
            }
            let pos = if buf[p2] == DLE {
                self.not_canonical("dead element");
                self.level(p2, stop)?
            } else if buf[p2] < 0x20 && buf[p2] != STX {
                return Ok(pos); // a dead record-level element: the caller's
            } else {
                self.not_canonical("comment");
                if buf[p2] == STX {
                    scan::text(buf, p2, stop)?
                } else {
                    scan::plain(buf, p2, stop)
                }
            };
            Ok(self.layout(pos, stop))
        } else {
            Ok(pos)
        }
    }

    // Notes (BEL + content) after a value inside a unit; a BEL before a code
    // is left for the caller.
    fn trailing_notes(&mut self, mut pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        while pos < stop && buf[pos] == BEL {
            let p2 = scan::layout(buf, pos + 1, stop);
            if p2 >= stop {
                return Err(Error::end("BEL at end of input", p2));
            }
            if !(buf[p2] >= 0x20 || buf[p2] == STX) {
                break;
            }
            self.not_canonical("comment");
            pos = if buf[p2] == STX {
                scan::text(buf, p2, stop)?
            } else {
                scan::plain(buf, p2, stop)
            };
            pos = self.layout(pos, stop);
        }
        Ok(pos)
    }

    // DLE at pos: a nested level. Returns the position after CAN.
    fn level(&mut self, pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        let pos = self.layout(pos + 1, stop);
        if pos >= stop {
            return Err(Error::end("level not closed", pos));
        }
        let b = buf[pos];
        if b == CAN {
            return Ok(pos + 1);
        }
        if !matches!(b, FS | GS | RS | SOH | BEL) {
            return Err(Error::syntax(
                "DLE must be followed by a structural code",
                pos,
            ));
        }
        let pos = self.body(pos, stop, false)?;
        let pos = self.layout(pos, stop);
        if pos >= stop {
            return Err(Error::end("level not closed", pos));
        }
        if buf[pos] != CAN {
            return Err(Error::syntax("unexpected byte inside level", pos));
        }
        Ok(pos + 1)
    }

    fn plain_name(&mut self, pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        let pos = self.layout(pos, stop);
        if pos < stop && buf[pos] == STX {
            return Err(Error::syntax("a name cannot be text", pos));
        }
        let start = pos;
        let pos = scan::plain(buf, pos, stop);
        if pos > start && is_layout(buf[pos - 1]) {
            self.not_canonical("layout bytes");
        }
        Ok(pos)
    }

    // ---- diff body -----------------------------------------------------

    fn diff_body(&mut self, pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        let mut pos = self.layout(pos, stop);
        while pos < stop {
            let b = buf[pos];
            if b == FS {
                pos = self.plain_name(pos + 1, stop)?;
            } else if b == GS {
                pos = self.pattern(pos + 1, stop)?;
            } else if b == RS {
                pos = self.replace(pos + 1, stop)?;
            } else if b == BEL {
                let p2 = scan::layout(buf, pos + 1, stop);
                if p2 < stop && buf[p2] == FS {
                    self.not_canonical("dead element");
                    pos = self.plain_name(p2 + 1, stop)?;
                } else if p2 < stop && buf[p2] == GS {
                    self.not_canonical("dead element");
                    pos = self.pattern(p2 + 1, stop)?;
                } else if p2 < stop && buf[p2] == RS {
                    self.not_canonical("dead element");
                    pos = self.replace(p2 + 1, stop)?;
                } else {
                    let p = self.notes(pos, stop)?;
                    if p == pos {
                        return Err(Error::syntax(
                            "BEL before a code that is not an element",
                            p2,
                        ));
                    }
                    pos = p;
                }
            } else if b == ETB {
                self.not_canonical("commit");
                let p = self.layout(pos + 1, stop);
                pos = scan::plain(buf, p, stop);
            } else if b == EOT || b == SYN || b == EM {
                break;
            } else {
                return Err(Error::syntax("unexpected byte in a diff body", pos));
            }
            pos = self.layout(pos, stop);
        }
        Ok(pos)
    }

    fn dvalue(&mut self, pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        let mut pos = self.layout(pos, stop);
        if pos >= stop {
            return Ok(pos);
        }
        if buf[pos] == STX {
            pos = scan::text(buf, pos, stop)?;
        } else if buf[pos] >= 0x20 {
            pos = scan::plain(buf, pos, stop);
        }
        Ok(self.layout(pos, stop))
    }

    fn pattern(&mut self, mut pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        loop {
            pos = self.dvalue(pos, stop)?;
            if pos < stop && buf[pos] == SUB {
                pos = self.dvalue(pos + 1, stop)?;
            }
            pos = self.trailing_notes(pos, stop)?;
            if !(pos < stop && buf[pos] == US) {
                break;
            }
            pos += 1;
        }
        Ok(pos)
    }

    fn replace(&mut self, pos: usize, stop: usize) -> Result<usize> {
        let buf = self.buf;
        let start = scan::layout(buf, pos, stop);
        let pos = self.dvalue(pos, stop)?;
        if trim(&buf[start..pos]).is_empty() {
            return Err(Error::syntax("replace-all with an empty old part", start));
        }
        if !(pos < stop && buf[pos] == SUB) {
            return Err(Error::syntax("replace-all without SUB", pos));
        }
        self.dvalue(pos + 1, stop)
    }
}
