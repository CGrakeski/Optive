#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_num, run_err};

#[test]
fn macro_call_uses_parse_time_ast() {
    assert_num(
        r"
macro twice(x) {
    return quote(ex) with (x) {
        eval(x) + eval(x)
    }
}
twice{21}
",
        "42",
    );
}

#[test]
fn macro_cannot_be_called_with_parens() {
    run_err(
        r"
macro m(x) { return x }
m(1)
",
    );
}

#[test]
fn function_cannot_be_called_with_braces() {
    run_err(
        r"
func f(x) { return x }
f{1}
",
    );
}

#[test]
fn macro_and_function_shadowing() {
    // Later macro binding replaces func in globals; macro call uses {}.
    assert_num(
        r"
func sq(x) { return x * x }
macro sq(x) {
    return quote(ex) with (x) {
        eval(x) * eval(x)
    }
}
sq{3}
",
        "9",
    );
}

#[test]
fn macro_pow4_nested_ast_compose() {
    assert_num(
        r"
macro sq(x) {
    return quote(ex) with (x) {
        var ex = eval(x)
        ex * ex
    }
}
macro pow4(x) {
    return sq{sq{x}}
}
pow4{2}
",
        "16",
    );
}

#[test]
fn macro_variadic_count() {
    assert_num(
        r"
macro COUNT(*msg) {
    return quote with (msg) {
        len(msg)
    }
}
COUNT{1, 2, 3}
",
        "3",
    );
}
