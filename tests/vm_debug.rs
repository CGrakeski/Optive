#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
use std::sync::Arc;

use optive::shared::Shared;

use optive::codegen::Generator;
use optive::debug::{self, DebugState, StepMode, StopReason};
use optive::diagnostics;
use optive::parser::Parser;
use optive::vm::Vm;

fn load(vm: &mut Vm, source: &str) {
    let file = "<test>";
    vm.source_file = file.into();
    vm.current_source = Some(Arc::from(source));
    let program = Parser::parse(source).expect("parse");
    let mut compiled = Generator::new().compile(&program).expect("compile");
    diagnostics::attach_function_sources(&mut compiled, source, file);
    vm.load_program(compiled).expect("load");
}

#[test]
fn debug_stops_on_entry() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: true,
        ..Default::default()
    });
    debug::attach(&mut vm, state.clone());
    load(
        &mut vm,
        r"
x = 1
x = x + 1
",
    );
    let done = vm.run_until_debug_break().expect("run");
    assert!(done.is_none(), "should pause at entry");
    assert_eq!(state.borrow().stop_reason, Some(StopReason::Entry));
}

#[test]
fn debug_line_breakpoint_and_continue() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    state.borrow_mut().add_line_breakpoint("", 3);
    debug::attach(&mut vm, state.clone());
    load(&mut vm, "a = 1\nb = 2\nc = a + b\n");
    let done = vm.run_until_debug_break().expect("run");
    assert!(done.is_none());
    assert_eq!(state.borrow().stop_reason, Some(StopReason::Breakpoint));
    assert_eq!(debug::line_at_pc(&vm), 3);

    let done = vm.run_until_debug_break().expect("continue");
    assert!(done.is_some());
}

#[test]
fn debug_breakpoint_builtin() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    debug::attach(&mut vm, state.clone());
    load(
        &mut vm,
        r"
use std.debug.{ breakpoint }
x = 10
breakpoint()
x = 20
",
    );
    let done = vm.run_until_debug_break().expect("run");
    assert!(done.is_none());
    assert_eq!(state.borrow().stop_reason, Some(StopReason::Explicit));

    // continue past breakpoint()
    let done = vm.run_until_debug_break().expect("continue");
    assert!(done.is_some());
}

#[test]
fn debug_step_over_advances_line() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: true,
        ..Default::default()
    });
    debug::attach(&mut vm, state.clone());
    load(&mut vm, "a = 1\nb = 2\nc = 3\n");
    assert!(vm.run_until_debug_break().unwrap().is_none());
    let line0 = debug::line_at_pc(&vm);
    let depth = vm.debug_call_depth();
    state.borrow_mut().step = Some(StepMode::Over { max_depth: depth });
    assert!(vm.run_until_debug_break().unwrap().is_none());
    let line1 = debug::line_at_pc(&vm);
    assert!(line1 >= line0);
    assert_eq!(state.borrow().stop_reason, Some(StopReason::Step));
}

#[test]
fn debug_eval_sees_globals() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    state.borrow_mut().add_line_breakpoint("", 2);
    debug::attach(&mut vm, state);
    load(&mut vm, "x = 41\ny = x + 1\n");
    assert!(vm.run_until_debug_break().unwrap().is_none());
    let v = debug::eval_in_paused_vm(&mut vm, "x").expect("eval");
    assert_eq!(v.display_string(), "41");
    // eval must not clobber pause location maps
    assert_eq!(debug::line_at_pc(&vm), 2);
}

#[test]
fn debug_stepi_advances_pc() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: true,
        ..Default::default()
    });
    debug::attach(&mut vm, state.clone());
    load(&mut vm, "a = 1\nb = 2\n");
    assert!(vm.run_until_debug_break().unwrap().is_none());
    let pc0 = vm.pc;
    state.borrow_mut().step = Some(StepMode::Insn);
    assert!(vm.run_until_debug_break().unwrap().is_none());
    assert!(vm.pc > pc0, "stepi should advance pc ({pc0} -> {})", vm.pc);
    assert_eq!(state.borrow().stop_reason, Some(StopReason::Step));
}

#[test]
fn debug_function_breakpoint_once() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    state.borrow_mut().function_breakpoints.insert("foo".into());
    debug::attach(&mut vm, state.clone());
    load(
        &mut vm,
        r"
func foo(n) {
    x = n + 1
    y = x + 1
    return y
}
foo(1)
",
    );
    assert!(vm.run_until_debug_break().unwrap().is_none());
    assert_eq!(state.borrow().stop_reason, Some(StopReason::Breakpoint));
    let name = vm.debug_current_func_name();
    assert!(
        name.as_deref()
            .is_some_and(|n| n == "foo" || n.ends_with("foo")),
        "expected foo, got {name:?}"
    );

    // continue should finish without re-breaking on every line inside foo
    let done = vm.run_until_debug_break().expect("continue");
    assert!(done.is_some(), "should not re-hit function bp each line");
}

#[test]
fn debug_finish_returns_from_call() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    state
        .borrow_mut()
        .function_breakpoints
        .insert("inner".into());
    debug::attach(&mut vm, state.clone());
    load(
        &mut vm,
        r"
func inner() {
    return 7
}
func outer() {
    return inner()
}
r = outer()
",
    );
    assert!(vm.run_until_debug_break().unwrap().is_none());
    assert_eq!(state.borrow().stop_reason, Some(StopReason::Breakpoint));
    let depth = vm.debug_call_depth();
    state.borrow_mut().step = Some(StepMode::Out {
        target_depth: depth,
    });
    assert!(vm.run_until_debug_break().unwrap().is_none());
    assert_eq!(state.borrow().stop_reason, Some(StopReason::Step));
    assert!(vm.debug_call_depth() < depth);
}

#[test]
fn debug_uncaught_stops() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    debug::attach(&mut vm, state.clone());
    load(
        &mut vm,
        r#"
throw "boom"
"#,
    );
    let done = vm.run_until_debug_break().expect("run");
    assert!(done.is_none());
    assert_eq!(state.borrow().stop_reason, Some(StopReason::Uncaught));
    assert!(state.borrow().last_uncaught.is_some());
    assert!(state.borrow().is_uncaught_stop());
}

#[test]
fn debug_source_window() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    state.borrow_mut().add_line_breakpoint("", 3);
    debug::attach(&mut vm, state);
    load(&mut vm, "a = 1\nb = 2\nc = 3\nd = 4\n");
    assert!(vm.run_until_debug_break().unwrap().is_none());
    let win = debug::format_source_window(&vm, 1);
    assert!(win.iter().any(|(n, cur, _)| *n == 3 && *cur));
    assert!(win.len() >= 2);
}

#[test]
fn debug_line_breakpoint_refires_each_loop_iteration() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    // 在累加行下断；循环每一轮都应再次停住
    state.borrow_mut().add_line_breakpoint("", 5);
    debug::attach(&mut vm, state);
    load(
        &mut vm,
        "total = 0\ni = 0\nloop {\n  if (i >= 3) { break }\n  total = total + i\n  i = i + 1\n}\n",
    );
    let mut hits = 0;
    for _ in 0..6 {
        match vm.run_until_debug_break() {
            Ok(None) => hits += 1,
            Ok(Some(_)) => break,
            Err(e) => panic!("run error: {e}"),
        }
    }
    assert_eq!(
        hits, 3,
        "line BP should fire once per iteration (got {hits})"
    );
}

#[test]
fn debug_eval_reads_locals() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    state.borrow_mut().add_line_breakpoint("", 5);
    debug::attach(&mut vm, state);
    load(
        &mut vm,
        "func f() {\n  x = 7\n  y = x + 1\n  z = y + 1\n  return z\n}\nr = f()\n",
    );
    assert!(vm.run_until_debug_break().unwrap().is_none());
    // 停在 z = y + 1 之前；y 与 x 应可被 eval 读取
    let y = debug::eval_in_paused_vm(&mut vm, "y").expect("eval y");
    assert_eq!(y.display_string(), "8");
    let x = debug::eval_in_paused_vm(&mut vm, "x").expect("eval x");
    assert_eq!(x.display_string(), "7");
    // 求值不应改变暂停位置
    assert_eq!(debug::line_at_pc(&vm), 5);
}

#[test]
fn debug_conditional_breakpoint() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    state
        .borrow_mut()
        .add_line_breakpoint_ex("", 3, Some("a == 1".into()), None);
    debug::attach(&mut vm, state.clone());
    load(&mut vm, "a = 1\nb = 2\nc = a + b\n");
    assert!(vm.run_until_debug_break().unwrap().is_none());
    assert_eq!(state.borrow().stop_reason, Some(StopReason::Breakpoint));
    assert_eq!(debug::line_at_pc(&vm), 3);

    // 条件为假：不应停下
    let mut vm2 = Vm::new();
    let state2 = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    state2
        .borrow_mut()
        .add_line_breakpoint_ex("", 3, Some("a == 0".into()), None);
    debug::attach(&mut vm2, state2);
    load(&mut vm2, "a = 1\nb = 2\nc = a + b\n");
    let done = vm2.run_until_debug_break().expect("run");
    assert!(done.is_some(), "false condition must not stop");
}

#[test]
fn debug_log_breakpoint_continues() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    state
        .borrow_mut()
        .add_line_breakpoint_ex("", 2, None, Some("x".into()));
    debug::attach(&mut vm, state);
    load(&mut vm, "x = 9\ny = x + 1\n");
    let done = vm.run_until_debug_break().expect("run");
    assert!(done.is_some(), "log breakpoint should not stop");
}

#[test]
fn debug_deep_set_index() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    state.borrow_mut().add_line_breakpoint("", 2);
    debug::attach(&mut vm, state);
    load(&mut vm, "a = [1, 2, 3]\nb = a[0]\n");
    assert!(vm.run_until_debug_break().unwrap().is_none());
    debug::debug_set(&mut vm, "a[1]", "99").expect("set");
    let v = debug::eval_in_paused_vm(&mut vm, "a[1]").expect("eval");
    assert_eq!(v.display_string(), "99");
}

#[test]
fn debug_parse_break_spec() {
    let (f, l, c, g) = debug::parse_break_spec("12 if n > 0").unwrap();
    assert!(f.is_empty());
    assert_eq!(l, 12);
    assert_eq!(c.as_deref(), Some("n > 0"));
    assert!(g.is_none());
    let (_, _, c2, g2) = debug::parse_break_spec("foo.tive:3 log x").unwrap();
    assert_eq!(c2, None);
    assert_eq!(g2.as_deref(), Some("x"));
    let (_, _, c3, g3) = debug::parse_break_spec("8 if n > 0 log n").unwrap();
    assert_eq!(c3.as_deref(), Some("n > 0"));
    assert_eq!(g3.as_deref(), Some("n"));
    let (_, _, c4, g4) = debug::parse_break_spec("8 log n if n > 0").unwrap();
    assert_eq!(c4.as_deref(), Some("n > 0"));
    assert_eq!(g4.as_deref(), Some("n"));
}

#[test]
fn debug_breakpoint_hits_go_task_by_default() {
    let mut vm = Vm::new();
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    debug::attach(&mut vm, state.clone());
    load(
        &mut vm,
        r"
use std.debug.{ breakpoint }
let t = go do {
  breakpoint()
  return 1
}
await t
",
    );
    match vm.run_until_debug_break() {
        Ok(None) => {}
        Ok(Some(v)) => panic!("expected pause, finished with {}", v.display_string()),
        Err(e) => panic!("run error: {}", e.message()),
    }
    assert_eq!(
        state.borrow().stop_reason,
        Some(StopReason::Explicit),
        "uncaught={:?}",
        state.borrow().last_uncaught
    );
}
