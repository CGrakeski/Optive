#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! M:N 真并行压力测试（`Vm::with_workers(n)`，n>1）。
//! 默认单测仍走 M:1；本文件显式构造多 worker。

mod common;

use std::time::{Duration, Instant};

use optive::error::ExceptionKind;
use optive::value::Value;
use optive::vm::Vm;

fn run_workers(workers: usize, source: &str) -> Value {
    let mut vm = Vm::with_workers(workers);
    optive::run_source_in_vm(&mut vm, source, "<mn-test>").expect("run")
}

fn assert_num_workers(workers: usize, source: &str, expected: &str) {
    match run_workers(workers, source) {
        Value::Num(n) => assert_eq!(n.to_string(), expected, "source: {source}"),
        other => panic!("expected num {expected}, got {other:?}"),
    }
}

#[test]
fn mn_go_await_basic() {
    assert_num_workers(
        4,
        r"
let t = go do { return 40 + 2 }
await t
",
        "42",
    );
}

#[test]
fn mn_mutex_counter_many_tasks() {
    assert_num_workers(
        4,
        r"
let m = Mutex(0)
let wg = WaitGroup(50)
loop (50) {
  go do {
    let g = m.lock()
    g.set(g.get() + 1)
    g.unlock()
    wg.done()
  }
}
wg.wait()
let g = m.lock()
let n = g.get()
g.unlock()
n
",
        "50",
    );
}

#[test]
fn mn_channel_pipeline() {
    assert_num_workers(
        4,
        r"
let ch = Channel()
go do {
  loop (20) {
    ch.send(1)
  }
  ch.close()
}
var sum = 0
loop {
  let v = ch.recv()
  if (v == none) {
    break
  }
  sum = sum + v
}
sum
",
        "20",
    );
}

#[test]
fn mn_bounded_channel_nested_ok() {
    // 有界 channel：recv 侧调度 send 任务填满后再继续 — 挂起重试，不得自死锁。
    assert_num_workers(
        1,
        r"
let ch = Channel(2)
go do {
  loop (10) {
    ch.send(1)
  }
  ch.close()
}
var sum = 0
loop {
  let v = ch.recv()
  if (v == none) {
    break
  }
  sum = sum + v
}
sum
",
        "10",
    );
}

#[test]
fn mn_bounded_channel_parallel_ok() {
    assert_num_workers(
        4,
        r"
let ch = Channel(3)
go do {
  loop (15) {
    ch.send(1)
  }
  ch.close()
}
var sum = 0
loop {
  let v = ch.recv()
  if (v == none) {
    break
  }
  sum = sum + v
}
sum
",
        "15",
    );
}

#[test]
fn mn_waitgroup_fanout() {
    assert_num_workers(
        4,
        r"
let wg = WaitGroup(32)
let m = Mutex(0)
loop (32) {
  go do {
    let g = m.lock()
    g.set(g.get() + 3)
    g.unlock()
    wg.done()
  }
}
wg.wait()
let g = m.lock()
let n = g.get()
g.unlock()
n
",
        "96",
    );
}

#[test]
fn mn_deadlock_is_typed_kind() {
    let mut vm = Vm::with_workers(1);
    let err = optive::run_source_in_vm(
        &mut vm,
        r"
let ch = Channel()
ch.recv()
",
        "<deadlock>",
    )
    .expect_err("expected deadlock");
    assert_eq!(
        err.kind(),
        ExceptionKind::DeadlockError,
        "message was: {}",
        err.message()
    );
}

#[test]
fn mn_deadlock_detected_with_parallel_workers() {
    // M:N 下主纤程阻塞在永无发送者的 channel：所有 worker 全局静默后
    // 必须报 DeadlockError 而非无限空转。
    let mut vm = Vm::with_workers(4);
    let err = optive::run_source_in_vm(
        &mut vm,
        r"
let ch = Channel()
ch.recv()
",
        "<mn-deadlock>",
    )
    .expect_err("expected deadlock under M:N");
    assert_eq!(
        err.kind(),
        ExceptionKind::DeadlockError,
        "message was: {}",
        err.message()
    );
}

#[test]
fn mn_no_false_deadlock_while_helper_works() {
    // helper 仍在跑任务时不得误判死锁。
    assert_num_workers(
        4,
        r"
let t = go do {
  std.time.sleep(0.2)
  return 7
}
await t
",
        "7",
    );
}

#[test]
fn mn_workers_1_still_matches_coop() {
    // 「suspend 两次后任务必跑到 n=2」只在 M:1 协作语义下成立；M:N 真并行下
    // 主纤程可与被唤醒任务并发推进（n=1 是合法结果）。默认 Vm 会读
    // OPTIVE_WORKERS，为避免环境影响，此处固定 workers=1。
    assert_num_workers(
        1,
        r"
var n = 0
go do {
  n = 1
  suspend
  n = 2
}
suspend
suspend
n
",
        "2",
    );
}

#[test]
fn mn_parallel_sum_via_channels() {
    assert_num_workers(
        4,
        r"
let out = Channel()
let wg = WaitGroup(4)
go do {
  out.send(10)
  wg.done()
}
go do {
  out.send(20)
  wg.done()
}
go do {
  out.send(30)
  wg.done()
}
go do {
  out.send(40)
  wg.done()
}
go do {
  wg.wait()
  out.close()
}
var sum = 0
loop {
  let v = out.recv()
  if (v == none) {
    break
  }
  sum = sum + v
}
sum
",
        "100",
    );
}

/// Helper 上启动的任务挂起后再被主线程偷走时，不得用 helper 的空
/// `saved_code` 覆盖主模块代码（否则 `wg.wait()` 后主 fiber 直接结束并返回 none）。
#[test]
fn mn_migrated_fiber_preserves_main_code() {
    for _ in 0..20 {
        assert_num_workers(
            8,
            r"
let wg = WaitGroup(8)
loop (8) {
  go do {
    var i = 0
    loop {
      if (i >= 20000) { break }
      i = i + 1
    }
    wg.done()
  }
}
wg.wait()
99
",
            "99",
        );
    }
}

/// Helper 在 `Vm::with_workers` 时即 fork，须共享主线程 `load_program` 后的
/// `struct_defs`；否则 `S(...)` 在 helper 上变成 “not callable” 并挂死流水线。
#[test]
fn mn_user_struct_ctor_on_helper() {
    assert_num_workers(
        4,
        r#"
struct S {
  let x
}
typed struct T {
  let path: text
  let n: num
}
let ch = Channel(8)
let done = Channel(1)
go do {
  var i = 0
  while (i < 8) {
    ch.send(i)
    i = i + 1
  }
  ch.close()
}
go do {
  let tasks = []
  var w = 0
  while (w < 4) {
    tasks.append(go do {
      for (i in ch) {
        let a = S(i)
        let b = T(f"p{i}", i)
        if (a.x != i or b.n != i) {
          throw RuntimeError("struct ctor mismatch")
        }
      }
    })
    w = w + 1
  }
  std.async.gather(tasks)
  done.send(1)
}
done.recv()
"#,
        "1",
    );
}

/// 慢 worker + 主 fiber 先 recv：曾因任务内 `block_suspend` 泄漏，
/// 使 `Channel.recv` 误得 `<Task>`。
#[test]
fn mn_recv_not_poisoned_by_blocker_task() {
    assert_num_workers(
        4,
        r#"
func busy(n) {
  var s = 0
  var i = 0
  loop {
    if (i >= n) { break }
    s = s + i
    i = i + 1
  }
  return s
}
func worker(id, ch, wg) {
  let _ = busy(8000)
  ch.send(id)
  wg.done()
}
func start(id, ch, wg) {
  go do { worker(id, ch, wg) }
}
let ch = Channel()
let wg = WaitGroup(4)
var wid = 0
loop (4) {
  start(wid, ch, wg)
  wid = wid + 1
}
go do {
  wg.wait()
  ch.close()
}
var sum = 0
var n = 0
loop {
  let v = ch.recv()
  if (v == none) { break }
  if (type(v) != "num") {
    return -1
  }
  sum = sum + v
  n = n + 1
}
if (n != 4) { return -2 }
sum
"#,
        "6",
    );
}

/// 2 个 CPU `go` 在 2 个 OS worker 上必须重叠推进，而不是轮流切片。
/// 墙钟倍率断言默认关闭，避免测试机争用导致间歇失败；设置
/// `OPTIVE_ASSERT_MN_OVERLAP=1` 时对多次重复取最佳 1-worker / 2-worker 时间再比。
#[test]
fn mn_two_cpu_tasks_overlap_on_two_workers() {
    if num_cpus::get() < 2 {
        return;
    }
    let src = r"
func burn() {
  var n = 0
  loop (400000) { n = n + 1 }
  return n
}
func start(wg) {
  go do {
    burn()
    wg.done()
  }
}
let wg = WaitGroup(2)
start(wg)
start(wg)
wg.wait()
1
";
    {
        let mut vm = Vm::with_workers(2);
        optive::run_source_in_vm(&mut vm, src, "<overlap-2>").expect("run 2");
    }
    if std::env::var_os("OPTIVE_ASSERT_MN_OVERLAP").is_none() {
        return;
    }
    const REPS: usize = 5;
    let mut best1 = Duration::MAX;
    let mut best2 = Duration::MAX;
    for _ in 0..REPS {
        let t1 = {
            let start = Instant::now();
            let mut vm = Vm::with_workers(1);
            optive::run_source_in_vm(&mut vm, src, "<overlap-1>").expect("run 1");
            start.elapsed()
        };
        let t2 = {
            let start = Instant::now();
            let mut vm = Vm::with_workers(2);
            optive::run_source_in_vm(&mut vm, src, "<overlap-2>").expect("run 2");
            start.elapsed()
        };
        best1 = best1.min(t1);
        best2 = best2.min(t2);
    }
    assert!(
        best2 * 4 < best1 * 3,
        "2 workers should overlap CPU tasks (2w < 0.75× 1w): 1w={best1:?} 2w={best2:?}"
    );
}
