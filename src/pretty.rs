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
