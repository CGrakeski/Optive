#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use optive::vm::Vm;

const CASES: &[(&str, &str, &str)] = &[
    (
        "arith_loop_200k",
        r#"
var total = 0
var i = 0
loop (200000) {
    total = total + i
    i = i + 1
}
total
"#,
        "19999900000",
    ),
    (
        "function_calls_100k",
        r#"
func step(x) { return x + 1 }
var value = 0
loop (100000) { value = step(value) }
value
"#,
        "100000",
    ),
    (
        "recursive_fib_25",
        r#"
func fib(n) {
    if (n < 2) { return n }
    return fib(n - 1) + fib(n - 2)
}
fib(25)
"#,
        "75025",
    ),
    (
        "recursive_fib_30",
        r#"
func fib(n) {
    if (n < 2) { return n }
    return fib(n - 1) + fib(n - 2)
}
fib(30)
"#,
        "832040",
    ),
    (
        "nested_loop_500x500",
        r#"
var total = 0
var i = 0
loop (500) {
    var j = 0
    loop (500) {
        total = total + i + j
        j = j + 1
    }
    i = i + 1
}
total
"#,
        "124750000",
    ),
    (
        "list_map_filter_20k",
        r#"
let values = std.math.range(20000)
let mapped = [x * 3 for (x in values) if (x % 2 == 0)]
len(mapped)
"#,
        "10000",
    ),
    (
        "list_build_50k",
        r#"
let values = [x * x for (x in std.math.range(50000))]
len(values)
"#,
        "50000",
    ),
    (
        "dict_counter_20k",
        r#"
var counts = {}
for (x in std.math.range(20000)) {
    let key = text.(x % 100)
    counts[key] = std.dict.get(counts, key, 0) + 1
}
counts["42"]
"#,
        "200",
    ),
    (
        "dict_lookup_100k",
        r#"
var values = {}
for (x in std.math.range(1000)) { values[text.(x)] = x }
var total = 0
for (x in std.math.range(100000)) {
    total = total + values[text.(x % 1000)]
}
total
"#,
        "49950000",
    ),
    (
        "string_join_10k",
        r#"
let parts = [text.(x) for (x in std.math.range(10000))]
len(std.text.join(",", parts))
"#,
        "48889",
    ),
    (
        "text_conversion_20k",
        r#"
var total = 0
for (x in std.math.range(20000)) { total = total + len(text.(x)) }
total
"#,
        "88890",
    ),
    (
        "json_roundtrip_2k",
        r#"
let source = [x for (x in std.math.range(2000))]
let encoded = std.json.stringify(source)
len(std.json.parse(encoded))
"#,
        "2000",
    ),
    (
        "exception_catch_10k",
        r#"
var caught = 0
loop (10000) {
    try { throw ValueError("x") } catch (e) { caught = caught + 1 }
}
caught
"#,
        "10000",
    ),
];

fn bench_case(c: &mut Criterion, name: &str, source: &str, expected: &str) {
    let program = optive::compile(source).expect("benchmark source must compile");
    let mut vm = Vm::with_workers(1);
    vm.source_file = "<language-compare>".into();
    vm.current_source = Some(Arc::from(source));
    vm.load_program(program)
        .expect("benchmark program must load");

    let warm = vm.run().expect("benchmark warmup must run");
    assert_eq!(warm.display_string(), expected, "wrong result for {name}");

    c.bench_function(&format!("language_compare/{name}"), |b| {
        b.iter(|| {
            vm.reset_script_bindings();
            black_box(vm.run().expect("benchmark iteration must run"));
        });
    });
}

fn language_compare(c: &mut Criterion) {
    for &(name, source, expected) in CASES {
        bench_case(c, name, source, expected);
    }
}

fn lifecycle(c: &mut Criterion) {
    const SOURCE: &str = r#"
func square(x) { return x * x }
let values = [square(x) for (x in std.math.range(1000))]
len(values)
"#;

    c.bench_function("lifecycle/compile_medium", |b| {
        b.iter(|| black_box(optive::compile(black_box(SOURCE)).expect("source must compile")));
    });

    c.bench_function("lifecycle/fresh_vm_compile_run", |b| {
        b.iter(|| {
            let mut vm = Vm::with_workers(1);
            vm.source_file = "<lifecycle>".into();
            vm.current_source = Some(Arc::from(SOURCE));
            let program = optive::compile(black_box(SOURCE)).expect("source must compile");
            vm.load_program(program).expect("program must load");
            black_box(vm.run().expect("program must run"))
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = language_compare, lifecycle
}
criterion_main!(benches);
