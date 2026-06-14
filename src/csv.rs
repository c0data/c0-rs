//! CSV ⇄ C0DATA conversion.

use crate::*;

/// Convert CSV text to C0DATA compact bytes.
///
/// The first row is treated as headers, the rest as records, producing a
/// single group (`GS` + name + `SOH` headers + `RS` records). Returns an
/// empty buffer for empty input.
pub fn from_csv(input: &str, group_name: &str) -> Vec<u8> {
    let rows = parse_csv(input);
    if rows.is_empty() {
        return Vec::new();
    }
    let headers: Vec<&str> = rows[0].iter().map(|s| s.as_str()).collect();
    Builder::build(|b| {
        b.group(group_name, Some(&headers));
        for row in &rows[1..] {
            let fields: Vec<&str> = row.iter().map(|s| s.as_str()).collect();
            b.record(&fields);
        }
    })
}

/// Convert C0DATA compact bytes to CSV text.
///
/// Reads the first table in the buffer (FS-prefixed document or a bare
/// group): headers, if present, become the first row, then each record.
pub fn to_csv(buf: &[u8]) -> String {
    let table = find_table(buf);
    let mut out = String::new();

    if table.header_count() > 0 {
        let row: Vec<String> = (0..table.header_count())
            .map(|i| csv_quote(&String::from_utf8_lossy(table.header(i))))
            .collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    for rec in table.records() {
        let row: Vec<String> = (0..rec.field_count())
            .map(|i| csv_quote(&String::from_utf8_lossy(&rec.value(i))))
            .collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}

// Find the first table in the buffer: a Document group if FS-prefixed, else
// a bare Table.
fn find_table(buf: &[u8]) -> Table<'_> {
    if buf.first() == Some(&FS) {
        let doc = Document::new(buf);
        if doc.group_count() > 0 {
            return doc.group(0).table();
        }
    }
    Table::new(buf)
}

// Minimal RFC 4180 CSV parser: comma-separated, `"`-quoted fields with
// doubled quotes, CR ignored, LF ends a row.
fn parse_csv(input: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut started = false; // any char seen on the current (pending) row
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        started = true;
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
        } else {
            match ch {
                '"' => in_quotes = true,
                ',' => row.push(std::mem::take(&mut field)),
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    started = false;
                }
                '\r' => {}
                _ => field.push(ch),
            }
        }
    }

    if started || !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

// Quote a field if it contains a comma, quote, or newline.
fn csv_quote(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        let mut s = String::with_capacity(field.len() + 2);
        s.push('"');
        for ch in field.chars() {
            if ch == '"' {
                s.push('"');
            }
            s.push(ch);
        }
        s.push('"');
        s
    } else {
        field.to_string()
    }
}
