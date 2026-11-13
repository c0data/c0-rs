//! C0DIFF: atomic multi-file edits with anchored patterns (DIFF.md). A
//! diff is an ordinary document of `shape` `diff`: files of records, each
//! record a pattern of units, with `old␚␟new` (supersession at the unit
//! grain) as the substitution.
//!
//! ```
//! use std::collections::HashMap;
//! let diff = c0::diff::build(|d| {
//!     d.file("greeting.txt", |d| {
//!         d.record(|r| {
//!             r.anchor("Hello ").sub("world", "universe").anchor("!");
//!         });
//!     });
//!     d.eot();
//! });
//! let mut files = HashMap::new();
//! files.insert("greeting.txt".to_string(), "Hello world!".to_string());
//! let out = c0::diff::apply(&diff, &files).unwrap();
//! assert_eq!(out["greeting.txt"], "Hello universe!");
//! ```

use crate::*;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Errors raised while parsing or applying a C0DIFF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError {
    /// The diff bytes are malformed.
    Syntax(Error),
    /// The heading declares a `shape` other than `diff`.
    WrongShape(String),
    /// The same path twice in one diff.
    DuplicatePath(String),
    /// A record before the first FS.
    BeforeAnyFile,
    /// A file line with no path.
    EmptyPath,
    /// Something a diff may not hold (DIFF.md): the reason.
    Refused(String),
    /// A target file is missing.
    FileNotFound(String),
    /// A path that leaves the target directory.
    PathEscapes(String),
    /// A record whose search text is empty.
    EmptySearch(String),
    /// A record's search text was not found after the cursor.
    PatternNotFound { path: String, pattern: String },
    /// A record's search text matched more than once.
    PatternAmbiguous { path: String, pattern: String },
    /// An I/O failure while applying to disk.
    Io(String),
}

impl fmt::Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiffError::Syntax(e) => write!(f, "{e}"),
            DiffError::WrongShape(s) => write!(f, "heading declares shape '{s}', not diff"),
            DiffError::DuplicatePath(p) => write!(f, "the same path twice: {p}"),
            DiffError::BeforeAnyFile => write!(f, "a record before the first FS"),
            DiffError::EmptyPath => write!(f, "empty path"),
            DiffError::Refused(why) => write!(f, "{why}"),
            DiffError::FileNotFound(p) => write!(f, "no such file: {p}"),
            DiffError::PathEscapes(p) => write!(f, "path escapes the target directory: {p}"),
            DiffError::EmptySearch(p) => write!(f, "empty search text in {p}"),
            DiffError::PatternNotFound { path, pattern } => {
                write!(f, "pattern not found in {path}: {pattern:?}")
            }
            DiffError::PatternAmbiguous { path, pattern } => {
                write!(f, "pattern matched more than once in {path}: {pattern:?}")
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

fn refused(why: &str) -> DiffError {
    DiffError::Refused(why.to_string())
}

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

/// One record: an anchored pattern.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pattern {
    pub units: Vec<Unit>,
}

impl Pattern {
    /// What the file must hold, once: anchors and superseded units.
    pub fn search(&self) -> Vec<u8> {
        self.units
            .iter()
            .flat_map(|u| u.old.iter().copied())
            .collect()
    }

    /// What it will hold: superseding units in place of superseded ones.
    pub fn replacement(&self) -> Vec<u8> {
        self.units
            .iter()
            .flat_map(|u| u.new.as_ref().unwrap_or(&u.old).iter().copied())
            .collect()
    }

    /// A record with no substitution: its search text must be present once.
    pub fn is_assertion(&self) -> bool {
        self.units.iter().all(Unit::is_anchor)
    }
}

/// A file's records, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    pub path: String,
    pub records: Vec<Pattern>,
}

/// Parse a diff document (with or without its SYN heading), refusing
/// everything DIFF.md lists under "What a diff may not hold".
pub fn parse(buf: &[u8]) -> DiffResult<Vec<FileEdit>> {
    let mut parser = Parser::new(buf, false);
    let container = Container::parse(buf, buf.len(), &mut parser)?;
    if container.documents().len() != 1 {
        return Err(refused("a diff is one document"));
    }
    let doc = &container.documents()[0];
    if let Some(shape) = doc.heading().shape() {
        if shape != "diff" {
            return Err(DiffError::WrongShape(shape.to_string()));
        }
    }
    if parser.dead_elements() > 0 {
        return Err(refused(
            "a dead element in a diff: a hunk is kept without applying by writing it in a note",
        ));
    }
    if parser.supersessions().iter().any(|&c| c != US) {
        return Err(refused("supersession at a grain other than the unit"));
    }
    let mut files: Vec<FileEdit> = Vec::new();
    for f in doc.body().files() {
        if f.is_leading() {
            if f.groups().is_empty() {
                continue;
            }
            return Err(DiffError::BeforeAnyFile);
        }
        let path = String::from_utf8_lossy(f.name()).into_owned();
        if path.is_empty() {
            return Err(DiffError::EmptyPath);
        }
        if files.iter().any(|e| e.path == path) {
            return Err(DiffError::DuplicatePath(path));
        }
        let mut records = Vec::new();
        for g in f.groups() {
            if g.depth() > 0 {
                return Err(refused("GS in a diff"));
            }
            if g.has_header() {
                return Err(refused("a header row in a diff"));
            }
            for r in g.records() {
                records.push(pattern_of(r)?);
            }
        }
        files.push(FileEdit { path, records });
    }
    Ok(files)
}

fn pattern_of(rec: &Record<'_>) -> DiffResult<Pattern> {
    if rec.keyed() {
        return Err(refused("a keyed unit in a diff record"));
    }
    let mut units = Vec::new();
    let mut pending: Option<Vec<u8>> = None;
    for u in rec.units() {
        let raw = u.raw();
        if is_level(raw) {
            return Err(refused("a nested level in a diff record"));
        }
        if is_reference(raw) {
            return Err(refused("a reference in a diff record"));
        }
        let v = value(raw)?.into_owned();
        if u.superseded() {
            if pending.is_some() {
                return Err(refused(
                    "a chained supersession: its search text would hold what the file never held",
                ));
            }
            pending = Some(v);
        } else if let Some(old) = pending.take() {
            if old.is_empty() && v.is_empty() {
                return Err(refused("a substitution of nothing by nothing"));
            }
            units.push(Unit { old, new: Some(v) });
        } else {
            units.push(Unit { old: v, new: None });
        }
    }
    Ok(Pattern { units })
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

/// Apply one file's records (DIFF.md "Records"): each search text must
/// match once from the cursor to the end of the file.
pub fn apply_file(data: &[u8], edit: &FileEdit) -> DiffResult<Vec<u8>> {
    let mut cursor = 0;
    let mut matches: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    for pat in &edit.records {
        let search = pat.search();
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
        matches.push((first, first + search.len(), pat.replacement()));
        cursor = first + search.len();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut pos = 0;
    for (s, e, repl) in matches {
        out.extend_from_slice(&data[pos..s]);
        out.extend_from_slice(&repl);
        pos = e;
    }
    out.extend_from_slice(&data[pos..]);
    Ok(out)
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

/// Build a diff document (with its `shape diff` heading).
pub fn build<F: FnOnce(&mut DiffBuilder)>(f: F) -> Vec<u8> {
    let mut b = DiffBuilder::new();
    f(&mut b);
    b.into_bytes()
}

/// Writes a C0DIFF document.
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

    /// FS and a path, then the file's records in `f`.
    pub fn file<F: FnOnce(&mut DiffBuilder)>(&mut self, path: &str, f: F) -> &mut Self {
        self.buf.push(FS);
        if let Err(e) = write_name(&mut self.buf, path.as_bytes()) {
            panic!("{e}");
        }
        f(self);
        self
    }

    /// RS and one anchored pattern built with a [`PatternBuilder`].
    pub fn record<F: FnOnce(&mut PatternBuilder)>(&mut self, f: F) -> &mut Self {
        self.buf.push(RS);
        let mut pb = PatternBuilder {
            buf: &mut self.buf,
            first: true,
        };
        f(&mut pb);
        self
    }

    /// Anchor, old → new, anchor in one record.
    pub fn replace(
        &mut self,
        context_before: &str,
        old_text: &str,
        new_text: &str,
        context_after: &str,
    ) -> &mut Self {
        self.record(|r| {
            if !context_before.is_empty() {
                r.anchor(context_before);
            }
            r.sub(old_text, new_text);
            if !context_after.is_empty() {
                r.anchor(context_after);
            }
        })
    }

    /// Insert `text` after `anchor`.
    pub fn insert_after(&mut self, anchor: &str, text: &str) -> &mut Self {
        self.record(|r| {
            r.anchor(anchor).sub("", text);
        })
    }

    /// Remove `old_text`, found once after the cursor.
    pub fn delete(
        &mut self,
        old_text: &str,
        context_before: &str,
        context_after: &str,
    ) -> &mut Self {
        self.replace(context_before, old_text, "", context_after)
    }

    /// Assert that `text` is present once; nothing changes.
    pub fn assert(&mut self, text: &str) -> &mut Self {
        self.record(|r| {
            r.anchor(text);
        })
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

/// Builds the units of one record.
pub struct PatternBuilder<'a> {
    buf: &'a mut Vec<u8>,
    first: bool,
}

impl PatternBuilder<'_> {
    /// An anchor: literal text that must be present.
    pub fn anchor(&mut self, text: &str) -> &mut Self {
        self.sep();
        write_value(self.buf, text.as_bytes());
        self
    }

    /// A substitution: `old␚␟new`.
    pub fn sub(&mut self, old_text: &str, new_text: &str) -> &mut Self {
        self.sep();
        write_value(self.buf, old_text.as_bytes());
        self.buf.push(SUB);
        self.buf.push(US);
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
