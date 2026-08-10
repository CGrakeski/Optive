#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::assert_num;

#[test]
fn pipeline_simple_double() {
    assert_num(
        r"
func double(n) { return n * 2 }
5 |> double(_)
",
        "10",
    );
}

#[test]
fn pipeline_chain() {
    assert_num(
        r"
func inc(n) { return n + 1 }
func double(n) { return n * 2 }
3 |> inc(_) |> double(_)
",
        "8",
    );
}

#[test]
fn pipeline_with_len() {
    assert_num(
        r"
func add1(n) { return n + 1 }
[1, 2, 3] |> len(_)
",
        "3",
    );
}

#[test]
fn pipeline_square() {
    assert_num(
        r"
func sq(n) { return n * n }
2 |> sq(_)
",
        "4",
    );
}

#[test]
fn pipeline_identity_func() {
    assert_num(
        r"
func id(n) { return n }
99 |> id(_)
",
        "99",
    );
}

#[test]
fn pipeline_three_step() {
    assert_num(
        r"
func a(n) { return n + 1 }
func b(n) { return n * 2 }
func c(n) { return n - 3 }
1 |> a(_) |> b(_) |> c(_)
",
        "1",
    );
}
