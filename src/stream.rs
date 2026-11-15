//! Stream mode: ETB-committed append logs (DESIGN.md "Stream Mode").
//!
//! An unescaped ETB is always a commit, inside open text or a level too. A
//! committed block that is not well-formed, cut by its ETB or holding any
//! other error, is damaged: nothing in it is delivered, and by default the
//! reader delivers the records before it and then fails with
//! [`Error::DamagedBlock`] naming the offset to resume at; a reader made
//! with [`StreamReader::continuing`] reads on past the block. The tail
//! after the last commit is torn unless it holds only layout, notes or EOT.
//! A SYN inside the stream tears the uncommitted bytes before it: they are
//! no block and deliver nothing ([`StreamReader::torn_regions`]); the new
//! document's blocks are read as usual.
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
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Scans a buffer for commits and exposes the committed region, block by
/// block, and its records. Zero-copy: accessors return slices into the
/// original buffer.
pub struct StreamReader<'a> {
    buf: &'a [u8],
    committed_end: usize,
    blocks: Vec<(usize, usize)>,
    damaged: Vec<usize>,
    // Uncommitted bytes torn by a mid-stream SYN, [start, end): skipped
    // like damaged blocks. Layout alone before a SYN is not a tear.
    torn_regions: Vec<(usize, usize)>,
    torn: bool,
    continue_past_damage: bool,
    // The committed region parsed with damaged blocks skipped; an error
    // only when one lies outside every block.
    container: Result<Container<'a>>,
}

impl<'a> StreamReader<'a> {
    /// A reader that stops at the first damaged block.
    pub fn new(buf: &'a [u8]) -> Self {
        Self::with_options(buf, false)
    }

    /// A reader that skips damaged blocks and reads on.
    pub fn continuing(buf: &'a [u8]) -> Self {
        Self::with_options(buf, true)
    }

    fn with_options(buf: &'a [u8], continue_past_damage: bool) -> Self {
        let mut r = StreamReader {
            buf,
            committed_end: 0,
            blocks: Vec::new(),
            damaged: Vec::new(),
            torn_regions: Vec::new(),
            torn: false,
            continue_past_damage,
            container: Err(Error::other("unparsed")),
        };
        r.scan();
        r.container = r.parse_committed();
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

    /// `[start, end)` of every region torn by a SYN inside the stream: the
    /// uncommitted bytes between a commit (or the start) and the SYN that
    /// began the next document. They belong to no block and deliver
    /// nothing. Layout alone before a SYN is not a tear. The torn tail
    /// after the last commit is [`StreamReader::torn`], not listed here.
    pub fn torn_regions(&self) -> &[(usize, usize)] {
        &self.torn_regions
    }

    /// Whether the reader goes on past a damaged block.
    pub fn continue_past_damage(&self) -> bool {
        self.continue_past_damage
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

    /// Offset after a block's ETB and payload: the start of the next block.
    pub fn block_end(&self, i: usize) -> usize {
        self.blocks
            .get(i + 1)
            .map_or(self.committed_end, |&(s, _)| s)
    }

    /// Indexes of committed blocks that are not well-formed.
    pub fn damaged(&self) -> &[usize] {
        &self.damaged
    }

    pub fn is_damaged(&self, i: usize) -> bool {
        self.damaged.contains(&i)
    }

    /// The committed region with damaged blocks removed: borrowed when no
    /// block is damaged, otherwise the sound blocks concatenated.
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

    /// The committed region as documents, damaged blocks left out. Fails
    /// with [`Error::DamagedBlock`] when a block is damaged, unless the
    /// reader continues past damage.
    pub fn container(&self) -> Result<&Container<'a>> {
        self.check_damage()?;
        self.container.as_ref().map_err(Clone::clone)
    }

    /// The first document of the committed region.
    pub fn document(&self) -> Result<&Document<'a>> {
        self.container()?
            .first()
            .ok_or_else(|| Error::other("the committed region holds no document"))
    }

    /// Every committed record in order, up to the first damaged block; then
    /// [`Error::DamagedBlock`], unless continuing, in which case every
    /// record of every sound block.
    pub fn each_record<F: FnMut(&Record<'a>)>(&self, mut f: F) -> Result<()> {
        let c = self.container.as_ref().map_err(Clone::clone)?;
        let limit = if self.continue_past_damage {
            usize::MAX
        } else {
            self.damaged
                .first()
                .map_or(usize::MAX, |&d| self.blocks[d].0)
        };
        for doc in c.documents() {
            for g in doc.body().groups() {
                for r in g.records() {
                    if r.offset() >= limit {
                        self.check_damage()?;
                    }
                    f(r);
                }
            }
        }
        self.check_damage()
    }

    /// Logical values of every committed record (headings and header rows
    /// excluded); see [`StreamReader::each_record`].
    pub fn records(&self) -> Result<Vec<Vec<Vec<u8>>>> {
        let mut out = Vec::new();
        let mut err = None;
        self.each_record(|r| match r.values() {
            Ok(vs) => out.push(vs.into_iter().map(|v| v.into_owned()).collect()),
            Err(e) => err = Some(e),
        })?;
        match err {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    fn check_damage(&self) -> Result<()> {
        if self.continue_past_damage {
            return Ok(());
        }
        match self.damaged.first() {
            Some(&d) => Err(Error::DamagedBlock {
                index: d,
                resume_offset: self.block_end(d),
            }),
            None => Ok(()),
        }
    }

    // Parse the committed region, torn regions skipped. Any error inside a
    // committed block marks that block damaged; the region is then parsed
    // again with the block skipped, so what follows is read in the context
    // before it.
    fn parse_committed(&mut self) -> Result<Container<'a>> {
        let mut skip: HashMap<usize, usize> = self.torn_regions.iter().copied().collect();
        let container = loop {
            let mut parser = Parser::with_skip(self.buf, skip.clone());
            match Container::parse(self.buf, self.committed_end, &mut parser) {
                Ok(c) => break c,
                Err(e) => {
                    let Some(pos) = e.position() else {
                        return Err(e);
                    };
                    let Some(i) = self.block_at(pos) else {
                        return Err(e);
                    };
                    if skip.contains_key(&self.blocks[i].0) {
                        return Err(e);
                    }
                    self.damaged.push(i);
                    skip.insert(self.blocks[i].0, self.block_end(i));
                }
            }
        };
        self.damaged.sort_unstable();
        Ok(container)
    }

    // The block holding byte `pos` of the committed region.
    fn block_at(&self, pos: usize) -> Option<usize> {
        self.blocks.iter().rposition(|&(s, _)| s <= pos)
    }

    fn scan(&mut self) {
        let buf = self.buf;
        let len = buf.len();
        let mut pos = 0;
        let mut block_start = 0;
        let mut in_text = false;
        while pos < len {
            let b = buf[pos];
            if in_text {
                if b == DLE {
                    pos += 2;
                    continue;
                } else if b == ETX {
                    in_text = false;
                } else if b == ETB {
                    self.blocks.push((block_start, pos));
                    pos = self.payload_end(pos + 1);
                    block_start = pos;
                    in_text = false;
                    continue;
                } else if b == SYN || b == EOT || b == EM {
                    // framing cuts the text
                    in_text = false;
                    if b == SYN {
                        self.tear(block_start, pos);
                        block_start = pos;
                    }
                }
                pos += 1;
            } else {
                if b == STX {
                    in_text = true;
                } else if b == ETB {
                    self.blocks.push((block_start, pos));
                    pos = self.payload_end(pos + 1);
                    block_start = pos;
                    continue;
                } else if b == SYN && pos > 0 {
                    // tears the uncommitted bytes before it; a new document begins
                    self.tear(block_start, pos);
                    block_start = pos;
                }
                pos += 1;
            }
        }
        self.torn = self.torn_tail();
    }

    // A SYN at `end` tears the uncommitted bytes from `start`: a torn
    // region unless it holds layout alone.
    fn tear(&mut self, start: usize, end: usize) {
        if scan::layout(self.buf, start, end) < end {
            self.torn_regions.push((start, end));
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
            } else if b == EM {
                return false; // the raw region is not data of the log
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
