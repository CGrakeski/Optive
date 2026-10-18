#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{assert_bool_w1 as assert_bool, assert_num_w1 as assert_num};

#[test]
fn descriptor_macro_parses_and_formats_without_parentheses() {
    let source = "func work() cancel_flag{a} display_as(\"worker\") -> int { if (a.requested()) { return 0 }\nreturn 42 }";
    optive::parse_program(source).unwrap();
    let formatted = optive::fmt::format_source(source).unwrap();
    assert!(formatted.contains("cancel_flag{a} display_as(\"worker\") -> int"));
    optive::parse_program(&formatted).unwrap();
    assert!(optive::semantic::analyze_source(source).is_empty());
}

#[test]
fn invalid_macro_bindings_are_rejected() {
    for source in [
        "func work() cancel_flag{} {}",
        "func work() cancel_flag{a, b} {}",
        "func work() cancel_flag{\"a\"} {}",
        "func work() cancel_flag{a + b} {}",
        "func work() cancel_flag(a) {}",
        "func work() cancel_flag{a} cancel_flag{b} {}",
        "const func work() cancel_flag{a} {}",
    ] {
        assert!(optive::parse_program(source).is_err(), "{source}");
    }
}

#[test]
fn binding_cannot_replace_parameters_or_same_scope_declarations() {
    for source in [
        "func work(a) cancel_flag{a} { return 42 }",
        "func work() cancel_flag{a} { let a = 42 }",
        "func work() cancel_flag{a} { var a = 42 }",
        "func work() cancel_flag{a} { let (a, b) = (1, 2) }",
        "func work() cancel_flag{a} { func a() {} }",
    ] {
        assert!(optive::run_source(source).is_err(), "{source}");
        assert!(
            !optive::semantic::analyze_source(source).is_empty(),
            "{source}"
        );
    }
}

#[test]
fn ordinary_call_has_no_requested_cancellation() {
    assert_num(
        "func work() cancel_flag{a} -> num { if (a.requested()) { return 0 }\nreturn 42 }\nwork()",
        "42",
    );
}

#[test]
fn cancellation_before_first_execution_is_delivered_to_function() {
    assert_num("func work() cancel_flag{a} -> num { if (a.requested()) { return 0 }\nreturn 42 }\nlet t = go work()\nt.cancel()\nawait t", "0");
}

#[test]
fn task_can_ignore_a_request_and_return_normally() {
    assert_num("func work() cancel_flag{a} { if (a.requested()) { suspend }\nreturn 42 }\nlet t = go work()\nt.cancel()\nawait t\nt.value()", "42");
}

#[test]
fn requests_are_sticky_across_suspension_and_repeat_queries() {
    assert_bool("func work() cancel_flag{a} { suspend\nlet first = a.requested()\nsuspend\nreturn first and a.requested() }\nlet t = go work()\nsuspend\nt.cancel()\nt.cancel()\nawait t", true);
}

#[test]
fn separate_tasks_have_separate_flags() {
    assert_num("func work() cancel_flag{a} { if (a.requested()) { return 0 }\nreturn 42 }\nlet first = go work()\nlet second = go work()\nfirst.cancel()\n(await first) + (await second)", "42");
}

#[test]
fn ordinary_nested_calls_share_current_task_signal() {
    assert_bool("func helper() cancel_flag{b} { return b.requested() }\nfunc work() cancel_flag{a} { return a.requested() and helper() }\nlet t = go work()\nt.cancel()\nawait t", true);
}

#[test]
fn nested_closure_captures_flag_without_an_outer_binding() {
    assert_bool("func work() cancel_flag{a} { func read() { return a.requested() }\nreturn read() }\nlet t = go work()\nt.cancel()\nawait t", true);
}

#[test]
fn unannotated_tasks_still_cancel() {
    assert!(common::run_with_caps(
        "func work() { return 42 }\nlet t = go work()\nt.cancel()\nawait t",
        optive::caps::Capabilities::default()
    )
    .is_err());
}

#[test]
fn flag_queries_reject_arguments_and_binding_is_immutable() {
    assert!(optive::run_source("func work() cancel_flag{a} { a.requested(1) }\nwork()").is_err());
    assert!(optive::run_source("func work() cancel_flag{a} { a = none }\nwork()").is_err());
}

#[test]
fn cancellation_polling_works_on_multiple_workers() {
    let mut vm = optive::vm::Vm::with_workers(4);
    let value = optive::run_source_in_vm(
        &mut vm,
        r"
let ready = Channel()
func work() cancel_flag{a} {
    ready.send(1)
    while (not a.requested()) { suspend }
    return 7
}
let t = go work()
ready.recv()
t.cancel()
await t
",
        "<cancel-flag-mn>",
    )
    .unwrap();
    assert_eq!(value.display_string(), "7");
}

#[test]
fn lowering_is_idempotent_and_source_ast_remains_unchanged() {
    let source = "func work() cancel_flag{a} { return a.requested() }";
    let parsed = optive::parse_program(source).unwrap();
    let once = optive::frontend::function_descriptors::lower_program(&parsed).unwrap();
    let twice = optive::frontend::function_descriptors::lower_program(&once).unwrap();
    assert_eq!(
        optive::fmt::format_program(&once),
        optive::fmt::format_program(&twice)
    );
    assert!(!optive::fmt::format_program(&parsed).contains("__cancel_flag__"));
}

#[test]
fn generic_specialization_keeps_cancellation_policy() {
    assert_num("func work[T](x: T) cancel_flag{a} { if (a.requested()) { return 0 }\nreturn x }\nlet t = go work[num](42)\nt.cancel()\nawait t", "0");
}

#[test]
fn lifting_and_display_descriptor_preserve_the_macro() {
    assert_num("func outer() { if (false) { func work() outside(1) cancel_flag{a} display_as(\"worker\") { if (a.requested()) { return 0 }\nreturn 42 } } }\nlet t = go work()\nt.cancel()\nawait t", "0");
}

#[test]
fn cancelled_task_still_performs_normal_return_cleanup() {
    assert_num("var cleaned = 0\nfunc work() cancel_flag{a} { defer { cleaned = 1 }\nif (a.requested()) { return 7 }\nreturn 42 }\nlet t = go work()\nt.cancel()\nlet result = await t\nresult + cleaned", "8");
}

#[test]
fn cancellation_does_not_interrupt_cooperative_sleep() {
    assert_bool("func work() cancel_flag{a} { std.time.sleep(0.001)\nreturn a.requested() }\nlet t = go work()\nt.cancel()\nawait t", true);
}

#[test]
fn new_child_task_does_not_inherit_parent_request() {
    assert_bool("func child() cancel_flag{b} { return b.requested() }\nfunc work() cancel_flag{a} { let t = go child()\nreturn a.requested() and not (await t) }\nlet t = go work()\nt.cancel()\nawait t", true);
}

#[test]
fn nested_cooperative_scope_does_not_leak_into_ordinary_caller() {
    assert_num("var stopped = 0\nfunc helper() cancel_flag{a} { while (not a.requested()) { suspend }\nreturn 7 }\nfunc work() { helper()\nsuspend\nreturn 42 }\nlet t = go work()\nsuspend\nt.cancel()\ntry { await t } catch (e: Cancelled) { stopped = 1 }\nstopped", "1");
}

#[test]
fn editor_recognizes_binding_as_a_local_symbol() {
    let source = "func work() cancel_flag{a} {\n    return a.requested()\n}\n";
    assert!(optive::lsp::diagnostics(source, "work.tive").is_empty());
    let completion = optive::lsp::completion(source, 1, 11);
    assert!(completion
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["label"] == "a"));
}

#[test]
fn resumed_task_exception_waits_for_await_instead_of_leaking_to_spinner() {
    for workers in [1, 4] {
        let mut vm = optive::vm::Vm::with_workers(workers);
        let value = optive::run_source_in_vm(
            &mut vm,
            r#"
func work() cancel_flag{stop} => bool {
    while (not stop.requested()) { suspend }
    throw Cancelled("accepted cancellation")
}
let t = go work()
suspend
t.cancel()
while (not t.done()) { suspend }
var caught = false
try { await t } catch (e: Cancelled) { caught = true }
caught
"#,
            "<cancel-exception-isolation>",
        )
        .unwrap();
        assert_eq!(value.display_string(), "true");
    }
}

#[test]
fn suspended_catch_keeps_its_exception_across_other_tasks_and_gc() {
    for workers in [1, 4] {
        let mut vm = optive::vm::Vm::with_workers(workers);
        let value = optive::run_source_in_vm(
            &mut vm,
            r#"
func work() {
    try { throw ValueError("original") } catch (e: ValueError) {
        suspend
        gc()
        throw e
    }
}
func other() { suspend
    throw RuntimeError("unrelated")
}
let t = go work()
let sibling = go other()
while (not t.done() or not sibling.done()) { suspend }
var caught = false
try { await t } catch (e: ValueError) { caught = e.message == "original" }
caught
"#,
            "<suspended-catch-isolation>",
        )
        .unwrap();
        assert_eq!(value.display_string(), "true");
    }
}

#[test]
fn foreground_catch_keeps_its_exception_while_scheduling_failed_task() {
    let mut vm = optive::vm::Vm::with_workers(1);
    let value = optive::run_source_in_vm(
        &mut vm,
        r#"
func work() { suspend
    gc()
    throw RuntimeError("background")
}
let t = go work()
var caught = false
try {
    try { throw ValueError("foreground") } catch (e: ValueError) {
        while (not t.done()) { suspend }
        gc()
        throw e
    }
} catch (e: ValueError) { caught = e.message == "foreground" }
caught
"#,
        "<foreground-catch-isolation>",
    )
    .unwrap();
    assert_eq!(value.display_string(), "true");
}
