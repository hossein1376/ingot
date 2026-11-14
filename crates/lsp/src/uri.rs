//! `file://` URIs: percent-encoding and decoding. Unit-tested here because a wrong
//! byte breaks every span the adapter publishes.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The `file://` URI of an absolute path. Bytes outside the unreserved set and `/`
/// are percent-encoded in upper case, so spaces, `#`, `?` and non-ASCII are safe.
pub fn to_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The path of a `file://` URI. Other schemes, a remote authority, a relative path or
/// a malformed escape give `None`.
pub fn from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let (authority, path) = rest.split_at(slash);
    if !authority.is_empty() && authority != "localhost" {
        return None;
    }
    // A file URI has no query or fragment; a raw `?`/`#` is not part of the path.
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let text = String::from_utf8(percent_decode(path)?).ok()?;
    if !text.starts_with('/') {
        return None;
    }
    Some(PathBuf::from(text))
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let high = *bytes.get(i + 1)?;
            let low = *bytes.get(i + 2)?;
            out.push(hex(high)? * 16 + hex(low)?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_escapes_spaces_hashes_and_non_ascii() {
        let uri = to_uri(Path::new("/tmp/a b#c?d/é日😀.ingot"));
        assert_eq!(
            uri,
            "file:///tmp/a%20b%23c%3Fd/%C3%A9%E6%97%A5%F0%9F%98%80.ingot"
        );
        assert_eq!(
            from_uri(&uri).unwrap(),
            Path::new("/tmp/a b#c?d/é日😀.ingot")
        );
    }

    #[test]
    fn plain_paths_round_trip() {
        for path in ["/", "/tmp/x.ingot", "/Users/a/.config/ingot/std"] {
            let uri = to_uri(Path::new(path));
            assert_eq!(from_uri(&uri).unwrap(), Path::new(path), "{path}");
        }
        assert_eq!(to_uri(Path::new("/tmp/x.ingot")), "file:///tmp/x.ingot");
    }

    #[test]
    fn localhost_is_the_only_authority() {
        assert_eq!(
            from_uri("file://localhost/tmp/x").unwrap(),
            Path::new("/tmp/x")
        );
        assert!(from_uri("file://example.com/tmp/x").is_none());
    }

    #[test]
    fn other_schemes_and_malformed_uris_are_rejected() {
        for uri in [
            "http://example.com/x",
            "untitled:Untitled-1",
            "file:///tmp/%zz",
            "file:///tmp/%2",
            "file://relative",
        ] {
            assert!(from_uri(uri).is_none(), "{uri}");
        }
        // Unescaped UTF-8 in a path is accepted; some clients send it.
        assert_eq!(from_uri("file:///tmp/é").unwrap(), Path::new("/tmp/é"));
    }

    #[test]
    fn percent_escapes_are_case_insensitive() {
        assert_eq!(from_uri("file:///a%2fb").unwrap(), Path::new("/a/b"));
        assert_eq!(from_uri("file:///a%2Fb").unwrap(), Path::new("/a/b"));
    }
}
