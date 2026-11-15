//! LSP base-protocol framing: `Content-Length` headers and UTF-8 JSON bodies, on a
//! reader thread and a single writer.

use ingot_diag::json::Json;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::mpsc::Sender;

/// What the reader thread delivers to the main thread.
pub enum Incoming {
    Message(String),
    /// A frame that could not be read. The reader skips a bad body and continues; a
    /// bad header (where the body length is unknown) ends the stream.
    Malformed(String),
}

pub enum Frame {
    Message(String),
    Malformed { message: String, fatal: bool },
}

/// Read frames until end of input, sending each to `sender`.
pub fn read_loop<R: Read>(input: R, sender: Sender<Incoming>) {
    let mut reader = BufReader::new(input);
    loop {
        match read_frame(&mut reader) {
            None => return,
            Some(Frame::Message(message)) => {
                if sender.send(Incoming::Message(message)).is_err() {
                    return;
                }
            }
            Some(Frame::Malformed { message, fatal }) => {
                if sender.send(Incoming::Malformed(message)).is_err() || fatal {
                    return;
                }
            }
        }
    }
}

/// Read one frame, or `None` at end of input.
pub fn read_frame<R: BufRead>(reader: &mut R) -> Option<Frame> {
    let mut length = None;
    let mut first = true;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                return if first {
                    None
                } else {
                    Some(Frame::Malformed {
                        message: "end of input in message headers".to_string(),
                        fatal: true,
                    })
                };
            }
            Ok(_) => {}
            Err(error) => {
                return Some(Frame::Malformed {
                    message: format!("could not read a message header: {error}"),
                    fatal: true,
                });
            }
        }
        first = false;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Some(Frame::Malformed {
                message: format!("malformed message header `{line}`"),
                fatal: true,
            });
        };
        if name.eq_ignore_ascii_case("content-length") {
            match value.trim().parse::<usize>() {
                Ok(bytes) => length = Some(bytes),
                Err(_) => {
                    return Some(Frame::Malformed {
                        message: format!("bad Content-Length `{}`", value.trim()),
                        fatal: true,
                    });
                }
            }
        }
    }
    let Some(length) = length else {
        return Some(Frame::Malformed {
            message: "a message has no Content-Length".to_string(),
            fatal: true,
        });
    };
    let mut body = vec![0u8; length];
    if let Err(error) = reader.read_exact(&mut body) {
        return Some(Frame::Malformed {
            message: format!("could not read a {length}-byte message body: {error}"),
            fatal: true,
        });
    }
    match String::from_utf8(body) {
        Ok(text) => Some(Frame::Message(text)),
        Err(_) => Some(Frame::Malformed {
            message: "a message body is not UTF-8".to_string(),
            fatal: false,
        }),
    }
}

/// The one writer: every message is framed and flushed at once.
pub struct MessageWriter<W: Write> {
    out: W,
}

impl<W: Write> MessageWriter<W> {
    pub fn new(out: W) -> Self {
        MessageWriter { out }
    }

    pub fn send(&mut self, message: &Json) -> io::Result<()> {
        let body = message.to_text();
        write!(self.out, "Content-Length: {}\r\n\r\n", body.len())?;
        self.out.write_all(body.as_bytes())?;
        self.out.flush()
    }

    pub fn send_all(&mut self, messages: &[Json]) -> io::Result<()> {
        for message in messages {
            self.send(message)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn framed(body: &[u8]) -> Vec<u8> {
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn a_frame_is_read_back() {
        let mut reader = Cursor::new(framed(br#"{"jsonrpc":"2.0"}"#));
        match read_frame(&mut reader) {
            Some(Frame::Message(message)) => assert_eq!(message, r#"{"jsonrpc":"2.0"}"#),
            _ => panic!("expected a message"),
        }
        assert!(read_frame(&mut reader).is_none());
    }

    #[test]
    fn headers_are_case_insensitive_and_extra_headers_are_ignored() {
        let mut data = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 2\r\n\r\n{}".to_vec();
        data.extend_from_slice(&framed(b"[]"));
        let mut reader = Cursor::new(data);
        assert!(matches!(read_frame(&mut reader), Some(Frame::Message(m)) if m == "{}"));
        assert!(matches!(read_frame(&mut reader), Some(Frame::Message(m)) if m == "[]"));
        assert!(read_frame(&mut reader).is_none());
    }

    #[test]
    fn a_non_utf8_body_is_skipped_and_the_next_frame_still_reads() {
        let mut data = framed(&[0xff, 0xfe]);
        data.extend_from_slice(&framed(b"{}"));
        let mut reader = Cursor::new(data);
        match read_frame(&mut reader) {
            Some(Frame::Malformed { fatal, .. }) => assert!(!fatal),
            _ => panic!("expected a malformed frame"),
        }
        assert!(matches!(read_frame(&mut reader), Some(Frame::Message(m)) if m == "{}"));
    }

    #[test]
    fn a_missing_content_length_is_fatal() {
        let mut reader = Cursor::new(b"Content-Type: text/plain\r\n\r\n".to_vec());
        match read_frame(&mut reader) {
            Some(Frame::Malformed { fatal, .. }) => assert!(fatal),
            _ => panic!("expected a malformed frame"),
        }
    }

    #[test]
    fn the_writer_frames_and_flushes() {
        let mut writer = MessageWriter::new(Vec::new());
        writer
            .send(&Json::Object(vec![(
                "jsonrpc".to_string(),
                Json::String("2.0".to_string()),
            )]))
            .unwrap();
        assert_eq!(
            writer.out,
            b"Content-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}".to_vec()
        );
    }
}
