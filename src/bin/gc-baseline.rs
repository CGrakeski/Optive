#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! GC 基线：对比 `stw` / `concurrent` 的停顿与总收集时间。
//!
//! ```text
//! cargo run --release --bin gc-baseline
//! OPTIVE_GC_MODE=concurrent cargo run --release --bin gc-baseline
//! cargo run --release --bin gc-baseline -- --load
//! ```
//!
//! - `stw_us`：mutator 被停住的累计时间（concurrent **不含**并发标记段）
//! - `collect_us`：整次 `gc()` 墙钟
//! - `mut_ops`：`--load` 时，`gc()` 调用期间后台任务推进的计数（越高越好）
//!
//! 基线会临时抬高 `OPTIVE_GC_THRESHOLD`，避免造环时被自动 GC 先清掉。

use std::env;
use std::sync::atomic::Ordering;

use optive::gc::GcMode;
use optive::run_source_in_vm;
use optive::vm::Vm;

fn main() {
    // 避免 make_cycles 中途触发 auto-gc，保证 cleared ≈ heap。
    env::set_var("OPTIVE_GC_THRESHOLD", "100000000");

    let mode = GcMode::from_env();
    let with_load = env::args().any(|a| a == "--load");
    println!("OPTIVE_GC_MODE={mode:?}  load={with_load}");
    println!(
        "{:>8} {:>8} {:>12} {:>12} {:>10} {:>10} {:>10}",
        "workers", "heap", "stw_us", "collect_us", "cleared", "fallback", "mut_ops"
    );

    for workers in [1usize, 2, 4] {
        for heap in [1024usize, 8192, 32768] {
            let row = run_case(workers, heap, mode, with_load);
            println!(
                "{:>8} {:>8} {:>12} {:>12} {:>10} {:>10} {:>10}",
                workers, heap, row.0, row.1, row.2, row.3, row.4
            );
        }
    }
}

fn run_case(
    workers: usize,
    heap: usize,
    mode: GcMode,
    with_load: bool,
) -> (u64, u64, i64, usize, i64) {
    let mut vm = Vm::with_workers_gc(workers, mode);
    let src = if with_load && workers > 1 {
        format!(
            r"
let counter = Mutex(0)
func spinner() {{
    loop {{
        with (counter.lock() as g) {{
            g.set(g.get() + 1)
        }}
        suspend
    }}
}}
func make_cycles(n) {{
    for (i in std.math.range(n)) {{
        let a = []
        a.append(a)
    }}
    return none
}}
let nspin = {w}
let spinners = [go spinner() for (_ in std.math.range(nspin))]
make_cycles({heap})
let before = counter.lock().get()
let cleared = gc()
let during = counter.lock().get() - before
for (t in spinners) {{
    t.cancel()
    handle await t
}}
[cleared, during]
",
            w = workers.saturating_mul(2),
            heap = heap
        )
    } else {
        format!(
            r"
func make_cycles(n) {{
    for (i in std.math.range(n)) {{
        let a = []
        a.append(a)
    }}
    return none
}}
make_cycles({heap})
let cleared = gc()
[cleared, 0]
"
        )
    };

    let v = run_source_in_vm(&mut vm, &src, "<gc-baseline>").expect("run");
    let (cleared, mut_ops) = match v {
        optive::value::Value::List(rc) => {
            let items = rc.borrow();
            let c = match items.first() {
                Some(optive::value::Value::Num(n)) => n.to_i64().unwrap_or(0),
                _ => -1,
            };
            let m = match items.get(1) {
                Some(optive::value::Value::Num(n)) => n.to_i64().unwrap_or(0),
                _ => 0,
            };
            (c, m)
        }
        _ => (-1, 0),
    };
    let stw_ns = vm.gc.last_stw_ns.load(Ordering::Relaxed);
    let collect_ns = vm.gc.last_collect_ns.load(Ordering::Relaxed);
    let fallback = vm.gc.stw_fallback_count.load(Ordering::Relaxed);
    (stw_ns / 1000, collect_ns / 1000, cleared, fallback, mut_ops)
}
