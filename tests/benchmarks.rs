//! Performance benchmarks with statistical reporting.
//! Run with: cargo test --test benchmarks -- --nocapture

mod common;

use std::time::Instant;

use optive::run_source;

pub struct BenchStats {
    pub name: String,
    pub runs: usize,
    pub samples_ms: Vec<f64>,
}

impl BenchStats {
    pub fn run<F: FnMut()>(name: impl Into<String>, runs: usize, mut f: F) -> Self {
        let name = name.into();
        let mut samples_ms = Vec::with_capacity(runs);
        for _ in 0..runs {
            let start = Instant::now();
            f();
            samples_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        Self {
            name,
            runs,
            samples_ms,
        }
    }

    pub fn avg_ms(&self) -> f64 {
        self.samples_ms.iter().sum::<f64>() / self.runs as f64
    }

    pub fn min_ms(&self) -> f64 {
        self.samples_ms.iter().copied().fold(f64::INFINITY, f64::min)
    }

    pub fn max_ms(&self) -> f64 {
        self.samples_ms.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    }

    pub fn variance_ms(&self) -> f64 {
        let avg = self.avg_ms();
        self.samples_ms
            .iter()
            .map(|x| {
                let d = x - avg;
                d * d
            })
            .sum::<f64>()
            / self.runs as f64
    }

    pub fn stddev_ms(&self) -> f64 {
        self.variance_ms().sqrt()
    }

    pub fn report(&self) {
        println!(
            "{}: runs={}, avg={:.3}ms, min={:.3}ms, max={:.3}ms, stddev={:.3}ms, variance={:.3}ms²",
            self.name,
            self.runs,
            self.avg_ms(),
            self.min_ms(),
            self.max_ms(),
            self.stddev_ms(),
            self.variance_ms()
        );
    }
}

const FIB_SRC: &str = r#"
func fib(n) {
    if (n <= 1) { return n }
    return fib(n - 1) + fib(n - 2)
}
fib(30)
"#;

const EMPTY_LOOP_SRC: &str = r#"
loop (1000000) { }
42
"#;

const ARITH_LOOP_SRC: &str = r#"
let sum = 0
loop (100000) {
    sum = sum + 1
}
sum
"#;

const CALL_LOOP_SRC: &str = r#"
func id(x) { return x }
let n = 0
loop (50000) {
    n = id(n + 1)
}
n
"#;

const NESTED_LOOP_1B_SRC: &str = r#"
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
"#;

/// Run ignored benchmarks: `cargo test --test benchmarks -- --ignored --nocapture`
#[test]
#[ignore = "slow benchmark; run with --ignored"]
fn bench_fib_30() {
    let stats = BenchStats::run("fib(30)", 20, || {
        let v = run_source(FIB_SRC).expect("fib");
        assert_eq!(v.display_string(), "832040");
    });
    stats.report();
}

#[test]
#[ignore = "slow benchmark; run with --ignored"]
fn bench_fib_30_vm_only() {
    let program = optive::compile(FIB_SRC).expect("compile");
    let mut vm = optive::vm::Vm::new();
    vm.load_program(program).expect("load_program");
    let stats = BenchStats::run("fib(30) vm-only", 20, || {
        vm.reset_execution();
        let v = vm.run().expect("fib");
        assert_eq!(v.display_string(), "832040");
    });
    stats.report();
}

#[test]
#[ignore = "slow benchmark; run with --ignored"]
fn bench_empty_loop_1m() {
    let stats = BenchStats::run("empty_loop(1_000_000)", 10, || {
        let v = run_source(EMPTY_LOOP_SRC).expect("loop");
        assert_eq!(v.display_string(), "42");
    });
    stats.report();
}

#[test]
#[ignore = "slow benchmark; run with --ignored"]
fn bench_arith_loop_100k() {
    let stats = BenchStats::run("arith_loop(100_000)", 15, || {
        let v = run_source(ARITH_LOOP_SRC).expect("arith");
        assert_eq!(v.display_string(), "100000");
    });
    stats.report();
}

#[test]
#[ignore = "slow benchmark; run with --ignored"]
fn bench_function_call_50k() {
    let stats = BenchStats::run("function_call_loop(50_000)", 15, || {
        let v = run_source(CALL_LOOP_SRC).expect("calls");
        assert_eq!(v.display_string(), "50000");
    });
    stats.report();
}

#[test]
#[ignore = "slow benchmark; run with --ignored"]
fn bench_parse_only_1k() {
    let src = "1 + 2 * 3\n";
    let stats = BenchStats::run("parse(1+2*3) x1000", 20, || {
        for _ in 0..1000 {
            optive::parse_program(src).expect("parse");
        }
    });
    stats.report();
}

#[test]
#[ignore = "slow benchmark; run with --ignored"]
fn bench_compile_run_simple() {
    let src = "func f(x) { return x + 1 }\nf(41)\n";
    let stats = BenchStats::run("compile+run(simple)", 30, || {
        let v = run_source(src).expect("run");
        assert_eq!(v.display_string(), "42");
    });
    stats.report();
}

#[test]
#[ignore = "slow benchmark; run with --ignored"]
fn bench_nested_loop_1b_vm_only() {
    let program = optive::compile(NESTED_LOOP_1B_SRC).expect("compile");
    let mut vm = optive::vm::Vm::new();
    vm.load_program(program).expect("load_program");

    let start = Instant::now();
    vm.reset_execution();
    let v = vm.run().expect("nested loop");
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    assert_eq!(v.display_string(), "42");
    println!(
        "nested_loop(1000x1000x1000) vm-only: 1 run, {:.3}ms ({:.3}s)",
        elapsed_ms,
        elapsed_ms / 1000.0
    );
}
