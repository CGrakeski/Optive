#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! Regression tests for review fixes: Once race, Barrier/Cond suspend, generic `TypeSpec`, struct seal.

mod common;

use common::{assert_num, run_err, value};
use optive::run_source_in_vm;
use optive::vm::Vm;

#[test]
fn once_run_caches_under_parallel_go() {
    // Many concurrent Once.run should all see the same value; bump runs once.
    let src = r"
let o = Once()
let counter = Mutex(0)
func bump() {
  let g = counter.lock()
  g.set(g.get() + 1)
  let v = g.get()
  g.unlock()
  return 100 + v
}
let wg = WaitGroup(16)
var i = 0
loop (16) {
  go do {
    let _ = o.run(bump)
    wg.done()
  }
  i = i + 1
}
wg.wait()
let a = o.run(bump)
let g = counter.lock()
let n = g.get()
g.unlock()
a + n
";
    let mut vm = Vm::with_workers(4);
    let v = run_source_in_vm(&mut vm, src, "<once-par>").expect("run");
    // bump once → 101; counter == 1 → total 102
    assert_eq!(v.display_string(), "102");
}

#[test]
fn barrier_with_suspend_does_not_release_early() {
    // 3 parties; one fiber suspends while waiting. Must not fire until all 3 wait.
    let src = r"
let b = Barrier(3)
let box = Mutex(0)
let wg = WaitGroup(2)
go do {
  suspend
  b.wait()
  let g = box.lock()
  g.set(g.get() + 1)
  g.unlock()
  wg.done()
}
go do {
  b.wait()
  let g = box.lock()
  g.set(g.get() + 1)
  g.unlock()
  wg.done()
}
b.wait()
wg.wait()
let g = box.lock()
g.set(g.get() + 10)
let n = g.get()
g.unlock()
n
";
    assert_eq!(value(src).display_string(), "12");
}

#[test]
fn generic_struct_typespec_rejects_wrong_args() {
    let src = r#"
struct Box[T] { let value }
func take(x:: Box[num]) { return x.value }
let b = Box[text]("hi")
take(b)
"#;
    run_err(src);
}

#[test]
fn struct_field_list_contract_seals_append() {
    let src = r#"
typed struct Holder {
  var xs:: list[num]
}
let h = Holder([1])
h.xs.append("bad")
"#;
    run_err(src);
}

#[test]
fn once_sequential_still_works() {
    assert_num(
        r"
let o = Once()
let counter = Mutex(0)
func bump() {
  let g = counter.lock()
  g.set(g.get() + 1)
  let v = g.get()
  g.unlock()
  return v
}
let a = o.run(bump)
let b = o.run(bump)
let g = counter.lock()
let n = g.get()
g.unlock()
a + b + n
",
        "3",
    );
}
