//! STW vs concurrent：分配风暴 + 后台 spinner（独立于并行素数）。
//!
//! ```text
//! cargo run --release --bin gc-mode-compare
//! cargo run --release --bin gc-mode-compare -- --workers 2 --allocs 2000 --spinners 4
//! ```

use std::env;
use std::sync::atomic::Ordering;
use std::time::Instant;

use optive::gc::GcMode;
use optive::run_source_in_vm;
use optive::value::Value;
use optive::vm::Vm;

fn main() {
    let workers = arg_usize("--workers", 2);
    let spinners = arg_usize("--spinners", 4);
    let allocs = arg_usize("--allocs", 2_000);
    let threshold = arg_usize("--threshold", 128);

    let src = storm_src(spinners, allocs);
    println!("workers={workers} spinners={spinners} allocs={allocs} threshold={threshold}");
    println!(
        "{:<12} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "mode", "wall_ms", "stw_ms", "gc_ms", "collects", "cleared", "mut_ops", "fallback"
    );

    let mut stw_ops = 0i64;
    let mut conc_ops = 0i64;
    for mode in [GcMode::Stw, GcMode::Concurrent] {
        let row = run_mode(mode, workers, &src, threshold);
        if mode == GcMode::Stw {
            stw_ops = row.6;
        } else {
            conc_ops = row.6;
        }
        println!(
            "{:<12} {:>10.2} {:>10.2} {:>10.2} {:>10} {:>10} {:>10} {:>10}",
            format!("{:?}", row.0),
            row.1,
            row.2,
            row.3,
            row.4,
            row.5,
            row.6,
            row.7
        );
    }
    if stw_ops > 0 {
        println!(
            "mut_ops ratio concurrent/stw = {:.2}",
            conc_ops as f64 / stw_ops as f64
        );
    }
}

fn storm_src(spinners: usize, allocs: usize) -> String {
    format!(
        r#"
let progress = Mutex(0)
let done = Mutex(false)
func spinner() {{
    loop {{
        if (done.lock().get()) {{ break }}
        with (progress.lock() as g) {{
            g.set(g.get() + 1)
        }}
        suspend
    }}
    return none
}}
func alloc_storm(n) {{
    var i = 0
    loop {{
        if (i >= n) {{ break }}
        let a = []
        a.append(a)
        if (i % 32 == 0) {{ suspend }}
        i = i + 1
    }}
    return none
}}
let spinners = [go spinner() for (_ in std.math.range({spinners}))]
let before = progress.lock().get()
alloc_storm({allocs})
let during = progress.lock().get() - before
with (done.lock() as g) {{ g.set(true) }}
for (t in spinners) {{
    await t
}}
during
"#
    )
}

fn run_mode(
    mode: GcMode,
    workers: usize,
    src: &str,
    threshold: usize,
) -> (GcMode, f64, f64, f64, usize, usize, i64, usize) {
    let mut vm = Vm::with_workers_gc(workers, mode).with_gc_threshold(threshold);
    let t0 = Instant::now();
    let v = run_source_in_vm(&mut vm, src, "<gc-mode-compare>").expect("run");
    let wall_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mut_ops = match v {
        Value::Num(n) => n.to_i64().unwrap_or(-1),
        other => panic!("expected num, got {}", other.display_string()),
    };
    (
        mode,
        wall_ms,
        vm.gc.total_stw_ns.load(Ordering::Relaxed) as f64 / 1e6,
        vm.gc.total_collect_ns.load(Ordering::Relaxed) as f64 / 1e6,
        vm.gc.total_collects.load(Ordering::Relaxed),
        vm.gc.total_cleared.load(Ordering::Relaxed),
        mut_ops,
        vm.gc.stw_fallback_count.load(Ordering::Relaxed),
    )
}

fn arg_usize(flag: &str, default: usize) -> usize {
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        if a == flag {
            if let Some(v) = args.next() {
                return v.parse().unwrap_or(default);
            }
        }
    }
    default
}
