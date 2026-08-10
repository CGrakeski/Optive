#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::assert_bool;

#[test]
fn is_none_identity() {
    assert_bool(
        r"
let x = none
x is none
",
        true,
    );
    assert_bool(
        r"
let x = none
x is not none
",
        false,
    );
}

#[test]
fn is_none_with_value() {
    assert_bool(
        r"
let x = 1
x is none
",
        false,
    );
    assert_bool(
        r"
let x = 1
x is not none
",
        true,
    );
}

#[test]
fn eq_none_not_type_error() {
    assert_bool("1 == none", false);
    assert_bool("none == 1", false);
    assert_bool("none == none", true);
}

#[test]
fn is_vs_eq_for_none() {
    assert_bool(
        r"
let a = none
let b = none
a is b
",
        true,
    );
    assert_bool(
        r"
let a = none
let b = none
a == b
",
        true,
    );
}
