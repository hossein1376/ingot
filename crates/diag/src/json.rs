//! A JSON value, a strict RFC 8259 parser and a compact writer.
//!
//! `ingot serve` reads one request object per line (tools.md §6) and writes request
//! `id`s back exactly as they were given. A number therefore keeps its source text.
//! Objects keep their member order. There are no dependencies: the parser is small and
//! builds on the same string escaper the diagnostic lines use.

use crate::diagnostic::json_string;
use std::fmt;

/// The largest number of nested arrays and objects the parser accepts.
pub const MAX_DEPTH: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// The number as written: `1e5` stays `1e5`, so an `id` is written back exactly.
    Number(String),
    String(String),
    Array(Vec<Json>),
    /// Members in source order.
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Number(n) => n.parse().ok(),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(n) => n.parse().ok(),
            _ => None,
        }
    }

    /// The last member with this key, or `None` when this is not an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members
                .iter()
                .rev()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The compact JSON text of this value.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    /// Append the compact JSON text of this value to `out`.
    pub fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Number(number) => out.push_str(number),
            Json::String(text) => json_string(out, text),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Object(members) => {
                out.push('{');
                for (i, (key, value)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    json_string(out, key);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Byte offset in the parsed text.
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

impl std::error::Error for Error {}

/// Parse one JSON value. Surrounding whitespace is allowed; anything after the value
/// is an error.
pub fn parse(text: &str) -> Result<Json, Error> {
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        pos: 0,
    };
    parser.skip_whitespace();
    let value = parser.value(0)?;
    parser.skip_whitespace();
    if parser.pos < parser.bytes.len() {
        return Err(parser.error("trailing characters after the value"));
    }
    Ok(value)
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, message: impl Into<String>) -> Error {
        Error {
            offset: self.pos,
            message: message.into(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, Error> {
        if depth > MAX_DEPTH {
            return Err(self.error("values are nested too deeply"));
        }
        match self.peek() {
            Some(b'n') => self.literal("null", Json::Null),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::String),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("expected a JSON value")),
            None => Err(self.error("expected a JSON value")),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, Error> {
        if self.text[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.error(format!("expected `{word}`")))
        }
    }

    fn string(&mut self) -> Result<String, Error> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.error("unterminated string"));
            };
            match byte {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    self.escape(&mut out)?;
                }
                0x00..=0x1f => return Err(self.error("control character in string")),
                _ => {
                    let ch = self.text[self.pos..]
                        .chars()
                        .next()
                        .expect("a byte of valid UTF-8 starts a char");
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<(), Error> {
        let Some(byte) = self.peek() else {
            return Err(self.error("unterminated escape"));
        };
        self.pos += 1;
        match byte {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{8}'),
            b'f' => out.push('\u{c}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => out.push(self.unicode_escape()?),
            _ => return Err(self.error("unknown escape")),
        }
        Ok(())
    }

    fn hex4(&mut self) -> Result<u16, Error> {
        let mut value = 0u16;
        for _ in 0..4 {
            let Some(byte) = self.peek() else {
                return Err(self.error("unterminated unicode escape"));
            };
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(self.error("bad hex digit in unicode escape")),
            };
            value = value * 16 + u16::from(digit);
            self.pos += 1;
        }
        Ok(value)
    }

    fn unicode_escape(&mut self) -> Result<char, Error> {
        let first = self.hex4()?;
        let code = if (0xd800..=0xdbff).contains(&first) {
            if !self.text[self.pos..].starts_with("\\u") {
                return Err(self.error("lone high surrogate"));
            }
            self.pos += 2;
            let second = self.hex4()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return Err(self.error("lone high surrogate"));
            }
            0x10000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
        } else if (0xdc00..=0xdfff).contains(&first) {
            return Err(self.error("lone low surrogate"));
        } else {
            u32::from(first)
        };
        char::from_u32(code).ok_or_else(|| self.error("invalid unicode escape"))
    }

    fn number(&mut self) -> Result<Json, Error> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    return Err(self.error("a number cannot have a leading zero"));
                }
            }
            Some(b'1'..=b'9') => {
                while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.error("expected a digit")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !self.peek().is_some_and(|b| b.is_ascii_digit()) {
                return Err(self.error("expected a digit after `.`"));
            }
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !self.peek().is_some_and(|b| b.is_ascii_digit()) {
                return Err(self.error("expected a digit in the exponent"));
            }
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        Ok(Json::Number(self.text[start..self.pos].to_string()))
    }

    fn object(&mut self, depth: usize) -> Result<Json, Error> {
        self.pos += 1;
        let mut members = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(members));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a string key"));
            }
            let key = self.string()?;
            self.skip_whitespace();
            if self.peek() != Some(b':') {
                return Err(self.error("expected `:` after an object key"));
            }
            self.pos += 1;
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            members.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Object(members));
                }
                _ => return Err(self.error("expected `,` or `}`")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, Error> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.error("expected `,` or `]`")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(text: &str) -> Json {
        let value = parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        let written = value.to_text();
        let again = parse(&written).unwrap_or_else(|e| panic!("{written}: {e}"));
        assert_eq!(value, again);
        again
    }

    #[test]
    fn every_value_kind_round_trips() {
        let value = round_trip(r#"{"a":null,"b":true,"c":false,"d":-0,"e":"text"}"#);
        assert_eq!(value.get("a"), Some(&Json::Null));
        assert_eq!(value.get("b"), Some(&Json::Bool(true)));
        assert_eq!(value.get("c"), Some(&Json::Bool(false)));
        assert_eq!(value.get("d").and_then(Json::as_i64), Some(0));
        assert_eq!(value.get("e").and_then(Json::as_str), Some("text"));
        round_trip("[1,2.5,[],{},[null]]");
    }

    #[test]
    fn objects_keep_their_order_and_get_returns_the_last_member() {
        let value = round_trip(r#"{"z":1,"a":2,"z":3}"#);
        let Json::Object(members) = &value else {
            panic!("object");
        };
        let keys: Vec<&str> = members.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["z", "a", "z"]);
        assert_eq!(value.get("z").and_then(Json::as_i64), Some(3));
        assert_eq!(value.to_text(), r#"{"z":1,"a":2,"z":3}"#);
    }

    #[test]
    fn strings_escape_and_unescape() {
        let value =
            parse(r#""quote \" backslash \\ slash \/ tab \t newline \n form \f bol \b cr \r""#)
                .unwrap();
        assert_eq!(
            value.as_str(),
            Some("quote \" backslash \\ slash / tab \t newline \n form \u{c} bol \u{8} cr \r")
        );
        let written = Json::String("a\"b\\c\n\u{1}".to_string()).to_text();
        assert_eq!(written, r#""a\"b\\c\n\u0001""#);
    }

    #[test]
    fn unicode_escapes_and_surrogate_pairs() {
        let value = parse(r#""\u00e9\u20ac\ud83d\ude00""#).unwrap();
        assert_eq!(value.as_str(), Some("\u{e9}\u{20ac}\u{1f600}"));
        // Raw UTF-8 is kept as it is.
        let value = parse("\"héllo\"").unwrap();
        assert_eq!(value.as_str(), Some("héllo"));
        assert_eq!(value.to_text(), "\"héllo\"");
    }

    #[test]
    fn a_lone_surrogate_is_an_error() {
        for text in [
            r#""\ud83d""#,
            r#""\ud83dx""#,
            r#""\ud83d\ud83d""#,
            r#""\ude00""#,
        ] {
            let error = parse(text).unwrap_err();
            assert!(error.message.contains("surrogate"), "{text}: {error}");
        }
    }

    #[test]
    fn numbers_keep_their_source_text() {
        for text in ["-0", "1e5", "1.5E-3", "0.0", "-12.25e+7"] {
            let value = parse(text).unwrap();
            assert_eq!(value, Json::Number(text.to_string()));
            assert_eq!(value.to_text(), text);
        }
        assert_eq!(parse("-0").unwrap().as_i64(), Some(0));
        assert_eq!(parse("1e5").unwrap().as_f64(), Some(100_000.0));
        assert_eq!(parse("1.5E-3").unwrap().as_f64(), Some(0.0015));
    }

    #[test]
    fn malformed_numbers_are_errors() {
        for text in ["01", "1.", ".5", "+1", "-", "1e", "1e+", "00", "1..0"] {
            assert!(parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn trailing_garbage_is_an_error() {
        for text in ["1 2", "nullx", "{} []", "\"a\"\"b\"", ""] {
            assert!(parse(text).is_err(), "{text}");
        }
        assert_eq!(
            parse("  \n\t {\"a\": 1} \r\n ")
                .unwrap()
                .get("a")
                .and_then(Json::as_i64),
            Some(1)
        );
    }

    #[test]
    fn object_and_array_syntax_is_strict() {
        for text in [
            "{",
            "{\"a\"}",
            "{\"a\":}",
            "{\"a\":1,}",
            "{\"a\" 1}",
            "{\"a\":1 \"b\":2}",
            "[1,]",
            "[1 2]",
            "[",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
        assert!(parse("{}").is_ok());
        assert!(parse("[]").is_ok());
    }

    #[test]
    fn control_characters_are_rejected_in_strings() {
        assert!(parse("\"a\nb\"").is_err());
        assert!(parse("\"a\tb\"").is_err());
    }

    #[test]
    fn the_depth_limit_is_enforced() {
        let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
        let error = parse(&deep).unwrap_err();
        assert!(error.message.contains("deep"), "{error}");
        let shallow = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(parse(&shallow).is_ok());
    }

    #[test]
    fn errors_carry_a_byte_offset() {
        let error = parse("{\"a\": nope}").unwrap_err();
        assert_eq!(error.offset, 6);
        assert!(error.to_string().contains("byte 6"), "{error}");
    }
}
