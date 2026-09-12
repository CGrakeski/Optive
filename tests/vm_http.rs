#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_num, run_err, value};
use optive::value::Value;
use optive::value::ValueKey;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

/// `std.http` 模块应当可被导入，且导出常见动词。
#[test]
fn import_std_http_module() {
    let v = value(
        r"
import std.http as http
http
",
    );
    match v {
        Value::Module(m) => assert_eq!(m.borrow().name, "http"),
        other => panic!("expected module, got {other:?}"),
    }
}

/// 各 HTTP 动词应当作为 builtin 导出。
#[test]
fn http_exports_are_builtins() {
    for name in ["get", "post", "put", "delete", "patch", "head", "request"] {
        let src = format!(
            r"
import std.http as http
http.{name}
"
        );
        match value(&src) {
            Value::Builtin(_) => {}
            other => panic!("http.{name} should be a builtin, got {other:?}"),
        }
    }
}

/// 非法 URL 应当返回运行时错误，而非 panic；此用例不依赖网络可达性。
#[test]
fn http_get_invalid_url_errors() {
    run_err(
        r#"
import std.http as http
http.get("ht!tp://%%%invalid-url")
"#,
    );
}

/// 参数类型错误应当报 type error。
#[test]
fn http_get_non_text_url_errors() {
    run_err(
        r"
import std.http as http
http.get(42)
",
    );
}

/// http.request 拒绝未知 method。
#[test]
fn http_request_unknown_method_errors() {
    run_err(
        r#"
import std.http as http
http.request("FROBNICATE", "https://example.com")
"#,
    );
}

#[test]
fn http_options_reject_invalid_limits_before_network_io() {
    run_err(
        r#"
import std.http as http
http.get("http://127.0.0.1:1", { "timeout": -1 })
"#,
    );
    run_err(
        r#"
import std.http as http
http.get("http://127.0.0.1:1", { "follow_redirects": 101 })
"#,
    );
}

#[test]
fn http_preserves_binary_body_and_duplicate_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).expect("read request");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nConnection: close\r\n\r\n\xff\0x",
            )
            .expect("write response");
    });
    let response = value(&format!(
        r#"
import std.http as http
http.get("http://{addr}")
"#
    ));
    server.join().expect("server thread");

    let Value::Dict(response) = response else {
        panic!("expected response dict");
    };
    let response = response.borrow();
    match response.get(&ValueKey::Text("bytes".into())) {
        Some(Value::Bytes(bytes)) => assert_eq!(bytes.as_slice(), &[0xff, 0, b'x']),
        other => panic!("expected raw response bytes, got {other:?}"),
    }
    let Some(Value::List(headers)) = response.get(&ValueKey::Text("raw_headers".into())) else {
        panic!("expected raw_headers list");
    };
    let cookie_count = headers
        .borrow()
        .iter()
        .filter(|entry| match entry {
            Value::List(pair) => {
                matches!(pair.borrow().first(), Some(Value::Text(name)) if name == "set-cookie")
            }
            _ => false,
        })
        .count();
    assert_eq!(cookie_count, 2);
}

/// 真实网络请求：对 example.com 做 GET，断言 200 与 body 含 HTML。
/// 默认忽略以保持 CI 离线稳定；手动运行：cargo test -- --ignored `http_real`
#[test]
#[ignore]
fn http_real_get_example_com() {
    assert_num(
        r#"
import std.http as http
let r = http.get("https://example.com")
r["status"]
"#,
        "200",
    );
    let body = value(
        r#"
import std.http as http
let r = http.get("https://example.com")
r["body"]
"#,
    );
    match body {
        Value::Text(b) => assert!(b.contains("Example Domain"), "unexpected body: {b}"),
        other => panic!("expected text body, got {other:?}"),
    }
}
