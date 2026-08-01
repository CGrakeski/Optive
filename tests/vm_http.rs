mod common;

use common::{assert_num, run_err, value};
use optive::value::Value;

/// `std.http` 模块应当可被导入，且导出常见动词。
#[test]
fn import_std_http_module() {
    let v = value(
        r#"
import std.http as http
http
"#,
    );
    match v {
        Value::Module(m) => assert_eq!(m.borrow().name, "http"),
        other => panic!("expected module, got {:?}", other),
    }
}

/// 各 HTTP 动词应当作为 builtin 导出。
#[test]
fn http_exports_are_builtins() {
    for name in ["get", "post", "put", "delete", "patch", "head", "request"] {
        let src = format!(
            r#"
import std.http as http
http.{name}
"#
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
        r#"
import std.http as http
http.get(42)
"#,
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

/// 真实网络请求：对 example.com 做 GET，断言 200 与 body 含 HTML。
/// 默认忽略以保持 CI 离线稳定；手动运行：cargo test -- --ignored http_real
#[test]
#[ignore]
fn http_real_get_example_com() {
    assert_num(
        r#"
import std.http as http
let r = http.get("https://example.com")
r.status
"#,
        "200",
    );
    let body = value(
        r#"
import std.http as http
let r = http.get("https://example.com")
r.body
"#,
    );
    match body {
        Value::Text(b) => assert!(b.contains("Example Domain"), "unexpected body: {b}"),
        other => panic!("expected text body, got {other:?}"),
    }
}
