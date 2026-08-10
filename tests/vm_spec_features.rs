#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_bool, assert_list, assert_num, assert_text};
use optive::run_source;

#[test]
fn list_comp_simple() {
    assert_list("[x for (x in [1, 2, 3])]", "[1, 2, 3]");
}

#[test]
fn list_comp_with_guard() {
    assert_list(
        "[x * x for (x in [1, 2, 3]) if (x > 1)]",
        "[4, 9]",
    );
}

#[test]
fn std_decos_log_decorator() {
    assert_num(
        r"
use std.decos.{ log }
let calls = 0
log func f(x) {
    calls = calls + 1
    return x * 2
}
f(3)
calls
",
        "1",
    );
}

#[test]
fn std_decos_memoize_caches_calls() {
    assert_num(
        r"
use std.decos.{ memoize }
let calls = 0
memoize func f(x) {
    calls = calls + 1
    return x * 2
}
f(3)
f(3)
calls
",
        "1",
    );
}

#[test]
fn std_decos_once_runs_once() {
    assert_num(
        r"
use std.decos.{ once }
let calls = 0
once func f() {
    calls = calls + 1
    return 42
}
f()
f()
calls
",
        "1",
    );
}

#[test]
fn type_convert_text_handler() {
    assert_text(
        r"
text.__convert__.__dispatch__.append(do(t, v) { return text(v + 1) })
text.(41)
",
        "42",
    );
}

#[test]
fn type_convert_bool_default_handler() {
    assert_bool(
        r"
bool.(1)
",
        true,
    );
}

#[test]
fn type_convert_struct_handler() {
    assert_num(
        r"
struct A { let n }
A.__convert__.__dispatch__.append(do(t, v) { return A(v) })
A.(7).n
",
        "7",
    );
}

#[test]
fn type_convert_list_iterator_roundtrip() {
    assert_list(
        r"
use std.math.{ range }
let it = iterator(range(3))
list.(it)
",
        "[0, 1, 2]",
    );
    assert_text(
        r"
let xs = [10, 20]
type(iterator.(xs))
",
        "iterator",
    );
}

#[test]
fn ctor_and_convert_errors_are_distinct() {
    for (src, needle) in [
        ("iterator(1)", "cannot construct iterator from num"),
        ("iterator.(1)", "cannot convert num to iterator"),
        ("iterator.()", "cannot convert nonetype to iterator"),
    ] {
        let err = run_source(src).unwrap_err().to_string();
        assert!(err.contains(needle), "source: {src}, got: {err}");
    }
}
