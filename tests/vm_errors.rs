#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{parse_err, run_err};

#[test]
fn runtime_div_by_zero() {
    run_err("1 / 0");
}

#[test]
fn runtime_undefined_var() {
    run_err("undefined_var_xyz");
}

#[test]
fn runtime_index_out_of_range() {
    run_err("[1][99]");
}

#[test]
fn runtime_bang_non_bool() {
    run_err("!1");
}

#[test]
fn runtime_break_outside_loop() {
    run_err("break");
}

#[test]
fn runtime_continue_outside_loop() {
    run_err("continue");
}

#[test]
fn runtime_unsupported_eq_types() {
    run_err("1 == \"a\"");
}

#[test]
fn parse_error_incomplete_expr() {
    parse_err("1 +");
}

#[test]
fn runtime_call_non_callable() {
    run_err("1(2)");
}

#[test]
fn runtime_dict_missing_key() {
    run_err("{1: 2}[99]");
}

#[test]
fn runtime_struct_wrong_arity() {
    run_err(
        r"
struct P { let x let y }
P(1)
",
    );
}

#[test]
fn runtime_immutable_struct_field() {
    run_err(
        r"
struct P { let x }
let p = P(1)
p.x = 2
",
    );
}

#[test]
fn const_reassign_rejected() {
    run_err(
        r"
const let x = 1
x = 2
",
    );
}

#[test]
fn const_reassign_in_func_rejected() {
    run_err(
        r"
func f() {
    const let x = 1
    x = 2
    return x
}
f()
",
    );
}
