#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_bool, assert_num, assert_text};

#[test]
fn ternary_true_branch() {
    assert_num("if true then 1 else 2", "1");
}

#[test]
fn ternary_false_branch() {
    assert_num("if false then 1 else 2", "2");
}

#[test]
fn ternary_in_let() {
    assert_num("let x = if true then 1 else 2\nx", "1");
}

#[test]
fn ternary_in_call_arg() {
    assert_num(
        r"
func id(x) { return x }
id(if true then 7 else 8)
",
        "7",
    );
}

#[test]
fn ternary_as_add_operand() {
    assert_num("1 + if true then 2 else 3", "3");
}

#[test]
fn ternary_nested() {
    assert_num("if true then if false then 1 else 2 else 3", "2");
}

#[test]
fn handle_success() {
    assert_num("handle 5", "5");
}

#[test]
fn handle_catches_throw() {
    assert_bool(
        r#"
func boom() { throw ValueError("boom") }
handle boom() is none
"#,
        true,
    );
}

#[test]
fn handle_catches_zero_division() {
    assert_bool("handle 1/0 is none", true);
}

#[test]
fn try_catch_zero_division() {
    assert_text(
        r"
try {
    1/0
} catch (e: ZeroDivisionError) {
    e.message
}
",
        "division by zero",
    );
}

#[test]
fn try_catch_arithmetic_error_base() {
    assert_text(
        r"
try {
    1/0
} catch (e: ArithmeticError) {
    e.message
}
",
        "division by zero",
    );
}

#[test]
fn walrus_named_assign() {
    assert_num(
        r"
x = 0
if (p := 3) { x = p }
x
",
        "3",
    );
}

#[test]
fn placeholder_in_pipeline() {
    assert_num(
        r"
func double(x) { return x * 2 }
5 |> double(_)
",
        "10",
    );
}

#[test]
fn is_none_still_works() {
    assert_bool("none is none", true);
}
