#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_bool, assert_num, assert_text, run_err};

#[test]
fn macro_must_return_ast() {
    run_err(
        r"
macro bad() { return 42 }
bad{}
",
    );
}

#[test]
fn macro_type_is_macro() {
    assert_text(
        r"
macro m(x) { return x }
type(m)
",
        "Macro",
    );
}

#[test]
fn macro_pow4_returns_nested_macro_call() {
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
fn macro_variadic_log() {
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

#[test]
fn friend_func_dispatch_num_and_text() {
    assert_text(
        r#"
friend func add(x:: num) { return text(x + 1) }
add.__dispatch__.append(do(x:: text) { return x + "!" })
add(41)
"#,
        "42",
    );
}

#[test]
fn friend_func_dispatch_text_handler() {
    assert_text(
        r#"
friend func add(x:: num) { return text(x) }
add.__dispatch__.append(do(x:: text) { return x + "!" })
add("hi")
"#,
        "hi!",
    );
}

#[test]
fn builtin_text_constructor() {
    assert_text(
        r"
text(42)
",
        "42",
    );
}

#[test]
fn in_operator_list() {
    assert_bool(
        r"
3 in [1, 2, 3]
",
        true,
    );
}

#[test]
fn in_operator_text_substring() {
    assert_bool(
        r#"
"ell" in "hello"
"#,
        true,
    );
}

#[test]
fn splat_call_expansion() {
    assert_num(
        r"
func sum3(a, b, c) { return a + b + c }
xs = [1, 2, 3]
sum3(*xs)
",
        "6",
    );
}

#[test]
fn eval_try_in_quoted_ast() {
    assert_num(
        r#"
eval(quote {
    try {
        throw ValueError("x")
    } catch (e) {
        99
    }
})
"#,
        "99",
    );
}

#[test]
fn struct_contains_magic() {
    assert_bool(
        r"
struct Bag {
    var items: list
    func __init__(self, xs) { self.items = xs }
    func __contains__(self, x) {
        for (item in self.items) {
            if (item == x) { return true }
        }
        return false
    }
}
b = Bag([1, 2])
2 in b
",
        true,
    );
}
