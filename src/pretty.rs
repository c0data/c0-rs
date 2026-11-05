//! Pretty form: Unicode Control Pictures (U+2400 block) for the codes,
//! line breaks and indentation as layout. [`format`] and [`parse`] are
//! inverses on well-formed input.

use crate::*;

/// Pretty layout mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatMode {
    /// One element per line, no padding.
    Compact,
    /// Column-aligned units within table groups.
    Aligned,
    /// Aligned, plus a space after the prefix and around `␟`.
    Spaced,
}

/// The Control Picture for a control byte.
#[inline]
pub fn glyph(byte: u8) -> char {
    char::from_u32(0x2400 + byte as u32).unwrap_or('\u{FFFD}')
}

#[inline]
fn push_glyph(out: &mut Vec<u8>, byte: u8) {
    let mut tmp = [0u8; 4];
    out.extend_from_slice(glyph(byte).encode_utf8(&mut tmp).as_bytes());
}

/// Format compact bytes as pretty form, two-space indent, compact mode.
pub fn format(buf: &[u8]) -> Result<String> {
    format_with(buf, "  ")
}

/// Format with a custom indent string, compact mode.
pub fn format_with(buf: &[u8], indent: &str) -> Result<String> {
    let mut out = Vec::new();
    format_compact(buf, indent, &mut out)?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Format with a layout mode.
pub fn format_mode(buf: &[u8], indent: &str, mode: FormatMode) -> Result<String> {
    let compact = format_with(buf, indent)?;
    Ok(match mode {
        FormatMode::Compact => compact,
        _ => align(&compact, mode),
    })
}

// ---- formatting ----------------------------------------------------

fn newline_unless(out: &mut Vec<u8>, line_start: bool) {
    if !line_start {
        out.push(b'\n');
    }
}

fn format_compact(buf: &[u8], indent: &str, out: &mut Vec<u8>) -> Result<()> {
    let len = buf.len();
    let mut pos = 0;
    let mut depth = 0usize; // nesting depth (levels)
    let mut gdepth = 0usize; // group depth within the current level
    let mut line_start = true;
    let mut stack: Vec<usize> = Vec::new();

    while pos < len {
        let byte = buf[pos];
        if byte >= 0x20 || is_layout(byte) {
            // stray plain or layout at element level: layout is dropped
            if is_layout(byte) {
                pos += 1;
            } else {
                let start = pos;
                pos = scan::plain(buf, pos, len);
                write_data(&buf[start..pos], out)?;
                line_start = false;
            }
            continue;
        }
        match byte {
            SYN => {
                newline_unless(out, line_start);
                push_glyph(out, SYN);
                pos = write_heading(buf, pos + 1, len, out)?;
                out.push(b'\n');
                line_start = true;
                gdepth = 0;
            }
            FS => {
                newline_unless(out, line_start);
                write_indent(out, indent, depth);
                push_glyph(out, FS);
                pos = write_plain(buf, pos + 1, len, out)?;
                out.push(b'\n');
                line_start = true;
                gdepth = 1;
            }
            GS => {
                let mut run = 0;
                while pos < len && buf[pos] == GS {
                    run += 1;
                    pos += 1;
                }
                newline_unless(out, line_start);
                write_indent(out, indent, depth);
                for _ in 0..run {
                    push_glyph(out, GS);
                }
                pos = write_plain(buf, pos, len, out)?;
                out.push(b'\n');
                line_start = true;
                gdepth = 1;
            }
            SOH | RS | BEL | ETB => {
                newline_unless(out, line_start);
                write_indent(out, indent, depth + gdepth);
                let (p, d, ls) = write_element(buf, pos, len, out, depth, gdepth, &mut stack)?;
                pos = p;
                depth = d;
                line_start = ls;
            }
            CAN => {
                depth = depth.saturating_sub(1);
                gdepth = stack.pop().unwrap_or(0);
                newline_unless(out, line_start);
                write_indent(out, indent, depth + gdepth);
                push_glyph(out, CAN);
                pos += 1;
                // the rest of the enclosing record continues on this line
                let (p, d, ls) = write_units(buf, pos, len, out, depth, gdepth, &mut stack)?;
                pos = p;
                depth = d;
                line_start = ls;
                if !line_start {
                    out.push(b'\n');
                    line_start = true;
                }
            }
            EOT => {
                newline_unless(out, line_start);
                push_glyph(out, EOT);
                out.push(b'\n');
                pos += 1;
                line_start = true;
            }
            EM => {
                newline_unless(out, line_start);
                push_glyph(out, EM);
                let tail = len - pos - 1;
                if tail > 0 {
                    out.extend_from_slice(format!(" {tail} bytes").as_bytes());
                }
                out.push(b'\n');
                return Ok(());
            }
            STX | DLE | ENQ | US | SUB => {
                // a unit-level code at element level (e.g. a diff pattern): inline
                let (p, d, ls) = write_units(buf, pos, len, out, depth, gdepth, &mut stack)?;
                pos = p;
                depth = d;
                let _ = ls;
                out.push(b'\n');
                line_start = true;
            }
            _ => {
                return Err(Error::UnassignedCode {
                    byte,
                    position: pos,
                })
            }
        }
    }
    if !line_start {
        out.push(b'\n');
    }
    Ok(())
}

// SOH / RS / BEL / ETB at element level: the element and its units on one
// line (a level opener breaks the line). Returns (pos, depth, line_start).
fn write_element(
    buf: &[u8],
    mut pos: usize,
    len: usize,
    out: &mut Vec<u8>,
    mut depth: usize,
    gdepth: usize,
    stack: &mut Vec<usize>,
) -> Result<(usize, usize, bool)> {
    let byte = buf[pos];
    let mut line_start = false;
    match byte {
        BEL => {
            push_glyph(out, BEL);
            pos = scan::layout(buf, pos + 1, len);
            if pos < len && (buf[pos] >= 0x20 || buf[pos] == STX) {
                let (p, d, _ls) = write_units(buf, pos, len, out, depth, gdepth, stack)?;
                out.push(b'\n');
                return Ok((p, d, true));
            }
            // dead element: render it on this line
            if pos >= len {
                return Ok((pos, depth, false));
            }
            return write_element(buf, pos, len, out, depth, gdepth, stack);
        }
        SOH => {
            push_glyph(out, SOH);
            pos = scan::layout(buf, pos + 1, len);
            if pos < len && buf[pos] == RS {
                push_glyph(out, RS);
                pos += 1;
            }
            let (p, d, ls) = write_units(buf, pos, len, out, depth, gdepth, stack)?;
            pos = p;
            depth = d;
            line_start = ls;
        }
        RS => {
            push_glyph(out, RS);
            let (p, d, ls) = write_units(buf, pos + 1, len, out, depth, gdepth, stack)?;
            pos = p;
            depth = d;
            line_start = ls;
        }
        ETB => {
            push_glyph(out, ETB);
            pos = write_plain(buf, pos + 1, len, out)?;
        }
        _ => {}
    }
    if !line_start {
        out.push(b'\n');
        line_start = true;
    }
    Ok((pos, depth, line_start))
}

// Units of a record/header/heading on the current line. Stops at a
// record-level code. A level opener writes ␐, breaks the line and returns
// the new depth; the caller resumes at the matching CAN. Returns
// (pos, depth, line_start).
fn write_units(
    buf: &[u8],
    mut pos: usize,
    len: usize,
    out: &mut Vec<u8>,
    depth: usize,
    gdepth: usize,
    stack: &mut Vec<usize>,
) -> Result<(usize, usize, bool)> {
    while pos < len {
        let b = buf[pos];
        if b >= 0x20 {
            let start = pos;
            pos = scan::plain(buf, pos, len);
            write_data(trim(&buf[start..pos]), out)?;
        } else if is_layout(b) {
            pos += 1;
        } else if b == US || b == ENQ || b == SUB {
            push_glyph(out, b);
            pos += 1;
        } else if b == SOH {
            let p2 = scan::layout(buf, pos + 1, len);
            if p2 < len && buf[p2] == RS {
                break;
            }
            push_glyph(out, SOH);
            pos += 1;
        } else if b == STX {
            let stop = scan::text(buf, pos, len)?;
            write_text(&buf[pos..stop], out)?;
            pos = stop;
        } else if b == DLE {
            push_glyph(out, DLE);
            out.push(b'\n');
            stack.push(gdepth);
            return Ok((pos + 1, depth + 1, true));
        } else if b == BEL {
            let p2 = scan::layout(buf, pos + 1, len);
            if p2 < len && (buf[p2] >= 0x20 || buf[p2] == STX || buf[p2] == DLE) {
                push_glyph(out, BEL);
                pos = p2;
            } else {
                break;
            }
        } else if b == ETB {
            push_glyph(out, ETB);
            pos = write_plain(buf, pos + 1, len, out)?;
        } else {
            break;
        }
    }
    Ok((pos, depth, false))
}

fn write_heading(buf: &[u8], mut pos: usize, len: usize, out: &mut Vec<u8>) -> Result<usize> {
    while pos < len {
        let b = buf[pos];
        if b == SOH {
            let p2 = scan::layout(buf, pos + 1, len);
            if p2 < len && buf[p2] == RS {
                break;
            }
            push_glyph(out, SOH);
            pos = write_plain(buf, p2, len, out)?;
            if pos < len && buf[pos] == US {
                push_glyph(out, US);
                pos = scan::layout(buf, pos + 1, len);
                if pos < len && buf[pos] == STX {
                    let stop = scan::text(buf, pos, len)?;
                    write_text(&buf[pos..stop], out)?;
                    pos = stop;
                } else if pos < len && buf[pos] == DLE {
                    let stop = scan::level(buf, pos, len)?;
                    // a nested heading value is written inline
                    write_inline_level(&buf[pos..stop], out)?;
                    pos = stop;
                } else {
                    pos = write_plain(buf, pos, len, out)?;
                }
            }
        } else if b == BEL {
            let p2 = scan::layout(buf, pos + 1, len);
            if p2 < len && (buf[p2] >= 0x20 || buf[p2] == STX) {
                push_glyph(out, BEL);
                pos = p2;
                if buf[pos] == STX {
                    let stop = scan::text(buf, pos, len)?;
                    write_text(&buf[pos..stop], out)?;
                    pos = stop;
                } else {
                    pos = write_plain(buf, pos, len, out)?;
                }
            } else if p2 < len && buf[p2] == SOH {
                push_glyph(out, BEL);
                pos = p2;
            } else {
                break;
            }
        } else if is_layout(b) {
            pos += 1;
        } else {
            break;
        }
    }
    Ok(pos)
}

// A level written on one line (used for heading values).
fn write_inline_level(raw: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        if b == STX {
            let stop = scan::text(raw, i, raw.len())?;
            write_text(&raw[i..stop], out)?;
            i = stop;
        } else if b >= 0x20 {
            let start = i;
            i = scan::plain(raw, i, raw.len());
            write_data(trim(&raw[start..i]), out)?;
        } else if is_layout(b) {
            i += 1;
        } else {
            push_glyph(out, b);
            i += 1;
        }
    }
    Ok(())
}

fn write_plain(buf: &[u8], pos: usize, len: usize, out: &mut Vec<u8>) -> Result<usize> {
    let start = scan::layout(buf, pos, len);
    let pos = scan::plain(buf, start, len);
    write_data(trim(&buf[start..pos]), out)?;
    Ok(pos)
}

// A whole text (STX … ETX): glyphs for the brackets and for any control
// byte inside; everything else as is.
fn write_text(text: &[u8], out: &mut Vec<u8>) -> Result<()> {
    check_glyphs(text)?;
    push_glyph(out, STX);
    for &b in &text[1..text.len() - 1] {
        if b < 0x20 && b != LF && b != CR && b != HT {
            push_glyph(out, b);
        } else {
            out.push(b);
        }
    }
    push_glyph(out, ETX);
    Ok(())
}

fn write_data(data: &[u8], out: &mut Vec<u8>) -> Result<()> {
    check_glyphs(data)?;
    out.extend_from_slice(data);
    Ok(())
}

// Pretty form cannot show a value that contains a Control Picture
// character (U+2400–U+241F, UTF-8 E2 90 80–9F).
fn check_glyphs(data: &[u8]) -> Result<()> {
    if data
        .windows(3)
        .any(|w| w[0] == 0xE2 && w[1] == 0x90 && (0x80..=0x9F).contains(&w[2]))
    {
        return Err(Error::GlyphInData);
    }
    Ok(())
}

fn write_indent(out: &mut Vec<u8>, indent: &str, depth: usize) {
    for _ in 0..depth {
        out.extend_from_slice(indent.as_bytes());
    }
}

// ---- parsing ---------------------------------------------------------

fn glyph_code(ch: char) -> Option<u8> {
    let cp = ch as u32;
    (0x2400..=0x241F).contains(&cp).then(|| (cp - 0x2400) as u8)
}

fn push_char(out: &mut Vec<u8>, ch: char) {
    let mut tmp = [0u8; 4];
    out.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
}

/// Pretty form → compact bytes. Glyphs become codes; outside text, layout
/// touching a code is dropped and interior spaces are kept; inside text
/// everything is kept.
pub fn parse(s: &str) -> Vec<u8> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut pending: Vec<u8> = Vec::new(); // whitespace between data characters
    let mut after_code = true;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if let Some(code) = glyph_code(ch) {
            pending.clear();
            out.push(code);
            i += 1;
            if code == STX {
                i = parse_text(&chars, i, &mut out);
            } else if code == EM {
                // the rest of the line is a summary, not data
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            after_code = true;
        } else if ch == '\n' || ch == '\r' {
            pending.clear();
            after_code = true;
            i += 1;
        } else if ch == ' ' || ch == '\t' {
            if !after_code {
                pending.push(ch as u8);
            }
            i += 1;
        } else {
            out.append(&mut pending);
            after_code = false;
            push_char(&mut out, ch);
            i += 1;
        }
    }
    out
}

// Inside text: everything verbatim until the ETX glyph; a DLE glyph takes
// the next glyph with it.
fn parse_text(chars: &[char], mut i: usize, out: &mut Vec<u8>) -> usize {
    while i < chars.len() {
        let ch = chars[i];
        if let Some(code) = glyph_code(ch) {
            out.push(code);
            i += 1;
            if code == DLE {
                if let Some(nx) = chars.get(i).and_then(|&c| glyph_code(c)) {
                    out.push(nx);
                    i += 1;
                }
            } else if code == ETX {
                return i;
            }
        } else {
            push_char(out, ch);
            i += 1;
        }
    }
    i
}

// ---- alignment -------------------------------------------------------

struct TableLine {
    line_index: usize,
    prefix: String,
    fields: Vec<String>,
    // seps[i] stood between fields[i] and fields[i+1]: true for ␟, false
    // when the next field begins with its own ␁ mark.
    seps: Vec<bool>,
}

/// Reformat a pretty-form string with column alignment. Keyed records
/// (`␁` units) are aligned on their `␟` like two-column tables.
pub fn align(pretty: &str, mode: FormatMode) -> String {
    let mut lines: Vec<String> = pretty.split('\n').map(str::to_string).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let groups = find_table_groups(&lines);
    let spaced = mode == FormatMode::Spaced;
    let sp = if spaced { " " } else { "" };
    let g_us = glyph(US);

    let mut table_lines = std::collections::HashSet::new();
    for group in &groups {
        for line in group {
            table_lines.insert(line.line_index);
        }
    }

    for group in &groups {
        let col_count = group.iter().map(|l| l.fields.len()).max().unwrap_or(0);
        let mut max_widths = vec![0usize; col_count];
        for line in group {
            for (c, f) in line.fields.iter().enumerate() {
                max_widths[c] = max_widths[c].max(f.chars().count());
            }
        }
        for line in group {
            let mut text = String::new();
            text.push_str(&line.prefix);
            text.push_str(sp);
            let n = line.fields.len();
            for (col, field) in line.fields.iter().enumerate() {
                text.push_str(field);
                if col < n - 1 {
                    let pad = max_widths[col] - field.chars().count();
                    text.extend(std::iter::repeat_n(' ', pad));
                    if line.seps[col] {
                        text.push_str(sp);
                        text.push(g_us);
                        text.push_str(sp);
                    } else {
                        text.push(' ');
                    }
                }
            }
            lines[line.line_index] = text;
        }
    }

    let prefixes = [
        glyph(FS),
        glyph(GS),
        glyph(RS),
        glyph(SOH),
        glyph(US),
        glyph(SYN),
        glyph(BEL),
    ];
    for (i, line) in lines.iter_mut().enumerate() {
        if table_lines.contains(&i) || line.trim().is_empty() {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut ws_end = 0;
        while ws_end < chars.len() && (chars[ws_end] == ' ' || chars[ws_end] == '\t') {
            ws_end += 1;
        }
        let mut glyph_end = ws_end;
        while glyph_end < chars.len() && prefixes.contains(&chars[glyph_end]) {
            glyph_end += 1;
        }
        if glyph_end == ws_end || glyph_end >= chars.len() {
            continue;
        }
        let indent_str: String = chars[..ws_end].iter().collect();
        let glyphs: String = chars[ws_end..glyph_end].iter().collect();
        let rest: String = chars[glyph_end..].iter().collect();
        let rest = rest.trim_start();
        *line = if spaced {
            format!("{indent_str}{glyphs} {rest}")
        } else {
            format!("{indent_str}{glyphs}{rest}")
        };
    }

    let mut result = lines.join("\n");
    result.push('\n');
    result
}

fn find_table_groups(lines: &[String]) -> Vec<Vec<TableLine>> {
    let breakers = [glyph(GS), glyph(FS), glyph(EOT), glyph(SYN), glyph(CAN)];
    let g_soh = glyph(SOH);
    let g_rs = glyph(RS);
    let mut groups: Vec<Vec<TableLine>> = Vec::new();
    let mut current: Option<Vec<TableLine>> = None;

    fn close(groups: &mut Vec<Vec<TableLine>>, current: &mut Option<Vec<TableLine>>) {
        if let Some(cur) = current.take() {
            if !cur.is_empty() {
                groups.push(cur);
            }
        }
    }

    for (i, text) in lines.iter().enumerate() {
        let trimmed = text.trim();
        let first = trimmed.chars().next();
        if trimmed.is_empty() || first.is_some_and(|c| breakers.contains(&c)) {
            close(&mut groups, &mut current);
            continue;
        }
        if first == Some(g_soh) || first == Some(g_rs) {
            match parse_table_line(i, text) {
                Some(parsed) if parsed.fields.len() >= 2 => {
                    current.get_or_insert_with(Vec::new).push(parsed);
                }
                _ => close(&mut groups, &mut current),
            }
        } else {
            close(&mut groups, &mut current);
        }
    }
    close(&mut groups, &mut current);
    groups
}

// Split a ␞ or ␁␞ line into its units on ␟ (text is not split; a keyed
// record splits on its ␁ marks too, so key and value align).
fn parse_table_line(line_index: usize, text: &str) -> Option<TableLine> {
    let g_soh = glyph(SOH);
    let g_rs = glyph(RS);
    let g_us = glyph(US);
    let g_stx = glyph(STX);
    let g_etx = glyph(ETX);
    let g_dle = glyph(DLE);
    let g_can = glyph(CAN);
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
        i += 1;
    }
    if i >= chars.len() {
        return None;
    }
    if chars[i] == g_soh {
        i += 1;
        if !(i < chars.len() && chars[i] == g_rs) {
            return None;
        }
        i += 1;
    } else if chars[i] == g_rs {
        i += 1;
    } else {
        return None;
    }
    let prefix: String = chars[..i].iter().collect();
    let prefix_len = i;
    let mut fields = Vec::new();
    let mut seps = Vec::new();
    let mut field = String::new();
    let mut in_text = false;
    let keyed = i < chars.len() && chars[i] == g_soh;
    while i < chars.len() {
        let ch = chars[i];
        if in_text {
            field.push(ch);
            if ch == g_etx {
                in_text = false;
            }
            if ch == g_dle && i + 1 < chars.len() {
                field.push(chars[i + 1]);
                i += 1;
            }
        } else if ch == g_stx {
            in_text = true;
            field.push(ch);
        } else if ch == g_dle || ch == g_can {
            return None; // nested levels are not aligned
        } else if ch == g_us {
            fields.push(field.trim().to_string());
            seps.push(true);
            field = String::new();
        } else if keyed && ch == g_soh && i > prefix_len {
            fields.push(field.trim().to_string());
            seps.push(false);
            field = String::new();
            field.push(ch);
        } else {
            field.push(ch);
        }
        i += 1;
    }
    if in_text {
        return None; // text continues on the next line
    }
    fields.push(field.trim().to_string());
    if fields.len() < 2 {
        return None;
    }
    Some(TableLine {
        line_index,
        prefix,
        fields,
        seps,
    })
}
