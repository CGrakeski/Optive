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
    assert!(
        idle_ok,
        "expected bare Suspend in select idle, got {body:?}"
    );
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
fn is_prime_fuses_trial_loop_and_is_lightweight() {
    let prog = compile(
        r"
func is_prime(n) {
  if (n < 2) { return false }
  if (n == 2) { return true }
  if (n % 2 == 0) { return false }
  var d = 3
  loop {
    if (d * d > n) { break }
    if (n % d == 0) { return false }
    d = d + 2
  }
  return true
}
",
    )
    .expect("compile");
    let func = prog.functions.get("is_prime").expect("is_prime");
    assert!(
        func.lightweight(),
        "is_prime should be a lightweight hot-callable (BindFast var is not a name-map)"
    );
    let body = func.body.as_ref();
    assert!(
        body.iter()
            .any(|ins| matches!(ins, Instruction::LoadFastSqrGt { .. })),
        "expected d*d > n fusion, got {body:?}"
    );
    assert!(
        body.iter()
            .any(|ins| matches!(ins, Instruction::LoadFastModEq0 { .. })),
        "expected n % d == 0 fusion, got {body:?}"
    );
    assert!(
        body.iter()
            .any(|ins| matches!(ins, Instruction::LoadFastAddImmStore { slot: 1, imm: 2 })),
        "expected d = d + 2 fusion, got {body:?}"
    );
    verify_stack_balance(body).expect("is_prime stack-balanced");
}

#[test]
fn fused_prime_helpers_match_trial_division() {
    common::assert_num(
        r"
func is_prime(n) {
  if (n < 2) { return false }
  if (n == 2) { return true }
  if (n % 2 == 0) { return false }
  var d = 3
  loop {
    if (d * d > n) { break }
    if (n % d == 0) { return false }
    d = d + 2
  }
  return true
}
func count_primes() {
  var total = 0
  var n = 2
  loop {
    if (n > 100) { break }
    if (is_prime(n)) { total = total + 1 }
    n = n + 1
  }
  return total
}
count_primes()
",
        "25",
    );
}

#[test]
fn const_local_still_rejects_assign() {
    common::run_err(
        r"
func f() {
  const let x = 1
  x = 2
  return x
}
f()
",
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

#[test]
fn script_unescaped_arith_fuses_add_imm_store() {
    let prog = compile(
        r"
let sum = 0
loop (10) {
    sum = sum + 1
}
sum
",
    )
    .expect("compile");
    assert!(
        prog.script_frame_slots > 0,
        "unescaped script let should open a script frame"
    );
    assert!(
        prog.code
            .iter()
            .any(|ins| matches!(ins, Instruction::LoadFastAddImmStore { imm: 1, .. })),
        "expected LoadFastAddImmStore on script body, got {:?}",
        prog.code
    );
    verify_stack_balance(&prog.code).expect("script stack-balanced");
}

#[test]
fn script_add_store_fuses_two_fast_locals() {
    let prog = compile(
        r"
let a = 1
let b = 2
a = a + b
a
",
    )
    .expect("compile");
    assert!(
        prog.code
            .iter()
            .any(|ins| matches!(ins, Instruction::LoadFastAddStore { .. })),
        "expected LoadFastAddStore, got {:?}",
        prog.code
    );
}

#[test]
fn escaped_script_name_stays_global() {
    let prog = compile(
        r"
let n = 1
func f() { return n }
n = 2
f()
",
    )
    .expect("compile");
    assert_eq!(prog.script_frame_slots, 0);
    assert!(
        !prog
            .code
            .iter()
            .any(|ins| matches!(ins, Instruction::LoadFast(_) | Instruction::StoreFast(_))),
        "escaped n must not be a script fast local, got {:?}",
        prog.code
    );
}
