#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
use criterion::{black_box, criterion_group, criterion_main, Criterion};
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

/// 较小区间，避免 criterion 采样过久；完整版见 `tests/benchmarks.rs`。
const PARALLEL_PRIMES: &str = r"
const let FROM = 2
const let TO = 10001
const let WORKERS = 8

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

func worker(id, lo, hi, box, wg) {
  var n = lo
  var local = 0
  loop {
    if (n > hi) { break }
    if (is_prime(n)) { local = local + 1 }
    n = n + 1
  }
  let g = box.lock()
  var rows = g.get()
  rows.append(local)
  g.set(rows)
  g.unlock()
  wg.done()
}

func start_worker(id, lo, hi, box, wg) {
  go do { worker(id, lo, hi, box, wg) }
}

let box = Mutex([])
let wg = WaitGroup(WORKERS)
let span = TO - FROM + 1
let chunk = span / WORKERS
var wid = 0
loop (WORKERS) {
  let lo = FROM + wid * chunk
  let hi = lo + chunk - 1
  start_worker(wid, lo, hi, box, wg)
  wid = wid + 1
}
wg.wait()
let g = box.lock()
let rows = g.get()
g.unlock()
var total = 0
var i = 0
loop {
  if (i >= rows.len()) { break }
  total = total + rows[i]
  i = i + 1
}
total
";

fn run_primes(workers: usize) {
    let mut vm = Vm::with_workers(workers);
    let v = run_source_in_vm(&mut vm, PARALLEL_PRIMES, "<bench>").unwrap();
    black_box(v);
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

fn bench_parallel_primes(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_primes_to_10001");
    group.sample_size(10);
    group.bench_function("workers=1", |b| b.iter(|| run_primes(1)));
    group.bench_function("workers=4", |b| b.iter(|| run_primes(4)));
    group.finish();
}

criterion_group!(
    benches,
    bench_fib,
    bench_empty_loop,
    bench_arith_loop,
    bench_parallel_primes
);
criterion_main!(benches);
