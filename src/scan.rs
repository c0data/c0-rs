//! Scanning primitives. Each takes a buffer, a position and a stop, and
//! returns the position after the construct it skips; the fallible ones
//! fail on malformed input. They are the bricks the readers are built
//! from and are exposed for tools that index C0DATA themselves.

use crate::*;

/// Skip layout bytes.
#[inline]
pub fn layout(buf: &[u8], mut pos: usize, stop: usize) -> usize {
    while pos < stop && is_layout(buf[pos]) {
        pos += 1;
    }
    pos
}

/// Skip a plain run of bytes >= 0x20 (a label, key, payload or plain value
/// with its layout). Returns the position of the next control code or
/// `stop`.
#[inline]
pub fn plain(buf: &[u8], mut pos: usize, stop: usize) -> usize {
    while pos < stop && buf[pos] >= 0x20 {
        pos += 1;
    }
    pos
}

/// Skip a text (STX … ETX) starting at the STX at `pos`. Returns the
/// position after the ETX. Fails on an unclosed text, a bad escape, a
/// nested STX, or framing inside the text.
pub fn text(buf: &[u8], pos: usize, stop: usize) -> Result<usize> {
    let start = pos;
    let mut pos = pos + 1; // STX
    while pos < stop {
        match buf[pos] {
            DLE => {
                pos += 1;
                if pos >= stop {
                    return Err(Error::end("DLE at end of text", pos));
                }
                if !(buf[pos] < 0x20 && is_text_escaped(buf[pos])) {
                    return Err(Error::syntax(
                        "DLE before a byte that is not one of the seven",
                        pos,
                    ));
                }
                pos += 1;
            }
            ETX => return Ok(pos + 1),
            STX => return Err(Error::syntax("STX inside text (text does not nest)", pos)),
            ETB | EOT | SYN | EM => return Err(Error::syntax("framing inside open text", pos)),
            _ => pos += 1,
        }
    }
    Err(Error::end(
        format!("text opened at {start} is not closed"),
        pos,
    ))
}

/// Skip a nested level starting at the DLE at `pos`. Returns the position
/// after the matching CAN.
pub fn level(buf: &[u8], pos: usize, stop: usize) -> Result<usize> {
    let start = pos;
    let mut pos = pos + 1; // DLE
    let mut depth = 1;
    while pos < stop {
        let byte = buf[pos];
        if byte >= 0x20 {
            pos += 1;
        } else if byte == STX {
            pos = text(buf, pos, stop)?;
        } else if byte == DLE {
            depth += 1;
            pos += 1;
        } else if byte == CAN {
            depth -= 1;
            pos += 1;
            if depth == 0 {
                return Ok(pos);
            }
        } else if byte == ETB || byte == EOT || byte == SYN || byte == EM {
            return Err(Error::syntax("framing inside open level", pos));
        } else {
            pos += 1;
        }
    }
    Err(Error::end(
        format!("level opened at {start} is not closed"),
        pos,
    ))
}

/// From `pos` (just after a US, RS or a key's US), find the end of one
/// unit's value: the next control code at this depth other than the ones
/// that belong to the value (STX…ETX, DLE…CAN, ENQ, BEL note). Layout at
/// the edges is included in the returned range.
pub fn unit(buf: &[u8], mut pos: usize, stop: usize) -> Result<usize> {
    while pos < stop {
        let byte = buf[pos];
        if byte >= 0x20 || is_layout(byte) {
            pos += 1;
        } else if byte == STX {
            pos = text(buf, pos, stop)?;
        } else if byte == DLE {
            pos = level(buf, pos, stop)?;
        } else if byte == ENQ {
            pos += 1;
        } else if byte == BEL {
            // a note or a dead level stays inside the unit; a dead
            // record-level element (BEL RS / SOH / GS / FS) ends it
            let p2 = layout(buf, pos + 1, stop);
            if p2 < stop && buf[p2] < 0x20 && buf[p2] != STX && buf[p2] != DLE {
                return Ok(pos);
            }
            pos = comment(buf, pos, stop)?;
        } else {
            return Ok(pos);
        }
    }
    Ok(pos)
}

/// Skip a comment starting at the BEL at `pos`: a note (text or plain to
/// the next code) or a dead element (the next code's whole element).
/// Returns the position after it.
pub fn comment(buf: &[u8], pos: usize, stop: usize) -> Result<usize> {
    let pos = layout(buf, pos + 1, stop); // BEL
    if pos >= stop {
        return Err(Error::end("BEL at end of input", pos));
    }
    let byte = buf[pos];
    if byte >= 0x20 {
        Ok(plain(buf, pos, stop))
    } else if byte == STX {
        text(buf, pos, stop)
    } else if byte == DLE {
        level(buf, pos, stop)
    } else if byte == RS {
        record_end(buf, pos + 1, stop)
    } else if byte == SOH {
        // header row or keyed unit
        let p2 = layout(buf, pos + 1, stop);
        if p2 < stop && buf[p2] == RS {
            record_end(buf, p2 + 1, stop)
        } else {
            keyed_unit_end(buf, pos + 1, stop)
        }
    } else if byte == GS {
        let mut p2 = pos;
        while p2 < stop && (buf[p2] == GS || is_layout(buf[p2])) {
            p2 += 1;
        }
        Ok(plain(buf, p2, stop))
    } else if byte == FS {
        Ok(plain(buf, pos + 1, stop))
    } else {
        Err(Error::syntax(
            "BEL before a code that is not an element",
            pos,
        ))
    }
}

/// From just after an RS, find the end of the record: the next
/// record-level code at this depth. Returns that position.
pub fn record_end(buf: &[u8], mut pos: usize, stop: usize) -> Result<usize> {
    while pos < stop {
        let byte = buf[pos];
        if byte >= 0x20 || is_layout(byte) || byte == US || byte == ENQ {
            pos += 1;
        } else if byte == SOH {
            // a header row (SOH RS) ends the record; a keyed unit is inside it
            let p2 = layout(buf, pos + 1, stop);
            if p2 < stop && buf[p2] == RS {
                return Ok(pos);
            }
            pos += 1;
        } else if byte == STX {
            pos = text(buf, pos, stop)?;
        } else if byte == DLE {
            pos = level(buf, pos, stop)?;
        } else if byte == BEL {
            // a note inside the record, or a dead element; a dead
            // record-level element (BEL RS / BEL GS …) ends this record first.
            let p2 = layout(buf, pos + 1, stop);
            if p2 < stop && (buf[p2] == RS || buf[p2] == GS || buf[p2] == FS) {
                return Ok(pos);
            }
            if p2 < stop && buf[p2] == SOH {
                let p3 = layout(buf, p2 + 1, stop);
                if p3 < stop && buf[p3] == RS {
                    return Ok(pos);
                }
            }
            pos = comment(buf, pos, stop)?;
        } else {
            return Ok(pos);
        }
    }
    Ok(pos)
}

/// From just after an SOH (not a header row), skip the key, US and value
/// of one keyed unit. Returns the position after the value.
pub fn keyed_unit_end(buf: &[u8], pos: usize, stop: usize) -> Result<usize> {
    let pos = plain(buf, layout(buf, pos, stop), stop);
    if !(pos < stop && buf[pos] == US) {
        return Err(Error::syntax("key without a value (no US)", pos));
    }
    unit(buf, pos + 1, stop)
}
