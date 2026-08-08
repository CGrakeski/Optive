//! std.macros 实用宏库冒烟测试。
mod common;

use common::{assert_num, run_err, value};
use optive::value::Value;

#[test]
fn macros_assert_eq_ok() {
    let _ = value(
        r#"
use std.macros.{ assert_eq }
assert_eq{1 + 1, 2}
0
"#,
    );
}

#[test]
fn macros_assert_eq_fails() {
    run_err(
        r#"
use std.macros.{ assert_eq }
assert_eq{1, 2}
"#,
    );
}

#[test]
fn macros_dbg_returns_value() {
    assert_num(
        r#"
use std.macros.{ dbg }
dbg{6 * 7}
"#,
        "42",
    );
}

#[test]
fn macros_or_else() {
    assert_num(
        r#"
use std.macros.{ or_else }
or_else{none, 9}
"#,
        "9",
    );
}

#[test]
fn macros_todo_throws() {
    run_err(
        r#"
use std.macros.{ todo }
todo{}
"#,
    );
}

#[test]
fn macros_when_unless() {
    assert_num(
        r#"
use std.macros.{ when, unless }
when{true, 7}
"#,
        "7",
    );
    assert_num(
        r#"
use std.macros.{ unless }
unless{false, 11}
"#,
        "11",
    );
}

#[test]
fn macros_identity_and_stringify() {
    assert_num(
        r#"
use std.macros.{ identity }
identity{3 + 4}
"#,
        "7",
    );
    match value(
        r#"
use std.macros.{ stringify }
stringify{1 + 2}
"#,
    ) {
        Value::Text(s) => assert_eq!(s, "3"),
        other => panic!("{}", other.display_string()),
    }
}
