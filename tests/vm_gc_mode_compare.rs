#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! STW vs concurrent GC 对照：分配风暴 + 后台 spinner（与素数基准无关）。
//!
//! 正确性：`cargo test --test vm_gc_mode_compare`
//! 对比表：`cargo test --test vm_gc_mode_compare compare_stw_vs_concurrent -- --ignored --nocapture`
//! 或：`cargo run --release --bin gc-mode-compare`

use std::sync::atomic::Ordering;
use std::time::Instant;

use optive::gc::GcMode;
use optive::run_source_in_vm;
use optive::value::Value;
use optive::vm::Vm;

/// 后台 spinner 在 `done` 前持续 `progress++`；主逻辑造短命环触发自动 GC。
/// 返回值 = 分配风暴期间的 spinner 推进量。
fn storm_src(spinners: usize, allocs: usize) -> String {
    format!(
        r"
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
",
    )
}

struct RunRow {
    mode: GcMode,
    wall_ms: f64,
    mut_ops: i64,
    collects: usize,
    stw_ms: f64,
    collect_ms: f64,
    cleared: usize,
    fallback: usize,
}

fn run_mode(mode: GcMode, workers: usize, src: &str, threshold: usize) -> RunRow {
    let mut vm = Vm::with_workers_gc(workers, mode).with_gc_threshold(threshold);
    let t0 = Instant::now();
    let v = run_source_in_vm(&mut vm, src, "<gc-mode-compare>").expect("run");
    let wall = t0.elapsed();
    let mut_ops = match v {
        Value::Num(n) => n.to_i64().unwrap_or(-1),
        other => panic!("expected num, got {}", other.display_string()),
    };
    RunRow {
        mode,
        wall_ms: wall.as_secs_f64() * 1000.0,
        mut_ops,
        collects: vm.gc.total_collects.load(Ordering::Relaxed),
        stw_ms: vm.gc.total_stw_ns.load(Ordering::Relaxed) as f64 / 1e6,
        collect_ms: vm.gc.total_collect_ns.load(Ordering::Relaxed) as f64 / 1e6,
        cleared: vm.gc.total_cleared.load(Ordering::Relaxed),
        fallback: vm.gc.stw_fallback_count.load(Ordering::Relaxed),
    }
}

#[test]
fn both_modes_run_alloc_storm() {
    let src = storm_src(2, 400);
    let stw = run_mode(GcMode::Stw, 2, &src, 128);
    let conc = run_mode(GcMode::Concurrent, 2, &src, 128);
    assert!(stw.collects >= 1, "stw collects={}", stw.collects);
    assert!(conc.collects >= 1, "conc collects={}", conc.collects);
    assert!(stw.cleared >= 1, "stw cleared={}", stw.cleared);
    assert!(conc.cleared >= 1, "conc cleared={}", conc.cleared);
    assert!(stw.mut_ops >= 0);
    assert!(conc.mut_ops >= 0);
}

#[test]
#[ignore]
fn compare_stw_vs_concurrent() {
    let workers = 2;
    // 小规模；完整对比用 release bin。
    let src = storm_src(4, 1200);
    let stw = run_mode(GcMode::Stw, workers, &src, 128);
    let conc = run_mode(GcMode::Concurrent, workers, &src, 128);

    println!();
    println!("=== STW vs concurrent：分配风暴 + 后台 spinner ===");
    println!("workers={workers}  spinners=4  allocs=1200  gc_threshold=128");
    println!(
        "{:<12} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "mode", "wall_ms", "stw_ms", "gc_ms", "collects", "cleared", "mut_ops", "fallback"
    );
    for row in [&stw, &conc] {
        println!(
            "{:<12} {:>10.2} {:>10.2} {:>10.2} {:>10} {:>10} {:>10} {:>10}",
            format!("{:?}", row.mode),
            row.wall_ms,
            row.stw_ms,
            row.collect_ms,
            row.collects,
            row.cleared,
            row.mut_ops,
            row.fallback
        );
    }
    if stw.stw_ms > 0.0 {
        println!("stw_ms  ratio conc/stw = {:.2}", conc.stw_ms / stw.stw_ms);
    }
    if stw.mut_ops > 0 {
        println!(
            "mut_ops ratio conc/stw = {:.2}",
            conc.mut_ops as f64 / stw.mut_ops as f64
        );
    }
    assert!(
        conc.mut_ops * 2 >= stw.mut_ops,
        "expected concurrent mut_ops (~{}) not much below stw (~{})",
        conc.mut_ops,
        stw.mut_ops
    );
    // 小阈值风暴下 concurrent 自适应 STW；墙钟仅作诊断，不作通过条件。
    println!("wall conc={:.2}ms stw={:.2}ms", conc.wall_ms, stw.wall_ms);
}
