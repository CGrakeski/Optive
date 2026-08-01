mod common;

use common::{parse_err, parse_ok};
use optive::parse_program;

fn stmt_count(source: &str) -> usize {
    parse_program(source).unwrap().stmts.len()
}

#[test]
fn parse_call_subtraction_in_args() {
    parse_ok("fib(n-1)");
    parse_ok("return fib(n-1) + fib(n-2)");
}

#[test]
fn parse_empty() {
    assert_eq!(stmt_count(""), 0);
}

#[test]
fn parse_literal_number() {
    assert_eq!(stmt_count("42"), 1);
}

#[test]
fn parse_literal_string() {
    assert_eq!(stmt_count(r#""hi""#), 1);
}

#[test]
fn parse_literal_bool() {
    assert_eq!(stmt_count("true"), 1);
}

#[test]
fn parse_literal_none() {
    assert_eq!(stmt_count("none"), 1);
}

#[test]
fn parse_addition() {
    parse_ok("1 + 2");
}

#[test]
fn parse_subtraction() {
    parse_ok("1 - 2");
}

#[test]
fn parse_multiplication() {
    parse_ok("2 * 3");
}

#[test]
fn parse_division() {
    parse_ok("6 / 2");
}

#[test]
fn parse_precedence() {
    parse_ok("1 + 2 * 3");
}

#[test]
fn parse_comparison_eq() {
    parse_ok("1 == 1");
}

#[test]
fn parse_comparison_ne() {
    parse_ok("1 != 2");
}

#[test]
fn parse_comparison_lt_gt() {
    parse_ok("1 < 2");
    parse_ok("3 > 2");
}

#[test]
fn parse_comparison_le_ge() {
    parse_ok("1 <= 1");
    parse_ok("2 >= 1");
}

#[test]
fn parse_unary_neg() {
    parse_ok("-5");
}

#[test]
fn parse_unary_not_bang() {
    parse_ok("!true");
}

#[test]
fn parse_unary_not_keyword() {
    parse_ok("not x");
}

#[test]
fn parse_and_or() {
    parse_ok("a and b or c");
}

#[test]
fn parse_pipeline() {
    parse_ok("x |> f(_)");
}

#[test]
fn parse_let_decl() {
    parse_ok("let x = 1");
}

#[test]
fn parse_var_decl() {
    parse_ok("var x = 1");
}

#[test]
fn parse_const_decl() {
    parse_ok("const let x = 1");
}

#[test]
fn parse_assign() {
    parse_ok("x = 1");
}

#[test]
fn parse_index_assign() {
    parse_ok("xs[0] = 1");
}

#[test]
fn parse_index_assign_var_index() {
    parse_ok("xs[i] = 1");
}

#[test]
fn parse_index_assign_var_indices() {
    parse_ok("xs[i] = xs[j]");
}

#[test]
fn parse_type_inst_expr() {
    parse_ok("Box[num](99)");
}

#[test]
fn parse_slice_expr() {
    parse_ok("xs[1:3]");
}

#[test]
fn parse_slice_step() {
    parse_ok("xs[0:5:2]");
}

#[test]
fn parse_del_index() {
    parse_ok("del xs[1]");
}

#[test]
fn parse_del_name() {
    parse_ok("del tmp");
}

#[test]
fn parse_func_decl() {
    parse_ok("func f(x) { return x }");
}

#[test]
fn parse_func_no_params() {
    parse_ok("func f() { return 1 }");
}

#[test]
fn parse_func_return_type_arrow() {
    parse_ok("func f() -> num { return 1 }");
}

#[test]
fn parse_func_return_type_fat_arrow() {
    parse_ok("func f() : wrap(_) { return 1 }");
    parse_ok("func f() => text { return \"a\" }");
    parse_ok("func f() -> text : wrap(_) { return 1 }");
    parse_ok("func f() => Result : Result(_) { return 1 }");
}

#[test]
fn parse_do_expr() {
    parse_ok("do(x) { return x }");
}

#[test]
fn parse_do_block_iife_sugar() {
    parse_ok("do { return 1 }");
    parse_ok("go do { return 2 }");
    parse_ok("await do { return 3 }");
}

#[test]
fn parse_if_stmt() {
    parse_ok("if (true) { let x = 1 }");
}

#[test]
fn parse_if_elif_else() {
    parse_ok("if (false) { } elif (true) { } else { }");
}

#[test]
fn parse_while_loop() {
    parse_ok("while (true) { break }");
}

#[test]
fn parse_match_value_case() {
    parse_ok(
        r#"
match (n) {
    case (0) { 0 }
} else { 1 }
"#,
    );
}

#[test]
fn parse_match_list_pattern() {
    parse_ok("match (xs) { case [a, b] { a } }");
}

#[test]
fn parse_counted_loop() {
    parse_ok("loop (10) { }");
}

#[test]
fn parse_infinite_loop() {
    parse_ok("loop { break }");
}

#[test]
fn parse_for_in() {
    parse_ok("for (x in xs) { }");
}

#[test]
fn parse_break_continue() {
    parse_ok("loop { break\ncontinue }");
}

#[test]
fn parse_throw() {
    parse_ok("throw none");
}

#[test]
fn parse_list_literal() {
    parse_ok("[1, 2, 3]");
}

#[test]
fn parse_empty_list() {
    parse_ok("[]");
}

#[test]
fn parse_dict_literal() {
    parse_ok("{1: 2, 3: 4}");
}

#[test]
fn parse_empty_dict() {
    parse_ok("{}");
}

#[test]
fn parse_index_access() {
    parse_ok("xs[0]");
}

#[test]
fn parse_member_access() {
    parse_ok("obj.field");
}

#[test]
fn parse_call() {
    parse_ok("f(1, 2)");
}

#[test]
fn parse_call_named_arg() {
    parse_ok("f(a = 1)");
}

#[test]
fn parse_struct_decl() {
    parse_ok("struct Point { let x\nlet y }");
}

#[test]
fn parse_variant_plain_struct_cases() {
    parse_ok(
        r#"
variant Expr {
    Lit = struct { let value }
    Bin = struct { let op let left let right }
}
"#,
    );
}

#[test]
fn parse_typed_struct() {
    parse_ok("typed struct P { let x: num }");
}

#[test]
fn parse_struct_with_method() {
    parse_ok("struct S { func m(self) { return 1 } }");
}

#[test]
fn parse_grouped_expr() {
    parse_ok("(1 + 2)");
}

#[test]
fn parse_multiline_in_parens() {
    parse_ok("(1\n+\n2)");
}

#[test]
fn parse_multiline_list() {
    parse_ok("[1,\n2,\n3]");
}

/// `{ foo(\n x\n) }` 内换行在 () 里，不应被当成多语句块。
#[test]
fn parse_brace_call_with_newline_inside_parens() {
    parse_ok("let s = { abs(\n  -1\n) }");
}

#[test]
fn parse_type_annotation_soft() {
    parse_ok("let x: num = 1");
}

#[test]
fn parse_type_annotation_strong() {
    parse_ok("let x:: num = 1");
}

#[test]
fn parse_param_strong_type() {
    parse_ok("func f(x:: num) { return x }");
}

#[test]
fn parse_param_soft_type() {
    parse_ok("func f(x: num) { return x }");
}

#[test]
fn parse_do_strong_return() {
    parse_ok("do(x) -> num { return x }");
}

#[test]
fn parse_struct_field_strong() {
    parse_ok("struct S { var n:: num }");
}

#[test]
fn parse_struct_field_soft() {
    parse_ok("struct S { var n: num }");
}

#[test]
fn parse_err_incomplete() {
    parse_err("let x =");
}

#[test]
fn parse_err_unclosed_paren() {
    parse_err("(1 + 2");
}

#[test]
fn parse_err_unclosed_brace() {
    parse_err("func f() { return 1");
}

#[test]
fn parse_placeholder_only_in_pipeline_or_wrapper() {
    parse_ok("x |> f(_)");
    parse_ok("func f() : wrap(_) { return 1 }");
    parse_err("_");
    parse_err("{ _ }");
    parse_err("let x = _");
}

#[test]
fn parse_ellipsis_empty_block() {
    parse_ok("func f() ...");
    parse_ok("func f() { }");
    parse_err("func f() { ... }");
}

#[test]
fn parse_catch_ellipsis() {
    parse_ok("try { } catch (...) { }");
    parse_ok("try { } catch (e) { }");
    parse_ok("try { } catch (e: ...) { }");
    parse_ok("try { } catch (e: ValueError) { }");
    parse_err("try { } catch (_) { }");
    parse_err("try { } catch (e: _) { }");
}

#[test]
fn parse_import_qualified() {
    parse_ok("import std.math as math");
}

#[test]
fn parse_import_string() {
    parse_ok(r#"import "foo/bar.tive" as bar"#);
}

#[test]
fn parse_use_items() {
    parse_ok("use std.math.{ range, abs as magnitude }");
}

#[test]
fn parse_intern_export() {
    parse_ok("intern let x = 1");
    parse_ok("export let y = 2");
}

#[test]
fn parse_macro_decl() {
    parse_ok("macro sq(x) { return x }");
}

#[test]
fn parse_quote_expr() {
    parse_ok("quote { 42 }");
    parse_ok("quote(a) with (x) { eval(x) + 1 }");
}

#[test]
fn parse_macro_call() {
    parse_ok("sq{6}");
}

#[test]
fn parse_friend_func() {
    parse_ok("friend func add(x:: num) { return x + 1 }");
    parse_ok("friend func placeholder");
}

#[test]
fn parse_in_operator() {
    parse_ok("1 in xs");
}
