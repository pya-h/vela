//! Converting between the three ways positions are written: the compiler's
//! line and column (1-based, columns counted in characters), byte offsets
//! (for slicing the source), and UTF-16 offsets (JavaScript string indices,
//! which editors such as CodeMirror use).

use std::ops::Range;

use vela::ast::Span;

/// Line starts of one source text, for fast position conversion.
pub struct LineIndex<'a> {
    source: &'a str,
    /// Byte offset of the start of each line.
    starts: Vec<usize>,
    /// UTF-16 offset of the start of each line.
    starts_utf16: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut starts = vec![0];
        let mut starts_utf16 = vec![0];
        let mut utf16 = 0;
        for (byte, c) in source.char_indices() {
            utf16 += c.len_utf16();
            if c == '\n' {
                starts.push(byte + 1);
                starts_utf16.push(utf16);
            }
        }
        LineIndex { source, starts, starts_utf16 }
    }

    /// The byte offset of a compiler position. A column past the end of its
    /// line stops at the line's end, and a line past the last one at the end
    /// of the source.
    pub fn byte_of(&self, span: Span) -> usize {
        let Some(&start) = span.line.checked_sub(1).and_then(|i| self.starts.get(i)) else {
            return self.source.len();
        };
        let line = self.source.get(start..).unwrap_or_default();
        let mut offset = start;
        for c in line.chars().take(span.col.saturating_sub(1)) {
            if c == '\n' {
                break;
            }
            offset += c.len_utf8();
        }
        offset
    }

    /// The UTF-16 offset of a byte offset, which is clamped to the source and
    /// rounded down to a character boundary.
    pub fn utf16_of(&self, byte: usize) -> usize {
        let byte = self.floor_char_boundary(byte);
        let line = self.starts.partition_point(|&start| start <= byte).saturating_sub(1);
        let (Some(&start), Some(&start_utf16)) = (self.starts.get(line), self.starts_utf16.get(line)) else {
            return 0;
        };
        let text = self.source.get(start..byte).unwrap_or_default();
        start_utf16 + text.chars().map(char::len_utf16).sum::<usize>()
    }

    /// The byte offset of a UTF-16 offset, clamped to the source. An offset
    /// in the middle of a character (between the two halves of a surrogate
    /// pair) moves to the start of that character.
    pub fn byte_of_utf16(&self, utf16: usize) -> usize {
        let line = self.starts_utf16.partition_point(|&start| start <= utf16).saturating_sub(1);
        let (Some(&start), Some(&start_utf16)) = (self.starts.get(line), self.starts_utf16.get(line)) else {
            return 0;
        };
        let mut offset = start;
        let mut remaining = utf16.saturating_sub(start_utf16);
        for c in self.source.get(start..).unwrap_or_default().chars() {
            if remaining < c.len_utf16() {
                break;
            }
            remaining -= c.len_utf16();
            offset += c.len_utf8();
        }
        offset
    }

    /// A byte range as a UTF-16 range.
    pub fn utf16_range(&self, range: &Range<usize>) -> Range<usize> {
        self.utf16_of(range.start)..self.utf16_of(range.end)
    }

    fn floor_char_boundary(&self, byte: usize) -> usize {
        let mut byte = byte.min(self.source.len());
        while !self.source.is_char_boundary(byte) {
            byte -= 1;
        }
        byte
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

    use super::*;

    #[test]
    fn converts_ascii_positions() {
        let index = LineIndex::new("ab\ncd\n");
        assert_eq!(index.byte_of(Span::new(2, 2)), 4);
        assert_eq!(index.utf16_of(4), 4);
        assert_eq!(index.byte_of_utf16(4), 4);
    }

    #[test]
    fn counts_utf16_units_for_non_ascii_text() {
        // 'é' is 2 bytes and 1 UTF-16 unit; '𝄞' is 4 bytes and 2 UTF-16 units.
        let source = "é𝄞x\n𝄞y";
        let index = LineIndex::new(source);
        let x = source.find('x').unwrap();
        let y = source.find('y').unwrap();
        assert_eq!((index.utf16_of(x), index.utf16_of(y)), (3, 7));
        assert_eq!((index.byte_of_utf16(3), index.byte_of_utf16(7)), (x, y));
        // Columns count characters: 'x' is the third character of line 1.
        assert_eq!(index.byte_of(Span::new(1, 3)), x);
        assert_eq!(index.byte_of(Span::new(2, 2)), y);
        // Inside the surrogate pair of '𝄞' on line 2.
        assert_eq!(index.byte_of_utf16(6), y - 4);
        // Inside the bytes of 'é'.
        assert_eq!(index.utf16_of(1), 0);
    }

    #[test]
    fn clamps_out_of_range_positions() {
        let index = LineIndex::new("ab\ncd");
        assert_eq!(index.byte_of(Span::new(1, 99)), 2);
        assert_eq!(index.byte_of(Span::new(9, 1)), 5);
        assert_eq!(index.byte_of(Span::new(0, 1)), 5);
        assert_eq!(index.utf16_of(99), 5);
        assert_eq!(index.byte_of_utf16(99), 5);
    }
}
