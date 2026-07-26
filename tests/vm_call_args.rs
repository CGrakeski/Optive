mod common;

use common::{assert_num, assert_text};

#[test]
fn default_args() {
    assert_num(
        r#"
func add(a, b = 2) { return a + b }
add(3)
"#,
        "5",
    );
    assert_num(
        r#"
func add(a, b = 2) { return a + b }
add(3, 10)
"#,
        "13",
    );
}

#[test]
fn default_expr_evaluated_at_def() {
    assert_num(
        r#"
let n = 1
func f(x = n) { return x }
n = 99
f()
"#,
        "1",
    );
}

#[test]
fn keyword_args() {
    assert_num(
        r#"
func sub(a, b) { return a - b }
sub(b = 3, a = 10)
"#,
        "7",
    );
}

#[test]
fn mix_positional_and_keyword() {
    assert_num(
        r#"
func f(a, b, c) { return a * 100 + b * 10 + c }
f(1, c = 3, b = 2)
"#,
        "123",
    );
}

#[test]
fn star_args() {
    assert_text(
        r#"
func f(a, *rest) { return str(a) + str(rest) }
f(1, 2, 3, 4)
"#,
        "1[2, 3, 4]",
    );
}

#[test]
fn star_args_empty() {
    assert_text(
        r#"
func f(a, *rest) { return str(rest) }
f(1)
"#,
        "[]",
    );
}

#[test]
fn kwargs() {
    assert_text(
        r#"
func f(a, **kw) { return str(a) + str(kw) }
f(1, x = 2, y = 3)
"#,
        "1{\"x\": 2, \"y\": 3}",
    );
}

#[test]
fn args_and_kwargs() {
    assert_text(
        r#"
func f(a, b = 0, *args, **kwargs) {
    return str(a) + str(b) + str(args) + str(kwargs)
}
f(1, 2, 3, 4, x = 9)
"#,
        "12[3, 4]{\"x\": 9}",
    );
}

#[test]
fn call_splat_kwargs() {
    assert_num(
        r#"
func f(a, b, c) { return a + b + c }
xs = [1, 2]
kw = { "c": 3 }
f(*xs, **kw)
"#,
        "6",
    );
}

#[test]
fn call_only_kwargs_splat() {
    assert_num(
        r#"
func f(a, b) { return a * 10 + b }
f(**{ "a": 4, "b": 2 })
"#,
        "42",
    );
}

#[test]
fn missing_required_errors() {
    let err = optive::run_source(
        r#"
func f(a, b) { return a + b }
f(1)
"#,
    );
    assert!(err.is_err(), "expected error");
}

#[test]
fn unexpected_keyword_errors() {
    let err = optive::run_source(
        r#"
func f(a) { return a }
f(a = 1, z = 2)
"#,
    );
    assert!(err.is_err(), "expected error");
}

#[test]
fn do_func_defaults() {
    assert_num(
        r#"
let f = do(a, b = 5) { return a + b }
f(2)
"#,
        "7",
    );
}
