//! C0DATA — structured data using ASCII C0 control codes.
//!
//! Values are plain UTF-8 text; structure is expressed through single-byte
//! control codes. This crate is a zero-copy reader/writer: accessors borrow
//! the input buffer and return slices into it.
//!
//! ```
//! let buf = c0::Builder::build(|b| {
//!     b.group("users", Some(&["name", "amount"]));
//!     b.record(&["Alice", "100"]);
//!     b.record(&["Bob", "200"]);
//! });
//! let table = c0::Table::new(&buf);
//! assert_eq!(table.record_count(), 2);
//! assert_eq!(table.record(0).field(0), b"Alice");
//! ```

use std::borrow::Cow;
use std::fmt;

mod builder;
mod document;
mod pretty;
mod stream;
mod table;
mod token;
mod tokenizer;

pub use builder::Builder;
pub use document::{Document, Group};
pub use pretty::{format, format_with, glyph, parse};
pub use stream::{open_log, read_log, FileLog, StreamReader, StreamWriter};
pub use table::{Record, Table};
pub use token::{Token, TokenType};
pub use tokenizer::{tokenize, Tokenizer};

/// Crate version (mirrors the Crystal reference's `C0::VERSION`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// Assigned C0 control codes.
pub const SOH: u8 = 0x01; // Header (field name declarations)
pub const STX: u8 = 0x02; // Open nested sub-structure / reference scope
pub const ETX: u8 = 0x03; // Close nested sub-structure / reference scope
pub const EOT: u8 = 0x04; // End of document / message
pub const ENQ: u8 = 0x05; // Reference (enquiry — look up named data)
pub const DLE: u8 = 0x10; // Escape (next byte is literal)
pub const ETB: u8 = 0x17; // Commit marker (stream mode block terminator)
pub const SUB: u8 = 0x1a; // Substitution (old → new, C0-DIFF)
pub const FS: u8 = 0x1c; // File / Database separator
pub const GS: u8 = 0x1d; // Group / Table / Section separator
pub const RS: u8 = 0x1e; // Record / Row separator
pub const US: u8 = 0x1f; // Unit / Field separator

/// Whether a byte is an assigned C0 control code.
#[inline]
pub fn is_assigned(byte: u8) -> bool {
    matches!(
        byte,
        SOH | STX | ETX | EOT | ENQ | DLE | ETB | SUB | FS | GS | RS | US
    )
}

/// Errors raised while tokenizing or decoding C0DATA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A control byte (< 0x20) that is not one of the assigned codes.
    UnassignedCode { byte: u8, position: usize },
    /// Input ended immediately after a DLE escape, with nothing to escape.
    UnexpectedEnd,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnassignedCode { byte, position } => write!(
                f,
                "Unassigned control code 0x{byte:02x} at position {position}"
            ),
            Error::UnexpectedEnd => write!(f, "Unexpected end of input after DLE escape"),
        }
    }
}

impl std::error::Error for Error {}

/// Crate result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Decode DLE escapes, returning the logical bytes of a value.
///
/// Zero-copy: returns a borrowed slice when no escapes are present, and an
/// owned buffer only when at least one DLE must be removed. A trailing DLE
/// with nothing to escape (only possible on a malformed slice) is dropped.
pub fn unescape(buf: &[u8]) -> Cow<'_, [u8]> {
    match buf.iter().position(|&b| b == DLE) {
        None => Cow::Borrowed(buf),
        Some(first) => {
            let mut out = Vec::with_capacity(buf.len());
            out.extend_from_slice(&buf[..first]);
            let mut i = first;
            while i < buf.len() {
                if buf[i] == DLE {
                    i += 1;
                    if i >= buf.len() {
                        break; // dangling escape on a malformed slice
                    }
                    out.push(buf[i]);
                } else {
                    out.push(buf[i]);
                }
                i += 1;
            }
            Cow::Owned(out)
        }
    }
}

/// Whether bytes are a canonical document unit for content addressing
/// (see the C0 spec's "Canonical Form"): well-formed, minimally escaped
/// (DLE appears only before bytes < 0x20), and free of framing bytes
/// (ETB, EOT). Stream logs validate per-block, not with this check.
pub fn canonical(buf: &[u8]) -> bool {
    let mut i = 0;
    while i < buf.len() {
        let byte = buf[i];
        if byte == DLE {
            if i + 1 >= buf.len() {
                return false; // dangling escape
            }
            if buf[i + 1] >= 0x20 {
                return false; // gratuitous escape
            }
            i += 2;
        } else if byte == ETB || byte == EOT {
            return false; // framing in a document unit
        } else if byte < 0x20 {
            if !is_assigned(byte) {
                return false; // unassigned code
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    true
}
