mod common;

use common::{assert_num, run_err, value};
use optive::value::Value;

#[test]
fn do_block_iife_sugar() {
    assert_num("do { return 6 * 7 }", "42");
}

#[test]
fn go_await_do_block() {
    assert_num(
        r#"
let t = go do { return 41 + 1 }
await t
"#,
        "42",
    );
}

#[test]
fn await_start_and_wait() {
    assert_num(
        r#"
await do { return 7 }
"#,
        "7",
    );
}

#[test]
fn suspend_runs_ready_task() {
    // 真正挂起：第一次 suspend 跑到任务内 suspend；第二次再跑完。
    assert_num(
        r#"
var n = 0
go do {
  n = 1
  suspend
  n = 2
}
suspend
suspend
n
"#,
        "2",
    );
}

#[test]
fn suspend_then_await_completes() {
    assert_num(
        r#"
var n = 0
let t = go do {
  n = 1
  suspend
  n = 2
  return 9
}
await t
n
"#,
        "2",
    );
}

#[test]
fn await_yield_removed() {
    run_err("await yield");
}

#[test]
fn await_join_task_result() {
    assert_num(
        r#"
var n = 0
let t = go do {
  n = 1
  n = 2
  return 9
}
await t
n + 0
"#,
        "2",
    );
}

#[test]
fn channel_send_recv() {
    assert_num(
        r#"
let ch = Channel()
go do {
  ch.send(42)
  ch.close()
}
ch.recv()
"#,
        "42",
    );
}

#[test]
fn channel_recv_closed_is_none() {
    let v = value(
        r#"
let ch = Channel()
ch.close()
ch.recv()
"#,
    );
    assert!(matches!(v, Value::None), "expected none, got {v:?}");
}

#[test]
fn select_recv_ready() {
    assert_num(
        r#"
let ch = Channel()
ch.send(7)
select {
  case ch.recv() as x {
    return x
  }
}
"#,
        "7",
    );
}

#[test]
fn select_await_task() {
    assert_num(
        r#"
let t = go do { return 3 }
select {
  case await t as x {
    return x
  }
}
"#,
        "3",
    );
}

#[test]
fn mutex_lock_unlock() {
    assert_num(
        r#"
let m = Mutex(0)
with (m.lock() as g) {
  g.set(g.get() + 5)
}
with (m.lock() as g) {
  return g.get()
}
"#,
        "5",
    );
}

#[test]
fn sync_yield_runs_ready_task() {
    assert_num(
        r#"
var n = 0
go do {
  n = 1
  std.sync.yield()
  n = 2
}
std.sync.yield()
std.sync.yield()
n
"#,
        "2",
    );
}

#[test]
fn waitgroup_add_done_wait() {
    assert_num(
        r#"
let wg = WaitGroup(1)
var n = 0
go do {
  n = 10
  wg.done()
}
wg.wait()
n
"#,
        "10",
    );
}

#[test]
fn semaphore_acquire_release() {
    assert_num(
        r#"
let sem = Semaphore(0)
var n = 0
go do {
  n = 1
  sem.release()
}
sem.acquire()
n
"#,
        "1",
    );
}

#[test]
fn once_do_runs_once() {
    assert_num(
        r#"
let o = Once()
let counter = Mutex(0)
func bump() {
  let g = counter.lock()
  g.set(g.get() + 1)
  let v = g.get()
  g.unlock()
  return v
}
let a = o.run(bump)
let b = o.run(bump)
let g = counter.lock()
let n = g.get()
g.unlock()
a + b + n
"#,
        "3",
    );
}

#[test]
fn barrier_two_parties() {
    assert_num(
        r#"
let b = Barrier(2)
var n = 0
go do {
  b.wait()
  n = n + 1
}
b.wait()
n = n + 10
n
"#,
        "11",
    );
}

#[test]
fn rwmutex_read_write() {
    assert_num(
        r#"
let m = RWMutex(0)
with (m.write() as g) {
  g.set(7)
}
with (m.read() as g) {
  return g.get()
}
"#,
        "7",
    );
}

#[test]
fn cond_wait_signal() {
    assert_num(
        r#"
let m = Mutex(0)
let cv = Cond()
var n = 0
go do {
  let g = m.lock()
  n = 42
  cv.signal()
  g.unlock()
}
let g = m.lock()
loop {
  if (n == 42) {
    break
  }
  cv.wait(g)
}
g.unlock()
n
"#,
        "42",
    );
}

#[test]
fn budget_interleaves_cpu_task() {
    // 强制小预算：无显式 suspend 时，长循环任务仍应能被 await 跑完。
    std::env::set_var("OPTIVE_SUSPEND_BUDGET", "64");
    let v = value(
        r#"
var n = 0
let t = go do {
  loop (300) {
    n = n + 1
  }
  return n
}
await t
"#,
    );
    std::env::remove_var("OPTIVE_SUSPEND_BUDGET");
    match v {
        Value::Num(n) => assert_eq!(n.to_string(), "300"),
        other => panic!("expected 200, got {other:?}"),
    }
}
