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

/// 固定 workers=1（M:1）的数值断言：协作顺序/挂起让出类断言专用，
/// 不受 `OPTIVE_WORKERS` 环境影响（M:N 真并行下此类顺序保证不成立）。
pub fn assert_num_w1(source: &str, expected: &str) {
    let mut vm = optive::vm::Vm::with_workers(1);
    let v = optive::run_source_in_vm(&mut vm, source, "<test>").expect("run");
    match v {
        Value::Num(n) => assert_eq!(n.to_string(), expected, "source: {source}"),
        other => panic!("expected num {expected}, got {other:?}; source: {source}"),
    }
}

/// 固定 workers=1 跑源码（M:1 协作语义）。
pub fn value_w1(source: &str) -> Value {
    let mut vm = optive::vm::Vm::with_workers(1);
    optive::run_source_in_vm(&mut vm, source, "<test>").expect("run")
}

pub fn assert_text_w1(source: &str, expected: &str) {
    let v = value_w1(source);
    match &v {
        Value::Text(s) => assert_eq!(s, expected, "source: {source}"),
        Value::TypeRef(s) => assert_eq!(s, expected, "source: {source}"),
        Value::TypeSpec(_) => assert_eq!(v.display_string(), expected, "source: {source}"),
        other => panic!(
            "expected text {expected:?}, got {}; source: {source}",
            other.display_string()
        ),
    }
}

pub fn assert_bool_w1(source: &str, expected: bool) {
    match value_w1(source) {
        Value::Bool(b) => assert_eq!(b, expected, "source: {source}"),
        other => panic!(
            "expected bool {expected}, got {}; source: {source}",
            other.display_string()
        ),
    }
}

pub fn run_err_w1(source: &str) {
    let mut vm = optive::vm::Vm::with_workers(1);
    assert!(
        optive::run_source_in_vm(&mut vm, source, "<test>").is_err(),
        "expected runtime error for: {source}"
    );
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
