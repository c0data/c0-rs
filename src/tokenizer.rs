use crate::*;

/// Zero-copy tokenizer for C0DATA.
///
/// Scans a byte buffer for control codes (`< 0x20`) and yields tokens as
/// offsets into the original buffer. The hot loop is a single comparison:
/// `byte < 0x20`.
///
/// Implements [`Iterator`] over `Result<Token>`; an error (unassigned code
/// or dangling DLE) is yielded once, after which iteration ends.
pub struct Tokenizer<'a> {
    buf: &'a [u8],
    pos: usize,
    done: bool,
}

impl<'a> Tokenizer<'a> {
    #[inline]
    pub fn new(buf: &'a [u8]) -> Self {
        Tokenizer {
            buf,
            pos: 0,
            done: false,
        }
    }
}

impl Iterator for Tokenizer<'_> {
    type Item = Result<Token>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.pos >= self.buf.len() {
            return None;
        }
        let byte = self.buf[self.pos];

        if byte < 0x20 {
            if byte == DLE {
                // Escape: consume DLE + next byte, emit Data for the escaped byte.
                self.pos += 1;
                if self.pos >= self.buf.len() {
                    self.done = true;
                    return Some(Err(Error::UnexpectedEnd));
                }
                let tok = Token {
                    kind: TokenType::Data,
                    start: self.pos,
                    end: self.pos + 1,
                };
                self.pos += 1;
                Some(Ok(tok))
            } else {
                match TokenType::control(byte) {
                    Some(kind) => {
                        let tok = Token {
                            kind,
                            start: self.pos,
                            end: self.pos + 1,
                        };
                        self.pos += 1;
                        Some(Ok(tok))
                    }
                    None => {
                        let position = self.pos;
                        self.done = true;
                        Some(Err(Error::UnassignedCode { byte, position }))
                    }
                }
            }
        } else {
            // Scan a run of data bytes (>= 0x20).
            let start = self.pos;
            self.pos += 1;
            while self.pos < self.buf.len() && self.buf[self.pos] >= 0x20 {
                self.pos += 1;
            }
            Some(Ok(Token {
                kind: TokenType::Data,
                start,
                end: self.pos,
            }))
        }
    }
}

/// Collect all tokens, returning the first error if the buffer is malformed.
pub fn tokenize(buf: &[u8]) -> Result<Vec<Token>> {
    Tokenizer::new(buf).collect()
}
