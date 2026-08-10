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
fn macro_sq_expands_at_call_site() {
    assert_num(
        r"
macro sq(x) {
    return quote(ex) with (x) {
        var ex = eval(x)
        ex * ex
    }
}
n = 6
sq{n}
",
        "36",
    );
}

#[test]
fn macro_identity_returns_ast_param() {
    assert_num(
        r"
macro identity(x) {
    return x
}
identity{6}
",
        "6",
    );
}

#[test]
fn quote_literal_eval() {
    assert_num(
        r"
eval(quote { 42 })
",
        "42",
    );
}

#[test]
fn quote_with_binding() {
    assert_num(
        r"
x = 5
macro inc(v) {
    return quote with (v) {
        eval(v) + 1
    }
}
inc{x}
",
        "6",
    );
}

#[test]
fn quote_hygienic_name() {
    assert_num(
        r"
macro double_it(x) {
    return quote(a) with (x) {
        var a = eval(x)
        a + a
    }
}
double_it{5}
",
        "10",
    );
}

#[test]
fn macro_forty_two_quote_block() {
    assert_num(
        r"
macro forty_two() {
    return quote {
        42
    }
}
forty_two{}
",
        "42",
    );
}

#[test]
fn macro_nested_call() {
    assert_num(
        r"
macro sq(x) {
    return quote(ex) with (x) {
        var ex = eval(x)
        ex * ex
    }
}
sq{sq{2}}
",
        "16",
    );
}

#[test]
fn eval_ast_var_ref() {
    assert_num(
        r"
a = 7
eval(quote { a })
",
        "7",
    );
}

#[test]
fn ast_struct_var_ref() {
    assert_text(
        r#"
macro inspect(x) {
    return quote(ex) with (x) {
        match (ast_struct(x)) {
            case AstVarRef { name } {
                return name
            }
        } else {
            return "other"
        }
    }
}
let v = 99
inspect{v}
"#,
        "v",
    );
}

#[test]
fn macro_param_ast_kind_strict() {
    run_err(
        r#"
macro inspect(x:: VarRefNode) {
    return quote(ex) with (x) {
        return "ok"
    }
}
inspect{42}
"#,
    );
}

#[test]
fn type_of_ast_value() {
    assert_bool(
        r"
type(quote { 1 }) == AST
",
        true,
    );
}

#[test]
fn std_ast_parse() {
    assert_num(
        r#"
use std.ast.{parse}
ast = parse("1 + 2")
eval(ast)
"#,
        "3",
    );
}
