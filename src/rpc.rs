//! LSP / DAP 共用的 `Content-Length` JSON 帧编解码。

use std::fmt;
use std::io::{self, BufRead, Read, Write};

use serde_json::Value as Json;

pub const MAX_CONTENT_LENGTH: usize = 16 * 1024 * 1024;
const MAX_HEADER_LINE: usize = 8 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    Protocol(String),
    Json(serde_json::Error),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "RPC I/O error: {e}"),
            Self::Protocol(e) => write!(f, "RPC protocol error: {e}"),
            Self::Json(e) => write!(f, "RPC JSON error: {e}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<io::Error> for ReadError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// 干净 EOF 返回 `Ok(None)`；截断、坏 header 与坏 JSON 分别返回明确错误。
pub fn read_json(reader: &mut impl BufRead) -> Result<Option<Json>, ReadError> {
    let mut content_length = None;
    let mut saw_header = false;
    let mut header_bytes = 0usize;
    loop {
        let mut buf = Vec::new();
        let n = reader
            .by_ref()
            .take(MAX_HEADER_LINE as u64 + 1)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            return if saw_header {
                Err(ReadError::Protocol("unexpected EOF in headers".into()))
            } else {
                Ok(None)
            };
        }
        header_bytes += n;
        if header_bytes > MAX_HEADER_BYTES {
            return Err(ReadError::Protocol("RPC headers exceed size limit".into()));
        }
        if n > MAX_HEADER_LINE || (n == MAX_HEADER_LINE + 1 && !buf.ends_with(b"\n")) {
            return Err(ReadError::Protocol(
                "RPC header line exceeds size limit".into(),
            ));
        }
        if !buf.ends_with(b"\n") {
            return Err(ReadError::Protocol("unexpected EOF in headers".into()));
        }
        saw_header = true;
        let line = String::from_utf8_lossy(&buf);
        if line == "\r\n" || line == "\n" {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        let Some((name, value)) = line.split_once(':') else {
            return Err(ReadError::Protocol(format!("malformed header `{line}`")));
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(ReadError::Protocol(
                    "duplicate Content-Length header".into(),
                ));
            }
            let len = value
                .trim()
                .parse::<usize>()
                .map_err(|_| ReadError::Protocol("invalid Content-Length".into()))?;
            if len > MAX_CONTENT_LENGTH {
                return Err(ReadError::Protocol(format!(
                    "Content-Length {len} exceeds {MAX_CONTENT_LENGTH}"
                )));
            }
            content_length = Some(len);
        }
    }
    let len = content_length
        .ok_or_else(|| ReadError::Protocol("missing Content-Length header".into()))?;
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(ReadError::Json)
}

pub fn write_json(out: &mut impl Write, value: &Json) -> io::Result<()> {
    let body = serde_json::to_vec(value)?;
    if body.len() > MAX_CONTENT_LENGTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "JSON RPC message exceeds 16 MiB",
        ));
    }
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn accepts_case_insensitive_header() {
        let mut input = Cursor::new(b"content-length: 7\r\nX-Test: yes\r\n\r\n{\"x\":1}");
        assert_eq!(read_json(&mut input).unwrap().unwrap()["x"], 1);
    }

    #[test]
    fn distinguishes_clean_eof_protocol_and_json_errors() {
        assert!(read_json(&mut Cursor::new(Vec::<u8>::new()))
            .unwrap()
            .is_none());
        assert!(matches!(
            read_json(&mut Cursor::new(b"X: y\r\n\r\n".to_vec())),
            Err(ReadError::Protocol(_))
        ));
        assert!(matches!(
            read_json(&mut Cursor::new(b"Content-Length: 1\r\n\r\n{".to_vec())),
            Err(ReadError::Json(_))
        ));
    }

    #[test]
    fn rejects_oversized_and_truncated_frames() {
        let too_large = format!("Content-Length: {}\r\n\r\n", MAX_CONTENT_LENGTH + 1);
        assert!(matches!(
            read_json(&mut Cursor::new(too_large)),
            Err(ReadError::Protocol(_))
        ));
        assert!(matches!(
            read_json(&mut Cursor::new(
                b"Content-Length: 4\r\n\r\n{}".to_vec()
            )),
            Err(ReadError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn write_round_trips() {
        let value = serde_json::json!({"hello":"world"});
        let mut framed = Vec::new();
        write_json(&mut framed, &value).unwrap();
        assert_eq!(read_json(&mut Cursor::new(framed)).unwrap(), Some(value));
    }
}
