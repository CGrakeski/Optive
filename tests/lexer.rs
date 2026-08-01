mod common;

use common::kinds;
use optive::{tokenize, TokenKind};

macro_rules! assert_kinds {
    ($src:expr, $($k:ident),+ $(,)?) => {
        assert_eq!(
            kinds($src),
            vec![$(TokenKind::$k),+],
            "source: {}",
            $src
        );
    };
}

#[test]
fn lex_let_var_assign() {
    assert_kinds!("let x = 1", KwLet, Identifier, Assign, NumLiteral);
}

#[test]
fn lex_var_keyword() {
    assert_kinds!("var y = 2", KwVar, Identifier, Assign, NumLiteral);
}

#[test]
fn lex_const() {
    assert_kinds!("const let z = 3", KwConst, KwLet, Identifier, Assign, NumLiteral);
}

#[test]
fn lex_func() {
    assert_kinds!("func f()", KwFunc, Identifier, LParen, RParen);
}

#[test]
fn lex_if_else() {
    assert_kinds!("if else elif", KwIf, KwElse, KwElif);
}

#[test]
fn lex_while_for_in() {
    assert_kinds!("while for in", KwWhile, KwFor, KwIn);
}

#[test]
fn lex_struct_typed() {
    assert_kinds!("struct typed", KwStruct, KwTyped);
}

#[test]
fn lex_match_case() {
    assert_kinds!("match case", KwMatch, KwCase);
}

#[test]
fn lex_try_catch_throw() {
    assert_kinds!("try catch throw", KwTry, KwCatch, KwThrow);
}

#[test]
fn lex_and_or_not() {
    assert_kinds!("and or not", KwAnd, KwOr, KwNot);
}

#[test]
fn lex_import_use_as() {
    assert_kinds!("import use as", KwImport, KwUse, KwAs);
}

#[test]
fn lex_bytes_literal() {
    assert_kinds!(r#"b"hi""#, BytesLiteral);
}

#[test]
fn lex_list_is_identifier() {
    assert_kinds!("list", Identifier);
}

#[test]
fn lex_do_macro_quote() {
    assert_kinds!("do macro quote", KwDo, KwMacro, KwQuote);
}

#[test]
fn lex_operators_arithmetic() {
    assert_kinds!("+ - * / %", Plus, Minus, Star, Slash, Percent);
}

#[test]
fn lex_operators_bitwise() {
    assert_kinds!("& | ^ ~ << >>", Ampersand, Bar, Caret, Tilde, LtLt, GtGt);
}

#[test]
fn lex_operators_compare() {
    assert_kinds!("== != < > <= >=", EqEq, Ne, Lt, Gt, Le, Ge);
}

#[test]
fn lex_operators_assign_arrow() {
    assert_kinds!("= -> =>", Assign, Arrow, FatArrow);
}

#[test]
fn lex_pipe_and_bar() {
    assert_kinds!("|> |", Pipe, Bar);
}

#[test]
fn lex_colon_coloncolon() {
    assert_kinds!(": ::", Colon, ColonColon);
}

#[test]
fn lex_brackets() {
    assert_kinds!("() {} []", LParen, RParen, LBrace, RBrace, LBracket, RBracket);
}

#[test]
fn lex_placeholder() {
    assert_kinds!("_", Placeholder);
}

#[test]
fn lex_ellipsis() {
    assert_kinds!("...", Ellipsis);
}

#[test]
fn lex_true_false_none_as_ident() {
    let k = kinds("true false none");
    assert_eq!(k.len(), 3);
    assert!(k.iter().all(|t| *t == TokenKind::Identifier));
}

#[test]
fn lex_integer_positive() {
    assert_kinds!("42", NumLiteral);
    assert_eq!(common::tokens("42")[0].value, "42");
}

#[test]
fn lex_integer_negative() {
    assert_kinds!("-7", NumLiteral);
}

#[test]
fn lex_decimal() {
    assert_kinds!("3.14", NumLiteral);
}

#[test]
fn lex_decimal_leading_dot() {
    assert_kinds!(".5", NumLiteral);
}

#[test]
fn lex_scientific_notation() {
    assert_kinds!("1.5e1", NumLiteral);
    assert_kinds!("1e-3", NumLiteral);
}

#[test]
fn lex_string_empty() {
    assert_kinds!(r#""""#, StringLiteral);
}

#[test]
fn lex_string_with_escapes() {
    let t = common::tokens(r#""a\n\t""#);
    assert_eq!(t[0].kind, TokenKind::StringLiteral);
    assert_eq!(t[0].value, "a\n\t");
}

#[test]
fn lex_string_hex_escape() {
    let t = common::tokens(r#""\x41\x42\x00""#);
    assert_eq!(t[0].kind, TokenKind::StringLiteral);
    assert_eq!(t[0].value, "AB\0");
}

#[test]
fn lex_string_hex_escape_invalid() {
    let err = optive::tokenize(r#""\xGG""#).expect_err("expected lex error");
    let msg = err.to_string();
    assert!(msg.contains("\\x") || msg.contains("invalid"), "msg={msg}");
}

#[test]
fn lex_string_concat_ops() {
    assert_kinds!(r###""hi" + "!""###, StringLiteral, Plus, StringLiteral);
}

#[test]
fn lex_line_comment() {
    assert_kinds!(
        "let x = 1 // comment\nlet y = 2",
        KwLet,
        Identifier,
        Assign,
        NumLiteral,
        KwLet,
        Identifier,
        Assign,
        NumLiteral
    );
}

#[test]
fn lex_block_comment() {
    assert_kinds!(
        "let a = 1 /* block */ + 2",
        KwLet,
        Identifier,
        Assign,
        NumLiteral,
        Plus,
        NumLiteral
    );
}

#[test]
fn lex_block_comment_multiline() {
    assert_kinds!("1 /* a\nb */ 2", NumLiteral, NumLiteral);
}

#[test]
fn lex_whitespace_separators() {
    assert_kinds!("  let\t x  =  1  ", KwLet, Identifier, Assign, NumLiteral);
}

#[test]
fn lex_newline_present_in_raw_tokens() {
    let all = common::tokens("1\n+\n2");
    assert!(all.iter().any(|t| t.kind == TokenKind::Newline));
}

#[test]
fn lex_unicode_identifier() {
    assert_kinds!("变量", Identifier);
}

#[test]
fn lex_underscore_ident() {
    assert_kinds!("_foo", Identifier);
}

#[test]
fn lex_friend_func() {
    assert_kinds!("friend func", KwFriend, KwFunc);
}

#[test]
fn lex_outside_overload() {
    assert_kinds!("outside overload", KwOutside, KwOverload);
}

#[test]
fn lex_with_make() {
    assert_kinds!("with make", KwWith, KwMake);
}

#[test]
fn lex_intern_export() {
    assert_kinds!("intern export", KwIntern, KwExport);
}

#[test]
fn lex_del() {
    assert_kinds!("del", KwDel);
}

#[test]
fn lex_bang_operator() {
    assert_kinds!("!", Bang);
}

#[test]
fn lex_comma_dot() {
    assert_kinds!(",.", Comma, Dot);
}

#[test]
fn lex_multiple_statements_newlines() {
    assert_kinds!(
        "let a = 1\nlet b = 2",
        KwLet,
        Identifier,
        Assign,
        NumLiteral,
        KwLet,
        Identifier,
        Assign,
        NumLiteral
    );
}

#[test]
fn lex_err_unterminated_string() {
    assert!(tokenize("\"hello").is_err());
}

#[test]
fn lex_err_unclosed_string_newline() {
    assert!(tokenize("\"hello\n").is_err());
}

#[test]
fn lex_err_unknown_char() {
    assert!(tokenize("@").is_err());
}

#[test]
fn lex_err_unknown_char_in_expr() {
    assert!(tokenize("1 @ 2").is_err());
}

#[test]
fn lex_longest_match_arrow() {
    assert_kinds!("->", Arrow);
    assert_kinds!("=>", FatArrow);
}

#[test]
fn lex_longest_match_pipe() {
    assert_kinds!("|>", Pipe);
}

#[test]
fn lex_longest_match_coloncolon() {
    assert_kinds!("::", ColonColon);
}
