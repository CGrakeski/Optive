#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! 编译期栈平衡：表达式契约与 Suspend 配对（零运行时开销的 verifier）。

mod common;

use optive::opcode::Instruction;
use optive::stack_effect::verify_stack_balance;
use optive::{compile, value::Value};

fn func_body(source: &str, name: &str) -> Vec<Instruction> {
    let prog = compile(source).expect("compile");
    prog.functions
        .get(name)
        .unwrap_or_else(|| panic!("missing function {name}"))
        .body
        .as_ref()
        .clone()
}

#[test]
fn suspend_stmt_emits_push_none_before_pop() {
    let body = func_body(
        r"
func f() {
  suspend
  return 1
}
",
        "f",
    );
    let mut saw = false;
    for i in 0..body.len().saturating_sub(2) {
        if matches!(body[i], Instruction::Suspend)
            && matches!(body[i + 1], Instruction::Push(Value::None))
            && matches!(body[i + 2], Instruction::Pop)
        {
            saw = true;
            break;
        }
    }
    assert!(
        saw,
        "expected Suspend; Push(None); Pop in function body, got {body:?}"
    );
    verify_stack_balance(&body).expect("function body stack-balanced");
}

#[test]
fn select_idle_uses_bare_suspend_without_push() {
    let body = func_body(
        r"
func f(ch) {
  select {
    case ch.recv() as x {
      return x
    }
  }
}
",
        "f",
    );
    // idle 路径：裸 Suspend 后应是 Goto，不得夹 Push(None)
    let mut idle_ok = false;
    for i in 0..body.len().saturating_sub(1) {
        if matches!(body[i], Instruction::Suspend) {
            assert!(
                !matches!(body[i + 1], Instruction::Push(Value::None)),
                "select idle must not Push(None) after Suspend"
            );
            idle_ok = true;
        }
    }
    assert!(idle_ok, "expected bare Suspend in select idle, got {body:?}");
    verify_stack_balance(&body).expect("select body stack-balanced");
}

#[test]
fn try_as_value_keeps_body_on_success() {
    common::assert_num(
        r"
func f() {
  try {
    42
  } catch (e: Exception) {
    0
  }
}
f()
",
        "42",
    );
}

#[test]
fn try_as_value_else_still_wins_on_success() {
    common::assert_num(
        r"
func f() {
  try {
    42
  } catch (e: Exception) {
    0
  } else {
    7
  }
}
f()
",
        "7",
    );
}

#[test]
fn module_top_level_passes_stack_verify() {
    let prog = compile(
        r"
let x = 1
suspend
x + 1
",
    )
    .expect("compile");
    verify_stack_balance(&prog.code).expect("module code stack-balanced");
}
