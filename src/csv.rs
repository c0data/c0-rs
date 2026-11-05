//! CSV ⇄ C0DATA conversion.

use crate::*;

/// CSV text → one group with a header row and positional records. Returns
/// an empty buffer for empty input.
pub fn from_csv(input: &str, group_name: &str) -> Vec<u8> {
    let rows = parse_csv(input);
    if rows.is_empty() {
        return Vec::new();
    }
    let headers: Vec<&str> = rows[0].iter().map(String::as_str).collect();
    Builder::build(|b| {
        b.group(group_name, Some(&headers));
        for row in &rows[1..] {
            b.record(row);
        }
    })
}

/// The first group of the buffer as CSV text (header row first when
/// present). A reference is written as its path (`tags/001`); a nested
/// level has no CSV form and is an error.
pub fn to_csv(buf: &[u8]) -> Result<String> {
    to_csv_group(buf, None)
}

/// The named group (or the first) as CSV text; see [`to_csv`].
pub fn to_csv_group(buf: &[u8], group_name: Option<&str>) -> Result<String> {
    let doc = Document::new(buf)?;
    let g = match group_name {
        Some(name) => doc
            .groups()
            .find(|g| g.name() == name.as_bytes())
            .ok_or_else(|| Error::Key(format!("no group named '{name}'")))?,
        None => doc
            .groups()
            .next()
            .ok_or_else(|| Error::other("no group in buffer"))?,
    };
    let mut out = String::new();
    if g.has_header() {
        let row: Vec<String> = g
            .headers()
            .iter()
            .map(|h| csv_quote(&String::from_utf8_lossy(h)))
            .collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    for rec in g.records() {
        let mut row = Vec::new();
        for (i, f) in rec.fields().into_iter().enumerate() {
            if is_level(f) {
                return Err(Error::other(format!(
                    "CSV cannot carry a nested level (group '{}', field {})",
                    String::from_utf8_lossy(g.name()),
                    i + 1
                )));
            } else if is_reference(f) {
                let r = Reference::parse(f)?;
                let segs: Vec<String> = r
                    .path()
                    .iter()
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect();
                row.push(csv_quote(&segs.join("/")));
            } else {
                row.push(csv_quote(&String::from_utf8_lossy(&value(f)?)));
            }
        }
        out.push_str(&row.join(","));
        out.push('\n');
    }
    Ok(out)
}

// Minimal RFC 4180 CSV parser: comma-separated, `"`-quoted fields with
// doubled quotes, CR ignored, LF ends a row.
fn parse_csv(input: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut started = false;
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
