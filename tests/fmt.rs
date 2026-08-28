#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_num, kinds, parse_ok, value};
use optive::fmt::format_source;
use optive::{tokenize, TokenKind};

#[test]
fn lex_emits_line_comment_token() {
    let toks = tokenize("let x = 1 // hi\n").expect("lex");
    assert!(toks
        .iter()
        .any(|t| t.kind == TokenKind::LineComment && t.value == " hi"));
}

#[test]
fn lex_emits_block_comment_token() {
    let toks = tokenize("1 /* block */ 2").expect("lex");
    assert!(toks
        .iter()
        .any(|t| t.kind == TokenKind::BlockComment && t.value == " block "));
}

#[test]
fn kinds_still_skips_comments() {
    assert_eq!(
        kinds("let x = 1 // c\nlet y = 2"),
        vec![
            TokenKind::KwLet,
            TokenKind::Identifier,
            TokenKind::Assign,
            TokenKind::NumLiteral,
            TokenKind::KwLet,
            TokenKind::Identifier,
            TokenKind::Assign,
            TokenKind::NumLiteral,
        ]
    );
}

#[test]
fn comment_in_ast_does_not_affect_runtime() {
    assert_num(
        r"
// leading
let x = 1
/* mid */
x + 2
",
        "3",
    );
}

#[test]
fn fmt_basic_spacing_and_indent() {
    let src = "func add(a,b){\nreturn a+b\n}\n";
    let out = format_source(src).expect("fmt");
    assert_eq!(
        out,
        "\
func add(a, b) {
    return a + b
}
"
    );
}

#[test]
fn fmt_preserves_multiline_call() {
    let src = "\
foo(
    a,
    b
)
";
    let out = format_source(src).expect("fmt");
    assert!(
        out.contains("foo(\n"),
        "expected multiline call, got:\n{out}"
    );
    assert!(out.contains("    a,\n"));
    assert!(out.contains("    b,\n"));
}

#[test]
fn fmt_keeps_short_call_single_line() {
    let out = format_source("foo(a, b, c)\n").expect("fmt");
    assert_eq!(out, "foo(a, b, c)\n");
}

#[test]
fn fmt_top_level_blank_line_between_decls() {
    let src = "\
func a() {
    return 1
}
func b() {
    return 2
}
";
    let out = format_source(src).expect("fmt");
    assert!(
        out.contains("}\n\nfunc b"),
        "expected blank between top-level funcs, got:\n{out}"
    );
}

#[test]
fn fmt_no_blank_inside_func_body() {
    let src = "\
func f() {
    let x = 1

    return x
}
";
    let out = format_source(src).expect("fmt");
    assert!(
        !out.contains("1\n\n    return"),
        "body should not keep blank lines, got:\n{out}"
    );
}

#[test]
fn fmt_preserves_comments() {
    let src = "\
// hello
func f() {
    return 1
}
";
    let out = format_source(src).expect("fmt");
    assert!(out.starts_with("// hello\n"), "got:\n{out}");
}

#[test]
fn fmt_forces_if_parens_and_braces() {
    let out = format_source("if (x) { return 1 } else { return 2 }\n").expect("fmt");
    assert!(out.contains("if (x) {"));
    assert!(out.contains("} else {"));
}

#[test]
fn fmt_preserves_export_and_intern() {
    let src = "\
export func a() {
    return 1
}

intern let x = 2
";
    let out = format_source(src).expect("fmt");
    assert!(out.contains("export func a"), "got:\n{out}");
    assert!(out.contains("intern let x"), "got:\n{out}");
}

#[test]
fn fmt_roundtrip_protocol_method_empty_body() {
    let src = r"
protocol Labeled {
    func label(self) {}
}
";
    let out = format_source(src).expect("fmt");
    assert!(
        out.contains("func label(self) {}"),
        "protocol methods need empty braces to parse, got:\n{out}"
    );
    parse_ok(&out);
}

#[test]
fn fmt_roundtrip_variant_cases() {
    let src = r#"
variant Probe {
    Hit = struct { let rec }
    Miss = struct { let path let reason }
}

variant Result {
    typed Ok(value: num)
    Err = typed struct { value: text }
}
"#;
    let out = format_source(src).expect("fmt");
    assert!(
        out.contains("Hit = struct {"),
        "untyped case must keep `= struct`, got:\n{out}"
    );
    assert!(
        !out.contains("Hit(let"),
        "must not print Hit(let rec), got:\n{out}"
    );
    parse_ok(&out);
}

#[test]
fn fmt_roundtrip_quote_with_and_return_wrapper() {
    let src = "\
macro fail(msg) {
    return quote with (msg) {
        die(eval(msg))
    }
}

func get_pid() -> num : num.(_) {
    return 1
}

fail{\"hi\"}
get_pid()
";
    let out = format_source(src).expect("fmt");
    assert!(
        out.contains("quote with (msg)"),
        "quote with bindings, got:\n{out}"
    );
    assert!(
        !out.contains("quote(msg)"),
        "must not dump bindings into quote(), got:\n{out}"
    );
    assert!(
        out.contains("-> num : num.(_)"),
        "return sig -> T : wrap, got:\n{out}"
    );
    assert!(
        !out.contains(": num ->"),
        "must not print : T -> wrap, got:\n{out}"
    );
    parse_ok(&out);
}

#[test]
fn fmt_roundtrip_macro_call_args_and_fstring() {
    let src = r#"
macro show(x) {
    return quote with (x) {
        eval(x)
    }
}

func greet(name) {
    return f"hi {name}"
}

var rest = [1, 2]
show{len(rest)}
"#;
    let out = format_source(src).expect("fmt");
    assert!(
        out.contains("show{len(rest)}"),
        "macro call args must survive fmt, got:\n{out}"
    );
    assert!(
        out.contains("f\"hi {name}\""),
        "f-string must survive fmt, got:\n{out}"
    );
    parse_ok(&out);
    assert_eq!(value(&out).display_string(), "2");
}

#[test]
fn fmt_roundtrip_still_runs() {
    let src = r"
func fib(n) {
    if (n <= 1) {
        return n
    }
    return fib(n - 1) + fib(n - 2)
}
fib(5)
";
    let formatted = format_source(src).expect("fmt");
    parse_ok(&formatted);
    assert_eq!(value(&formatted).display_string(), "5");
}
