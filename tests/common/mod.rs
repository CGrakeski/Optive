//! Shared helpers for integration tests.
#![allow(dead_code)]

use optive::{eval_bool, eval_num, eval_text, parse_program, run_source, tokenize, TokenKind};
use optive::value::Value;

pub fn tokens(source: &str) -> Vec<optive::Token> {
    tokenize(source).expect("lex error")
}

pub fn kinds(source: &str) -> Vec<TokenKind> {
    tokens(source)
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::End
                    | TokenKind::Newline
                    | TokenKind::LineComment
                    | TokenKind::BlockComment
            )
        })
        .map(|t| t.kind)
        .collect()
}

pub fn num(source: &str) -> String {
    eval_num(source).expect("eval num")
}

pub fn bool_val(source: &str) -> bool {
    eval_bool(source).expect("eval bool")
}

pub fn text(source: &str) -> String {
    eval_text(source).expect("eval text")
}

pub fn value(source: &str) -> Value {
    run_source(source).expect("run")
}

pub fn parse_ok(source: &str) {
    parse_program(source).expect("parse");
}

pub fn parse_err(source: &str) {
    assert!(parse_program(source).is_err(), "expected parse error for: {source}");
}

pub fn run_err(source: &str) {
    assert!(run_source(source).is_err(), "expected runtime error for: {source}");
}

/// 用给定能力集跑源码（沙箱测试用）。
pub fn run_with_caps(
    source: &str,
    caps: optive::caps::Capabilities,
) -> Result<Value, optive::RuntimeError> {
    let mut vm = optive::vm::Vm::new();
    vm.caps = caps;
    optive::run_source_in_vm(&mut vm, source, "<test>")
}

/// 断言给定源码在指定能力集下抛错，且消息包含 `needle`。
pub fn assert_caps_err(source: &str, caps: optive::caps::Capabilities, needle: &str) {
    match run_with_caps(source, caps) {
        Ok(v) => panic!("expected error containing '{needle}', got ok value: {}", v.display_string()),
        Err(e) => {
            let msg = e.message();
            assert!(
                msg.contains(needle),
                "expected error containing '{needle}', got: {msg}"
            );
        }
    }
}

pub fn assert_num(source: &str, expected: &str) {
    assert_eq!(num(source), expected, "source: {source}");
}

pub fn assert_bool(source: &str, expected: bool) {
    assert_eq!(bool_val(source), expected, "source: {source}");
}

pub fn assert_text(source: &str, expected: &str) {
    assert_eq!(text(source), expected, "source: {source}");
}

pub fn assert_list(source: &str, expected: &str) {
    assert_eq!(
        value(source).display_string(),
        expected,
        "source: {source}"
    );
}
