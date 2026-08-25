//! `std.http.serve` / `serve_tls`：阻塞 HTTP/1.1（无 HTTP/2 / chunked 上传）。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use crate::error::RuntimeError;
use crate::value::{DictMap, Value, ValueKey};
use crate::vm::{OutputStream, Vm};
use crate::Result;

use super::net::{accept_tls, load_server_config};
use super::{expect_int, expect_text, io_map};

const MAX_BODY: usize = 1024 * 1024;
const MAX_HEADER: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
enum RequestError {
    BadRequest(&'static str),
    PayloadTooLarge(&'static str),
}

impl RequestError {
    fn response(&self) -> (u16, &'static str, &'static [u8]) {
        match self {
            Self::BadRequest(_) => (400, "Bad Request", b"Bad Request"),
            Self::PayloadTooLarge(_) => (413, "Payload Too Large", b"Payload Too Large"),
        }
    }

    fn detail(&self) -> &'static str {
        match self {
            Self::BadRequest(detail) | Self::PayloadTooLarge(detail) => detail,
        }
    }
}

pub(super) fn http_serve(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("serve")?;
    let (host, port, handler) = match args.len() {
        2 => (
            "127.0.0.1".to_string(),
            expect_int("serve", args, 0)?,
            args[1].clone(),
        ),
        3 => (
            expect_text("serve", args, 0)?,
            expect_int("serve", args, 1)?,
            args[2].clone(),
        ),
        _ => {
            return Err(RuntimeError::type_err(
                "serve requires (port, handler) or (host, port, handler)",
            ));
        }
    };
    if !(0..=65535).contains(&port) {
        return Err(RuntimeError::value_err(format!(
            "serve: port {port} out of range"
        )));
    }
    match &handler {
        Value::Function(_) | Value::Builtin(_) => {}
        _ => {
            return Err(RuntimeError::type_err("serve: handler must be a function"));
        }
    }
    let bind = format!("{host}:{port}");
    let listener = TcpListener::bind(&bind).map_err(|e| io_map(&format!("serve {bind}"), e))?;
    loop {
        let (mut stream, _) = match listener.accept() {
            Ok(conn) => conn,
            Err(e) => {
                diagnose(vm, "accept", &io_map("serve accept", e));
                vm.request_cooperative_yield();
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
        if let Err(conn) = handle_one(vm, handler.clone(), &mut stream) {
            diagnose(vm, "connection", &conn);
        }
        vm.request_cooperative_yield();
    }
}

pub(super) fn http_serve_tls(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("serve_tls")?;
    let (host, port, handler, cert, key) = match args.len() {
        4 => (
            "127.0.0.1".to_string(),
            expect_int("serve_tls", args, 0)?,
            args[1].clone(),
            expect_text("serve_tls", args, 2)?,
            expect_text("serve_tls", args, 3)?,
        ),
        5 => (
            expect_text("serve_tls", args, 0)?,
            expect_int("serve_tls", args, 1)?,
            args[2].clone(),
            expect_text("serve_tls", args, 3)?,
            expect_text("serve_tls", args, 4)?,
        ),
        _ => {
            return Err(RuntimeError::type_err(
                "serve_tls requires (port, handler, cert, key) or (host, port, handler, cert, key)",
            ));
        }
    };
    if !(0..=65535).contains(&port) {
        return Err(RuntimeError::value_err(format!(
            "serve_tls: port {port} out of range"
        )));
    }
    match &handler {
        Value::Function(_) | Value::Builtin(_) => {}
        _ => {
            return Err(RuntimeError::type_err(
                "serve_tls: handler must be a function",
            ));
        }
    }
    let cert = vm.caps.open_read("serve_tls certificate", &cert)?;
    let key = vm.caps.open_read("serve_tls private key", &key)?;
    let config = load_server_config(cert, key)?;
    let bind = format!("{host}:{port}");
    let listener = TcpListener::bind(&bind).map_err(|e| io_map(&format!("serve_tls {bind}"), e))?;
    loop {
        let (tcp, _) = match listener.accept() {
            Ok(conn) => conn,
            Err(e) => {
                diagnose(vm, "accept", &io_map("serve_tls accept", e));
                vm.request_cooperative_yield();
                continue;
            }
        };
        let _ = tcp.set_read_timeout(Some(READ_TIMEOUT));
        match accept_tls(tcp, &config) {
            Ok(mut stream) => {
                if let Err(conn) = handle_one(vm, handler.clone(), &mut stream) {
                    diagnose(vm, "TLS connection", &conn);
                }
                stream.shutdown_tls();
            }
            Err(err) => diagnose(vm, "TLS handshake", &err),
        }
        vm.request_cooperative_yield();
    }
}

fn handle_one<S: Read + Write>(vm: &mut Vm, handler: Value, stream: &mut S) -> Result<()> {
    let req = match read_request(stream) {
        Ok(req) => req,
        Err(err) => {
            diagnose_text(vm, "request", err.detail());
            let (status, reason, body) = err.response();
            return write_simple_response(stream, status, reason, body);
        }
    };
    let resp = match invoke_handler(vm, handler, req) {
        Ok(resp) => resp,
        Err(err) => {
            diagnose(vm, "handler", &err);
            return write_simple_response(
                stream,
                500,
                "Internal Server Error",
                b"Internal Server Error",
            );
        }
    };
    if let Err(err) = write_response(stream, &resp) {
        diagnose(vm, "response", &err);
        write_simple_response(
            stream,
            500,
            "Internal Server Error",
            b"Internal Server Error",
        )?;
    }
    Ok(())
}

fn diagnose(vm: &Vm, stage: &str, err: &RuntimeError) {
    diagnose_text(vm, stage, &err.to_string());
}

fn diagnose_text(vm: &Vm, stage: &str, detail: &str) {
    vm.write_output(
        OutputStream::Stderr,
        &format!("std.http.serve {stage} error: {detail}\n"),
    );
}

fn invoke_handler(vm: &mut Vm, handler: Value, req: Value) -> Result<Value> {
    match handler {
        Value::Function(f) => vm.call_user_function(f, vec![req]),
        Value::Builtin(b) => b.call(vm, &[req]),
        other => Err(RuntimeError::type_err(format!(
            "serve: handler must be a function, got {}",
            other.type_name()
        ))),
    }
}

fn read_request<S: Read>(stream: &mut S) -> std::result::Result<Value, RequestError> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream
            .read(&mut tmp)
            .map_err(|_| RequestError::BadRequest("failed or timed out while reading headers"))?;
        if n == 0 {
            return Err(RequestError::BadRequest("incomplete request headers"));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            if pos > MAX_HEADER {
                return Err(RequestError::PayloadTooLarge("request headers too large"));
            }
            let header = &buf[..pos];
            let rest = buf[pos + 4..].to_vec();
            return parse_http(stream, header, rest);
        }
        if buf.len() > MAX_HEADER {
            return Err(RequestError::PayloadTooLarge("request headers too large"));
        }
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_http<S: Read>(
    stream: &mut S,
    header: &[u8],
    already: Vec<u8>,
) -> std::result::Result<Value, RequestError> {
    let text = std::str::from_utf8(header)
        .map_err(|_| RequestError::BadRequest("headers are not valid UTF-8"))?;
    let mut lines = text.split("\r\n");
    let req_line = lines
        .next()
        .ok_or(RequestError::BadRequest("missing request line"))?;
    let mut parts = req_line.split(' ');
    let method = parts
        .next()
        .filter(|part| is_token(part))
        .ok_or(RequestError::BadRequest("invalid request method"))?
        .to_string();
    let raw_path = parts
        .next()
        .filter(|part| !part.is_empty() && !part.bytes().any(|b| b.is_ascii_control()))
        .ok_or(RequestError::BadRequest("invalid request target"))?
        .to_string();
    if parts.next() != Some("HTTP/1.1") || parts.next().is_some() {
        return Err(RequestError::BadRequest("request line must use HTTP/1.1"));
    }
    let (path, query) = match raw_path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (raw_path, String::new()),
    };
    let mut headers = DictMap::new();
    let mut content_len = None;
    for line in lines {
        if line.is_empty() || line.starts_with(' ') || line.starts_with('\t') {
            return Err(RequestError::BadRequest("invalid folded or empty header"));
        }
        let (k, v) = line
            .split_once(':')
            .ok_or(RequestError::BadRequest("header is missing ':'"))?;
        if !is_token(k) {
            return Err(RequestError::BadRequest("invalid header name"));
        }
        let key = k.to_ascii_lowercase();
        let val = v.trim().to_string();
        if val
            .bytes()
            .any(|b| b == 0 || (b.is_ascii_control() && b != b'\t'))
        {
            return Err(RequestError::BadRequest("invalid header value"));
        }
        if key == "transfer-encoding" {
            return Err(RequestError::BadRequest(
                "transfer-encoding is not supported",
            ));
        }
        if key == "content-length" {
            if content_len.is_some() {
                return Err(RequestError::BadRequest(
                    "content-length must occur exactly once",
                ));
            }
            if val.is_empty() || !val.bytes().all(|b| b.is_ascii_digit()) {
                return Err(RequestError::BadRequest("invalid content-length"));
            }
            content_len = Some(
                val.parse()
                    .map_err(|_| RequestError::BadRequest("invalid content-length"))?,
            );
        }
        headers.insert(ValueKey::Text(key), Value::Text(val));
    }
    let content_len = content_len.unwrap_or(0);
    if content_len > MAX_BODY {
        return Err(RequestError::PayloadTooLarge("request body too large"));
    }
    let mut body = already;
    while body.len() < content_len {
        let mut tmp = vec![0u8; (content_len - body.len()).min(8192)];
        let n = stream
            .read(&mut tmp)
            .map_err(|_| RequestError::BadRequest("failed or timed out while reading body"))?;
        if n == 0 {
            return Err(RequestError::BadRequest(
                "request body shorter than content-length",
            ));
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_len);
    let mut req = DictMap::new();
    req.insert(ValueKey::Text("method".into()), Value::Text(method));
    req.insert(ValueKey::Text("path".into()), Value::Text(path));
    req.insert(ValueKey::Text("query".into()), Value::Text(query));
    req.insert(
        ValueKey::Text("headers".into()),
        Value::Dict(crate::shared::Shared::new(headers)),
    );
    req.insert(
        ValueKey::Text("body".into()),
        Value::Text(String::from_utf8_lossy(&body).into_owned()),
    );
    Ok(Value::Dict(crate::shared::Shared::new(req)))
}

fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn write_response<S: Write>(stream: &mut S, resp: &Value) -> Result<()> {
    let (status, headers, body) = match resp {
        Value::Dict(d) => {
            let g = d.borrow();
            let status = match g.get(&ValueKey::Text("status".into())) {
                Some(Value::Num(n)) => n.to_i64().unwrap_or(200).clamp(100, 599) as u16,
                _ => 200,
            };
            let mut extra: Vec<(String, String)> = Vec::new();
            if let Some(Value::Dict(hd)) = g.get(&ValueKey::Text("headers".into())) {
                for (k, v) in hd.borrow().iter() {
                    let ks = match k {
                        ValueKey::Text(s) => s.clone(),
                        _ => continue,
                    };
                    let vs = match v {
                        Value::Text(s) => s.clone(),
                        Value::Num(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => continue,
                    };
                    extra.push((ks, vs));
                }
            }
            let body = match g.get(&ValueKey::Text("body".into())) {
                Some(Value::Text(s)) => s.as_bytes().to_vec(),
                Some(Value::Bytes(b)) => b.as_ref().clone(),
                Some(Value::None) | None => Vec::new(),
                Some(other) => other.display_string().into_bytes(),
            };
            (status, extra, body)
        }
        other => (
            200,
            vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            other.display_string().into_bytes(),
        ),
    };
    let reason = if status == 200 { "OK" } else { "Status" };
    let mut out = format!(
        "HTTP/1.1 {status} {reason}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    let mut has_ct = false;
    for (k, v) in &headers {
        if !is_token(k) || v.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
            return Err(RuntimeError::value_err(
                "serve: response header contains an invalid name or value",
            ));
        }
        if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("connection") {
            continue;
        }
        if k.eq_ignore_ascii_case("content-type") {
            has_ct = true;
        }
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_ct {
        out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    }
    out.push_str("\r\n");
    stream
        .write_all(out.as_bytes())
        .map_err(|e| io_map("serve write", e))?;
    stream
        .write_all(&body)
        .map_err(|e| io_map("serve write", e))?;
    let _ = stream.flush();
    Ok(())
}

fn write_simple_response<S: Write>(
    stream: &mut S,
    status: u16,
    reason: &str,
    body: &[u8],
) -> Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .and_then(|()| stream.write_all(body))
        .and_then(|()| stream.flush())
        .map_err(|e| io_map("serve write", e))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::vm::OutputSink;

    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Duplex {
        fn new(input: impl Into<Vec<u8>>) -> Self {
            Self {
                input: Cursor::new(input.into()),
                output: Vec::new(),
            }
        }
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn malformed_requests_are_rejected_explicitly() {
        let cases: &[(&[u8], u16)] = &[
            (
                b"POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx",
                400,
            ),
            (b"POST / HTTP/1.1\r\nContent-Length: nope\r\n\r\n", 400),
            (b"POST / HTTP/1.1\r\nContent-Length: 3\r\n\r\nx", 400),
            (
                b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
                400,
            ),
            (b"POST / HTTP/1.1\r\nContent-Length: 1048577\r\n\r\n", 413),
        ];
        for (request, expected) in cases {
            let err = read_request(&mut Cursor::new(request.to_vec())).expect_err("malformed");
            assert_eq!(err.response().0, *expected, "{request:?}: {err:?}");
        }

        let mut oversized = b"GET / HTTP/1.1\r\nX-Large: ".to_vec();
        oversized.resize(oversized.len() + MAX_HEADER, b'a');
        let err = read_request(&mut Cursor::new(oversized)).expect_err("oversized headers");
        assert_eq!(err.response().0, 413);
    }

    #[test]
    fn response_headers_reject_line_injection_and_nul() {
        for (name, value) in [
            ("X-Test\r\nInjected", "safe"),
            ("X-Test", "bad\nInjected"),
            ("X-Test", "bad\0value"),
        ] {
            let mut headers = DictMap::new();
            headers.insert(ValueKey::Text(name.into()), Value::Text(value.into()));
            let mut response = DictMap::new();
            response.insert(
                ValueKey::Text("headers".into()),
                Value::Dict(crate::shared::Shared::new(headers)),
            );
            let mut output = Vec::new();
            let err = write_response(
                &mut output,
                &Value::Dict(crate::shared::Shared::new(response)),
            )
            .expect_err("invalid response header");
            assert!(err.to_string().contains("invalid"));
            assert!(output.is_empty());
        }
    }

    #[test]
    fn protocol_errors_write_400_or_413() {
        let handler = Value::builtin("unused", |_vm, _args| Ok(Value::None));
        for (request, status) in [
            (
                b"POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx".as_slice(),
                400,
            ),
            (
                b"POST / HTTP/1.1\r\nContent-Length: 1048577\r\n\r\n".as_slice(),
                413,
            ),
        ] {
            let mut vm = Vm::new();
            vm.set_output_sink(OutputSink::new(|_, _| {}));
            let mut stream = Duplex::new(request.to_vec());
            handle_one(&mut vm, handler.clone(), &mut stream).expect("error response");
            let response = String::from_utf8(stream.output).unwrap();
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status} ")),
                "{response}"
            );
        }
    }

    #[test]
    fn handler_failure_is_private_but_diagnosed() {
        let captured = Arc::new(Mutex::new(String::new()));
        let captured_h = captured.clone();
        let mut vm = Vm::new();
        vm.set_output_sink(OutputSink::new(move |stream, text| {
            if stream == OutputStream::Stderr {
                captured_h.lock().unwrap().push_str(text);
            }
        }));
        let handler = Value::builtin("broken", |_vm, _args| {
            Err(RuntimeError::msg("private traceback detail"))
        });
        let mut stream = Duplex::new(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec());

        handle_one(&mut vm, handler, &mut stream).expect("500 response");

        let response = String::from_utf8(stream.output).unwrap();
        assert!(response.starts_with("HTTP/1.1 500 Internal Server Error\r\n"));
        assert!(!response.contains("private traceback detail"));
        assert!(captured
            .lock()
            .unwrap()
            .contains("private traceback detail"));
    }
}
