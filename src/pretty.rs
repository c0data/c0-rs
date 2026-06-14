//! Pretty form: Unicode Control Pictures (U+2400 block) with indentation.
//!
//! This core port implements [`format`]/[`format_with`] (compact layout) and
//! the round-tripping [`parse`]. Column-alignment modes are not yet ported.

use crate::*;
use std::str::Chars;

/// Convert a C0 control byte to its Unicode Control Picture character.
#[inline]
pub fn glyph(byte: u8) -> char {
    char::from_u32(0x2400 + byte as u32).unwrap_or('\u{FFFD}')
}

#[inline]
fn push_glyph(out: &mut Vec<u8>, byte: u8) {
    let mut tmp = [0u8; 4];
    out.extend_from_slice(glyph(byte).encode_utf8(&mut tmp).as_bytes());
}

/// Format a compact buffer as a human-readable Unicode string with newlines
/// and two-space indentation.
pub fn format(buf: &[u8]) -> String {
    format_with(buf, "  ")
}

/// Format with a custom indent string.
pub fn format_with(buf: &[u8], indent: &str) -> String {
    let mut out: Vec<u8> = Vec::new();
    format_compact(buf, indent, &mut out);
    // Glyphs and conventionally-UTF-8 values reassemble into valid UTF-8.
    String::from_utf8_lossy(&out).into_owned()
}

fn format_compact(buf: &[u8], indent: &str, out: &mut Vec<u8>) {
    let len = buf.len();
    let mut pos = 0;
    let mut depth: i32 = 0;
    let mut line_start = true;

    while pos < len {
        let byte = buf[pos];
        if byte >= 0x20 {
            out.push(byte);
            pos += 1;
            line_start = false;
            continue;
        }
        match byte {
            FS => {
                if !line_start {
                    out.push(b'\n');
                }
                push_glyph(out, byte);
                pos += 1;
                depth = 1;
                pos = write_data_until_control(buf, pos, out);
                out.push(b'\n');
                line_start = true;
            }
            GS => {
                let mut gs_run = 0;
                while pos < len && buf[pos] == GS {
                    gs_run += 1;
                    pos += 1;
                }
                if !line_start {
                    out.push(b'\n');
                }
                write_indent(out, indent, depth);
                for _ in 0..gs_run {
                    push_glyph(out, GS);
                }
                pos = write_data_until_control(buf, pos, out);
                out.push(b'\n');
                line_start = true;
            }
            SOH => {
                write_indent(out, indent, depth + 1);
                push_glyph(out, byte);
                pos += 1;
                pos = write_fields_line(buf, pos, out);
                out.push(b'\n');
                line_start = true;
            }
            RS => {
                write_indent(out, indent, depth + 1);
                push_glyph(out, byte);
                pos += 1;
                pos = write_fields_line(buf, pos, out);
                out.push(b'\n');
                line_start = true;
            }
            STX => {
                push_glyph(out, byte);
                pos += 1;
                depth += 1;
                out.push(b'\n');
                line_start = true;
            }
            ETX => {
                if depth > 0 {
                    depth -= 1;
                }
                write_indent(out, indent, depth + 1);
                push_glyph(out, byte);
                pos += 1;
            }
            EOT => {
                if !line_start {
                    out.push(b'\n');
                }
                push_glyph(out, byte);
                out.push(b'\n');
                pos += 1;
                line_start = true;
            }
            ENQ => {
                push_glyph(out, byte);
                pos += 1;
            }
            DLE => {
                push_glyph(out, byte);
                pos += 1;
                if pos < len {
                    if buf[pos] < 0x20 {
                        push_glyph(out, buf[pos]);
                    } else {
                        out.push(buf[pos]);
                    }
                    pos += 1;
                }
            }
            SUB | US => {
                push_glyph(out, byte);
                pos += 1;
            }
            ETB => {
                // Commit marker not attached to a record line. Indented on its
                // own line with any payload.
                if line_start {
                    write_indent(out, indent, depth + 1);
                }
                push_glyph(out, byte);
                pos += 1;
                pos = write_data_until_control(buf, pos, out);
                out.push(b'\n');
                line_start = true;
            }
            _ => {
                push_glyph(out, byte);
                pos += 1;
            }
        }
    }
    if !line_start {
        out.push(b'\n');
    }
}

fn write_indent(out: &mut Vec<u8>, indent: &str, depth: i32) {
    for _ in 0..depth.max(0) {
        out.extend_from_slice(indent.as_bytes());
    }
}

fn write_data_until_control(buf: &[u8], mut pos: usize, out: &mut Vec<u8>) -> usize {
    while pos < buf.len() && buf[pos] >= 0x20 {
        out.push(buf[pos]);
        pos += 1;
    }
    pos
}

fn write_fields_line(buf: &[u8], mut pos: usize, out: &mut Vec<u8>) -> usize {
    let len = buf.len();
    while pos < len {
        let byte = buf[pos];
        if byte == US {
            push_glyph(out, US);
            pos += 1;
        } else if byte == DLE {
            push_glyph(out, DLE);
            pos += 1;
            if pos < len {
                if buf[pos] < 0x20 {
                    push_glyph(out, buf[pos]);
                } else {
                    out.push(buf[pos]);
                }
                pos += 1;
            }
        } else if byte == ENQ {
            push_glyph(out, ENQ);
            pos += 1;
        } else if byte == ETB {
            // Commit marker stays on the record's line, with any payload.
            push_glyph(out, ETB);
            pos += 1;
            while pos < len && buf[pos] >= 0x20 {
                out.push(buf[pos]);
                pos += 1;
            }
        } else if byte == STX {
            push_glyph(out, STX);
            pos += 1;
            while pos < len {
                let b = buf[pos];
                if b == ETX {
                    push_glyph(out, ETX);
                    pos += 1;
                    break;
                } else if b == US {
                    push_glyph(out, US);
                    pos += 1;
                } else if b < 0x20 {
                    push_glyph(out, b);
                    pos += 1;
                } else {
                    out.push(b);
                    pos += 1;
                }
            }
        } else if byte < 0x20 {
            break; // next structural code
        } else {
            out.push(byte);
            pos += 1;
        }
    }
    pos
}

/// Parse pretty-form text back to compact bytes.
///
/// Control Pictures (U+2400–U+241F) become C0 bytes; LF/CR are ignored;
/// whitespace adjacent to control codes is trimmed; inside STX/ETX everything
/// is preserved verbatim.
pub fn parse(s: &str) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut ws: Vec<u8> = Vec::new();
    let mut trim_after = true;
    let mut chars = s.chars();

    while let Some(ch) = chars.next() {
        let cp = ch as u32;
        if (0x2400..=0x241F).contains(&cp) {
            let code = (cp - 0x2400) as u8;
            ws.clear();
            out.push(code);
            if code == STX {
                parse_quoted(&mut chars, &mut out);
            }
            trim_after = true;
        } else if ch == '\n' || ch == '\r' {
            ws.clear();
            trim_after = true;
        } else if ch == ' ' || ch == '\t' {
            if trim_after {
                continue;
            }
            ws.push(ch as u8);
        } else {
            trim_after = false;
            if !ws.is_empty() {
                out.extend_from_slice(&ws);
                ws.clear();
            }
            let mut tmp = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
        }
    }
    out
}

fn parse_quoted(chars: &mut Chars, out: &mut Vec<u8>) {
    let mut depth = 1;
    for ch in chars.by_ref() {
        let cp = ch as u32;
        if (0x2400..=0x241F).contains(&cp) {
            let code = (cp - 0x2400) as u8;
            out.push(code);
            if code == STX {
                depth += 1;
            } else if code == ETX {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
        } else {
            let mut tmp = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
        }
    }
}

// --- Column alignment ---

/// Pretty layout mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatMode {
    /// No padding between fields.
    Compact,
    /// Column-aligned fields within table groups.
    Aligned,
    /// Column-aligned, plus a space after prefix glyphs and around `␟`.
    Spaced,
}

/// Format with a layout mode (`Compact` is what [`format`] produces).
pub fn format_mode(buf: &[u8], indent: &str, mode: FormatMode) -> String {
    let compact = format_with(buf, indent);
    match mode {
        FormatMode::Compact => compact,
        _ => align(&compact, mode),
    }
}

struct TableLine {
    line_index: usize,
    prefix: String,
    fields: Vec<String>,
}

/// Reformat a compact pretty string with column alignment.
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

    // Format table groups with column alignment.
    for group in &groups {
        if group.is_empty() {
            continue;
        }
        let col_count = group[0].fields.len();
        let mut max_widths = vec![0usize; col_count];
        for line in group {
            for (col, field) in line.fields.iter().enumerate() {
                max_widths[col] = max_widths[col].max(field.chars().count());
            }
        }
        for line in group {
            let mut text = String::new();
            text.push_str(&line.prefix);
            text.push_str(sp);
            let n = line.fields.len();
            for (col, field) in line.fields.iter().enumerate() {
                if col < n - 1 {
                    text.push_str(field);
                    let pad = max_widths[col].saturating_sub(field.chars().count());
                    text.extend(std::iter::repeat(' ').take(pad));
                    text.push_str(sp);
                    text.push(g_us);
                    text.push_str(sp);
                } else {
                    text.push_str(field);
                }
            }
            lines[line.line_index] = text;
        }
    }

    // Format non-table lines: add/remove a space after prefix glyphs.
    let prefixes = [glyph(FS), glyph(GS), glyph(RS), glyph(SOH), glyph(US)];
    let g_soh = glyph(SOH);
    // Index needed for both the table-line lookup and the in-place rewrite.
    #[allow(clippy::needless_range_loop)]
    for i in 0..lines.len() {
        if table_lines.contains(&i) || lines[i].trim().is_empty() {
            continue;
        }
        let chars: Vec<char> = lines[i].chars().collect();

        let mut ws_end = 0;
        while ws_end < chars.len() && (chars[ws_end] == ' ' || chars[ws_end] == '\t') {
            ws_end += 1;
        }
        let mut glyph_end = ws_end;
        while glyph_end < chars.len() && prefixes.contains(&chars[glyph_end]) {
            glyph_end += 1;
        }
        if glyph_end > ws_end && glyph_end < chars.len() && chars[glyph_end] == g_soh {
            glyph_end += 1;
        }
        if glyph_end == ws_end || glyph_end >= chars.len() {
            continue;
        }

        let indent_str: String = chars[0..ws_end].iter().collect();
        let glyphs: String = chars[ws_end..glyph_end].iter().collect();
        let rest: String = chars[glyph_end..].iter().collect();
        let rest = rest.trim_start();
        lines[i] = if spaced {
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
    let g_fs = glyph(FS);
    let g_gs = glyph(GS);
    let g_eot = glyph(EOT);
    let g_soh = glyph(SOH);
    let g_rs = glyph(RS);

    let mut groups: Vec<Vec<TableLine>> = Vec::new();
    let mut current: Option<Vec<TableLine>> = None;
    let mut expected_cols: i32 = -1;

    let close = |groups: &mut Vec<Vec<TableLine>>, current: &mut Option<Vec<TableLine>>| {
        if let Some(cur) = current.take() {
            if !cur.is_empty() {
                groups.push(cur);
            }
        }
    };

    for (i, text) in lines.iter().enumerate() {
        let trimmed = text.trim();
        let first = trimmed.chars().next();

        if trimmed.is_empty() || first == Some(g_gs) || first == Some(g_fs) || first == Some(g_eot)
        {
            close(&mut groups, &mut current);
            expected_cols = -1;
            continue;
        }

        if first == Some(g_soh) || first == Some(g_rs) {
            match parse_table_line(i, text) {
                Some(parsed) if parsed.fields.len() >= 2 => {
                    let col_count = parsed.fields.len() as i32;
                    if current.is_none() || (expected_cols != -1 && col_count != expected_cols) {
                        close(&mut groups, &mut current);
                        current = Some(Vec::new());
                        expected_cols = col_count;
                    }
                    current.as_mut().unwrap().push(parsed);
                }
                _ => {
                    close(&mut groups, &mut current);
                    expected_cols = -1;
                }
            }
        } else {
            close(&mut groups, &mut current);
            expected_cols = -1;
        }
    }

    close(&mut groups, &mut current);
    groups
}

fn parse_table_line(line_index: usize, text: &str) -> Option<TableLine> {
    let g_soh = glyph(SOH);
    let g_rs = glyph(RS);
    let g_us = glyph(US);
    let g_dle = glyph(DLE);
    let chars: Vec<char> = text.chars().collect();

    let mut marker_pos: Option<usize> = None;
    for (i, &ch) in chars.iter().enumerate() {
        if ch == ' ' || ch == '\t' {
            continue;
        }
        if ch == g_soh || ch == g_rs {
            marker_pos = Some(i);
        }
        break;
    }
    let marker_pos = marker_pos?;

    let prefix: String = chars[0..=marker_pos].iter().collect();
    let rest = &chars[(marker_pos + 1)..];

    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == g_dle && i + 1 < rest.len() {
            field.push(rest[i]);
            field.push(rest[i + 1]);
            i += 2;
        } else if rest[i] == g_us {
            fields.push(field.trim().to_string());
            field = String::new();
            i += 1;
        } else {
            field.push(rest[i]);
            i += 1;
        }
    }
    fields.push(field.trim().to_string());

    if fields.len() < 2 {
        return None;
    }
    Some(TableLine {
        line_index,
        prefix,
        fields,
    })
}
