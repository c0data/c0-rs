use crate::*;
use std::borrow::Cow;

/// Zero-copy accessor for a tabular C0DATA group.
///
/// Scans the buffer once to index record positions, then provides O(1)
/// access to records and fields as slices into the original buffer.
pub struct Table<'a> {
    buf: &'a [u8],
    name: (usize, usize),
    headers: Vec<(usize, usize)>,
    records: Vec<(usize, usize)>,
}

impl<'a> Table<'a> {
    /// Index a tabular group starting at the beginning of `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Self::with_offset(buf, 0)
    }

    /// Index a tabular group starting at `offset` (e.g. a group's GS byte).
    pub fn with_offset(buf: &'a [u8], offset: usize) -> Self {
        let mut t = Table {
            buf,
            name: (0, 0),
            headers: Vec::new(),
            records: Vec::new(),
        };
        t.index(offset);
        t
    }

    /// Group/table name as a slice into the buffer.
    #[inline]
    pub fn name(&self) -> &'a [u8] {
        &self.buf[self.name.0..self.name.1]
    }

    /// Number of header fields.
    #[inline]
    pub fn header_count(&self) -> usize {
        self.headers.len()
    }

    /// Header field name by index.
    #[inline]
    pub fn header(&self, i: usize) -> &'a [u8] {
        let (s, e) = self.headers[i];
        &self.buf[s..e]
    }

    /// All header names.
    pub fn headers(&self) -> Vec<&'a [u8]> {
        self.headers.iter().map(|&(s, e)| &self.buf[s..e]).collect()
    }

    /// Number of records.
    #[inline]
    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    /// Access a record by index.
    #[inline]
    pub fn record(&self, i: usize) -> Record<'a> {
        let (s, e) = self.records[i];
        Record {
            buf: self.buf,
            start: s,
            end: e,
        }
    }

    /// Iterate all records.
    pub fn records(&self) -> impl Iterator<Item = Record<'a>> + '_ {
        let buf = self.buf;
        self.records.iter().map(move |&(s, e)| Record {
            buf,
            start: s,
            end: e,
        })
    }

    fn index(&mut self, offset: usize) {
        let buf = self.buf;
        let len = buf.len();
        let mut pos = offset;

        if pos >= len {
            return;
        }

        // Expect GS to start the group.
        if buf[pos] == GS {
            pos += 1;
            let name_start = pos;
            while pos < len && buf[pos] >= 0x20 {
                pos += 1;
            }
            self.name = (name_start, pos);
        }

        // Skip ETB commit markers and their payloads (stream mode framing).
        while pos < len && buf[pos] == ETB {
            pos += 1;
            while pos < len && buf[pos] >= 0x20 {
                pos += 1;
            }
        }

        // Read SOH header if present.
        if pos < len && buf[pos] == SOH {
            pos += 1;
            let mut field_start = pos;
            while pos < len {
                let byte = buf[pos];
                if byte == US {
                    self.headers.push((field_start, pos));
                    pos += 1;
                    field_start = pos;
                } else if byte < 0x20 {
                    self.headers.push((field_start, pos));
                    break;
                } else {
                    pos += 1;
                }
            }
            if pos >= len {
                self.headers.push((field_start, pos));
            }
        }

        // Read records.
        while pos < len {
            let byte = buf[pos];
            if byte == GS || byte == FS || byte == EOT || byte == ETX {
                break;
            }
            if byte == RS {
                pos += 1;
                let rec_start = pos;
                while pos < len {
                    let b = buf[pos];
                    if b == RS || b == GS || b == FS || b == EOT || b == ETX || b == ETB {
                        break;
                    }
                    if b == DLE {
                        pos += 2;
                    } else if b == STX {
                        pos += 1;
                        let mut depth = 1;
                        while pos < len && depth > 0 {
                            match buf[pos] {
                                STX => depth += 1,
                                ETX => depth -= 1,
                                DLE => pos += 1,
                                _ => {}
                            }
                            pos += 1;
                        }
                    } else {
                        pos += 1;
                    }
                }
                self.records.push((rec_start, pos));
            } else {
                pos += 1;
            }
        }
    }
}

/// Zero-copy accessor for a single record within a table.
pub struct Record<'a> {
    buf: &'a [u8],
    start: usize,
    end: usize,
}

impl<'a> Record<'a> {
    /// Access field by index. Scans for the Nth US separator. Respects
    /// STX/ETX nesting — US inside a nested scope is not a boundary.
    pub fn field(&self, n: usize) -> &'a [u8] {
        let buf = self.buf;
        let mut pos = self.start;
        let mut field_idx = 0;
        let mut field_start = pos;

        while pos < self.end {
            let byte = buf[pos];
            if byte == US {
                if field_idx == n {
                    return &buf[field_start..pos];
                }
                field_idx += 1;
                pos += 1;
                field_start = pos;
            } else if byte == DLE {
                pos += 2;
            } else if byte == STX {
                pos = skip_nested(buf, pos, self.end);
            } else {
                pos += 1;
            }
        }

        if field_idx == n {
            return &buf[field_start..pos.min(self.end)];
        }
        &[]
    }

    /// Number of fields in this record. Respects STX/ETX nesting.
    pub fn field_count(&self) -> usize {
        let buf = self.buf;
        let mut count = 1;
        let mut pos = self.start;
        while pos < self.end {
            let byte = buf[pos];
            if byte == US {
                count += 1;
                pos += 1;
            } else if byte == DLE {
                pos += 2;
            } else if byte == STX {
                pos = skip_nested(buf, pos, self.end);
            } else {
                pos += 1;
            }
        }
        count
    }

    /// All fields as slices.
    pub fn fields(&self) -> Vec<&'a [u8]> {
        (0..self.field_count()).map(|i| self.field(i)).collect()
    }

    /// Logical bytes of field `n`: the raw slice with DLE escapes decoded.
    /// Zero-copy when the field contains no escapes.
    pub fn value(&self, n: usize) -> Cow<'a, [u8]> {
        unescape(self.field(n))
    }

    /// All logical field values.
    pub fn values(&self) -> Vec<Cow<'a, [u8]>> {
        (0..self.field_count()).map(|i| self.value(i)).collect()
    }

    /// Raw bytes of the entire record.
    #[inline]
    pub fn raw(&self) -> &'a [u8] {
        &self.buf[self.start..self.end]
    }
}

/// Skip over a STX/ETX nested scope, returning the position after the
/// matching ETX. The cursor may advance past `stop` on truncated input.
#[inline]
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
