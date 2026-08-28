#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

// 本套件验证协作调度语义（顺序敏感断言在 M:N 真并行下不成立），
// 统一固定 workers=1；M:N 专项测试见 vm_mn_parallel.rs。
use common::{
    assert_bool_w1 as assert_bool, assert_num_w1 as assert_num, assert_text_w1 as assert_text,
    run_err_w1 as run_err, value_w1 as value,
};
use optive::value::Value;

#[test]
fn do_block_iife_sugar() {
    assert_num("do { return 6 * 7 }", "42");
}

#[test]
fn go_await_do_block() {
    assert_num(
        r"
let t = go do { return 41 + 1 }
await t
",
        "42",
    );
}

#[test]
fn await_start_and_wait() {
    assert_num(
        r"
await do { return 7 }
",
        "7",
    );
}

#[test]
fn suspend_runs_ready_task() {
    // 真正挂起：第一次 suspend 跑到任务内 suspend；第二次再跑完。
    // 顺序保证仅 M:1 成立，固定 workers=1 避免 OPTIVE_WORKERS 影响。
    assert_num(
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
fn suspend_then_await_completes() {
    assert_num(
        r"
var n = 0
let t = go do {
  n = 1
  suspend
  n = 2
  return 9
}
await t
n
",
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
        r"
var n = 0
let t = go do {
  n = 1
  n = 2
  return 9
}
await t
n + 0
",
        "2",
    );
}

#[test]
fn channel_send_recv() {
    assert_num(
        r"
let ch = Channel()
go do {
  ch.send(42)
  ch.close()
}
ch.recv()
",
        "42",
    );
}

#[test]
fn channel_recv_closed_is_none() {
    let v = value(
        r"
let ch = Channel()
ch.close()
ch.recv()
",
    );
    assert!(matches!(v, Value::None), "expected none, got {v:?}");
}

#[test]
fn select_recv_ready() {
    assert_num(
        r"
let ch = Channel()
ch.send(7)
select {
  case ch.recv() as x {
    return x
  }
}
",
        "7",
    );
}

#[test]
fn select_await_task() {
    assert_num(
        r"
let t = go do { return 3 }
select {
  case await t as x {
    return x
  }
}
",
        "3",
    );
}

#[test]
fn mutex_lock_get_temp_does_not_leak() {
    // `m.lock().get()` 不得永久占锁，否则后续 lock 会 Deadlock。
    assert_num(
        r"
let m = Mutex(7)
let a = m.lock().get()
let b = m.lock().get()
a + b
",
        "14",
    );
}

#[test]
fn mutex_lock_unlock() {
    assert_num(
        r"
let m = Mutex(0)
with (m.lock() as g) {
  g.set(g.get() + 5)
}
with (m.lock() as g) {
  return g.get()
}
",
        "5",
    );
}

#[test]
fn sync_yield_runs_ready_task() {
    // 「yield 两次后任务必跑完」仅 M:1 协作语义保证；固定 workers=1。
    assert_num(
        r"
var n = 0
go do {
  n = 1
  std.sync.yield()
  n = 2
}
std.sync.yield()
std.sync.yield()
n
",
        "2",
    );
}

#[test]
fn waitgroup_add_done_wait() {
    assert_num(
        r"
let wg = WaitGroup(1)
var n = 0
go do {
  n = 10
  wg.done()
}
wg.wait()
n
",
        "10",
    );
}

#[test]
fn semaphore_acquire_release() {
    assert_num(
        r"
let sem = Semaphore(0)
var n = 0
go do {
  n = 1
  sem.release()
}
sem.acquire()
n
",
        "1",
    );
}

#[test]
fn once_do_runs_once() {
    assert_num(
        r"
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
",
        "3",
    );
}

#[test]
fn barrier_two_parties() {
    assert_num(
        r"
let b = Barrier(2)
var n = 0
go do {
  b.wait()
  n = n + 1
}
b.wait()
n = n + 10
n
",
        "11",
    );
}

#[test]
fn rwmutex_read_write() {
    assert_num(
        r"
let m = RWMutex(0)
with (m.write() as g) {
  g.set(7)
}
with (m.read() as g) {
  return g.get()
}
",
        "7",
    );
}

#[test]
fn cond_wait_signal() {
    assert_num(
        r"
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
",
        "42",
    );
}

#[test]
fn budget_interleaves_cpu_task() {
    // 强制小预算：无显式 suspend 时，长循环任务仍应能被 await 跑完。
    // 用 Vm API 设预算，避免 set_var 污染并行跑的其它用例。
    let mut vm = optive::vm::Vm::with_workers(1).with_suspend_budget(64);
    let v = optive::run_source_in_vm(
        &mut vm,
        r"
var n = 0
let t = go do {
  loop (300) {
    n = n + 1
  }
  return n
}
await t
",
        "<test>",
    )
    .expect("run");
    match v {
        Value::Num(n) => assert_eq!(n.to_string(), "300"),
        other => panic!("expected 200, got {other:?}"),
    }
}

#[test]
fn select_as_placeholder_discards_bind() {
    assert_num(
        r"
let ch = Channel()
ch.send(1)
select {
  case ch.recv() as _ {
    return 42
  }
}
",
        "42",
    );
}

#[test]
fn select_sleep_as_placeholder() {
    // sleep case 绑定值为 none；`as _` 应可解析并丢弃。
    assert_num(
        r"
select {
  case std.time.sleep(0) as _ {
    return 7
  }
}
",
        "7",
    );
}

#[test]
fn async_taskgroup_joins_on_exit() {
    // 断言 join 后两个任务的累积效果；共享捕获变量的非原子自增在 M:N 真并行下
    // 存在丢失更新（用户级数据竞争），故固定 workers=1 验证 join 语义本身。
    assert_num(
        r"
var n = 0
with (std.async.taskgroup() as g) {
  g.run(do() {
    n = n + 10
  })
  g.run(do() {
    n = n + 32
  })
}
n
",
        "42",
    );
}

#[test]
fn async_gather_joins_tasks() {
    assert_num(
        r"
let tasks = [go do { return 1 }, go do { return 2 }, go do { return 3 }]
let xs = std.async.gather(tasks)
xs[0] + xs[1] + xs[2]
",
        "6",
    );
}

#[test]
fn async_race_returns_first() {
    assert_num(
        r"
let a = go do { return 11 }
let b = go do {
  suspend
  return 99
}
std.async.race([a, b])
",
        "11",
    );
}

#[test]
fn async_with_timeout_enter_exit() {
    assert_num(
        r"
var ok = 0
with (std.async.with_timeout(1.0) as ctx) {
  if (!ctx.expired()) {
    ok = 1
  }
}
ok
",
        "1",
    );
}

#[test]
fn task_cancel_before_run() {
    assert_bool(
        r"
let t = go do {
  suspend
  return 1
}
t.cancel()
t.cancelled()
",
        true,
    );
}

#[test]
fn await_cancelled_task_throws() {
    run_err(
        r"
let t = go do {
  std.time.sleep(10.0)
  return 1
}
t.cancel()
await t
",
    );
}

#[test]
fn race_cancels_losers() {
    assert_bool(
        r"
let slow = go do {
  suspend
  suspend
  return 99
}
let fast = go do { return 7 }
let v = std.async.race([slow, fast])
v == 7 and slow.cancelled()
",
        true,
    );
}

#[test]
fn taskgroup_cancels_siblings_on_error() {
    assert_bool(
        r#"
var cancelled = false
try {
  with (std.async.taskgroup() as g) {
    g.run(do() {
      std.time.sleep(10.0)
    })
    g.run(do() {
      throw ValueError("boom")
    })
  }
} catch (e) {
  cancelled = true
}
cancelled
"#,
        true,
    );
}

#[test]
fn par_map_sums() {
    assert_num(
        r"
func double(x) { return x * 2 }
let ys = std.async.par_map([1, 2, 3], double)
ys[0] + ys[1] + ys[2]
",
        "12",
    );
}

/// memoize 走 `call_user_function`；任务里 IO 让出不得报 `task suspended outside scheduler`。
#[test]
fn par_map_memoize_list_dir_does_not_abort() {
    assert_bool(
        r#"
use std.decos.{ memoize }
use std.fs.{ list_dir }
use std.path.{ abspath }
memoize func listing(p) {
    return list_dir(p)
}
let root = abspath("tests")
let ys = std.async.par_map([root, root], listing)
len(ys[0]) > 0 and len(ys[1]) > 0
"#,
        true,
    );
}

#[test]
fn par_map_memoize_sleep_returns_values() {
    assert_num(
        r#"
use std.decos.{ memoize }
memoize func nap(x) {
    std.time.sleep(0.01)
    return x * 2
}
let ys = std.async.par_map([1, 2, 3], nap)
ys[0] + ys[1] + ys[2]
"#,
        "12",
    );
}

#[test]
fn atomic_add() {
    assert_num(
        r"
let a = std.sync.Atomic.num(10)
a.add(5)
a.get()
",
        "15",
    );
}

#[test]
fn async_workers_at_least_one() {
    // 本套件固定 workers=1；API 本身返回当前调度器 worker 数。
    assert_num(
        r"
std.async.workers()
",
        "1",
    );
}

#[test]
fn par_for_maps() {
    assert_num(
        r"
let ys = par for (x in [1, 2, 3]) { x * 10 }
ys[0] + ys[1] + ys[2]
",
        "60",
    );
}

#[test]
fn par_for_zips_multiple_iterators() {
    assert_num(
        r"
let ys = par for (x in [1, 2, 3], y in [10, 20, 30]) { x + y }
ys[0] + ys[1] + ys[2]
",
        "66",
    );
}

#[test]
fn par_block_gathers() {
    assert_num(
        r"
let r = par {
  10 + 1
  20 + 2
}
r[0] + r[1]
",
        "33",
    );
}

#[test]
fn par_for_rejects_shared_assign() {
    run_err(
        r"
var n = 0
par for (x in [1, 2]) {
  n = n + x
}
",
    );
}

#[test]
fn stream_of_for_in() {
    assert_num(
        r"
let s = std.async.stream_of([1, 2, 3])
var t = 0
for (x in s) {
  t = t + x
}
t
",
        "6",
    );
}

#[test]
fn stream_is_not_channel_type() {
    assert_text(r"type(std.async.stream_of([]))", "Stream");
    assert_text(r"type(Stream())", "Stream");
    assert_text(r"type(Channel())", "Channel");
}

#[test]
fn stream_send_forbidden() {
    let err = optive::run_source(
        r"
let s = Stream()
s.send(1)
",
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("send") || msg.contains("Stream"), "{msg}");
}

#[test]
fn channel_as_stream_view() {
    assert_num(
        r"
let ch = Channel()
go do {
  ch.send(9)
  ch.close()
}
let s = ch.as_stream()
s.next()
",
        "9",
    );
}

#[test]
fn stream_take_map() {
    assert_num(
        r"
func double(x) { return x * 2 }
let s = std.async.stream_of([1, 2, 3, 4])
let m = std.async.stream_map(s, double)
let t = std.async.stream_take(m, 2)
var sum = 0
for (v in t) { sum = sum + v }
sum
",
        "6",
    );
}

#[test]
fn stream_from_gen_pull() {
    assert_num(
        r"
gen count() {
  yield 1
  yield 2
  yield 3
}
let s = std.async.stream_from_gen(count)
var t = 0
for (x in s) { t = t + x }
t
",
        "6",
    );
}

#[test]
fn stream_next_alias() {
    assert_num(
        r"
let s = std.async.stream_of([7])
s.next()
",
        "7",
    );
}

#[test]
fn channel_for_in_task_blocks_then_receives() {
    // IterNext 必须在 channel 阻塞时挂起，不能把 none 当元素。
    assert_num(
        r"
let ch = Channel()
let t = go do {
  for (x in ch) {
    return x
  }
  return -1
}
go do { ch.send(42) }
await t
",
        "42",
    );
}

#[test]
fn task_throw_inside_host_with_does_not_panic() {
    // 宿主 with/try 水位下任务抛错，不得 capture_fiber panic，且宿主可继续。
    assert_num(
        r#"
var n = 0
with (Mutex(0).lock() as g) {
  let t = go do {
    throw ValueError("boom")
  }
  try {
    await t
  } catch (e) {
    n = 1
  }
  g.set(7)
}
n
"#,
        "1",
    );
}

#[test]
fn task_throw_host_with_scope_intact() {
    assert_num(
        r#"
var seen = 0
try {
  with (Mutex(0).lock() as g) {
    let t = go do {
      throw ValueError("x")
    }
    try { await t } catch (e) { }
    g.set(3)
    seen = g.get()
  }
} catch (e) {
  seen = -1
}
seen
"#,
        "3",
    );
}

#[test]
fn with_catch_rethrows_in_do_block() {
    assert_num(
        r#"
var n = 0
go do {
  try {
    with (Mutex(0).lock() as g) {
      throw ValueError("inside")
    }
  } catch (e) {
    n = 9
  }
}
suspend
suspend
n
"#,
        "9",
    );
}

#[test]
fn for_placeholder_discards_bind() {
    assert_num(
        r"
var n = 0
for (_ in [1, 2, 3]) {
  n = n + 1
}
let xs = [0 for (_ in std.math.range(0, 4))]
n + len(xs)
",
        "7",
    );
}

/// B11：`go do { sibling(...) }` 应能解析同模块顶层函数（勿把模块名塞进闭包捕获）。
#[test]
fn go_do_calls_sibling_module_func() {
    assert_num(
        r"
func sibling(n) {
  return n + 1
}
func run() {
  let t = go do {
    return sibling(41)
  }
  return await t
}
run()
",
        "42",
    );
}

/// 两 channel 同时可读时，select 多轮命中应大致均匀（书写顺序不再是优先级）。
#[test]
fn select_multi_ready_fairness() {
    let v = value(
        r"
let a = Channel(1)
let b = Channel(1)
a.send(1)
b.send(1)
var ca = 0
var cb = 0
loop (200) {
  select {
    case a.recv() as _ {
      ca = ca + 1
      a.send(1)
    }
    case b.recv() as _ {
      cb = cb + 1
      b.send(1)
    }
  }
}
[ca, cb]
",
    );
    let Value::List(list) = v else {
        panic!("expected list");
    };
    let items = list.borrow();
    assert_eq!(items.len(), 2);
    let ca = match &items[0] {
        Value::Num(n) => n.to_i64().unwrap(),
        _ => panic!("ca"),
    };
    let cb = match &items[1] {
        Value::Num(n) => n.to_i64().unwrap(),
        _ => panic!("cb"),
    };
    assert_eq!(ca + cb, 200, "ca={ca} cb={cb}");
    assert!(ca >= 60 && cb >= 60, "unfair ca={ca} cb={cb}");
    assert!((ca - cb).abs() <= 80, "skew ca={ca} cb={cb}");
}

#[test]
fn select_recv_on_channel_stream_view() {
    assert_num(
        r"
let ch = Channel(1)
ch.send(7)
let s = ch.as_stream()
select {
  case s.recv() as x { x }
}
",
        "7",
    );
}

#[test]
fn stream_close_stops_take() {
    assert_num(
        r"
let s = std.async.stream_of([1, 2, 3, 4])
let t = std.async.stream_take(s, 10)
t.close()
var n = 0
for (x in t) { n = n + 1 }
n
",
        "0",
    );
}

/// B12：多 sleep case 时短周期应先就绪，不被长 sleep / Suspend 饿死。
#[test]
fn select_short_sleep_not_starved_by_long_sleep() {
    assert_text(
        r#"
select {
  case std.time.sleep(0.05) as _ { "tick" }
  case std.time.sleep(2.0) as _ { "timeout" }
}
"#,
        "tick",
    );
}

/// B12 + 协作 sleep：通道发送方 `sleep` 不得霸住调度，短 tick 仍应先到。
#[test]
fn select_short_sleep_beats_channel_after_coop_sleep() {
    assert_text(
        r#"
let done = Channel()
go do {
  std.time.sleep(0.25)
  done.send(1)
}
select {
  case done.recv() as _ { "done" }
  case std.time.sleep(0.05) as _ { "tick" }
  case std.time.sleep(2.0) as _ { "timeout" }
}
"#,
        "tick",
    );
}

/// go 纤程内 `list_dir` / `read_bytes` 在协作让出后仍须返回真实值（非 Task/none）。
#[test]
fn go_fiber_fs_list_dir_and_read_bytes() {
    assert_bool(
        r#"
use std.fs.{ list_dir, read_bytes, write_bytes, remove }
use std.path.{ abspath, join }
use std.debug.{ type_name }
let root = abspath("tests")
let tmp = join(root, "__optive_go_fs_probe__.bin")
write_bytes(tmp, b"abc")
let done = Channel(1)
go do {
  let names = list_dir(root)
  let b = read_bytes(tmp)
  done.send([
    type_name(names) == "list",
    len(names) > 0,
    type_name(b) == "bytes",
    len(b) == 3,
  ])
}
let r = done.recv()
remove(tmp)
r[0] and r[1] and r[2] and r[3]
"#,
        true,
    );
}
