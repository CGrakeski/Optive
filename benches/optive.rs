use criterion::{black_box, criterion_group, criterion_main, Criterion};
use optive::run_source;

const FIB: &str = r#"
func fib(n) {
    if (n <= 1) { return n }
    return fib(n - 1) + fib(n - 2)
}
fib(30)
"#;

const EMPTY_LOOP: &str = r#"
loop (1000000) { }
42
"#;

const ARITH_LOOP: &str = r#"
let sum = 0
loop (100000) { sum = sum + 1 }
sum
"#;

fn bench_fib(c: &mut Criterion) {
    c.bench_function("fib(30)", |b| {
        b.iter(|| {
            let v = run_source(FIB).unwrap();
            black_box(v);
        });
    });
}

fn bench_empty_loop(c: &mut Criterion) {
    c.bench_function("empty_loop(1_000_000)", |b| {
        b.iter(|| {
            let v = run_source(EMPTY_LOOP).unwrap();
            black_box(v);
        });
    });
}

fn bench_arith_loop(c: &mut Criterion) {
    c.bench_function("arith_loop(100_000)", |b| {
        b.iter(|| {
            let v = run_source(ARITH_LOOP).unwrap();
            black_box(v);
        });
    });
}

criterion_group!(benches, bench_fib, bench_empty_loop, bench_arith_loop);
criterion_main!(benches);
