#![allow(clippy::unwrap_used)]

use optive::vm::Vm;

fn run(workers: usize, source: &str) -> String {
    let mut vm = Vm::with_workers(workers).with_suspend_budget(64);
    optive::run_source_in_vm(&mut vm, source, "<timed-wait>")
        .unwrap()
        .display_string()
}

#[test]
fn main_sleep_runs_pending_tasks_on_one_or_many_workers() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r"
let t = go do { return 42 }
std.time.sleep(0.05)
t.done() and (await t) == 42
"
            ),
            "true"
        );
    }
}

#[test]
fn main_sleep_advances_cpu_task_across_many_budget_slices() {
    assert_eq!(
        run(
            1,
            r"
var progress = 0
let t = go do {
    while (progress < 20000) { progress = progress + 1 }
    return progress
}
std.time.sleep(0.02)
let advanced = progress > 64
advanced and (await t) == 20000
"
        ),
        "true"
    );
}

#[test]
fn main_sleep_ms_also_drives_the_scheduler() {
    assert_eq!(
        run(
            1,
            r"
let t = go do { return 7 }
std.time.sleep_ms(30)
t.done() and (await t) == 7
"
        ),
        "true"
    );
}

#[test]
fn sleepers_resume_at_deadline_without_blocking_other_tasks() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r"
let slow = go do { std.time.sleep(0.12)
    return 1
}
let fast = go do { std.time.sleep(0.01)
    return 2
}
let result = await fast
let ordered = result == 2 and not slow.done()
ordered and (await slow) == 1
"
            ),
            "true"
        );
    }
}

#[test]
fn repeated_sleep_preserves_fiber_state_and_gc_roots() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r"
let t = go do {
    let values = [11, 13, 17]
    std.time.sleep(0.01)
    std.time.sleep(0.01)
    return values[0] + values[1] + values[2]
}
suspend
gc()
await t
"
            ),
            "41"
        );
    }
}

#[test]
fn hard_cancel_wakes_a_parked_sleep_without_waiting_for_its_deadline() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r"
let t = go do { std.time.sleep(60)
    return 42
}
std.time.sleep(0.01)
t.cancel()
while (not t.done()) { suspend }
var caught = false
try { await t } catch (e: Cancelled) { caught = true }
caught
"
            ),
            "true"
        );
    }
}

#[test]
fn cooperative_cancel_does_not_shorten_requested_sleep() {
    for workers in [1, 4] {
        assert_eq!(
            run(
                workers,
                r"
func work() cancel_flag{flag} {
    std.time.sleep(0.08)
    return flag.requested()
}
let t = go work()
std.time.sleep(0.01)
t.cancel()
let still_waiting = not t.done()
still_waiting and (await t)
"
            ),
            "true"
        );
    }
}

#[test]
fn polling_done_with_suspend_resumes_timer_only_tasks() {
    assert_eq!(
        run(
            1,
            r"
let t = go do { std.time.sleep(0.01)
    return 19
}
while (not t.done()) { suspend }
await t
"
        ),
        "19"
    );
}

#[test]
fn failed_background_task_does_not_interrupt_main_sleep() {
    assert_eq!(
        run(
            1,
            r#"
let t = go do { std.time.sleep(0.01)
    throw ValueError("background")
}
std.time.sleep(0.03)
var caught = false
try { await t } catch (e: ValueError) { caught = e.message == "background" }
caught
"#
        ),
        "true"
    );
}

#[test]
fn host_cancellation_interrupts_idle_main_sleep() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let signal = Arc::new(AtomicBool::new(false));
    let mut vm = Vm::with_workers(1);
    vm.set_host_cancel(signal.clone());
    let cancel = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        signal.store(true, Ordering::Release);
    });
    let started = std::time::Instant::now();
    let error =
        optive::run_source_in_vm(&mut vm, "std.time.sleep(5)", "<cancel-main-sleep>").unwrap_err();
    cancel.join().unwrap();
    assert!(error.to_string().contains("host cancelled"));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}
