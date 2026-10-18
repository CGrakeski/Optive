#![allow(clippy::expect_used, clippy::panic)]

mod common;

#[test]
fn comments_preserve_operator_and_member_token_context() {
    for comment in ["/**/", "/* 中文 */", "/* first\nsecond */"] {
        let source = format!("10{comment}-3");
        common::assert_num(&source, "7");
        let spans = optive::lexer::Lexer::new(&source).tokenize_spans();
        assert!(spans
            .iter()
            .any(|(_, _, kind)| *kind == optive::TokenKind::Minus));
        let member = format!("(10, 20){comment}.1");
        assert!(common::kinds(&member).contains(&optive::TokenKind::Dot));
    }
}

#[test]
fn range_terminates_when_the_next_step_exceeds_i64_bounds() {
    for (start, stop, step) in [
        (i64::MAX - 1, i64::MAX, 2),
        (i64::MIN + 1, i64::MIN, -2),
        (0, i64::MIN, i64::MIN),
    ] {
        let mut state = optive::value::IteratorState::from_range(start, stop, step);
        let mut vm = optive::vm::Vm::with_workers(1);
        assert_eq!(
            state
                .next_value(&mut vm)
                .expect("first")
                .expect("value")
                .display_string(),
            start.to_string()
        );
        assert!(state.next_value(&mut vm).expect("exhausted").is_none());
        assert!(state
            .next_value(&mut vm)
            .expect("still exhausted")
            .is_none());
        common::assert_list(
            &format!("std.iter.to_list(std.iter.take(std.math.range({start}, {stop}, {step}), 3))"),
            &format!("[{start}]"),
        );
    }
}

#[test]
fn enum_numbering_crosses_the_small_integer_boundary() {
    common::assert_num(
        "enum E { A = 9223372036854775807 B }\nE.B.__value__",
        "9223372036854775808",
    );
    common::assert_num(
        "enum E { A = 9223372036854775808 B }\nE.B.__value__",
        "9223372036854775809",
    );
    common::assert_num(
        "enum E { A = -(-9223372036854775808) B }\nE.B.__value__",
        "9223372036854775809",
    );
    common::run_err("enum E { A = 1.5 }");
}

#[test]
fn divmod_returns_an_integer_quotient_consistent_with_remainder() {
    for (a, b, expected) in [
        (7, 3, "[2, 1]"),
        (-7, 3, "[-3, 2]"),
        (7, -3, "[-3, -2]"),
        (-7, -3, "[2, -1]"),
    ] {
        common::assert_list(&format!("std.math.divmod({a}, {b})"), expected);
        common::assert_bool(
            &format!("let qr = std.math.divmod({a}, {b})\nqr[0] * ({b}) + qr[1] == ({a})"),
            true,
        );
    }
    common::assert_list(
        "std.math.divmod(-9223372036854775808, -1)",
        "[9223372036854775808, 0]",
    );
    common::run_err("std.math.divmod(1, 0)");
    common::run_err("std.math.divmod(1.5, 2)");
}

#[test]
fn smallest_integer_negation_and_absolute_value_promote_to_bigint() {
    let min = optive::value::Value::Num(optive::value::Num::Small(i64::MIN));
    assert_eq!(
        min.neg().expect("negate").display_string(),
        "9223372036854775808"
    );
    assert_eq!(
        optive::value::Num::Small(i64::MIN).abs_num().to_string(),
        "9223372036854775808"
    );
    for source in [
        "func negate(x) { return -x }\nnegate(-9223372036854775808)",
        "std.math.abs(-9223372036854775808)",
        "const x = -(-9223372036854775808)\nx",
    ] {
        common::assert_num(source, "9223372036854775808");
    }
    let diagnostics = optive::lsp::diagnostics("1 / -(-9223372036854775808)", "test.tive");
    assert!(diagnostics.is_empty());
}

#[test]
fn lsp_string_ranges_use_source_spelling_and_utf16() {
    let tokens = optive::lsp::semantic_tokens(r#""\u0041""#);
    assert_eq!(tokens["data"], serde_json::json!([0, 0, 8, 4, 0]));
    let tokens = optive::lsp::semantic_tokens("\"\"\"😀\r\nx\"\"\" let n = 1");
    assert_eq!(
        tokens["data"],
        serde_json::json!([
            0, 0, 5, 4, 0, 1, 0, 4, 4, 0, 0, 5, 3, 0, 0, 0, 4, 1, 1, 0, 0, 2, 1, 6, 0, 0, 2, 1, 3,
            0
        ])
    );
    let source = "let x=\"😀\"";
    let edits = optive::lsp::formatting(source);
    assert_eq!(
        edits[0]["range"]["end"]["character"],
        source.encode_utf16().count()
    );
}
