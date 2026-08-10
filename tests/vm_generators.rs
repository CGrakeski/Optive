#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_list, assert_num, run_err, value};

#[test]
fn generator_func_lazy_call_returns_iterator() {
    assert_eq!(
        value(
            r"
gen count() {
  yield 1
  yield 2
}
type(count())
"
        )
        .display_string(),
        "iterator"
    );
}

#[test]
fn generator_next_and_stop() {
    assert_num(
        r"
gen count() {
  yield 10
  yield 20
}
let g = count()
next(g) + next(g)
",
        "30",
    );
    run_err(
        r"
gen once() {
  yield 1
}
let g = once()
next(g)
next(g)
",
    );
}

#[test]
fn generator_for_in() {
    assert_num(
        r"
gen count(n) {
  var i = 0
  while (i < n) {
    yield i
    i = i + 1
  }
}
var sum = 0
for (x in count(4)) {
  sum = sum + x
}
sum
",
        "6",
    );
}

#[test]
fn generator_do_closure() {
    assert_list(
        r"
let g = do() {
  yield 1
  yield 2
  yield 3
}
[x for (x in g())]
",
        "[1, 2, 3]",
    );
}

#[test]
fn generator_bare_yield_is_none() {
    assert_eq!(
        value(
            r"
gen g() {
  yield
}
next(g())
"
        )
        .display_string(),
        "none"
    );
}

#[test]
fn generator_return_expr_yields_then_ends() {
    assert_num(
        r"
gen g() {
  yield 1
  return 2
}
let it = g()
next(it) + next(it)
",
        "3",
    );
    run_err(
        r"
func g() {
  return 9
}
let it = g()
next(it)
next(it)
",
    );
}

#[test]
fn generator_yield_from() {
    assert_list(
        r"
gen inner() {
  yield 1
  yield 2
}
gen outer() {
  yield 0
  yield from inner()
  yield 3
}
[x for (x in outer())]
",
        "[0, 1, 2, 3]",
    );
}

#[test]
fn generator_yield_from_list() {
    assert_list(
        r"
gen g() {
  yield from [10, 20]
}
[x for (x in g())]
",
        "[10, 20]",
    );
}

#[test]
fn nested_do_yield_does_not_make_outer_generator() {
    // 外层无 yield：调用应直接跑完并返回内层生成器对象（iterator）。
    assert_eq!(
        value(
            r"
func outer() {
  return do() {
    yield 1
    yield 2
  }
}
type(outer())
"
        )
        .display_string(),
        "function"
    );
    assert_list(
        r"
func outer() {
  return do() {
    yield 1
    yield 2
  }
}
[x for (x in outer()())]
",
        "[1, 2]",
    );
}

#[test]
fn yield_at_module_level_errors() {
    run_err("yield 1");
}
