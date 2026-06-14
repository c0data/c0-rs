use crate::*;

/// Builds C0DATA documents in compact form.
///
/// ```
/// let buf = c0::Builder::build(|b| {
///     b.file("mydb");
///     b.group("users", Some(&["name", "amount"]));
///     b.record(&["Alice", "1502.30"]);
///     b.record(&["Bob", "340.00"]);
///     b.eot();
/// });
/// ```
///
/// Names (file/group labels and SOH header fields) are identifiers: passing a
/// control byte (`< 0x20`) in a name panics, matching the spec's "Canonical
/// Form" rule. Record/field values are escaped automatically.
#[derive(Default)]
pub struct Builder {
    buf: Vec<u8>,
}

impl Builder {
    pub fn new() -> Self {
        Builder { buf: Vec::new() }
    }

    /// Build a buffer by driving a fresh builder, returning its bytes.
    pub fn build<F: FnOnce(&mut Builder)>(f: F) -> Vec<u8> {
        let mut b = Builder::new();
        f(&mut b);
        b.into_bytes()
    }

    /// Write a file/database scope (FS + name).
    pub fn file(&mut self, name: &str) -> &mut Self {
        self.buf.push(FS);
        self.write_name(name);
        self
    }

    /// Write a group/table scope (GS + name) with optional SOH headers.
    pub fn group(&mut self, name: &str, headers: Option<&[&str]>) -> &mut Self {
        self.buf.push(GS);
        self.write_name(name);
        if let Some(h) = headers {
            self.buf.push(SOH);
            for (i, field) in h.iter().enumerate() {
                if i > 0 {
                    self.buf.push(US);
                }
                self.write_name(field);
            }
        }
        self
    }

    /// Write a standalone SOH header (e.g. for stream mode, where the header
    /// is appended and committed separately from the group).
    pub fn header(&mut self, names: &[&str]) -> &mut Self {
        self.buf.push(SOH);
        for (i, field) in names.iter().enumerate() {
            if i > 0 {
                self.buf.push(US);
            }
            self.write_name(field);
        }
        self
    }

    /// Write a record with positional fields.
    pub fn record(&mut self, fields: &[&str]) -> &mut Self {
        self.buf.push(RS);
        for (i, field) in fields.iter().enumerate() {
            if i > 0 {
                self.buf.push(US);
            }
            self.write_escaped(field);
        }
        self
    }

    /// Write an EOT marker.
    pub fn eot(&mut self) -> &mut Self {
        self.buf.push(EOT);
        self
    }

    /// Write an ETB commit marker (stream mode).
    pub fn etb(&mut self) -> &mut Self {
        self.buf.push(ETB);
        self
    }

    /// Write an ETB commit marker followed by an integrity payload. The
    /// payload may not contain control bytes (it is terminated by the next
    /// control code on read); a control byte panics.
    pub fn etb_payload(&mut self, payload: &str) -> &mut Self {
        self.buf.push(ETB);
        for &byte in payload.as_bytes() {
            assert!(byte >= 0x20, "ETB payload may not contain control bytes");
            self.buf.push(byte);
        }
        self
    }

    /// Write a nested sub-structure (STX … ETX).
    pub fn nested<F: FnOnce(&mut Builder)>(&mut self, f: F) -> &mut Self {
        self.buf.push(STX);
        f(self);
        self.buf.push(ETX);
        self
    }

    /// Write a reference to a named group (ENQ + name).
    pub fn reference(&mut self, name: &str) -> &mut Self {
        self.buf.push(ENQ);
        self.write_name(name);
        self
    }

    /// Write a single US-prefixed field value (escaped).
    pub fn field(&mut self, value: &str) -> &mut Self {
        self.buf.push(US);
        self.write_escaped(value);
        self
    }

    /// Write GS×`depth` for document-mode depth, then the section name.
    pub fn section(&mut self, name: &str, depth: usize) -> &mut Self {
        for _ in 0..depth {
            self.buf.push(GS);
        }
        self.write_name(name);
        self
    }

    /// Write a content block (RS + escaped text) for document mode.
    pub fn block(&mut self, text: &str) -> &mut Self {
        self.buf.push(RS);
        self.write_escaped(text);
        self
    }

    /// Write a list item (US + escaped text) for document mode.
    pub fn item(&mut self, text: &str) -> &mut Self {
        self.buf.push(US);
        self.write_escaped(text);
        self
    }

    /// The bytes built so far.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Consume the builder, returning the built buffer.
    #[inline]
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    // Writes a value, DLE-escaping any control bytes.
    fn write_escaped(&mut self, s: &str) {
        for &byte in s.as_bytes() {
            if byte < 0x20 {
                self.buf.push(DLE);
            }
            self.buf.push(byte);
        }
    }

    // Writes a name (label or header). Names are identifiers — control bytes
    // are illegal in them (see the spec's "Canonical Form").
    fn write_name(&mut self, s: &str) {
        for &byte in s.as_bytes() {
            assert!(
                byte >= 0x20,
                "Names may not contain control bytes (got 0x{byte:02x})"
            );
            self.buf.push(byte);
        }
    }
}
