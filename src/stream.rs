//! Stream mode: ETB-committed append logs (DESIGN.md "Stream Mode").
//!
//! An unescaped ETB is always a commit, inside open text or a level too:
//! such a block is damaged and yields nothing. The tail after the last
//! commit is torn unless it holds only layout, notes or EOT.
//!
//! ```no_run
//! let mut log = c0::open_log("events.c0")?;        // repairs a torn tail first
//! log.start(&[("shape", "stream")])?;
//! log.record(&["create", "a1b2", "1718208000"])?;
//! let bytes = c0::read_log("events.c0")?;
//! let r = c0::StreamReader::new(&bytes);
//! assert!(!r.torn());
//! # Ok::<(), std::io::Error>(())
//! ```

use crate::*;
use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Scans a buffer for commits and exposes the committed region, block by
/// block. Zero-copy: accessors return slices into the original buffer.
pub struct StreamReader<'a> {
    buf: &'a [u8],
    committed_end: usize,
    blocks: Vec<(usize, usize)>,
    damaged: Vec<usize>,
    torn: bool,
}

impl<'a> StreamReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        let mut r = StreamReader {
            buf,
            committed_end: 0,
            blocks: Vec::new(),
            damaged: Vec::new(),
            torn: false,
        };
        r.scan();
        r
    }

    pub fn buf(&self) -> &'a [u8] {
        self.buf
    }

    /// Offset just past the last ETB and its payload.
    pub fn committed_end(&self) -> usize {
        self.committed_end
    }

    /// The committed region of the buffer.
    pub fn committed(&self) -> &'a [u8] {
        &self.buf[..self.committed_end]
    }

    /// Everything after the last commit.
    pub fn tail(&self) -> &'a [u8] {
        &self.buf[self.committed_end..]
    }

    /// Whether data trails the last commit (layout, notes and EOT alone do
    /// not count).
    pub fn torn(&self) -> bool {
        self.torn
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Committed block `i`: the bytes from the previous commit to this
    /// block's ETB (marker and payload excluded).
    pub fn block(&self, i: usize) -> &'a [u8] {
        let (s, e) = self.blocks[i];
        &self.buf[s..e]
    }

    /// `[start, end)` of every committed block.
    pub fn block_ranges(&self) -> &[(usize, usize)] {
        &self.blocks
    }

    pub fn blocks(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        (0..self.blocks.len()).map(move |i| self.block(i))
    }

    /// Indexes of blocks cut by an ETB inside open text or a level.
    pub fn damaged(&self) -> &[usize] {
        &self.damaged
    }

    pub fn is_damaged(&self, i: usize) -> bool {
        self.damaged.contains(&i)
    }

    /// The committed region with damaged blocks removed: borrowed when no
    /// block is damaged, otherwise the undamaged blocks concatenated.
    /// Read it with [`Document::new`] or [`Body::new`].
    pub fn undamaged(&self) -> Cow<'a, [u8]> {
        if self.damaged.is_empty() {
            return Cow::Borrowed(self.committed());
        }
        let mut out = Vec::new();
        for (i, &(s, e)) in self.blocks.iter().enumerate() {
            if !self.damaged.contains(&i) {
                out.extend_from_slice(&self.buf[s..e]);
            }
        }
        Cow::Owned(out)
    }

    /// Logical values of every committed record (headings and header rows
    /// excluded), undamaged blocks only.
    pub fn records(&self) -> Result<Vec<Vec<Vec<u8>>>> {
        let bytes = self.undamaged();
        let doc = Document::new(&bytes)?;
        let mut out = Vec::new();
        for g in doc.groups() {
            for r in g.records() {
                out.push(r.values()?.into_iter().map(|v| v.into_owned()).collect());
            }
        }
        Ok(out)
    }

    fn scan(&mut self) {
        let buf = self.buf;
        let len = buf.len();
        let mut pos = 0;
        let mut block_start = 0;
        let mut in_text = false;
        let mut depth = 0usize;
        while pos < len {
            let b = buf[pos];
            if in_text {
                if b == DLE {
                    pos += 2;
                    continue;
                } else if b == ETX {
                    in_text = false;
                } else if b == ETB {
                    self.commit(block_start, pos, true);
                    pos = self.payload_end(pos + 1);
                    block_start = pos;
                    in_text = false;
                    depth = 0;
                    continue;
                } else if b == SYN || b == EOT || b == EM {
                    // framing cuts the text; the block is torn or damaged
                    in_text = false;
                    depth = 0;
                    if b == SYN {
                        block_start = pos;
                    }
                }
                pos += 1;
            } else {
                if b == STX {
                    in_text = true;
                } else if b == DLE {
                    depth += 1;
                } else if b == CAN {
                    depth = depth.saturating_sub(1);
                } else if b == ETB {
                    self.commit(block_start, pos, depth > 0);
                    pos = self.payload_end(pos + 1);
                    block_start = pos;
                    depth = 0;
                    continue;
                } else if b == SYN && pos > 0 {
                    // tears the uncommitted bytes before it; a new document begins
                    block_start = pos;
                    depth = 0;
                }
                pos += 1;
            }
        }
        self.torn = self.torn_tail();
    }

    fn commit(&mut self, start: usize, stop: usize, damaged: bool) {
        self.blocks.push((start, stop));
        if damaged {
            self.damaged.push(self.blocks.len() - 1);
        }
    }

    // After an ETB: skip its payload and return committed_end (which
    // excludes layout after the payload).
    fn payload_end(&mut self, pos: usize) -> usize {
        let buf = self.buf;
        let len = buf.len();
        let mut p = scan::layout(buf, pos, len);
        if p < len && buf[p] >= 0x20 {
            p = scan::plain(buf, p, len);
            // trailing spaces of the payload are layout
            while p > pos && buf[p - 1] == SP {
                p -= 1;
            }
        } else {
            p = pos; // no payload: layout after ETB belongs to the next block
        }
        self.committed_end = p;
        p
    }

    // Is the tail more than layout, notes and EOT?
    fn torn_tail(&self) -> bool {
        let buf = self.buf;
        let len = buf.len();
        let mut pos = self.committed_end;
        loop {
            pos = scan::layout(buf, pos, len);
            if pos >= len {
                return false;
            }
            let b = buf[pos];
            if b == EOT {
                pos += 1;
            } else if b == BEL {
                let p2 = scan::layout(buf, pos + 1, len);
                if p2 < len && buf[p2] < 0x20 && buf[p2] != STX {
                    return true; // a dead element is data
                }
                match scan::comment(buf, pos, len) {
                    Ok(p) => pos = p,
                    Err(_) => return true,
                }
            } else {
                return true;
            }
        }
    }
}

/// Position just past the last commit, found by scanning backward
/// (treating an ETB behind an odd run of DLEs as escaped). A shortcut for
/// well-formed input; the forward scan of [`StreamReader`] is authoritative.
pub fn last_commit_end(buf: &[u8]) -> usize {
    let mut i = buf.len();
    while i > 0 {
        i -= 1;
        if buf[i] != ETB {
            continue;
        }
        let dles = buf[..i].iter().rev().take_while(|&&b| b == DLE).count();
        if dles % 2 == 0 {
            return scan::plain(buf, i + 1, buf.len());
        }
    }
    0
}

// Build one block's bytes plus its trailing ETB.
fn commit_bytes<F: FnOnce(&mut Builder)>(f: F) -> Vec<u8> {
    let mut b = Builder::new();
    f(&mut b);
    b.etb();
    b.into_bytes()
}

/// Appends committed blocks to any [`Write`] sink. Each `record`, `map`,
/// `header`, `group` or `batch` is one block, written with its ETB in a
/// single append and flushed. For files prefer [`open_log`], which repairs
/// a torn tail first and can fsync each commit.
pub struct StreamWriter<W: Write> {
    sink: W,
}

impl<W: Write> StreamWriter<W> {
    pub fn new(sink: W) -> Self {
        StreamWriter { sink }
    }

    /// A new log: SYN and its heading, committed.
    pub fn start(&mut self, heading: &[(&str, &str)]) -> io::Result<()> {
        self.batch(|b| {
            b.syn();
            for (k, v) in heading {
                b.heading(k, v);
            }
        })
    }

    /// Append one positional record as a committed block.
    pub fn record<T: AsRef<[u8]>>(&mut self, fields: &[T]) -> io::Result<()> {
        self.batch(|b| {
            b.record(fields);
        })
    }

    /// Append one keyed record as a committed block.
    pub fn map<T: AsRef<[u8]>>(&mut self, pairs: &[(&str, T)]) -> io::Result<()> {
        self.batch(|b| {
            b.map(pairs);
        })
    }

    /// Append a header row as a committed block.
    pub fn header(&mut self, names: &[&str]) -> io::Result<()> {
        self.batch(|b| {
            b.header(names);
        })
    }

    /// Append a group line (and header row) as a committed block.
    pub fn group(&mut self, name: &str, headers: Option<&[&str]>) -> io::Result<()> {
        self.batch(|b| {
            b.group(name, headers);
        })
    }

    /// Several writes under one ETB: atomic as a batch.
    pub fn batch<F: FnOnce(&mut Builder)>(&mut self, f: F) -> io::Result<()> {
        self.sink.write_all(&commit_bytes(f))?;
        self.sink.flush()
    }

    pub fn get_ref(&self) -> &W {
        &self.sink
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.sink
    }

    /// Recover the underlying sink.
    pub fn into_inner(self) -> W {
        self.sink
    }
}

/// An append-only log file with ETB commits. Each commit is flushed and,
/// when `sync` is set, fsync'd. Dropping the value closes the file.
pub struct FileLog {
    w: StreamWriter<File>,
    sync: bool,
}

impl FileLog {
    /// A new log: SYN and its heading, committed.
    pub fn start(&mut self, heading: &[(&str, &str)]) -> io::Result<()> {
        self.w.start(heading)?;
        self.sync()
    }

    pub fn record<T: AsRef<[u8]>>(&mut self, fields: &[T]) -> io::Result<()> {
        self.w.record(fields)?;
        self.sync()
    }

    pub fn map<T: AsRef<[u8]>>(&mut self, pairs: &[(&str, T)]) -> io::Result<()> {
        self.w.map(pairs)?;
        self.sync()
    }

    pub fn header(&mut self, names: &[&str]) -> io::Result<()> {
        self.w.header(names)?;
        self.sync()
    }

    pub fn group(&mut self, name: &str, headers: Option<&[&str]>) -> io::Result<()> {
        self.w.group(name, headers)?;
        self.sync()
    }

    /// Several writes under one ETB: atomic as a batch.
    pub fn batch<F: FnOnce(&mut Builder)>(&mut self, f: F) -> io::Result<()> {
        self.w.batch(f)?;
        self.sync()
    }

    fn sync(&mut self) -> io::Result<()> {
        if self.sync {
            self.w.get_mut().sync_all()?;
        }
        Ok(())
    }
}

/// Open an append-only log file, repairing any torn tail first. Each
/// commit is fsync'd.
pub fn open_log(path: impl AsRef<Path>) -> io::Result<FileLog> {
    open_log_with(path, true)
}

/// Like [`open_log`], with explicit control over per-commit fsync.
pub fn open_log_with(path: impl AsRef<Path>, sync: bool) -> io::Result<FileLog> {
    let path = path.as_ref();
    if path.exists() {
        repair_log(path)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    Ok(FileLog {
        w: StreamWriter::new(file),
        sync,
    })
}

/// Truncate an uncommitted tail so the file ends at a commit.
pub fn repair_log(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    let r = StreamReader::new(&bytes);
    if r.torn() {
        let f = OpenOptions::new().write(true).open(path)?;
        f.set_len(r.committed_end() as u64)?;
    }
    Ok(())
}

/// Read a log file into a byte buffer; wrap it with [`StreamReader::new`].
pub fn read_log(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    fs::read(path)
}
