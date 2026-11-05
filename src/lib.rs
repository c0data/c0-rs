//! C0DATA — structured data using ASCII C0 control codes.
//!
//! Values are UTF-8 text (any bytes, as text); structure is sixteen
//! single-byte control codes. This crate is a zero-copy reader/writer for
//! the 1.0 design: [`Container`], [`Document`] and [`Body`] index a buffer
//! and hand back slices into it, [`Builder`] writes canonical compact form,
//! [`validate`] and [`canonical`] check well-formedness and canonicality,
//! and the `stream`, `pretty`, `diff`, `csv` and `json` modules cover the
//! rest of the spec.
//!
//! ```
//! let buf = c0::Builder::build(|b| {
//!     b.group("users", Some(&["name", "amount"]));
//!     b.record(&["Alice", "100"]);
//!     b.record(&["Bob", "200"]);
//! });
//! let body = c0::Body::new(&buf).unwrap();
//! let users = body.group(0);
//! assert_eq!(users.headers(), [b"name".as_slice(), b"amount"]);
//! assert_eq!(users.record(1).value(0).unwrap().as_ref(), b"Bob");
//! assert!(c0::canonical(&buf));
//! ```

use std::borrow::Cow;
use std::fmt;

mod body;
mod builder;
mod document;
mod pretty;
mod stream;
mod validator;

pub mod csv;
pub mod diff;
pub mod json;
pub mod scan;

pub use body::{Body, FileView, Group, Pair, Record, Reference, Table};
pub use builder::Builder;
pub use document::{Container, Document, Heading};
pub use pretty::{align, format, format_mode, format_with, glyph, parse, FormatMode};
pub use stream::{
    last_commit_end, open_log, open_log_with, read_log, repair_log, FileLog, StreamReader,
    StreamWriter,
};
pub use validator::{canonical, validate, Validator};

/// Crate version (mirrors the Crystal reference's `C0::VERSION`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// Assigned C0 control codes (DESIGN.md "Assigned Control Codes").
pub const SOH: u8 = 0x01; // Heading: a key for the value after it, or SOH RS a header row
pub const STX: u8 = 0x02; // Start of text (literal value)
pub const ETX: u8 = 0x03; // End of text
pub const EOT: u8 = 0x04; // End of document
pub const ENQ: u8 = 0x05; // Reference
pub const BEL: u8 = 0x07; // Comment: a note, or marks the next element dead
pub const DLE: u8 = 0x10; // Recurse: opens a nested level (outside text); escape (inside text)
pub const SYN: u8 = 0x16; // Start of document, then the heading
pub const ETB: u8 = 0x17; // Commit marker (stream mode)
pub const CAN: u8 = 0x18; // Close a nested level
pub const EM: u8 = 0x19; // End of medium: raw blob follows
pub const SUB: u8 = 0x1a; // Substitution (C0-DIFF)
pub const FS: u8 = 0x1c; // File separator
pub const GS: u8 = 0x1d; // Group separator
pub const RS: u8 = 0x1e; // Record separator
pub const US: u8 = 0x1f; // Unit separator

// Layout codes outside text.
pub const HT: u8 = 0x09;
pub const LF: u8 = 0x0a;
pub const CR: u8 = 0x0d;
pub const SP: u8 = 0x20;

/// Whether a byte is layout outside text (SP, HT, LF, CR).
#[inline]
pub fn is_layout(byte: u8) -> bool {
    byte == SP || byte == HT || byte == LF || byte == CR
}

/// Whether a byte is a control code (below 0x20).
#[inline]
pub fn is_control(byte: u8) -> bool {
    byte < 0x20
}

/// Whether a byte is one of the sixteen assigned structural or framing codes.
#[inline]
pub fn is_assigned(byte: u8) -> bool {
    matches!(
        byte,
        SOH | STX | ETX | EOT | ENQ | BEL | DLE | SYN | ETB | CAN | EM | SUB | FS | GS | RS | US
    )
}

/// Whether a byte is one of the seven codes that take a DLE inside text.
#[inline]
pub fn is_text_escaped(byte: u8) -> bool {
    matches!(byte, STX | ETX | DLE | ETB | EOT | SYN | EM)
}

/// Errors raised while scanning, validating or decoding C0DATA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Malformed input: what went wrong and the byte offset.
    Syntax { message: String, position: usize },
    /// A control byte that is not one of the assigned codes, outside text.
    UnassignedCode { byte: u8, position: usize },
    /// The input ended inside a construct.
    UnexpectedEnd { what: String, position: usize },
    /// No group or key by that name.
    Key(String),
    /// A value holds a Control Picture character and cannot be shown in
    /// pretty form without ambiguity.
    GlyphInData,
    /// A misuse of the API: a unit that is not a level, a level that is
    /// not a list, a name that is not legal, and the like.
    Other(String),
}

impl Error {
    pub(crate) fn syntax(message: impl Into<String>, position: usize) -> Self {
        Error::Syntax {
            message: message.into(),
            position,
        }
    }

    pub(crate) fn end(what: impl Into<String>, position: usize) -> Self {
        Error::UnexpectedEnd {
            what: what.into(),
            position,
        }
    }

    pub(crate) fn other(message: impl Into<String>) -> Self {
        Error::Other(message.into())
    }

    /// The byte offset the error refers to, when it has one.
    pub fn position(&self) -> Option<usize> {
        match self {
            Error::Syntax { position, .. }
            | Error::UnassignedCode { position, .. }
            | Error::UnexpectedEnd { position, .. } => Some(*position),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax { message, position } => write!(f, "{message} at byte {position}"),
            Error::UnassignedCode { byte, position } => {
                write!(f, "Unassigned control code 0x{byte:02x} at byte {position}")
            }
            Error::UnexpectedEnd { what, position } => {
                write!(f, "Unexpected end of input: {what} at byte {position}")
            }
            Error::Key(what) => write!(f, "{what}"),
            Error::GlyphInData => write!(
                f,
                "value contains a Control Picture character and cannot be shown in pretty form"
            ),
            Error::Other(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

/// Crate result type.
pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------

/// Trim layout from both ends of a raw slice.
pub fn trim(raw: &[u8]) -> &[u8] {
    let mut s = 0;
    let mut e = raw.len();
    while s < e && is_layout(raw[s]) {
        s += 1;
    }
    while e > s && is_layout(raw[e - 1]) {
        e -= 1;
    }
    &raw[s..e]
}

/// Whether the raw unit (layout included) is a text value.
pub fn is_text(raw: &[u8]) -> bool {
    trim(raw).first() == Some(&STX)
}

/// Whether the raw unit is a nested level.
pub fn is_level(raw: &[u8]) -> bool {
    trim(raw).first() == Some(&DLE)
}

/// Whether the raw unit is a reference.
pub fn is_reference(raw: &[u8]) -> bool {
    trim(raw).first() == Some(&ENQ)
}

/// The logical value of a raw unit slice: for text, the inside with its
/// escapes decoded (zero-copy when there are none); for a plain value, the
/// slice with edge layout removed. A unit holding only a note or a dead
/// level is the empty value. Fails on malformed input, on a level and on a
/// reference (see [`Record::level`] and [`Record::reference`]).
pub fn value(raw: &[u8]) -> Result<Cow<'_, [u8]>> {
    let t = trim(raw);
    let Some(&first) = t.first() else {
        return Ok(Cow::Borrowed(t));
    };
    match first {
        STX => {
            let stop = scan::text(t, 0, t.len())?;
            only_notes_after(t, stop, "text cannot be part of a plain value")?;
            Ok(decode_text(&t[1..stop - 1]))
        }
        BEL => {
            only_notes_after(t, 0, "value after a comment in the same unit")?;
            Ok(Cow::Borrowed(&[]))
        }
        DLE => Err(Error::syntax("a nested level is not a scalar value", 0)),
        ENQ => Err(Error::syntax("a reference is not a scalar value", 0)),
        _ => {
            for (i, &b) in t.iter().enumerate() {
                if b == BEL {
                    only_notes_after(t, i, "value after a note in the same unit")?;
                    return Ok(Cow::Borrowed(trim(&t[..i])));
                }
                if b < 0x20 {
                    let what = if is_layout(b) {
                        "layout code inside a plain value"
                    } else {
                        "control code inside a plain value"
                    };
                    return Err(Error::syntax(what, i));
                }
            }
            Ok(Cow::Borrowed(t))
        }
    }
}

// From pos to the end of the unit only notes (and layout) may follow.
fn only_notes_after(t: &[u8], pos: usize, what: &str) -> Result<()> {
    let mut pos = scan::layout(t, pos, t.len());
    while pos < t.len() {
        if t[pos] != BEL {
            return Err(Error::syntax(what, pos));
        }
        pos = scan::layout(t, scan::comment(t, pos, t.len())?, t.len());
    }
    Ok(())
}

/// Decode the inside of a text: DLE escapes removed. Zero-copy when there
/// are none. The inside of a scanned text never ends in a lone DLE; one on
/// an arbitrary slice is dropped.
pub fn decode_text(inside: &[u8]) -> Cow<'_, [u8]> {
    let Some(first) = inside.iter().position(|&b| b == DLE) else {
        return Cow::Borrowed(inside);
    };
    let mut out = Vec::with_capacity(inside.len());
    out.extend_from_slice(&inside[..first]);
    let mut i = first;
    while i < inside.len() {
        if inside[i] == DLE {
            i += 1;
            if i >= inside.len() {
                break;
            }
        }
        out.push(inside[i]);
        i += 1;
    }
    Cow::Owned(out)
}

/// Whether a value can be written plain: no byte below 0x20 and no edge
/// whitespace.
pub fn is_plain(value: &[u8]) -> bool {
    match (value.first(), value.last()) {
        (None, _) | (_, None) => true,
        (Some(&f), Some(&l)) => !is_layout(f) && !is_layout(l) && value.iter().all(|&b| b >= 0x20),
    }
}

/// Write a value in its one canonical spelling: plain when it can be, text
/// otherwise, with only the seven codes escaped.
pub fn write_value(out: &mut Vec<u8>, value: &[u8]) {
    if is_plain(value) {
        out.extend_from_slice(value);
    } else {
        out.push(STX);
        for &b in value {
            if b < 0x20 && is_text_escaped(b) {
                out.push(DLE);
            }
            out.push(b);
        }
        out.push(ETX);
    }
}

/// Whether bytes are a legal name (label, key, header name): no byte below
/// 0x20 and no edge whitespace. A name may be empty.
pub fn is_name(value: &[u8]) -> bool {
    is_plain(value)
}

/// Write a name; fails if it is not one.
pub fn write_name(out: &mut Vec<u8>, name: &[u8]) -> Result<()> {
    if !is_name(name) {
        return Err(Error::other(format!(
            "not a legal name: {:?}",
            String::from_utf8_lossy(name)
        )));
    }
    out.extend_from_slice(name);
    Ok(())
}
