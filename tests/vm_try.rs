#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_num, assert_text, run_err};

#[test]
fn try_catch_from_nested_call() {
    assert_num(
        r#"
func boom() {
    throw ValueError("nested")
}
try {
    boom()
} catch (e: ValueError) {
    7
}
"#,
        "7",
    );
}

#[test]
fn try_catch_value_error() {
    assert_num(
        r#"
try {
    throw ValueError("bad")
} catch (e: ValueError) {
    1
} else {
    0
}
"#,
        "1",
    );
}

#[test]
fn try_else_on_success() {
    assert_num(
        r"
try {
    42
} catch (e: Exception) {
    0
} else {
    1
}
",
        "1",
    );
}

#[test]
fn try_catch_subtype() {
    assert_num(
        r#"
try {
    throw RuntimeError("x")
} catch (e: Exception) {
    2
}
"#,
        "2",
    );
}

#[test]
fn try_uncaught_propagates() {
    run_err(
        r#"
throw ValueError("fail")
"#,
    );
}

#[test]
fn throw_non_exception_fails() {
    run_err("throw 1");
}

#[test]
fn exception_message_field() {
    assert_text(
        r#"
try {
    throw ValueError("hello")
} catch (e: ValueError) {
    e.message
}
"#,
        "hello",
    );
}

#[test]
fn host_undefined_name_is_name_error() {
    assert_text(
        r"
try {
    no_such_var
} catch (e: NameError) {
    e.message
}
",
        "undefined name: no_such_var",
    );
}

#[test]
fn host_index_error_catchable() {
    assert_text(
        r"
try {
    [1][99]
} catch (e: IndexError) {
    e.message
}
",
        "index out of range",
    );
}

#[test]
fn host_key_error_from_dict_index() {
    assert_text(
        r#"
try {
    {"a": 1}["b"]
} catch (e: KeyError) {
    e.message
}
"#,
        "key not found",
    );
}

#[test]
fn host_type_error_from_bang() {
    assert_text(
        r"
try {
    !1
} catch (e: TypeError) {
    e.message
}
",
        "! requires bool",
    );
}

#[test]
fn host_type_error_not_callable() {
    assert_num(
        r"
try {
    1(2)
} catch (e: TypeError) {
    1
}
",
        "1",
    );
}

#[test]
fn handle_host_errors() {
    use common::assert_bool;
    assert_bool("handle no_such is none", true);
    assert_bool("handle [1][9] is none", true);
    assert_bool(r#"handle {"a":1}[2] is none"#, true);
}

/// break 越过 handle 时必须 PopTry，否则后续异常会跳进已废弃的 catch PC。
#[test]
fn handle_break_clears_try_frame() {
    assert_num(
        r"
let n = 0
loop {
    let _ = handle (1 / 0)
    n = n + 1
    if (n == 1) { break }
}
try {
    1 / 0
} catch (e: ZeroDivisionError) {
    n + 10
}
",
        "11",
    );
}

/// break 从 handle 操作数内的 match 块跳出时也要 `PopTry`。
#[test]
fn handle_break_inside_match_operand() {
    assert_num(
        r"
let n = 0
loop {
    n = n + 1
    let _ = handle (match (n) {
        case (1) { break }
    } else { 0 })
    n = n + 100
}
n
",
        "1",
    );
}

#[test]
fn catch_lookup_error_base_for_index() {
    assert_num(
        r"
try {
    [0][1]
} catch (e: LookupError) {
    3
}
",
        "3",
    );
}

#[test]
fn throw_non_exception_is_type_error() {
    assert_text(
        r"
try {
    throw 1
} catch (e: TypeError) {
    e.message
}
",
        "can only throw exception",
    );
}
