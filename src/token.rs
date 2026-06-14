use crate::*;

/// The kind of a token emitted by the [`Tokenizer`](crate::Tokenizer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenType {
    /// Data content between control codes.
    Data,
    Soh,
    Stx,
    Etx,
    Eot,
    Enq,
    /// Escape — consumed during tokenization, never emitted as its own token.
    Dle,
    Etb,
    Sub,
    Fs,
    Gs,
    Rs,
    Us,
}

impl TokenType {
    /// The token type for an assigned control byte, or `None` if unassigned.
    /// `DLE` returns `None` because it is consumed, not emitted.
    #[inline]
    pub(crate) fn control(byte: u8) -> Option<TokenType> {
        Some(match byte {
            SOH => TokenType::Soh,
            STX => TokenType::Stx,
            ETX => TokenType::Etx,
            EOT => TokenType::Eot,
            ENQ => TokenType::Enq,
            ETB => TokenType::Etb,
            SUB => TokenType::Sub,
            FS => TokenType::Fs,
            GS => TokenType::Gs,
            RS => TokenType::Rs,
            US => TokenType::Us,
            _ => return None,
        })
    }
}

/// A token as offsets into the source buffer. Zero-copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenType,
    pub start: usize,
    pub end: usize,
}

impl Token {
    /// Byte length of this token's span.
    #[inline]
    pub fn size(&self) -> usize {
        self.end - self.start
    }

    /// The token's bytes as a slice into `buf`. Zero-copy.
    #[inline]
    pub fn value<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.start..self.end]
    }
}
