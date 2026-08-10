#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_bool, assert_num};

#[test]
fn eq_ints_true() {
    assert_bool("1 == 1", true);
}

#[test]
fn eq_ints_false() {
    assert_bool("1 == 2", false);
}

#[test]
fn ne_ints() {
    assert_bool("1 != 2", true);
}

#[test]
fn lt_ints() {
    assert_bool("1 < 2", true);
}

#[test]
fn gt_ints() {
    assert_bool("3 > 2", true);
}

#[test]
fn le_equal() {
    assert_bool("2 <= 2", true);
}

#[test]
fn ge_equal() {
    assert_bool("5 >= 5", true);
}

#[test]
fn text_lexicographic_cmp() {
    // B18：text 支持字典序关系比较。
    assert_bool(r#""a" >= "0""#, true);
    assert_bool(r#""0" < "a""#, true);
    assert_bool(r#""ab" <= "ab""#, true);
    assert_bool(r#""b" > "a""#, true);
}

#[test]
fn bang_true() {
    assert_bool("!false", true);
}

#[test]
fn bang_false() {
    assert_bool("!true", false);
}

#[test]
fn not_truthy_zero() {
    assert_bool("not 0", true);
}

#[test]
fn not_truthy_one() {
    assert_bool("not 1", false);
}

#[test]
fn not_empty_string() {
    assert_bool("not \"\"", true);
}

#[test]
fn not_nonempty_string() {
    assert_bool("not \"x\"", false);
}

#[test]
fn and_short_circuit_true() {
    assert_num("0 and 5", "0");
}

#[test]
fn and_short_circuit_false() {
    assert_num("3 and 5", "5");
}

#[test]
fn or_short_circuit_true() {
    assert_num("1 or 99", "1");
}

#[test]
fn or_short_circuit_false() {
    assert_num("0 or 7", "7");
}

#[test]
fn compare_rational() {
    assert_bool("1/2 < 1", true);
}

#[test]
fn eq_rational() {
    assert_bool("1/2 == 1/2", true);
}
