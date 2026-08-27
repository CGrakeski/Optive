#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! Optive CPU 分析负载入口（供 samply / 外部采样器挂载）。
//!
//! ```text
//! cargo build --profile profiling --bin benchmark-analysis
//! samply record --save-only -o target/profile-nested.json.gz -- \
//!   ./target/profiling/benchmark-analysis.exe nested_loop --iters 1
//! ```
//!
//! 也可用 release（符号可能被 strip）：
//! `cargo run --release --bin benchmark-analysis -- --list`

use std::env;
use std::process;
use std::time::Instant;

use optive::run_source;
use optive::run_source_in_vm;
use optive::vm::Vm;

const FIB_SRC: &str = r"
func fib(n) {
    if (n <= 1) { return n }
    return fib(n - 1) + fib(n - 2)
}
fib(30)
";

const EMPTY_LOOP_SRC: &str = r"
loop (1000000) { }
42
";

const ARITH_LOOP_SRC: &str = r"
let sum = 0
loop (100000) {
    sum = sum + 1
}
sum
";

const CALL_LOOP_SRC: &str = r"
func id(x) { return x }
let n = 0
loop (50000) {
    n = id(n + 1)
}
n
";

const NESTED_LOOP_1B_SRC: &str = r"
loop (1000) {
    {
        loop (1000) {
            {
                loop (1000) {
                }
            }
        }
    }
}
42
";

const PRIMES_SEQ_SRC: &str = r"
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
    if (n > 50001) { break }
    if (is_prime(n)) { total = total + 1 }
    n = n + 1
  }
  return total
}

count_primes()
";

const PARALLEL_PRIMES_SRC: &str = r"
const let FROM = 2
const let TO = 50001
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
  if (i >= len(rows)) { break }
  total = total + rows[i]
  i = i + 1
}
total
";

const CHANNEL_PING_SRC: &str = r"
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

#[derive(Clone, Copy)]
struct Bench {
    name: &'static str,
    expect: Option<&'static str>,
    kind: BenchKind,
}

#[derive(Clone, Copy)]
enum BenchKind {
    Source(&'static str),
    VmOnlyCompileOnce(&'static str),
    Workers { src: &'static str, workers: usize },
}

const BENCHES: &[Bench] = &[
    Bench {
        name: "fib30",
        expect: Some("832040"),
        kind: BenchKind::Source(FIB_SRC),
    },
    Bench {
        name: "empty_loop",
        expect: Some("42"),
        kind: BenchKind::Source(EMPTY_LOOP_SRC),
    },
    Bench {
        name: "arith_loop",
        expect: Some("100000"),
        kind: BenchKind::Source(ARITH_LOOP_SRC),
    },
    Bench {
        name: "call_loop",
        expect: Some("50000"),
        kind: BenchKind::Source(CALL_LOOP_SRC),
    },
    Bench {
        name: "nested_loop",
        expect: Some("42"),
        kind: BenchKind::VmOnlyCompileOnce(NESTED_LOOP_1B_SRC),
    },
    Bench {
        name: "primes_seq",
        expect: Some("5133"),
        kind: BenchKind::Source(PRIMES_SEQ_SRC),
    },
    Bench {
        name: "primes_par",
        expect: Some("5133"),
        kind: BenchKind::Workers {
            src: PARALLEL_PRIMES_SRC,
            workers: 8,
        },
    },
    Bench {
        name: "channel_ping",
        expect: Some("20000"),
        kind: BenchKind::Workers {
            src: CHANNEL_PING_SRC,
            workers: 4,
        },
    },
];

fn usage() -> ! {
    eprintln!(
        "Usage:
  benchmark-analysis --list
  benchmark-analysis <bench>|all [--iters N] [--workers N] [--warmup N]

Benches: {}",
        BENCHES
            .iter()
            .map(|b| b.name)
            .collect::<Vec<_>>()
            .join(", ")
    );
    process::exit(2);
}

fn parse_args() -> (Vec<&'static Bench>, usize, usize, Option<usize>) {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        usage();
    }
    if args[0] == "--list" {
        for b in BENCHES {
            println!("{}", b.name);
        }
        process::exit(0);
    }

    let mut iters = 3usize;
    let mut warmup = 1usize;
    let mut workers_override: Option<usize> = None;
    let mut names: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--iters" => {
                i += 1;
                iters = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage());
            }
            "--warmup" => {
                i += 1;
                warmup = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage());
            }
            "--workers" => {
                i += 1;
                workers_override = Some(
                    args.get(i)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_else(|| usage()),
                );
            }
            other if other.starts_with('-') => usage(),
            other => names.push(other.to_string()),
        }
        i += 1;
    }

    if names.is_empty() {
        usage();
    }

    let selected: Vec<&'static Bench> = if names.len() == 1 && names[0] == "all" {
        BENCHES.iter().collect()
    } else {
        let mut out = Vec::new();
        for n in &names {
            if let Some(b) = BENCHES.iter().find(|b| b.name == n) {
                out.push(b)
            } else {
                eprintln!("unknown bench: {n}");
                usage();
            }
        }
        out
    };

    (selected, iters, warmup, workers_override)
}

#[allow(clippy::large_enum_variant)]
enum Prepared {
    Source(&'static str),
    VmReady(Vm),
    Workers { src: &'static str, workers: usize },
}

fn prepare(bench: &Bench, workers_override: Option<usize>) -> Prepared {
    match bench.kind {
        BenchKind::Source(src) => Prepared::Source(src),
        BenchKind::VmOnlyCompileOnce(src) => {
            let program = optive::compile(src).expect("compile");
            let mut vm = Vm::new();
            vm.load_program(program).expect("load");
            Prepared::VmReady(vm)
        }
        BenchKind::Workers { src, workers } => Prepared::Workers {
            src,
            workers: workers_override.unwrap_or(workers),
        },
    }
}

fn run_prepared(bench_name: &str, prepared: &mut Prepared) -> String {
    match prepared {
        Prepared::Source(src) => run_source(src)
            .unwrap_or_else(|e| panic!("{bench_name}: {e}"))
            .display_string(),
        Prepared::VmReady(vm) => {
            vm.reset_execution();
            vm.run()
                .unwrap_or_else(|e| panic!("{bench_name}: {e}"))
                .display_string()
        }
        Prepared::Workers { src, workers } => {
            let mut vm = Vm::with_workers(*workers);
            run_source_in_vm(&mut vm, src, &format!("<bench-{bench_name}>"))
                .unwrap_or_else(|e| panic!("{bench_name}: {e}"))
                .display_string()
        }
    }
}

fn main() {
    let (benches, iters, warmup, workers_override) = parse_args();

    // 结构化摘要：samply 负责函数级采样；这里给 wall-time 对照。
    println!(
        "{{\"tool\":\"benchmark-analysis\",\"iters\":{iters},\"warmup\":{warmup},\"benches\":["
    );

    for (bi, bench) in benches.iter().enumerate() {
        let mut prepared = prepare(bench, workers_override);

        for _ in 0..warmup {
            let got = run_prepared(bench.name, &mut prepared);
            if let Some(exp) = bench.expect {
                assert_eq!(got, exp, "{} warmup mismatch", bench.name);
            }
        }

        let mut samples_ms = Vec::with_capacity(iters);
        for _ in 0..iters {
            let t0 = Instant::now();
            let got = run_prepared(bench.name, &mut prepared);
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if let Some(exp) = bench.expect {
                assert_eq!(got, exp, "{} result mismatch", bench.name);
            }
            samples_ms.push(ms);
        }

        let sum: f64 = samples_ms.iter().sum();
        let avg = sum / samples_ms.len() as f64;
        let min = samples_ms.iter().copied().fold(f64::INFINITY, f64::min);
        let max = samples_ms.iter().copied().fold(f64::NEG_INFINITY, f64::max);

        let samples_json = samples_ms
            .iter()
            .map(|x| format!("{x:.3}"))
            .collect::<Vec<_>>()
            .join(",");

        if bi > 0 {
            print!(",");
        }
        println!(
            "{{\"name\":\"{}\",\"avg_ms\":{:.3},\"min_ms\":{:.3},\"max_ms\":{:.3},\"samples_ms\":[{samples_json}]}}",
            bench.name, avg, min, max
        );
        eprintln!(
            "{}: avg={:.3}ms min={:.3}ms max={:.3}ms (n={})",
            bench.name,
            avg,
            min,
            max,
            samples_ms.len()
        );
    }

    println!("]}}");
}
