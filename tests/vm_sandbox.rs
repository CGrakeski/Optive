#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! 运行时能力隔离（沙箱）集成测试。

mod common;

use std::path::PathBuf;

use common::{assert_caps_err, run_with_caps};
use optive::caps::{Capabilities, FsPolicy};
use optive::value::Value;

fn cwd_root() -> Vec<PathBuf> {
    vec![std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))]
}

#[test]
fn full_caps_allow_fs_roundtrip() {
    let caps = Capabilities::full();
    let v = run_with_caps(
        r#"
use std.fs.{ write_text, read_text, remove }
let p = "__ol_test_sandbox_full.txt"
write_text(p, "ok")
let t = read_text(p)
remove(p)
t
"#,
        caps,
    )
    .expect("full caps should allow fs");
    match v {
        Value::Text(s) => assert_eq!(s.as_str(), "ok"),
        other => panic!("expected text, got {}", other.display_string()),
    }
}

#[test]
fn no_network_blocks_http() {
    let caps = Capabilities {
        network: false,
        fs: FsPolicy::Unrestricted,
        env: true,
        ffi: true,
    };
    // caps 检查在真正发请求之前触发，故无需真实网络。
    assert_caps_err(
        r#"
import std.http as http
http.get("https://example.com")
"#,
        caps,
        "network access disabled",
    );
}

#[test]
fn sandbox_blocks_fs_escape() {
    let caps = Capabilities::sandbox(cwd_root());
    // `..` 逃逸到沙箱根之外，应在触盘前被拦下。
    assert_caps_err(
        r#"
use std.fs.{ read_text }
read_text("../__ol_test_sandbox_escape.txt")
"#,
        caps,
        "outside sandbox",
    );
}

#[test]
fn sandbox_allows_under_root() {
    let caps = Capabilities::sandbox(cwd_root());
    let v = run_with_caps(
        r#"
use std.fs.{ write_text, read_text, remove }
let p = "__ol_test_sandbox_inside.txt"
write_text(p, "inside-ok")
let t = read_text(p)
remove(p)
t
"#,
        caps,
    )
    .expect("sandbox should allow under-root file");
    match v {
        Value::Text(s) => assert_eq!(s.as_str(), "inside-ok"),
        other => panic!("expected text, got {}", other.display_string()),
    }
}

#[test]
fn sandbox_blocks_env_mutation() {
    let caps = Capabilities::sandbox(cwd_root());
    assert_caps_err(
        r#"
use std.os.{ setenv }
setenv("OPTIVE_SANDBOX_TEST", "1")
"#,
        caps,
        "environment mutation disabled",
    );
}

#[test]
fn sandbox_blocks_chdir() {
    let caps = Capabilities::sandbox(cwd_root());
    assert_caps_err(
        r#"
use std.os.{ chdir }
chdir("..")
"#,
        caps,
        "environment mutation disabled",
    );
}

#[test]
fn empty_roots_blocks_all_fs() {
    let caps = Capabilities::sandbox(vec![]);
    assert_caps_err(
        r#"
use std.fs.{ exists }
exists("anything.txt")
"#,
        caps,
        "filesystem access disabled",
    );
}

#[test]
fn allow_path_alone_keeps_network() {
    // --allow-path 不应顺带禁网。
    let caps = Capabilities {
        network: true,
        fs: FsPolicy::Allow(cwd_root()),
        env: true,
        ffi: true,
    };
    // 文件在根下应可访问。
    let v = run_with_caps(
        r#"
use std.fs.{ write_text, read_text, remove }
let p = "__ol_test_sandbox_allowpath.txt"
write_text(p, "x")
let t = read_text(p)
remove(p)
t
"#,
        caps,
    )
    .expect("allow-path should permit under-root fs");
    match v {
        Value::Text(s) => assert_eq!(s.as_str(), "x"),
        other => panic!("expected text, got {}", other.display_string()),
    }
}

#[test]
fn allow_path_blocks_escape() {
    let caps = Capabilities {
        network: true,
        fs: FsPolicy::Allow(cwd_root()),
        env: true,
        ffi: true,
    };
    assert_caps_err(
        r#"
use std.fs.{ write_text }
write_text("../__ol_test_sandbox_escape2.txt", "x")
"#,
        caps,
        "outside sandbox",
    );
}

#[test]
fn sandbox_blocks_remove_dir_escape() {
    let caps = Capabilities::sandbox(cwd_root());
    assert_caps_err(
        r#"
use std.fs.{ remove_dir }
remove_dir("../__ol_test_sandbox_escape_dir")
"#,
        caps,
        "outside sandbox",
    );
}

#[test]
fn sandbox_blocks_rename_escape() {
    let caps = Capabilities::sandbox(cwd_root());
    assert_caps_err(
        r#"
use std.fs.{ rename }
rename("../__ol_a.txt", "../__ol_b.txt")
"#,
        caps,
        "outside sandbox",
    );
}

#[test]
fn full_caps_value_is_none_when_script_returns_none() {
    // 回归：full caps 下普通脚本仍正常返回。
    let v = run_with_caps("none", Capabilities::full()).expect("run");
    assert!(matches!(v, Value::None));
}
