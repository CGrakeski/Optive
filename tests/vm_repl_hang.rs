#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use optive::{run_source_in_vm, vm::Vm};

#[test]
fn repl_undefined_name_in_callee_should_error() {
    let mut vm = Vm::new();
    run_source_in_vm(&mut vm, "func a() { b() }", "<repl>").expect("define a");
    run_source_in_vm(&mut vm, "func b() { c }", "<repl>").expect("define b");
    let err = run_source_in_vm(&mut vm, "a()", "<repl>").expect_err("should error");
    let msg = err.to_string();
    assert!(
        msg.contains("undefined name") || msg.contains('c'),
        "unexpected error: {msg}"
    );
}

#[test]
fn repl_error_shows_call_stack_and_context() {
    let mut vm = Vm::new();
    run_source_in_vm(&mut vm, "func a() { b() }", "<repl>").expect("define a");
    run_source_in_vm(&mut vm, "func b() { c }", "<repl>").expect("define b");
    let err = run_source_in_vm(&mut vm, "a()", "<repl>").expect_err("should error");
    let msg = err.to_string();
    assert!(msg.contains("Traceback"), "missing traceback:\n{msg}");
    assert!(msg.contains("in a"), "missing frame a:\n{msg}");
    assert!(msg.contains("in b"), "missing frame b:\n{msg}");
    assert!(msg.contains("b()"), "missing call site context:\n{msg}");
    assert!(
        msg.contains("undefined name: c"),
        "missing error message:\n{msg}"
    );
    let lines: Vec<&str> = msg.lines().collect();
    let caret = lines.iter().rev().find(|l| l.contains('^')).unwrap();
    let src = lines
        .iter()
        .rev()
        .find(|l| l.contains("func b()"))
        .unwrap();
    // `c` in `func b() { c }` is column 12 (1-based); traceback indents with 4 spaces.
    const C_COLUMN: usize = 12;
    assert_eq!(
        caret.find('^'),
        Some(4 + (C_COLUMN - 1)),
        "caret should point at c (column {C_COLUMN}):\n{msg}"
    );
    assert_eq!(
        src.chars().nth(4 + (C_COLUMN - 1)),
        Some('c'),
        "expected source column {C_COLUMN} to be 'c':\n{msg}"
    );
}

#[test]
fn repl_assignment_does_not_echo_stale_stack() {
    let mut vm = Vm::new();
    let v = run_source_in_vm(&mut vm, "1", "<repl>").unwrap();
    assert_eq!(v.display_string(), "1");
    let v = run_source_in_vm(&mut vm, "a = 1", "<repl>").unwrap();
    assert!(
        matches!(v, optive::value::Value::None),
        "assignment should not echo prior stack top, got {}",
        v.display_string()
    );
    let v = run_source_in_vm(&mut vm, "a = 2", "<repl>").unwrap();
    assert!(
        matches!(v, optive::value::Value::None),
        "assignment should yield none, got {}",
        v.display_string()
    );
    let v = run_source_in_vm(&mut vm, "a", "<repl>").unwrap();
    assert_eq!(v.display_string(), "2");
}
