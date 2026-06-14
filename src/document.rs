use crate::*;

/// Zero-copy navigator for a full C0DATA document.
///
/// Walks a buffer containing FS/GS/RS/US structure and provides access to
/// the file, its groups, records, and fields as slices into the buffer.
pub struct Document<'a> {
    buf: &'a [u8],
    name: (usize, usize),
    group_offsets: Vec<usize>,
    group_names: Vec<(usize, usize)>,
}

impl<'a> Document<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        let mut d = Document {
            buf,
            name: (0, 0),
            group_offsets: Vec::new(),
            group_names: Vec::new(),
        };
        d.index();
        d
    }

    /// Document/file name (text after FS). Empty if no FS is present.
    #[inline]
    pub fn name(&self) -> &'a [u8] {
        &self.buf[self.name.0..self.name.1]
    }

    /// Number of top-level groups.
    #[inline]
    pub fn group_count(&self) -> usize {
        self.group_offsets.len()
    }

    /// Access a group by index.
    pub fn group(&self, i: usize) -> Group<'a> {
        let gs_start = self.group_offsets[i];
        let gs_end = if i + 1 < self.group_offsets.len() {
            self.group_offsets[i + 1]
        } else {
            self.find_end(gs_start)
        };
        Group {
            buf: self.buf,
            start: gs_start,
            end: gs_end,
        }
    }

    /// Access a group by name, or `None` if no such group exists.
    pub fn group_by_name(&self, name: &str) -> Option<Group<'a>> {
        let needle = name.as_bytes();
        self.group_names
            .iter()
            .position(|&(s, e)| &self.buf[s..e] == needle)
            .map(|i| self.group(i))
    }

    /// Iterate all groups.
    pub fn groups(&self) -> impl Iterator<Item = Group<'a>> + '_ {
        (0..self.group_count()).map(move |i| self.group(i))
    }

    /// All group names.
    pub fn group_names(&self) -> Vec<&'a [u8]> {
        self.group_names
            .iter()
            .map(|&(s, e)| &self.buf[s..e])
            .collect()
    }

    fn index(&mut self) {
        let buf = self.buf;
        let len = buf.len();
        let mut pos = 0;

        // Skip FS + file name if present.
        if pos < len && buf[pos] == FS {
            pos += 1;
            let name_start = pos;
            while pos < len && buf[pos] >= 0x20 {
                pos += 1;
            }
            self.name = (name_start, pos);
        }

        // Find all top-level GS groups.
        while pos < len {
            let byte = buf[pos];
            if byte == EOT {
                break;
            }
            if byte == GS {
                let gs_pos = pos;
                let mut gs_count = 0;
                while pos < len && buf[pos] == GS {
                    gs_count += 1;
                    pos += 1;
                }
                if gs_count == 1 {
                    self.group_offsets.push(gs_pos);
                    let name_start = pos;
                    while pos < len && buf[pos] >= 0x20 {
                        pos += 1;
                    }
                    self.group_names.push((name_start, pos));
                } else {
                    // Deeper section (GS×N) — skip past its name.
                    while pos < len && buf[pos] >= 0x20 {
                        pos += 1;
                    }
                }
            } else {
                pos += 1;
            }
        }
    }

    fn find_end(&self, gs_start: usize) -> usize {
        let buf = self.buf;
        let len = buf.len();
        let mut pos = gs_start + 1;

        // Skip past the initial GS + name.
        while pos < len && buf[pos] >= 0x20 {
            pos += 1;
        }

        while pos < len {
            let byte = buf[pos];
            if byte == FS || byte == EOT {
                break;
            }
            if byte == GS {
                let mut count = 0;
                let mut peek = pos;
                while peek < len && buf[peek] == GS {
                    count += 1;
                    peek += 1;
                }
                if count == 1 {
                    break; // next top-level group
                }
                pos = peek;
                while pos < len && buf[pos] >= 0x20 {
                    pos += 1;
                }
            } else if byte == DLE {
                pos += 2;
            } else {
                pos += 1;
            }
        }
        pos.min(len)
    }
}

/// A group within a document. Can be read as a [`Table`] or as document-mode
/// content.
pub struct Group<'a> {
    buf: &'a [u8],
    start: usize, // offset of the GS byte
    end: usize,   // offset past the last byte of this group
}

impl<'a> Group<'a> {
    /// Group name.
    pub fn name(&self) -> &'a [u8] {
        let mut pos = self.start + 1; // skip GS
        let name_start = pos;
        while pos < self.end && self.buf[pos] >= 0x20 {
            pos += 1;
        }
        &self.buf[name_start..pos]
    }

    /// Access as a [`Table`] (for tabular/key-value data).
    pub fn table(&self) -> Table<'a> {
        Table::with_offset(self.buf, self.start)
    }

    /// Whether this group has an SOH header.
    pub fn has_header(&self) -> bool {
        let mut pos = self.start + 1;
        while pos < self.end && self.buf[pos] >= 0x20 {
            pos += 1;
        }
        pos < self.end && self.buf[pos] == SOH
    }

    /// Access record by index.
    pub fn record(&self, i: usize) -> Record<'a> {
        self.table().record(i)
    }

    /// Number of records.
    pub fn record_count(&self) -> usize {
        self.table().record_count()
    }

    /// Raw bytes of this group.
    #[inline]
    pub fn raw(&self) -> &'a [u8] {
        &self.buf[self.start..self.end]
    }
}
