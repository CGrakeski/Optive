#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! 并行 FFI：per-callable 锁重叠 + 可选卸荷池。

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

use optive::run_source_in_vm;
use optive::value::Value;
use optive::vm::Vm;

fn build_sleep_dll() -> PathBuf {
    static DLL: OnceLock<PathBuf> = OnceLock::new();
    DLL.get_or_init(|| {
        let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ffi_sleep_dll");
        fs::create_dir_all(&out_dir).expect("create dll dir");
        let src = out_dir.join("sleeping.rs");
        fs::write(
            &src,
            r#"
use std::thread;
use std::time::Duration;

#[no_mangle]
pub extern "C" fn sleep_ms_a(ms: u32) {
    thread::sleep(Duration::from_millis(ms as u64));
}

#[no_mangle]
pub extern "C" fn sleep_ms_b(ms: u32) {
    thread::sleep(Duration::from_millis(ms as u64));
}

#[no_mangle]
pub extern "C" fn sleep_ms(ms: u32) {
    thread::sleep(Duration::from_millis(ms as u64));
}
"#,
        )
        .expect("write sleeping.rs");

        #[cfg(windows)]
        let lib_name = "sleeping.dll";
        #[cfg(target_os = "macos")]
        let lib_name = "libsleeping.dylib";
        #[cfg(all(unix, not(target_os = "macos")))]
        let lib_name = "libsleeping.so";

        let lib_path = out_dir.join(lib_name);
        let status = Command::new("rustc")
            .args(["--crate-type", "cdylib", "-O", "-o"])
            .arg(&lib_path)
            .arg(&src)
            .status()
            .expect("spawn rustc");
        assert!(status.success(), "rustc failed to build sleep DLL");
        lib_path
    })
    .clone()
}

fn dll_path_literal() -> String {
    let p = build_sleep_dll();
    let s = p.to_string_lossy().replace('\\', "/");
    format!("\"{s}\"")
}

fn dual_sleep_source(ms: u32) -> String {
    let path = dll_path_literal();
    format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h, "sleep_ms_a") func sleep_a(implicit ms: C.types.uint32_t) ...
extern(h, "sleep_ms_b") func sleep_b(implicit ms: C.types.uint32_t) ...
let t1 = go sleep_a({ms})
let t2 = go sleep_b({ms})
await t1
await t2
1
"#
    )
}

#[test]
fn parallel_distinct_symbols_overlap_wall_clock() {
    let src = dual_sleep_source(200);
    let mut vm = Vm::with_workers(4).with_ffi_serial(false);
    let t0 = Instant::now();
    let v = run_source_in_vm(&mut vm, &src, "<ffi_parallel>").expect("run");
    let elapsed = t0.elapsed();
    assert!(matches!(v, Value::Num(_)), "got {v:?}");
    // 并行：≈200ms；串行：≈400ms。松弛上界 320ms。
    assert!(
        elapsed.as_millis() < 320,
        "expected overlap (~200ms), got {}ms (serial would be ~400ms)",
        elapsed.as_millis()
    );
}

#[test]
fn serial_mode_forces_no_overlap() {
    let src = dual_sleep_source(150);
    let mut vm = Vm::with_workers(4).with_ffi_serial(true);
    let t0 = Instant::now();
    let v = run_source_in_vm(&mut vm, &src, "<ffi_serial>").expect("run");
    let elapsed = t0.elapsed();
    assert!(matches!(v, Value::Num(_)), "got {v:?}");
    // 全局锁串行：两段 sleep 相加；下界略松（调度开销）。
    assert!(
        elapsed.as_millis() >= 250,
        "expected serial (~300ms), got {}ms",
        elapsed.as_millis()
    );
}

#[test]
fn offload_pool_lets_other_fibers_progress() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h, "sleep_ms") func sleep_ms(implicit ms: C.types.uint32_t) ...
var progressed = 0
let sleeper = go sleep_ms(300)
let ticker = go do {{
    var i = 0
    while (i < 50) {{
        progressed = progressed + 1
        i = i + 1
        suspend
    }}
    return progressed
}}
let n = await ticker
await sleeper
n
"#
    );
    let mut vm = Vm::with_workers(2).with_ffi_threads(2).with_ffi_serial(false);
    let t0 = Instant::now();
    let v = run_source_in_vm(&mut vm, &src, "<ffi_offload>").expect("run");
    let elapsed = t0.elapsed();
    match v {
        Value::Num(n) => {
            let n = n.to_i64().unwrap_or(0);
            assert!(
                n >= 10,
                "ticker should progress during offloaded sleep, got {n}"
            );
        }
        other => panic!("expected num, got {other:?}"),
    }
    // 有卸荷时 ticker 与 sleep 重叠；整段不应远超 sleep。
    assert!(
        elapsed.as_millis() < 900,
        "offload should not serialize wall clock excessively, got {}ms",
        elapsed.as_millis()
    );
}

#[test]
fn ptr_registry_concurrent_alloc_free() {
    // 压力：多 fiber 并发 alloc/free；不在 free 后再查 ptr_live（地址可能被其它 fiber 立刻复用）。
    let src = r"
use std.language.{ C }
var n = 0
let tasks = []
var i = 0
while (i < 32) {
    tasks.append(go do {
        let p = C.alloc(64)
        if (not C.ptr_live(p)) { return 0 }
        C.free(p)
        return 1
    })
    i = i + 1
}
for (t in tasks) {
    n = n + (await t)
}
n
";
    let mut vm = Vm::with_workers(4);
    let v = run_source_in_vm(&mut vm, src, "<ptr_stress>").expect("run");
    match v {
        Value::Num(num) => assert_eq!(num.to_i64().unwrap_or(0), 32),
        other => panic!("expected 32, got {other:?}"),
    }
}

#[test]
#[ignore = "manual bench: compare serial vs parallel wall clock"]
fn bench_serial_vs_parallel_sleep() {
    let src = dual_sleep_source(100);
    for serial in [true, false] {
        let mut vm = Vm::with_workers(4).with_ffi_serial(serial);
        let t0 = Instant::now();
        run_source_in_vm(&mut vm, &src, "<bench>").expect("run");
        eprintln!(
            "serial={serial} elapsed_ms={}",
            t0.elapsed().as_millis()
        );
    }
}
