//! LSP positions: 0-based lines and UTF-8 or UTF-16 characters, against the compiler's
//! 1-based line/byte-column positions and byte offsets.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

impl Encoding {
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16 => "utf-16",
        }
    }
}

/// The byte offsets of a 0-based line's content: `(start, end)`, excluding the line
/// terminator (including a `\r` of `\r\n`).
pub fn line_bounds(text: &str, line: usize) -> Option<(usize, usize)> {
    let mut current = 0;
    let mut start = 0;
    for (i, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            if current == line {
                let end = if i > start && text.as_bytes()[i - 1] == b'\r' {
                    i - 1
                } else {
                    i
                };
                return Some((start, end));
            }
            current += 1;
            start = i + 1;
        }
    }
    (current == line).then_some((start, text.len()))
}

/// The LSP position of a byte offset: `(line, character)`, 0-based, with the offset
/// clamped to the text and moved to a character boundary.
pub fn offset_to_position(text: &str, offset: usize, encoding: Encoding) -> (u32, u32) {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let mut line = 0u32;
    let mut start = 0usize;
    for (i, byte) in text.bytes().enumerate() {
        if i >= offset {
            break;
        }
        if byte == b'\n' {
            line += 1;
            start = i + 1;
        }
    }
    let byte = offset - start;
    let byte = match line_bounds(text, line as usize) {
        Some((_, end)) => byte.min(end - start),
        None => 0,
    };
    let character = match encoding {
        Encoding::Utf8 => byte,
        Encoding::Utf16 => text[start..start + byte].encode_utf16().count(),
    };
    (line, character as u32)
}

/// The byte offset of an LSP position. A line or character past the text is clamped:
/// past the last line gives the end of the text, past a line's content gives its end,
/// and a character inside a multi-unit code point moves to its start.
pub fn position_to_offset(text: &str, line: u32, character: u32, encoding: Encoding) -> usize {
    let Some((start, end)) = line_bounds(text, line as usize) else {
        return text.len();
    };
    let content = &text[start..end];
    let byte = match encoding {
        Encoding::Utf8 => {
            let mut byte = (character as usize).min(content.len());
            while !content.is_char_boundary(byte) {
                byte -= 1;
            }
            byte
        }
        Encoding::Utf16 => {
            let wanted = character as usize;
            let mut units = 0usize;
            let mut byte = content.len();
            for (i, ch) in content.char_indices() {
                if units >= wanted {
                    byte = i;
                    break;
                }
                let next = units + ch.len_utf16();
                if next > wanted {
                    byte = i;
                    break;
                }
                units = next;
            }
            byte
        }
    };
    start + byte
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bytes: a=1, é=2, 日=3, 😀=4, b=1. UTF-16 units: 1, 1, 1, 2, 1.
    const LINE: &str = "aé日😀b";

    #[test]
    fn offsets_map_to_utf8_and_utf16_positions() {
        for (offset, utf8, utf16) in [
            (0, 0, 0),
            (1, 1, 1),
            (3, 3, 2),
            (6, 6, 3),
            (10, 10, 5),
            (11, 11, 6),
        ] {
            assert_eq!(
                offset_to_position(LINE, offset, Encoding::Utf8),
                (0, utf8),
                "{offset}"
            );
            assert_eq!(
                offset_to_position(LINE, offset, Encoding::Utf16),
                (0, utf16),
                "{offset}"
            );
        }
    }

    #[test]
    fn positions_map_back_to_offsets() {
        for (character, offset) in [(0, 0), (1, 1), (2, 3), (3, 6), (5, 10), (6, 11)] {
            assert_eq!(
                position_to_offset(LINE, 0, character, Encoding::Utf16),
                offset,
                "{character}"
            );
        }
        for (character, offset) in [(0, 0), (1, 1), (3, 3), (6, 6), (10, 10), (11, 11)] {
            assert_eq!(
                position_to_offset(LINE, 0, character, Encoding::Utf8),
                offset,
                "{character}"
            );
        }
        // A character inside a 4-byte code point moves to its start.
        assert_eq!(position_to_offset("é", 0, 1, Encoding::Utf8), 0);
        assert_eq!(position_to_offset("日", 0, 2, Encoding::Utf8), 0);
        // A character inside the 4-byte, 2-unit emoji moves to its start.
        assert_eq!(position_to_offset(LINE, 0, 4, Encoding::Utf16), 6);
    }

    #[test]
    fn lines_are_zero_based_and_exclude_terminators() {
        let text = "ab\ncd\r\nef\n";
        assert_eq!(line_bounds(text, 0), Some((0, 2)));
        assert_eq!(line_bounds(text, 1), Some((3, 5)));
        assert_eq!(line_bounds(text, 2), Some((7, 9)));
        assert_eq!(line_bounds(text, 3), Some((10, 10)));
        assert_eq!(line_bounds(text, 4), None);
        assert_eq!(offset_to_position(text, 6, Encoding::Utf8), (1, 2));
        assert_eq!(position_to_offset(text, 1, 2, Encoding::Utf8), 5);
        assert_eq!(position_to_offset(text, 2, 0, Encoding::Utf8), 7);
    }

    #[test]
    fn positions_at_and_past_the_end_of_a_line_are_clamped() {
        for encoding in [Encoding::Utf8, Encoding::Utf16] {
            assert_eq!(
                position_to_offset(LINE, 0, 1000, encoding),
                LINE.len(),
                "{encoding:?}"
            );
            assert_eq!(
                position_to_offset(LINE, 9, 0, encoding),
                LINE.len(),
                "{encoding:?}"
            );
            let expected = match encoding {
                Encoding::Utf8 => LINE.len() as u32,
                Encoding::Utf16 => 6,
            };
            assert_eq!(
                offset_to_position(LINE, LINE.len() + 100, encoding),
                (0, expected),
                "{encoding:?}"
            );
        }
    }

    #[test]
    fn a_line_with_three_byte_characters() {
        let line = "ééé";
        assert_eq!(offset_to_position(line, 6, Encoding::Utf8), (0, 6));
        assert_eq!(offset_to_position(line, 6, Encoding::Utf16), (0, 3));
        assert_eq!(position_to_offset(line, 0, 3, Encoding::Utf16), 6);
        assert_eq!(position_to_offset(line, 0, 2, Encoding::Utf16), 4);
    }

    #[test]
    fn empty_text_has_one_empty_line() {
        assert_eq!(line_bounds("", 0), Some((0, 0)));
        assert_eq!(offset_to_position("", 0, Encoding::Utf16), (0, 0));
        assert_eq!(position_to_offset("", 0, 0, Encoding::Utf16), 0);
        assert_eq!(position_to_offset("", 3, 3, Encoding::Utf16), 0);
    }
}
