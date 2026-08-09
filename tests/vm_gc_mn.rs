//! M:N / concurrent GC 环收集回归。

use optive::run_source;
use optive::vm::Vm;
use optive::value::Value;

fn assert_cleared_at_least(src: &str, workers: usize, min: i64) {
    let mut vm = Vm::with_workers(workers);
    let v = optive::run_source_in_vm(&mut vm, src, "<gc-test>").expect("run");
    match v {
        Value::Num(n) => {
            let c = n.to_i64().unwrap_or(0);
            assert!(c >= min, "workers={workers}: expected cleared >= {min}, got {c}");
        }
        other => panic!("expected num, got {}", other.display_string()),
    }
}

const CYCLE_SRC: &str = r#"
func make_cycle() {
    let a = []
    a.append(a)
    return none
}
make_cycle()
gc()
"#;

#[test]
fn gc_breaks_list_cycle_m1() {
    assert_cleared_at_least(CYCLE_SRC, 1, 1);
}

#[test]
fn gc_breaks_list_cycle_mn2() {
    assert_cleared_at_least(CYCLE_SRC, 2, 1);
}

#[test]
fn gc_breaks_list_cycle_mn4() {
    assert_cleared_at_least(CYCLE_SRC, 4, 1);
}

#[test]
fn gc_concurrent_mode_breaks_cycle() {
    let mut vm = Vm::with_workers_gc(2, optive::gc::GcMode::Concurrent);
    let v = optive::run_source_in_vm(&mut vm, CYCLE_SRC, "<gc-conc>").expect("run");
    match v {
        Value::Num(n) => assert!(n.to_i64().unwrap_or(0) >= 1),
        other => panic!("expected num, got {}", other.display_string()),
    }
}

#[test]
fn gc_suspended_fiber_cycle_survives_until_join() {
    // Suspended fiber holds a cycle on its stack; GC must not clear it.
    // 返回 (await 结果, gc 清扫数)：纤程根丢失时 len 会变成 0 或 await 失败。
    let src = r#"
func hold() {
    let a = []
    a.append(a)
    suspend
    return a.len()
}
let t = go hold()
let cleared = gc()
let n = await t
[n, cleared]
"#;
    let mut vm = Vm::with_workers(2);
    let v = optive::run_source_in_vm(&mut vm, src, "<gc-fiber>").expect("run");
    let Value::List(items) = v else {
        panic!("expected [n, cleared], got {}", v.display_string());
    };
    let items = items.borrow();
    assert_eq!(items.len(), 2, "expected pair, got {}", items.len());
    match &items[0] {
        Value::Num(n) => {
            let len = n.to_i64().expect("cycle length");
            assert_eq!(len, 1, "suspended fiber cycle must survive until join");
        }
        other => panic!("expected await len, got {}", other.display_string()),
    }
    match &items[1] {
        Value::Num(n) => {
            let cleared = n.to_i64().expect("cleared count");
            assert_eq!(
                cleared, 0,
                "GC must not clear the cycle still rooted by the suspended fiber"
            );
        }
        other => panic!("expected cleared count, got {}", other.display_string()),
    }
}

#[test]
fn gc_many_cycles_under_workers() {
    let src = r#"
func make_n(n) {
    for (i in std.math.range(n)) {
        let a = []
        a.append(a)
    }
    return none
}
make_n(64)
gc()
"#;
    assert_cleared_at_least(src, 4, 1);
}

#[test]
fn gc_smoke_default() {
    let _ = run_source("gc()").expect("gc");
}
