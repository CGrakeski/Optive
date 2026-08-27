#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
use std::sync::Arc;
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use optive::run_source;
use optive::run_source_in_vm;
use optive::vm::Vm;

const FIB: &str = r"
func fib(n) {
    if (n <= 1) { return n }
    return fib(n - 1) + fib(n - 2)
}
fib(30)
";

const EMPTY_LOOP: &str = r"
loop (1000000) { }
42
";

const ARITH_LOOP: &str = r"
let sum = 0
loop (100000) { sum = sum + 1 }
sum
";

const CALL_LOOP: &str = r"
func id(x) { return x }
let n = 0
loop (50000) {
    n = id(n + 1)
}
n
";

const CHANNEL_PING: &str = r"
const let N = 20000
let a = Channel(1)
let b = Channel(1)
go do {
  var i = 0
  loop (N) {
    a.send(i)
    let _ = b.recv()
    i = i + 1
  }
}
var i = 0
loop (N) {
  let x = a.recv()
  b.send(x)
  i = i + 1
}
N
";

const IS_PRIME: &str = r"
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
";

/// π(100001) = 9592（100001 非素数）。串行与并行必须同值。
const PRIMES_TO: u32 = 100_001;
const PRIMES_EXPECT: &str = "9592";

/// 较小区间 + 固定 8 个 `go` 切块。测启动税，不测加速比。
fn parallel_primes_chunked_src(to: u32) -> String {
    format!(
        r"
const let FROM = 2
const let TO = {to}
const let WORKERS = 8
{IS_PRIME}
func worker(id, lo, hi, box, wg) {{
  var n = lo
  var local = 0
  loop {{
    if (n > hi) {{ break }}
    if (is_prime(n)) {{ local = local + 1 }}
    n = n + 1
  }}
  let g = box.lock()
  var rows = g.get()
  rows.append(local)
  g.set(rows)
  g.unlock()
  wg.done()
}}

func start_worker(id, lo, hi, box, wg) {{
  go do {{ worker(id, lo, hi, box, wg) }}
}}

let box = Mutex([])
let wg = WaitGroup(WORKERS)
let span = TO - FROM + 1
let chunk = span / WORKERS
var wid = 0
loop (WORKERS) {{
  let lo = FROM + wid * chunk
  let hi = lo + chunk - 1
  start_worker(wid, lo, hi, box, wg)
  wid = wid + 1
}}
wg.wait()
let g = box.lock()
let rows = g.get()
g.unlock()
var total = 0
var i = 0
loop {{
  if (i >= len(rows)) {{ break }}
  total = total + rows[i]
  i = i + 1
}}
total
"
    )
}

/// 无 `go` 的单循环。加速比的分子。
///
/// 只扫奇数（2 单独计入）：与并行相同的试除量。若串行仍走 `n+=1`，
/// 偶数 worker 在 `n=2+id; n+=N`（N 为偶数）下几乎不做试除，加速比会被钉死。
fn sequential_primes_src(to: u32) -> String {
    format!(
        r"
const let TO = {to}
{IS_PRIME}
func count_primes() {{
  var total = 1
  var n = 3
  loop {{
    if (n > TO) {{ break }}
    if (is_prime(n)) {{ total = total + 1 }}
    n = n + 2
  }}
  return total
}}
count_primes()
"
    )
}

/// `tasks` 个 `go`，在奇数上轮转：`n = 3+2*id; n += 2*tasks`。任务数 = OS worker。
fn cyclic_primes_src(to: u32, tasks: usize) -> String {
    format!(
        r"
const let TO = {to}
const let STEP = {tasks}
const let ODD_STEP = STEP + STEP
{IS_PRIME}
func worker(id, box, wg) {{
  var n = 3 + id * 2
  var local = 0
  if (id == 0) {{ local = 1 }}
  loop {{
    if (n > TO) {{ break }}
    if (is_prime(n)) {{ local = local + 1 }}
    n = n + ODD_STEP
  }}
  let g = box.lock()
  var rows = g.get()
  rows.append(local)
  g.set(rows)
  g.unlock()
  wg.done()
}}

func start_worker(id, box, wg) {{
  go do {{ worker(id, box, wg) }}
}}

let box = Mutex([])
let wg = WaitGroup(STEP)
var wid = 0
loop (STEP) {{
  start_worker(wid, box, wg)
  wid = wid + 1
}}
wg.wait()
let g = box.lock()
let rows = g.get()
g.unlock()
var total = 0
var i = 0
loop {{
  if (i >= len(rows)) {{ break }}
  total = total + rows[i]
  i = i + 1
}}
total
"
    )
}

fn run_primes(workers: usize, src: &str) {
    let mut vm = Vm::with_workers(workers);
    let v = run_source_in_vm(&mut vm, src, "<bench>").unwrap();
    black_box(v);
}

fn bench_reused_vm(bencher: &mut criterion::Bencher<'_>, os_workers: usize, src: &str) {
    let compiled = optive::compile(src).expect("compile primes");
    let mut vm = Vm::with_workers(os_workers);
    vm.source_file = "<bench>".into();
    vm.current_source = Some(Arc::from(src));
    vm.load_program(compiled).expect("load primes");
    let warm = vm.run().expect("primes warmup");
    assert_eq!(
        warm.display_string(),
        PRIMES_EXPECT,
        "prime count must match sequential and parallel"
    );
    black_box(warm);
    bencher.iter(|| {
        vm.reset_script_bindings();
        let v = vm.run().unwrap();
        black_box(v);
    });
}

fn bench_run_source(c: &mut Criterion, name: &str, src: &'static str) {
    c.bench_function(name, |b| {
        b.iter(|| {
            let v = run_source(src).unwrap();
            black_box(v);
        });
    });
}

fn bench_fib(c: &mut Criterion) {
    bench_run_source(c, "fib(30)", FIB);
}

fn bench_empty_loop(c: &mut Criterion) {
    bench_run_source(c, "empty_loop(1_000_000)", EMPTY_LOOP);
}

fn bench_arith_loop(c: &mut Criterion) {
    bench_run_source(c, "arith_loop(100_000)", ARITH_LOOP);
}

fn bench_function_call(c: &mut Criterion) {
    c.bench_function("function_call_loop(50_000)", |b| {
        b.iter(|| {
            let v = run_source(CALL_LOOP).unwrap();
            assert_eq!(v.display_string(), "50000");
            black_box(v);
        });
    });
}

fn bench_channel_ping(c: &mut Criterion) {
    let mut group = c.benchmark_group("channel_ping_20000");
    group.sample_size(10);
    for w in [1usize, 4] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("workers={w}")),
            &w,
            |b, &workers| {
                b.iter(|| {
                    let mut vm = Vm::with_workers(workers);
                    let v = run_source_in_vm(&mut vm, CHANNEL_PING, "<bench-ping>").unwrap();
                    assert_eq!(v.display_string(), "20000");
                    black_box(v);
                });
            },
        );
    }
    group.finish();
}

/// `[2, 50001]` 固定 8 个 `go` 切块。每次迭代新建 VM / 线程池。
/// 测启动税 + 加 OS worker；不要当公平加速比。8 worker 可以慢于 4。
fn bench_parallel_primes_50001(c: &mut Criterion) {
    let src = parallel_primes_chunked_src(50_001);
    let warm = {
        let mut vm = Vm::with_workers(1);
        run_source_in_vm(&mut vm, &src, "<bench>").unwrap()
    };
    assert_eq!(warm.display_string(), "5133");
    black_box(warm);

    let mut group = c.benchmark_group("parallel_primes_to_50001");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(8));
    for w in [1usize, 2, 4, 8] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("workers={w}")),
            &w,
            |b, &workers| {
                b.iter(|| run_primes(workers, &src));
            },
        );
    }
    group.finish();
}

fn bench_parallel_primes(c: &mut Criterion) {
    let src = parallel_primes_chunked_src(10_001);
    let mut group = c.benchmark_group("parallel_primes_to_10001");
    group.sample_size(10);
    group.bench_function("workers=1", |b| b.iter(|| run_primes(1, &src)));
    group.bench_function("workers=4", |b| b.iter(|| run_primes(4, &src)));
    group.finish();
}

/// 加速比 = sequential / par/N。串行无 `go`；并行 `go` 个数 = OS worker，奇数轮转划分。
fn bench_primes_speedup(c: &mut Criterion) {
    let mut group = c.benchmark_group("primes_to_100001");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(12));

    let seq = sequential_primes_src(PRIMES_TO);
    group.bench_function("sequential", |b| bench_reused_vm(b, 1, &seq));

    for n in [2usize, 4, 8] {
        let src = cyclic_primes_src(PRIMES_TO, n);
        group.bench_with_input(BenchmarkId::new("par", n), &n, |b, &w| {
            bench_reused_vm(b, w, &src);
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_fib,
    bench_empty_loop,
    bench_arith_loop,
    bench_function_call,
    bench_channel_ping,
    bench_parallel_primes,
    bench_parallel_primes_50001,
    bench_primes_speedup
);
criterion_main!(benches);
