//! C0-DIFF: atomic multi-file edits with anchored patterns (DIFF.md).
//!
//! ```
//! use std::collections::HashMap;
//! let diff = c0::diff::build(|d| {
//!     d.file("greeting.txt", |d| {
//!         d.section(|s| {
//!             s.anchor("Hello ").sub("world", "universe").anchor("!");
//!         });
//!     });
//!     d.eot();
//! });
//! let mut files = HashMap::new();
//! files.insert("greeting.txt".to_string(), "Hello world!".to_string());
//! let out = c0::diff::apply(&diff, &files).unwrap();
//! assert_eq!(out["greeting.txt"], "Hello universe!");
//! ```

use crate::document::parse_heading;
use crate::*;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Errors raised while parsing or applying a C0-DIFF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError {
    /// The diff bytes are malformed.
    Syntax(Error),
    /// The heading declares a `shape` other than `diff`.
    WrongShape(String),
    /// The same path twice in one diff.
    DuplicatePath(String),
    /// A section or replace-all before any FS.
    BeforeAnyFile,
    /// A path written as text.
    TextPath,
    /// A replace-all with an empty old part.
    EmptyOld,
    /// A replace-all with no SUB.
    MissingSub,
    /// A target file is missing.
    FileNotFound(String),
    /// A path that leaves the target directory.
    PathEscapes(String),
    /// A section whose search text is empty.
    EmptySearch(String),
    /// A section's search text was not found after the cursor.
    PatternNotFound { path: String, pattern: String },
    /// A section's search text matched more than once.
    PatternAmbiguous { path: String, pattern: String },
    /// A replace-all found no occurrence in its scope.
    ReplaceAllNothing { path: String, old: String },
    /// An I/O failure while applying to disk.
    Io(String),
}

impl fmt::Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiffError::Syntax(e) => write!(f, "{e}"),
            DiffError::WrongShape(s) => write!(f, "heading declares shape '{s}', not diff"),
            DiffError::DuplicatePath(p) => write!(f, "the same path twice: {p}"),
            DiffError::BeforeAnyFile => write!(f, "section or replace-all before any file"),
            DiffError::TextPath => write!(f, "a path cannot be text"),
            DiffError::EmptyOld => write!(f, "replace-all with an empty old part"),
            DiffError::MissingSub => write!(f, "replace-all without SUB"),
            DiffError::FileNotFound(p) => write!(f, "no such file: {p}"),
            DiffError::PathEscapes(p) => write!(f, "path escapes the target directory: {p}"),
            DiffError::EmptySearch(p) => write!(f, "empty search text in {p}"),
            DiffError::PatternNotFound { path, pattern } => {
                write!(f, "pattern not found in {path}: {pattern:?}")
            }
            DiffError::PatternAmbiguous { path, pattern } => {
                write!(f, "pattern matched more than once in {path}: {pattern:?}")
            }
            DiffError::ReplaceAllNothing { path, old } => {
                write!(f, "replace-all found nothing in {path}: {old:?}")
            }
            DiffError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DiffError {}

impl From<Error> for DiffError {
    fn from(e: Error) -> Self {
        DiffError::Syntax(e)
    }
}

impl From<std::io::Error> for DiffError {
    fn from(e: std::io::Error) -> Self {
        DiffError::Io(e.to_string())
    }
}

type DiffResult<T> = std::result::Result<T, DiffError>;

/// One unit of a pattern: an anchor (`new` is `None`) or a substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub old: Vec<u8>,
    pub new: Option<Vec<u8>>,
}

impl Unit {
    pub fn is_anchor(&self) -> bool {
        self.new.is_none()
    }
}

/// A section: one anchored pattern, plus the replace-alls that follow it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Section {
    pub units: Vec<Unit>,
    pub replaces: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Section {
    /// The search text: anchors and old parts concatenated.
    pub fn search(&self) -> Vec<u8> {
        self.units
            .iter()
            .flat_map(|u| u.old.iter().copied())
            .collect()
    }

    /// The replacement: anchors and new parts concatenated.
    pub fn replacement(&self) -> Vec<u8> {
        self.units
            .iter()
            .flat_map(|u| u.new.as_ref().unwrap_or(&u.old).iter().copied())
            .collect()
    }
}

/// A file's edits: replace-alls before any section apply from the start
/// of the file to the first section's match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    pub path: String,
    pub leading_replaces: Vec<(Vec<u8>, Vec<u8>)>,
    pub sections: Vec<Section>,
}

/// Parse a diff document (with or without its SYN heading).
pub fn parse(buf: &[u8]) -> DiffResult<Vec<FileEdit>> {
    let len = buf.len();
    let mut pos = scan::layout(buf, 0, len);
    if pos < len && buf[pos] == SYN {
        let (heading, p) = parse_heading(buf, pos + 1, len)?;
        pos = p;
        if let Some(shape) = heading.shape() {
            if shape != "diff" {
                return Err(DiffError::WrongShape(shape.to_string()));
            }
        }
    }
    let mut files: Vec<FileEdit> = Vec::new();
    let mut path: Option<String> = None;
    let mut leading = Vec::new();
    let mut sections: Vec<Section> = Vec::new();
    let flush = |path: &mut Option<String>,
                 leading: &mut Vec<(Vec<u8>, Vec<u8>)>,
                 sections: &mut Vec<Section>,
                 files: &mut Vec<FileEdit>| {
        if let Some(p) = path.take() {
            files.push(FileEdit {
                path: p,
                leading_replaces: std::mem::take(leading),
                sections: std::mem::take(sections),
            });
        }
        leading.clear();
        sections.clear();
    };
    while pos < len {
        pos = scan::layout(buf, pos, len);
        if pos >= len {
            break;
        }
        let mut b = buf[pos];
        let mut dead = false;
        if b == BEL {
            let p2 = scan::layout(buf, pos + 1, len);
            if p2 < len && (buf[p2] == FS || buf[p2] == GS || buf[p2] == RS) {
                dead = true;
                pos = p2;
                b = buf[pos];
            } else {
                pos = scan::comment(buf, pos, len)?; // a note
                continue;
            }
        }
        match b {
            FS => {
                let (name, p) = read_name(buf, pos + 1, len)?;
                pos = p;
                if !dead {
                    flush(&mut path, &mut leading, &mut sections, &mut files);
                    if files.iter().any(|f| f.path == name) {
                        return Err(DiffError::DuplicatePath(name));
                    }
                    path = Some(name);
                }
            }
            GS => {
                if path.is_none() {
                    return Err(DiffError::BeforeAnyFile);
                }
                let (units, p) = parse_pattern(buf, pos + 1, len)?;
                pos = p;
                if !dead {
                    sections.push(Section {
                        units,
                        replaces: Vec::new(),
                    });
                }
            }
            RS => {
                if path.is_none() {
                    return Err(DiffError::BeforeAnyFile);
                }
                let (old, p) = read_dvalue(buf, pos + 1, len)?;
                pos = p;
                if old.is_empty() {
                    return Err(DiffError::EmptyOld);
                }
                if !(pos < len && buf[pos] == SUB) {
                    return Err(DiffError::MissingSub);
                }
                let (new, p) = read_dvalue(buf, pos + 1, len)?;
                pos = p;
                if !dead {
                    match sections.last_mut() {
                        Some(s) => s.replaces.push((old, new)),
                        None => leading.push((old, new)),
                    }
                }
            }
            ETB => {
                pos = scan::plain(buf, scan::layout(buf, pos + 1, len), len);
            }
            EOT => {
                pos = scan::layout(buf, pos + 1, len);
                if pos < len && buf[pos] != SYN && buf[pos] != EM {
                    return Err(Error::syntax("unexpected byte after EOT", pos).into());
                }
                break;
            }
            _ => return Err(Error::syntax("unexpected byte in a diff body", pos).into()),
        }
    }
    flush(&mut path, &mut leading, &mut sections, &mut files);
    Ok(files)
}

/// Apply a diff to in-memory text files. Validates everything first and
/// fails without touching anything on any error. Files the diff does not
/// name pass through unchanged.
pub fn apply(
    diff_buf: &[u8],
    files: &HashMap<String, String>,
) -> DiffResult<HashMap<String, String>> {
    let edits = parse(diff_buf)?;
    let mut results = Vec::new();
    for e in &edits {
        let content = files
            .get(&e.path)
            .ok_or_else(|| DiffError::FileNotFound(e.path.clone()))?;
        results.push((e.path.clone(), apply_file(content.as_bytes(), e)?));
    }
    let mut out = files.clone();
    for (k, v) in results {
        out.insert(k, String::from_utf8_lossy(&v).into_owned());
    }
    Ok(out)
}

/// Apply a diff to in-memory binary files; see [`apply`].
pub fn apply_bytes(
    diff_buf: &[u8],
    files: &HashMap<String, Vec<u8>>,
) -> DiffResult<HashMap<String, Vec<u8>>> {
    let edits = parse(diff_buf)?;
    let mut results = Vec::new();
    for e in &edits {
        let content = files
            .get(&e.path)
            .ok_or_else(|| DiffError::FileNotFound(e.path.clone()))?;
        results.push((e.path.clone(), apply_file(content, e)?));
    }
    let mut out = files.clone();
    for (k, v) in results {
        out.insert(k, v);
    }
    Ok(out)
}

/// Apply a diff to files on disk under `base_dir`, atomically: every file
/// is validated before any is written, and each is written through a
/// temporary file and a rename.
pub fn apply_files(diff_buf: &[u8], base_dir: impl AsRef<Path>) -> DiffResult<()> {
    let edits = parse(diff_buf)?;
    let base = std::fs::canonicalize(base_dir.as_ref())?;
    let mut planned: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for e in &edits {
        let target = base.join(&e.path);
        let resolved =
            std::fs::canonicalize(&target).map_err(|_| DiffError::FileNotFound(e.path.clone()))?;
        if !resolved.starts_with(&base) {
            return Err(DiffError::PathEscapes(e.path.clone()));
        }
        let data = std::fs::read(&resolved)?;
        planned.push((resolved, apply_file(&data, e)?));
    }
    for (target, bytes) in planned {
        let mut tmp = target.clone().into_os_string();
        tmp.push(".c0diff.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &target)?;
    }
    Ok(())
}

/// Apply one file's edits (DIFF.md "Sections", "Replace-all").
pub fn apply_file(data: &[u8], edit: &FileEdit) -> DiffResult<Vec<u8>> {
    let mut cursor = 0;
    let mut matches: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    for sec in &edit.sections {
        let search = sec.search();
        if search.is_empty() {
            return Err(DiffError::EmptySearch(edit.path.clone()));
        }
        let Some(first) = find(data, &search, cursor) else {
            return Err(DiffError::PatternNotFound {
                path: edit.path.clone(),
                pattern: String::from_utf8_lossy(&search).into_owned(),
            });
        };
        if find(data, &search, first + 1).is_some() {
            return Err(DiffError::PatternAmbiguous {
                path: edit.path.clone(),
                pattern: String::from_utf8_lossy(&search).into_owned(),
            });
        }
        matches.push((first, first + search.len(), sec.replacement()));
        cursor = first + search.len();
    }
    // scopes: before the first match, and after each match to the next
    let mut out = Vec::with_capacity(data.len());
    let first_start = matches.first().map_or(data.len(), |m| m.0);
    out.extend(replace_all(
        &data[..first_start],
        &edit.leading_replaces,
        &edit.path,
    )?);
    for (i, (_, e, repl)) in matches.iter().enumerate() {
        out.extend_from_slice(repl);
        let next = matches.get(i + 1).map_or(data.len(), |m| m.0);
        out.extend(replace_all(
            &data[*e..next],
            &edit.sections[i].replaces,
            &edit.path,
        )?);
    }
    Ok(out)
}

fn replace_all(segment: &[u8], replaces: &[(Vec<u8>, Vec<u8>)], path: &str) -> DiffResult<Vec<u8>> {
    let mut segment = segment.to_vec();
    for (old, new) in replaces {
        let mut count = 0;
        let mut out = Vec::with_capacity(segment.len());
        let mut pos = 0;
        while let Some(i) = find(&segment, old, pos) {
            out.extend_from_slice(&segment[pos..i]);
            out.extend_from_slice(new);
            pos = i + old.len();
            count += 1;
        }
        if count == 0 {
            return Err(DiffError::ReplaceAllNothing {
                path: path.to_string(),
                old: String::from_utf8_lossy(old).into_owned(),
            });
        }
        out.extend_from_slice(&segment[pos..]);
        segment = out;
    }
    Ok(segment)
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

fn read_name(buf: &[u8], pos: usize, len: usize) -> DiffResult<(String, usize)> {
    let pos = scan::layout(buf, pos, len);
    if pos < len && buf[pos] == STX {
        return Err(DiffError::TextPath);
    }
    let stop = scan::plain(buf, pos, len);
    Ok((
        String::from_utf8_lossy(trim(&buf[pos..stop])).into_owned(),
        stop,
    ))
}

fn read_dvalue(buf: &[u8], pos: usize, len: usize) -> DiffResult<(Vec<u8>, usize)> {
    let pos = scan::layout(buf, pos, len);
    if pos >= len {
        return Ok((Vec::new(), pos));
    }
    if buf[pos] == STX {
        let stop = scan::text(buf, pos, len)?;
        let val = decode_text(&buf[pos + 1..stop - 1]).into_owned();
        Ok((val, scan::layout(buf, stop, len)))
    } else if buf[pos] >= 0x20 {
        let stop = scan::plain(buf, pos, len);
        Ok((trim(&buf[pos..stop]).to_vec(), stop))
    } else {
        Ok((Vec::new(), pos))
    }
}

fn parse_pattern(buf: &[u8], mut pos: usize, len: usize) -> DiffResult<(Vec<Unit>, usize)> {
    let mut units = Vec::new();
    loop {
        let (old, p) = read_dvalue(buf, pos, len)?;
        pos = p;
        if pos < len && buf[pos] == SUB {
            let (new, p) = read_dvalue(buf, pos + 1, len)?;
            pos = p;
            units.push(Unit {
                old,
                new: Some(new),
            });
        } else {
            units.push(Unit { old, new: None });
        }
        // notes after a unit
        while pos < len && buf[pos] == BEL {
            let p2 = scan::layout(buf, pos + 1, len);
            if !(p2 < len && (buf[p2] >= 0x20 || buf[p2] == STX)) {
                break;
            }
            pos = scan::layout(buf, scan::comment(buf, pos, len)?, len);
        }
        if !(pos < len && buf[pos] == US) {
            break;
        }
        pos += 1;
    }
    Ok((units, pos))
}

/// Build a diff document (with its `shape diff` heading).
pub fn build<F: FnOnce(&mut DiffBuilder)>(f: F) -> Vec<u8> {
    let mut b = DiffBuilder::new();
    f(&mut b);
    b.into_bytes()
}

/// Writes a C0-DIFF document.
pub struct DiffBuilder {
    buf: Vec<u8>,
}

impl Default for DiffBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DiffBuilder {
    /// A diff with its heading: `SYN SOH shape US diff`.
    pub fn new() -> Self {
        let mut b = DiffBuilder { buf: Vec::new() };
        b.buf.push(SYN);
        b.buf.push(SOH);
        b.buf.extend_from_slice(b"shape");
        b.buf.push(US);
        b.buf.extend_from_slice(b"diff");
        b
    }

    /// A diff body with no heading.
    pub fn bare() -> Self {
        DiffBuilder { buf: Vec::new() }
    }

    /// FS and a path, then the file's lines in `f`.
    pub fn file<F: FnOnce(&mut DiffBuilder)>(&mut self, path: &str, f: F) -> &mut Self {
        self.buf.push(FS);
        if let Err(e) = write_name(&mut self.buf, path.as_bytes()) {
            panic!("{e}");
        }
        f(self);
        self
    }

    /// GS and a pattern built with a [`SectionBuilder`].
    pub fn section<F: FnOnce(&mut SectionBuilder)>(&mut self, f: F) -> &mut Self {
        self.buf.push(GS);
        let mut sb = SectionBuilder {
            buf: &mut self.buf,
            first: true,
        };
        f(&mut sb);
        self
    }

    /// Anchor, old → new, anchor in one section.
    pub fn replace(
        &mut self,
        context_before: &str,
        old_text: &str,
        new_text: &str,
        context_after: &str,
    ) -> &mut Self {
        self.section(|s| {
            if !context_before.is_empty() {
                s.anchor(context_before);
            }
            s.sub(old_text, new_text);
            if !context_after.is_empty() {
                s.anchor(context_after);
            }
        })
    }

    /// `RS old SUB new`: every occurrence within the current section's
    /// scope. The old part must not be empty.
    pub fn replace_all(&mut self, old_text: &str, new_text: &str) -> &mut Self {
        assert!(
            !old_text.is_empty(),
            "replace-all needs a non-empty old part"
        );
        self.buf.push(RS);
        write_value(&mut self.buf, old_text.as_bytes());
        self.buf.push(SUB);
        write_value(&mut self.buf, new_text.as_bytes());
        self
    }

    /// A note.
    pub fn note(&mut self, text: &str) -> &mut Self {
        self.buf.push(BEL);
        write_value(&mut self.buf, text.as_bytes());
        self
    }

    pub fn eot(&mut self) -> &mut Self {
        self.buf.push(EOT);
        self
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// Builds the units of one section.
pub struct SectionBuilder<'a> {
    buf: &'a mut Vec<u8>,
    first: bool,
}

impl SectionBuilder<'_> {
    /// An anchor: literal text that must be present.
    pub fn anchor(&mut self, text: &str) -> &mut Self {
        self.sep();
        write_value(self.buf, text.as_bytes());
        self
    }

    /// A substitution: `old SUB new`.
    pub fn sub(&mut self, old_text: &str, new_text: &str) -> &mut Self {
        self.sep();
        write_value(self.buf, old_text.as_bytes());
        self.buf.push(SUB);
        write_value(self.buf, new_text.as_bytes());
        self
    }

    fn sep(&mut self) {
        if !self.first {
            self.buf.push(US);
        }
        self.first = false;
    }
}
