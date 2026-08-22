//! `std.http.serve` / `serve_tls`：阻塞 HTTP/1.1（无 HTTP/2 / chunked 上传）。

use std::io::{Read, Write};
use std::net::TcpListener;

use crate::error::RuntimeError;
use crate::value::{DictMap, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use super::net::{accept_tls, load_server_config};
use super::{expect_int, expect_text};

const MAX_BODY: usize = 1024 * 1024;
const MAX_HEADER: usize = 64 * 1024;

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
    let listener =
        TcpListener::bind(&bind).map_err(|e| RuntimeError::io_err(format!("serve {bind}: {e}")))?;
    loop {
        let (mut stream, _) = listener
            .accept()
            .map_err(|e| RuntimeError::io_err(format!("serve accept: {e}")))?;
        let _ = stream.set_nodelay(true);
        if let Err(_conn) = handle_one(vm, handler.clone(), &mut stream) {
            // 单连接失败不拆掉整个服务。
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
    vm.caps.check_fs("serve_tls", &cert)?;
    vm.caps.check_fs("serve_tls", &key)?;
    let config = load_server_config(&cert, &key)?;
    let bind = format!("{host}:{port}");
    let listener = TcpListener::bind(&bind)
        .map_err(|e| RuntimeError::io_err(format!("serve_tls {bind}: {e}")))?;
    loop {
        let (tcp, _) = listener
            .accept()
            .map_err(|e| RuntimeError::io_err(format!("serve_tls accept: {e}")))?;
        match accept_tls(tcp, &config) {
            Ok(mut stream) => {
                if let Err(_conn) = handle_one(vm, handler.clone(), &mut stream) {
                    // 单连接失败不拆掉整个服务。
                }
                stream.shutdown_tls();
            }
            Err(_) => {}
        }
        vm.request_cooperative_yield();
    }
}

fn handle_one<S: Read + Write>(vm: &mut Vm, handler: Value, stream: &mut S) -> Result<()> {
    let req = read_request(stream)?;
    let resp = invoke_handler(vm, handler, req).unwrap_or_else(|e| {
        dict_response(
            500,
            b"Internal Server Error",
            &[("content-type", "text/plain; charset=utf-8")],
            e.to_string().into_bytes(),
        )
    });
    write_response(stream, &resp)?;
    Ok(())
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

fn read_request<S: Read>(stream: &mut S) -> Result<Value> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream
            .read(&mut tmp)
            .map_err(|e| RuntimeError::io_err(format!("serve read: {e}")))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_HEADER + MAX_BODY {
            return Err(RuntimeError::io_err("serve: request too large"));
        }
        if let Some(pos) = find_header_end(&buf) {
            let header = &buf[..pos];
            let rest = buf[pos + 4..].to_vec();
            return parse_http(stream, header, rest);
        }
        if n < tmp.len() && find_header_end(&buf).is_none() && buf.len() > MAX_HEADER {
            return Err(RuntimeError::io_err("serve: headers too large"));
        }
    }
    Err(RuntimeError::io_err("serve: incomplete request"))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_http<S: Read>(stream: &mut S, header: &[u8], already: Vec<u8>) -> Result<Value> {
    let text = String::from_utf8_lossy(header);
    let mut lines = text.split("\r\n");
    let req_line = lines.next().unwrap_or("");
    let mut parts = req_line.splitn(3, ' ');
    let method = parts.next().unwrap_or("GET").to_string();
    let raw_path = parts.next().unwrap_or("/").to_string();
    let (path, query) = match raw_path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (raw_path, String::new()),
    };
    let mut headers = DictMap::new();
    let mut content_len = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key == "content-length" {
                content_len = val.parse().unwrap_or(0);
            }
            headers.insert(ValueKey::Text(key), Value::Text(val));
        }
    }
    if content_len > MAX_BODY {
        return Err(RuntimeError::io_err("serve: body too large"));
    }
    let mut body = already;
    while body.len() < content_len {
        let mut tmp = vec![0u8; (content_len - body.len()).min(8192)];
        let n = stream
            .read(&mut tmp)
            .map_err(|e| RuntimeError::io_err(format!("serve read body: {e}")))?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
        if body.len() > MAX_BODY {
            return Err(RuntimeError::io_err("serve: body too large"));
        }
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
        .map_err(|e| RuntimeError::io_err(format!("serve write: {e}")))?;
    stream
        .write_all(&body)
        .map_err(|e| RuntimeError::io_err(format!("serve write: {e}")))?;
    let _ = stream.flush();
    Ok(())
}

fn dict_response(status: i64, _reason: &[u8], headers: &[(&str, &str)], body: Vec<u8>) -> Value {
    let mut h = DictMap::new();
    for (k, v) in headers {
        h.insert(ValueKey::Text((*k).into()), Value::Text((*v).into()));
    }
    let mut d = DictMap::new();
    d.insert(
        ValueKey::Text("status".into()),
        Value::Num(Num::Small(status)),
    );
    d.insert(
        ValueKey::Text("headers".into()),
        Value::Dict(crate::shared::Shared::new(h)),
    );
    d.insert(
        ValueKey::Text("body".into()),
        Value::Bytes(std::sync::Arc::new(body)),
    );
    Value::Dict(crate::shared::Shared::new(d))
}
