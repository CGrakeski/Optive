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

#[test]
fn script_nested_assign_updates_unescaped() {
    assert_num(
        r"
let x = 1
if (true) {
    x = x + 10
}
x
",
        "11",
    );
}

#[test]
fn script_unescaped_arith_loop() {
    assert_num(
        r"
let sum = 0
loop (1000) {
    sum = sum + 1
}
sum
",
        "1000",
    );
}

#[test]
fn script_add_store_two_lets() {
    assert_num(
        r"
let a = 10
let b = 32
a = a + b
a
",
        "42",
    );
}

#[test]
fn escaped_top_level_sees_later_stores() {
    assert_num(
        r"
let n = 1
func f() { return n }
n = 2
f()
",
        "2",
    );
}

#[test]
fn heavy_call_from_script_does_not_clobber_fast_local() {
    assert_num(
        r"
let x = 1
func g() { return 1 }
func f() { return g() + 1 }
x = x + 1
f() + x
",
        "4",
    );
}

#[test]
fn repl_flushes_unescaped_let_to_next_snippet() {
    let mut vm = optive::vm::Vm::new();
    let first = optive::run_source_in_vm(&mut vm, "let n = 1\nn", "<repl>").expect("first snippet");
    assert_eq!(first.display_string(), "1");
    let second = optive::run_source_in_vm(&mut vm, "n + 1", "<repl>").expect("second snippet");
    assert_eq!(second.display_string(), "2");
}

#[test]
fn go_do_ticker_increments_escaped_global() {
    common::assert_num_w1(
        r"
var progressed = 0
let ticker = go do {
    var i = 0
    while (i < 20) {
        progressed = progressed + 1
        i = i + 1
        suspend
    }
    return progressed
}
await ticker
",
        "20",
    );
}

#[test]
fn go_do_ticker_increments_with_workers() {
    let mut vm = optive::vm::Vm::with_workers(2);
    let v = optive::run_source_in_vm(
        &mut vm,
        r"
var progressed = 0
await (go do {
    var i = 0
    while (i < 20) {
        progressed = progressed + 1
        i = i + 1
        suspend
    }
    return progressed
})
",
        "<test>",
    )
    .expect("run");
    assert_eq!(v.display_string(), "20");
}

/// 脚本未逃逸槽（`n` 在 await 完成前为空）+ helper 上的 `go` 重帧迁回主线程。
/// 若 install 后 `lw_depth` 误为 1，`LoadFast i` 会读到空的 `n` 槽。
#[test]
fn go_do_ticker_with_unescaped_await_binding() {
    let mut vm = optive::vm::Vm::with_workers(2);
    let v = optive::run_source_in_vm(
        &mut vm,
        r"
let n = await (go do {
    var i = 0
    while (i < 20) {
        i = i + 1
        suspend
    }
    return i
})
n
",
        "<test>",
    )
    .expect("run");
    assert_eq!(v.display_string(), "20");
}

#[test]
fn go_do_reads_live_top_level() {
    assert_num(
        r"
let n = 1
n = 2
let t = go do { return n + 1 }
await t
",
        "3",
    );
}
