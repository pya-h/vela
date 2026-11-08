use std::borrow::Cow;
use std::ops::Range;

use vela::ast::Span;

/// Converts between byte offsets, 1-based char line/column `Span`s, and JS UTF-16 offsets.
pub struct LineIndex<'a> {
    source: Cow<'a, str>,
    starts: Vec<usize>,
    starts_utf16: Vec<usize>,
    ascii: Vec<bool>,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        LineIndex::of(Cow::Borrowed(source))
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn owned(source: String) -> LineIndex<'static> {
        LineIndex::of(Cow::Owned(source))
    }

    fn of(source: Cow<'a, str>) -> Self {
        let mut starts = vec![0];
        let mut starts_utf16 = vec![0];
        let mut ascii = vec![true];
        let mut utf16 = 0;
        for (byte, c) in source.char_indices() {
            utf16 += c.len_utf16();
            if !c.is_ascii()
                && let Some(last) = ascii.last_mut()
            {
                *last = false;
            }
            if c == '\n' {
                starts.push(byte + 1);
                starts_utf16.push(utf16);
                ascii.push(true);
            }
        }
        LineIndex {
            source,
            starts,
            starts_utf16,
            ascii,
        }
    }

    pub fn byte_of(&self, span: Span) -> usize {
        let Some(&start) = span.line.checked_sub(1).and_then(|i| self.starts.get(i)) else {
            return self.source.len();
        };
        // Compiler columns start after a leading BOM.
        let start = if start == 0 && self.source.starts_with('\u{feff}') {
            '\u{feff}'.len_utf8()
        } else {
            start
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

    pub fn utf16_of(&self, byte: usize) -> usize {
        let byte = self.floor_char_boundary(byte);
        let line = self
            .starts
            .partition_point(|&start| start <= byte)
            .saturating_sub(1);
        let (Some(&start), Some(&start_utf16)) =
            (self.starts.get(line), self.starts_utf16.get(line))
        else {
            return 0;
        };
        if self.ascii.get(line) == Some(&true) {
            return start_utf16 + (byte - start);
        }
        let text = self.source.get(start..byte).unwrap_or_default();
        start_utf16 + text.chars().map(char::len_utf16).sum::<usize>()
    }

    pub fn byte_of_utf16(&self, utf16: usize) -> usize {
        let line = self
            .starts_utf16
            .partition_point(|&start| start <= utf16)
            .saturating_sub(1);
        let (Some(&start), Some(&start_utf16)) =
            (self.starts.get(line), self.starts_utf16.get(line))
        else {
            return 0;
        };
        if self.ascii.get(line) == Some(&true) {
            let line_end = self
                .starts
                .get(line + 1)
                .copied()
                .unwrap_or(self.source.len());
            return (start + utf16.saturating_sub(start_utf16)).min(line_end);
        }
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

    pub fn utf16_range(&self, range: &Range<usize>) -> Range<usize> {
        self.utf16_of(range.start)..self.utf16_of(range.end)
    }

    pub(crate) fn cursor(&self) -> Utf16Cursor<'_, 'a> {
        Utf16Cursor {
            index: self,
            byte: 0,
            utf16: 0,
        }
    }

    fn floor_char_boundary(&self, byte: usize) -> usize {
        let mut byte = byte.min(self.source.len());
        while !self.source.is_char_boundary(byte) {
            byte -= 1;
        }
        byte
    }
}

pub(crate) struct Utf16Cursor<'i, 'a> {
    index: &'i LineIndex<'a>,
    byte: usize,
    utf16: usize,
}

impl Utf16Cursor<'_, '_> {
    pub(crate) fn utf16_of(&mut self, byte: usize) -> usize {
        let byte = self.index.floor_char_boundary(byte);
        if byte < self.byte {
            self.utf16 = self.index.utf16_of(byte);
        } else {
            let text = self.index.source.get(self.byte..byte).unwrap_or_default();
            self.utf16 += if text.is_ascii() {
                text.len()
            } else {
                text.chars().map(char::len_utf16).sum()
            };
        }
        self.byte = byte;
        self.utf16
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]

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
    fn columns_start_after_a_byte_order_mark() {
        let index = LineIndex::new("\u{feff}fi\nx");
        assert_eq!(index.byte_of(Span::new(1, 1)), 3);
        assert_eq!(index.byte_of(Span::new(2, 1)), 6);
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

    #[test]
    fn a_cursor_converts_offsets_in_order_as_the_index_does() {
        let source = "é𝄞x\n𝄞y ab\ncd é";
        let index = LineIndex::new(source);
        let mut cursor = index.cursor();
        let offsets: Vec<usize> = source
            .char_indices()
            .map(|(byte, _)| byte)
            .chain([source.len(), source.len() + 3])
            .collect();
        for &byte in &offsets {
            assert_eq!(cursor.utf16_of(byte), index.utf16_of(byte), "at {}", byte);
        }
        // An offset before the last one starts over.
        assert_eq!(cursor.utf16_of(2), index.utf16_of(2));
    }
}
