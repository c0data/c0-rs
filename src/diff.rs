//! C0DIFF: atomic, anchored multi-file edits.
//!
//! Format: `[FS]<path>[GS]<literal>[US]<old>[SUB]<new>[US]<literal>`. FS starts
//! a file block, GS a pattern section, US separates pattern units (anchor ↔
//! replacement), SUB separates old from new within a replacement, and DLE
//! escapes literal control codes.

use crate::*;
use std::collections::HashMap;
use std::fmt;

/// A pattern unit: literal anchor text, or a substitution (old → new).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit {
    Anchor(Vec<u8>),
    Sub { old: Vec<u8>, new: Vec<u8> },
}

/// A section: a sequential pattern of units (anchors + substitutions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub units: Vec<Unit>,
}

impl Section {
    /// The search pattern (old text of every unit, concatenated).
    pub fn search_pattern(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in &self.units {
            match unit {
                Unit::Anchor(b) => out.extend_from_slice(b),
                Unit::Sub { old, .. } => out.extend_from_slice(old),
            }
        }
        out
    }

    /// The replacement (new text of every unit, concatenated).
    pub fn replacement(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in &self.units {
            match unit {
                Unit::Anchor(b) => out.extend_from_slice(b),
                Unit::Sub { new, .. } => out.extend_from_slice(new),
            }
        }
        out
    }
}

/// A file edit: a path and its sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    pub path: Vec<u8>,
    pub sections: Vec<Section>,
}

/// Errors raised while applying a C0DIFF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError {
    FileNotFound(String),
    /// A section's pattern was not found in the target file.
    PatternNotFound {
        path: String,
        section: usize,
    },
    /// A section's pattern matched more than once (must be exactly one).
    PatternAmbiguous {
        path: String,
        section: usize,
        count: usize,
    },
}

impl fmt::Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiffError::FileNotFound(p) => write!(f, "File not found: {p}"),
            DiffError::PatternNotFound { path, section } => {
                write!(f, "Pattern not found in {path} (section {section})")
            }
            DiffError::PatternAmbiguous {
                path,
                section,
                count,
            } => write!(
                f,
                "Pattern found {count} times in {path} (section {section}), expected exactly 1"
            ),
        }
    }
}

impl std::error::Error for DiffError {}

/// Parse a C0DIFF buffer into a list of file edits.
pub fn parse(buf: &[u8]) -> Vec<FileEdit> {
    let mut edits = Vec::new();
    let len = buf.len();
    let mut pos = 0;
    while pos < len {
        let byte = buf[pos];
        if byte == EOT {
            break;
        }
        if byte == FS {
            pos += 1;
            let (edit, next) = parse_file(buf, pos);
            edits.push(edit);
            pos = next;
        } else {
            pos += 1;
        }
    }
    edits
}

fn parse_file(buf: &[u8], mut pos: usize) -> (FileEdit, usize) {
    let len = buf.len();

    let path_start = pos;
    while pos < len && buf[pos] >= 0x20 {
        pos += 1;
    }
    let path = buf[path_start..pos].to_vec();

    let mut sections = Vec::new();
    while pos < len {
        let byte = buf[pos];
        if byte == FS || byte == EOT {
            break;
        }
        if byte == GS {
            pos += 1;
            let (units, next) = parse_section(buf, pos);
            sections.push(Section { units });
            pos = next;
        } else {
            pos += 1;
        }
    }

    (FileEdit { path, sections }, pos)
}

fn parse_section(buf: &[u8], mut pos: usize) -> (Vec<Unit>, usize) {
    let len = buf.len();
    let mut units: Vec<Unit> = Vec::new();
    let mut in_sub = false;
    let mut data_start = pos;

    // Close the current data span into a unit (or complete a pending Sub).
    fn flush(units: &mut Vec<Unit>, in_sub: &mut bool, span: Vec<u8>) {
        if *in_sub {
            let old = match units.pop() {
                Some(Unit::Anchor(b)) => b,
                _ => Vec::new(),
            };
            units.push(Unit::Sub { old, new: span });
            *in_sub = false;
        } else {
            units.push(Unit::Anchor(span));
        }
    }

    while pos < len {
        let byte = buf[pos];
        if byte == GS || byte == FS || byte == EOT {
            break;
        }
        match byte {
            US => {
                if pos > data_start {
                    let span = collect_data(buf, data_start, pos);
                    flush(&mut units, &mut in_sub, span);
                }
                pos += 1;
                data_start = pos;
            }
            SUB => {
                if pos > data_start {
                    let span = collect_data(buf, data_start, pos);
                    units.push(Unit::Anchor(span)); // temporarily the "old" part
                    in_sub = true;
                }
                pos += 1;
                data_start = pos;
            }
            DLE => pos += 2,
            _ => pos += 1,
        }
    }

    if pos > data_start {
        let span = collect_data(buf, data_start.min(len), pos.min(len));
        flush(&mut units, &mut in_sub, span);
    }

    (units, pos)
}

// Collect data bytes from a span, removing DLE escapes.
fn collect_data(buf: &[u8], start: usize, stop: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(stop - start);
    let mut pos = start;
    while pos < stop {
        if buf[pos] == DLE {
            pos += 1;
            if pos < stop {
                out.push(buf[pos]);
                pos += 1;
            }
        } else {
            out.push(buf[pos]);
            pos += 1;
        }
    }
    out
}

/// Apply a C0DIFF to a map of file contents, returning the modified map.
///
/// All sections are validated before any edits are made (atomic semantics):
/// each search pattern must occur exactly once in its target file. Files not
/// named by the diff are passed through unchanged.
pub fn apply(
    diff_buf: &[u8],
    files: &HashMap<String, String>,
) -> std::result::Result<HashMap<String, String>, DiffError> {
    let edits = parse(diff_buf);

    // Validate all edits first.
    for edit in &edits {
        let path = String::from_utf8_lossy(&edit.path).into_owned();
        let content = files
            .get(&path)
            .ok_or_else(|| DiffError::FileNotFound(path.clone()))?;
        for (i, section) in edit.sections.iter().enumerate() {
            let pattern = String::from_utf8_lossy(&section.search_pattern()).into_owned();
            let count = count_occurrences(content, &pattern);
            if count == 0 {
                return Err(DiffError::PatternNotFound {
                    path: path.clone(),
                    section: i,
                });
            } else if count > 1 {
                return Err(DiffError::PatternAmbiguous {
                    path: path.clone(),
                    section: i,
                    count,
                });
            }
        }
    }

    // Apply all edits.
    let mut results: HashMap<String, String> = HashMap::new();
    for edit in &edits {
        let path = String::from_utf8_lossy(&edit.path).into_owned();
        let mut content = files[&path].clone();
        for section in &edit.sections {
            let pattern = String::from_utf8_lossy(&section.search_pattern()).into_owned();
            let replacement = String::from_utf8_lossy(&section.replacement()).into_owned();
            content = content.replacen(&pattern, &replacement, 1);
        }
        results.insert(path, content);
    }

    // Include unmodified files.
    for (path, content) in files {
        results
            .entry(path.clone())
            .or_insert_with(|| content.clone());
    }

    Ok(results)
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.match_indices(needle).count()
}

/// Build a C0DIFF document.
pub fn build<F: FnOnce(&mut DiffBuilder)>(f: F) -> Vec<u8> {
    let mut b = DiffBuilder::new();
    f(&mut b);
    b.into_bytes()
}

/// Builder for C0DIFF documents.
#[derive(Default)]
pub struct DiffBuilder {
    buf: Vec<u8>,
}

impl DiffBuilder {
    pub fn new() -> Self {
        DiffBuilder { buf: Vec::new() }
    }

    /// Begin a file edit (FS + path), then add its sections in `f`.
    pub fn file<F: FnOnce(&mut DiffBuilder)>(&mut self, path: &str, f: F) -> &mut Self {
        self.buf.push(FS);
        self.buf.extend_from_slice(path.as_bytes());
        f(self);
        self
    }

    /// Begin a section (GS), then add its units via a [`SectionBuilder`].
    pub fn section<F: FnOnce(&mut SectionBuilder)>(&mut self, f: F) -> &mut Self {
        self.buf.push(GS);
        let mut sb = SectionBuilder {
            buf: &mut self.buf,
            first: true,
        };
        f(&mut sb);
        self
    }

    /// Convenience: a single anchored find/replace section.
    pub fn replace(
        &mut self,
        context_before: &str,
        old_text: &str,
        new_text: &str,
        context_after: &str,
    ) -> &mut Self {
        self.buf.push(GS);
        if !context_before.is_empty() {
            write_escaped(&mut self.buf, context_before);
            self.buf.push(US);
        }
        write_escaped(&mut self.buf, old_text);
        self.buf.push(SUB);
        write_escaped(&mut self.buf, new_text);
        if !context_after.is_empty() {
            self.buf.push(US);
            write_escaped(&mut self.buf, context_after);
        }
        self
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// Builder for the units of a single diff section.
pub struct SectionBuilder<'a> {
    buf: &'a mut Vec<u8>,
    first: bool,
}

impl SectionBuilder<'_> {
    /// Add literal anchor text.
    pub fn anchor(&mut self, text: &str) -> &mut Self {
        if !self.first {
            self.buf.push(US);
        }
        write_escaped(self.buf, text);
        self.first = false;
        self
    }

    /// Add a substitution (old → new).
    pub fn sub(&mut self, old_text: &str, new_text: &str) -> &mut Self {
        if !self.first {
            self.buf.push(US);
        }
        write_escaped(self.buf, old_text);
        self.buf.push(SUB);
        write_escaped(self.buf, new_text);
        self.first = false;
        self
    }
}

fn write_escaped(buf: &mut Vec<u8>, s: &str) {
    for &byte in s.as_bytes() {
        if byte < 0x20 {
            buf.push(DLE);
        }
        buf.push(byte);
    }
}
