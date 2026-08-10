#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_num, assert_text};

#[test]
fn variant_case_and_wrap() {
    assert_num(
        r"
variant Result {
    typed Ok(value: num)
    Err = typed struct { value: text }
}

ok = Result.Ok(7)
wrapped = Result(ok)
1
",
        "1",
    );
}

#[test]
fn variant_case_plain_struct_body() {
    assert_num(
        r#"
variant Expr {
    Lit = struct { let value }
    Bin = struct { let op let left let right }
}

e = Expr.Lit(42)
b = Expr.Bin("+", e, Expr.Lit(1))
1
"#,
        "1",
    );
}

#[test]
fn return_wrapper_double_wrap() {
    assert_text(
        r#"
variant Result {
    typed Ok(value: num)
    Err = typed struct { value: text }
}

func mk() : Result(_) {
    return Result.Ok(42)
}

v = mk()
if v is none { "none" } else { "ok" }
"#,
        "ok",
    );
}

#[test]
fn ternary_in_return_wrapper() {
    assert_num(
        r#"
variant Result {
    typed Ok(value: num)
    Err = typed struct { value: text }
}

func half(n: num) : Result(_) {
    return if n != 0 then Result.Ok(1 / n) else Result.Err("zero")
}

r = half(2)
1
"#,
        "1",
    );
}

#[test]
fn match_nested_variant_pattern() {
    assert_num(
        r"
variant Result {
    typed Ok(value: num)
    Err = typed struct { value: text }
}

func run() -> num {
    v = Result(Result.Ok(9))
    match (v) {
        case Result(Result.Ok(x)) { return x }
        else { return 0 }
    }
}
run()
",
        "9",
    );
}

#[test]
fn match_typed_variant_direct() {
    // B9：`ParseOutcome.Ok(...)` 应直接用 `case ParseOutcome.Ok(v)`，无需外层再包一层。
    assert_text(
        r#"
variant ParseOutcome {
    typed Ok(value: text)
    typed Err(msg: text)
}
let o = ParseOutcome.Ok("ok")
match (o) {
    case ParseOutcome.Ok(v) { v }
} else {
    "other"
}
"#,
        "ok",
    );
}
