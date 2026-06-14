//! Stream mode: ETB commits for append-only logs.
//!
//! C0DATA records are start-delimited, so a crashed append leaves a truncated
//! final record indistinguishable from a complete one. In stream mode every
//! appended block (one or more records, or an SOH header) is terminated by an
//! ETB commit marker. A block is complete iff terminated by ETB.

use crate::*;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Scans a buffer for ETB commit markers and exposes only the committed
/// region. Zero-copy: accessors return slices into the original buffer.
pub struct StreamReader<'a> {
    buf: &'a [u8],
    commits: Vec<(usize, usize)>, // (etb offset, end of payload)
}

impl<'a> StreamReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        let mut r = StreamReader {
            buf,
            commits: Vec::new(),
        };
        r.scan();
        r
    }

    /// Offset just past the last commit marker and its payload.
    pub fn committed_end(&self) -> usize {
        self.commits.last().map_or(0, |&(_, e)| e)
    }

    /// The committed region of the buffer.
    pub fn committed(&self) -> &'a [u8] {
        let end = self.committed_end();
        &self.buf[..end]
    }

    /// Uncommitted trailing bytes — residue of an interrupted append.
    pub fn tail(&self) -> &'a [u8] {
        let end = self.committed_end();
        &self.buf[end..]
    }

    /// True if uncommitted bytes trail the last commit marker.
    pub fn torn(&self) -> bool {
        self.committed_end() < self.buf.len()
    }

    /// Number of committed blocks.
    pub fn block_count(&self) -> usize {
        self.commits.len()
    }

    /// Committed block by index: the bytes between the previous commit and
    /// this block's ETB (marker and payload excluded).
    pub fn block(&self, i: usize) -> &'a [u8] {
        let start = if i == 0 { 0 } else { self.commits[i - 1].1 };
        &self.buf[start..self.commits[i].0]
    }

    /// Iterate committed blocks.
    pub fn blocks(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        (0..self.block_count()).map(move |i| self.block(i))
    }

    /// The committed region as a [`Table`].
    pub fn table(&self) -> Table<'a> {
        Table::new(self.committed())
    }

    // Find every ETB at structural level: DLE-escaped bytes are data, and an
    // ETB inside an STX/ETX scope is record content, not a commit.
    fn scan(&mut self) {
        let buf = self.buf;
        let len = buf.len();
        let mut pos = 0;
        while pos < len {
            let byte = buf[pos];
            if byte == DLE {
                pos += 2;
            } else if byte == STX {
                pos = skip_nested(buf, pos, len);
            } else if byte == ETB {
                let etb_pos = pos;
                pos += 1;
                while pos < len && buf[pos] >= 0x20 {
                    pos += 1;
                }
                self.commits.push((etb_pos, pos));
            } else {
                pos += 1;
            }
        }
    }
}

fn skip_nested(buf: &[u8], mut pos: usize, stop: usize) -> usize {
    pos += 1; // skip STX
    let mut depth = 1;
    while pos < stop && depth > 0 {
        match buf[pos] {
            STX => depth += 1,
            ETX => depth -= 1,
            DLE => pos += 1,
            _ => {}
        }
        pos += 1;
    }
    pos
}

// Build one block's bytes plus its trailing ETB commit marker.
fn commit_bytes<F: FnOnce(&mut Builder)>(f: F) -> Vec<u8> {
    let mut b = Builder::new();
    f(&mut b);
    b.etb();
    b.into_bytes()
}

/// Appends ETB-committed blocks to any [`Write`] sink (e.g. an in-memory
/// buffer or a socket). Each block and its ETB are written as one unit and
/// flushed. For files, prefer [`open_log`], which repairs a torn tail and can
/// fsync each commit.
pub struct StreamWriter<W: Write> {
    sink: W,
}

impl<W: Write> StreamWriter<W> {
    pub fn new(sink: W) -> Self {
        StreamWriter { sink }
    }

    /// Append one record as a committed block.
    pub fn record(&mut self, fields: &[&str]) -> io::Result<()> {
        self.emit(commit_bytes(|b| {
            b.record(fields);
        }))
    }

    /// Append an SOH header as a committed block.
    pub fn header(&mut self, names: &[&str]) -> io::Result<()> {
        self.emit(commit_bytes(|b| {
            b.header(names);
        }))
    }

    /// Append a group preamble (GS + name) as a committed block.
    pub fn group(&mut self, name: &str, headers: Option<&[&str]>) -> io::Result<()> {
        self.emit(commit_bytes(|b| {
            b.group(name, headers);
        }))
    }

    /// Append several records under a single commit (an atomic batch).
    pub fn batch<F: FnOnce(&mut Builder)>(&mut self, f: F) -> io::Result<()> {
        self.emit(commit_bytes(f))
    }

    /// Recover the underlying sink.
    pub fn into_inner(self) -> W {
        self.sink
    }

    fn emit(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        self.sink.write_all(&bytes)?;
        self.sink.flush()
    }
}

/// An append-only log file with ETB commits. Each commit is flushed and,
/// when `sync` is set, fsync'd. Dropping the value closes the file.
pub struct FileLog {
    file: File,
    sync: bool,
}

impl FileLog {
    /// Append one record as a committed block.
    pub fn record(&mut self, fields: &[&str]) -> io::Result<()> {
        self.commit(commit_bytes(|b| {
            b.record(fields);
        }))
    }

    /// Append an SOH header as a committed block.
    pub fn header(&mut self, names: &[&str]) -> io::Result<()> {
        self.commit(commit_bytes(|b| {
            b.header(names);
        }))
    }

    /// Append a group preamble (GS + name) as a committed block.
    pub fn group(&mut self, name: &str, headers: Option<&[&str]>) -> io::Result<()> {
        self.commit(commit_bytes(|b| {
            b.group(name, headers);
        }))
    }

    /// Append several records under a single commit (an atomic batch).
    pub fn batch<F: FnOnce(&mut Builder)>(&mut self, f: F) -> io::Result<()> {
        self.commit(commit_bytes(f))
    }

    fn commit(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        self.file.write_all(&bytes)?;
        self.file.flush()?;
        if self.sync {
            self.file.sync_all()?;
        }
        Ok(())
    }
}

/// Open an append-only log file, repairing any torn tail first (truncating to
/// the last commit). Each commit is fsync'd.
pub fn open_log(path: impl AsRef<Path>) -> io::Result<FileLog> {
    open_log_with(path, true)
}

/// Like [`open_log`], but with explicit control over per-commit fsync.
pub fn open_log_with(path: impl AsRef<Path>, sync: bool) -> io::Result<FileLog> {
    let path = path.as_ref();

    // Repair: truncate an uncommitted tail so the log ends at a commit marker.
    if path.exists() {
        let bytes = fs::read(path)?;
        let reader = StreamReader::new(&bytes);
        if reader.torn() {
            let f = OpenOptions::new().write(true).open(path)?;
            f.set_len(reader.committed_end() as u64)?;
        }
    }

    let file = OpenOptions::new().create(true).append(true).open(path)?;
    Ok(FileLog { file, sync })
}

/// Read a log file into a byte buffer; wrap it with [`StreamReader::new`].
pub fn read_log(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    fs::read(path)
}
