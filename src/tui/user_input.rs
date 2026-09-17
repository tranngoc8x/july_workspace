//! Byte-range markers the composer overlays on its text buffer.
//!
//! A [`TextElement`] marks a span of the composer buffer that behaves as one atomic unit: a paste
//! placeholder, a file attachment, an `@`-mention. The span stays a plain UTF-8 byte range into the
//! text so the buffer itself is never rewritten, and every edit remaps the ranges instead.
//!
//! Vendored from `codex-rs/protocol/src/user_input.rs` without the serde/ts-rs derives; July keeps
//! these types in-process and never puts them on the wire.

/// Conservative cap so one user message cannot monopolize a large context window.
pub const MAX_USER_INPUT_TEXT_CHARS: usize = 1 << 20;

/// Half-open byte range into a UTF-8 text buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// Start byte offset (inclusive).
    pub start: usize,
    /// End byte offset (exclusive).
    pub end: usize,
}

impl From<std::ops::Range<usize>> for ByteRange {
    fn from(range: std::ops::Range<usize>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}

/// A span of the parent text buffer rendered as a single element.
#[derive(Debug, Clone, PartialEq)]
pub struct TextElement {
    /// Byte range in the parent `text` buffer that this element occupies.
    pub byte_range: ByteRange,
    /// Display text shown in place of the span; falls back to the span itself.
    placeholder: Option<String>,
}

impl TextElement {
    pub fn new(byte_range: ByteRange, placeholder: Option<String>) -> Self {
        Self {
            byte_range,
            placeholder,
        }
    }

    /// Returns a copy of this element with a remapped byte range.
    ///
    /// The placeholder is preserved as-is; callers must ensure the new range still refers to the
    /// same logical element within the new text.
    pub fn map_range<F>(&self, map: F) -> Self
    where
        F: FnOnce(ByteRange) -> ByteRange,
    {
        Self {
            byte_range: map(self.byte_range),
            placeholder: self.placeholder.clone(),
        }
    }

    pub fn set_placeholder(&mut self, placeholder: Option<String>) {
        self.placeholder = placeholder;
    }

    /// Display text for this element, falling back to the slice of `text` it covers.
    pub fn placeholder<'a>(&'a self, text: &'a str) -> Option<&'a str> {
        self.placeholder
            .as_deref()
            .or_else(|| text.get(self.byte_range.start..self.byte_range.end))
    }
}

#[cfg(test)]
mod tests {
    use super::{ByteRange, TextElement};

    #[test]
    fn placeholder_falls_back_to_the_covered_slice() {
        let text = "hello world";
        let element = TextElement::new(ByteRange::from(6..11), None);
        assert_eq!(element.placeholder(text), Some("world"));

        let labelled = TextElement::new(ByteRange::from(6..11), Some("[image]".into()));
        assert_eq!(labelled.placeholder(text), Some("[image]"));
    }

    #[test]
    fn out_of_bounds_range_yields_no_placeholder() {
        let element = TextElement::new(ByteRange::from(3..99), None);
        assert_eq!(element.placeholder("abc"), None);
    }

    #[test]
    fn map_range_keeps_the_placeholder() {
        let element = TextElement::new(ByteRange::from(0..2), Some("x".into()));
        let moved = element.map_range(|range| ByteRange {
            start: range.start + 5,
            end: range.end + 5,
        });
        assert_eq!(moved.byte_range, ByteRange { start: 5, end: 7 });
        assert_eq!(moved.placeholder("0123456789"), Some("x"));
    }
}
