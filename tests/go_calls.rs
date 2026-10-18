#![allow(clippy::unwrap_used)]

use optive::vm::Vm;

fn run(workers: usize, source: &str) -> String {
    let mut vm = Vm::with_workers(workers).with_suspend_budget(64);
    optive::run_source_in_vm(&mut vm, source, "<go-calls>")
        .unwrap()
        .display_string()
}

#[test]
fn all_call_shapes_are_pending_not_eager_on_one_worker() {
    for call in [
        "work(42)",
        "work(x = 42)",
        "work(*[42])",
        "work(**{\"x\": 42})",
    ] {
        assert_eq!(
            run(
                1,
                &format!(
                    r"
var called = 0
func work(x) {{ called = called + 1
    return x }}
let t = go {call}
let lazy = called == 0 and not t.done()
lazy and (await t) == 42 and called == 1
"
                )
            ),
            "true",
            "{call}"
        );
    }
}

#[test]
fn named_splat_arguments_survive_sleep_and_gc() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r#"
func work(x, y = 1, *rest, **extra) {
    std.time.sleep_ms(5)
    gc()
    return x + y + rest[0] + extra["z"][0]
}
let t = go work(*[10, 11, 12], **{"z": [9]})
gc()
(await t) == 42
"#
            ),
            "true"
        );
    }
}

#[test]
fn argument_expressions_evaluate_once_at_spawn() {
    assert_eq!(
        run(
            1,
            r"
var evaluated = 0
func arg() { evaluated = evaluated + 1
    return 42 }
func work(x) { std.time.sleep_ms(5)
    return x }
let t = go work(x = arg())
let once = evaluated == 1
once and (await t) == 42 and evaluated == 1
"
        ),
        "true"
    );
}

#[test]
fn bad_named_binding_fails_at_await_not_spawn() {
    assert_eq!(
        run(
            1,
            r"
func work(x) { return x }
let t = go work(bad = 42)
var caught = false
try { await t } catch (e) { caught = true }
caught and t.done()
"
        ),
        "true"
    );
}

#[test]
fn named_and_splat_builtin_tasks_retry_without_losing_arguments() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r"
let a = go std.time.sleep_ms(*[5])
let b = go std.time.sleep_ms(*[5], **{})
await a
await b
a.done() and b.done()
"
            ),
            "true"
        );
    }
}

#[test]
fn synchronous_splat_builtin_call_restores_list_on_suspend() {
    assert_eq!(
        run(
            1,
            r"
let t = go do { std.time.sleep_ms(*[5])
    return 42 }
(await t) == 42
"
        ),
        "true"
    );
}

#[test]
fn await_call_supports_named_arguments_and_splats() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r"
func work(x, y = 2) { std.time.sleep_ms(3)
    return x + y }
(await work(x = 40)) == 42 and (await work(*[40], **{})) == 42
"
            ),
            "true"
        );
    }
}

#[test]
fn named_dispatch_binds_before_scoring_defaults() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r#"
friend func choose(x:: num, offset = 2) { return x + offset }
choose.__dispatch__.append(do(x:: text, offset = 2) { return len(x) + offset })
let first = go choose(x = 40)
let second = go choose(offset = 2, x = "abc")
(await first) + (await second)
"#
            ),
            "47"
        );
    }
}

#[test]
fn named_generic_function_infers_type_from_keyword() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r"
func identity[T](x: T) { std.time.sleep_ms(3)
    return x }
let t = go identity(x = 42)
(await t) == 42
"
            ),
            "true"
        );
    }
}

#[test]
fn temporary_bound_method_and_alias_remain_gc_roots() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r"
struct A {
    let base
    func work(self, x = 2) { std.time.sleep_ms(3)
        gc()
        return self.base + x }
}
let alias = A(40).work
let task = go A(40).work(x = 2)
gc()
alias(x = 2) + (await task)
"
            ),
            "84"
        );
    }
}

#[test]
fn named_method_task_preserves_self_and_result() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r"
struct A {
    let base
    func work(self, x = 1) { std.time.sleep_ms(3)
        return self.base + x }
}
let a = A(40)
let t = go a.work(x = 2)
(await t) == 42
"
            ),
            "true"
        );
    }
}
