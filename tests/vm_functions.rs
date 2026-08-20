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
fn simple_function() {
    assert_num(
        r"
func double(x) { return x * 2 }
double(21)
",
        "42",
    );
}

#[test]
fn function_two_params() {
    assert_num(
        r"
func add(a, b) { return a + b }
add(2, 3)
",
        "5",
    );
}

#[test]
fn function_no_params() {
    assert_num(
        r"
func answer() { return 42 }
answer()
",
        "42",
    );
}

#[test]
fn function_recursive_factorial() {
    assert_num(
        r"
func fact(n) {
    if (n <= 1) { return 1 }
    return n * fact(n - 1)
}
fact(5)
",
        "120",
    );
}

#[test]
fn function_recursive_fib() {
    assert_num(
        r"
func fib(n) {
    if (n <= 1) { return n }
    return fib(n - 1) + fib(n - 2)
}
fib(10)
",
        "55",
    );
}

#[test]
fn function_recursive_fib_30_no_stack_overflow() {
    assert_num(
        r"
func fib(n) {
    if (n <= 2) { return 1 }
    return fib(n - 1) + fib(n - 2)
}
fib(30)
",
        "832040",
    );
}

#[test]
fn do_anonymous_function() {
    assert_num(
        r"
let sq = do(x) { return x * x }
sq(7)
",
        "49",
    );
}

#[test]
fn function_call_in_expr() {
    assert_num(
        r"
func inc(x) { return x + 1 }
inc(1) + inc(2)
",
        "5",
    );
}

#[test]
fn nested_function_calls() {
    assert_num(
        r"
func f(x) { return x + 1 }
func g(x) { return f(x) * 2 }
g(5)
",
        "12",
    );
}

#[test]
fn function_returns_none_expr() {
    assert_num(
        r"
func noop() { return }
1 + 1
",
        "2",
    );
}

#[test]
fn function_ellipsis_empty_body() {
    let v = common::value("func f() ...\nf()");
    assert!(
        matches!(v, optive::value::Value::None),
        "expected none, got {}",
        v.display_string()
    );
}

#[test]
fn global_mutation_in_function() {
    assert_num(
        r"
let counter = 0
func bump() { counter = counter + 1 }
bump()
bump()
counter
",
        "2",
    );
}

#[test]
fn function_implicit_last_expr_return() {
    assert_num(
        r"
func a() {
    1 + 1
}
a()
",
        "2",
    );
}

#[test]
fn function_empty_return_is_none() {
    let v = common::value(
        r"
func b() {
    42
    return
}
b()
",
    );
    assert!(
        matches!(v, optive::value::Value::None),
        "expected none, got {}",
        v.display_string()
    );
}

#[test]
fn do_block_implicit_last_expr() {
    assert_num(
        r"
let result = do {
    let x = 10
    x * 2
}
result
",
        "20",
    );
}

#[test]
fn function_implicit_if_value() {
    assert_num(
        r"
func pick(x) {
    if (x) { 10 } else { 20 }
}
pick(false)
",
        "20",
    );
}

#[test]
fn bare_block_is_not_expression() {
    common::parse_err(
        r"
let result = {
    let x = 10
    x * 2
}
",
    );
}

