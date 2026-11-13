//! Checks a buffer against GRAMMAR.md by parsing it with the [`Parser`].
//! [`validate`] fails on the first structural error; [`canonical`]
//! additionally applies the canonical constraints and answers `false`
//! instead of failing.

use crate::*;

/// The well-formedness and canonicality checker. Use [`validate`] and
/// [`canonical`] unless you want the reasons a span is not canonical.
pub struct Validator<'a> {
    buf: &'a [u8],
    parser: Parser<'a>,
}

/// Fail on the first structural error.
pub fn validate(buf: &[u8]) -> Result<()> {
    Validator::new(buf, false).run()
}

/// Well-formed and canonical: no layout, one spelling per value, keys
/// unique within a keyed record, no framing, no comments. Supersession is
/// content and is kept.
pub fn canonical(buf: &[u8]) -> bool {
    let mut v = Validator::new(buf, true);
    v.run().is_ok() && v.ok()
}

impl<'a> Validator<'a> {
    pub fn new(buf: &'a [u8], canonical: bool) -> Self {
        Validator {
            buf,
            parser: Parser::new(buf, canonical),
        }
    }

    /// Parse the whole buffer as a container of documents.
    pub fn run(&mut self) -> Result<()> {
        Container::parse(self.buf, self.buf.len(), &mut self.parser).map(|_| ())
    }

    /// Whether every canonical constraint held (meaningful after `run`).
    pub fn ok(&self) -> bool {
        self.parser.ok()
    }

    /// Why the span is not canonical, one entry per finding.
    pub fn warnings(&self) -> &[&'static str] {
        self.parser.warnings()
    }
}
