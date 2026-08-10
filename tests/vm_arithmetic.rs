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
fn add_two_ints() {
    assert_num("1 + 2", "3");
}

#[test]
fn sub_two_ints() {
    assert_num("10 - 3", "7");
}

#[test]
fn mul_two_ints() {
    assert_num("6 * 7", "42");
}

#[test]
fn div_ints_to_rational() {
    assert_num("1 / 2", "1/2");
}

#[test]
fn div_even() {
    assert_num("8 / 2", "4");
}

#[test]
fn precedence_mul_before_add() {
    assert_num("1 + 2 * 3", "7");
}

#[test]
fn precedence_left_assoc_sub() {
    assert_num("10 - 3 - 2", "5");
}

#[test]
fn unary_neg() {
    assert_num("-5 + 10", "5");
}

#[test]
fn double_neg() {
    assert_num("--3", "3");
}

#[test]
fn decimal_add() {
    assert_num("3.14 + 3.14", "157/25");
}

#[test]
fn leading_dot_decimal() {
    assert_num(".5 + .5", "1");
}

#[test]
fn scientific_notation() {
    assert_num("1.5e1", "15");
}

#[test]
fn large_integer() {
    assert_num("999999999999999999 + 1", "1000000000000000000");
}

#[test]
fn grouped_expr() {
    assert_num("(1 + 2) * 3", "9");
}

#[test]
fn chained_addition() {
    assert_num("1 + 2 + 3 + 4", "10");
}

#[test]
fn chained_multiplication() {
    assert_num("2 * 3 * 4", "24");
}

#[test]
fn mixed_rational_and_int() {
    assert_num("1 + 1/2", "3/2");
}

#[test]
fn zero_add() {
    assert_num("0 + 0", "0");
}

#[test]
fn multiply_by_zero() {
    assert_num("12345 * 0", "0");
}

#[test]
fn negative_times_negative() {
    assert_num("-3 * -4", "12");
}

#[test]
fn subtract_negative() {
    assert_num("5 - -3", "8");
}
