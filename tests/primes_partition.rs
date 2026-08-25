#![allow(clippy::unwrap_used, clippy::expect_used)]

use optive::run_source;
use optive::run_source_in_vm;
use optive::vm::Vm;

const IS_PRIME: &str = r"
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
";

#[test]
fn cyclic_parallel_matches_sequential_small() {
    let to = 10_001u32;
    let seq = format!(
        r"
const let TO = {to}
{IS_PRIME}
var total = 1
var n = 3
loop {{
  if (n > TO) {{ break }}
  if (is_prime(n)) {{ total = total + 1 }}
  n = n + 2
}}
total
"
    );
    let expected = run_source(&seq).unwrap().display_string();
    let tasks = 4usize;
    let par = format!(
        r"
const let TO = {to}
const let STEP = {tasks}
const let ODD_STEP = STEP + STEP
{IS_PRIME}
func worker(id, box, wg) {{
  var n = 3 + id * 2
  var local = 0
  if (id == 0) {{ local = 1 }}
  loop {{
    if (n > TO) {{ break }}
    if (is_prime(n)) {{ local = local + 1 }}
    n = n + ODD_STEP
  }}
  let g = box.lock()
  var rows = g.get()
  rows.append(local)
  g.set(rows)
  g.unlock()
  wg.done()
}}
func start_worker(id, box, wg) {{
  go do {{ worker(id, box, wg) }}
}}
let box = Mutex([])
let wg = WaitGroup(STEP)
var wid = 0
loop (STEP) {{
  start_worker(wid, box, wg)
  wid = wid + 1
}}
wg.wait()
let g = box.lock()
let rows = g.get()
g.unlock()
var total = 0
var i = 0
loop {{
  if (i >= rows.len()) {{ break }}
  total = total + rows[i]
  i = i + 1
}}
total
"
    );
    let mut vm = Vm::with_workers(tasks);
    let got = run_source_in_vm(&mut vm, &par, "<primes-part>")
        .unwrap()
        .display_string();
    assert_eq!(got, expected);
    assert!(
        vm.helper_runs() > 0,
        "M:N helpers must run at least one worker task, got {}",
        vm.helper_runs()
    );
}

/// 中等规模循环划分：并行路径必须真正跑起 helper（不做墙钟快慢断言）。
#[test]
fn cyclic_parallel_runs_helpers_medium() {
    if num_cpus::get() < 4 {
        return;
    }
    let to = 50_001u32;
    let seq = format!(
        r"
const let TO = {to}
{IS_PRIME}
var total = 1
var n = 3
loop {{
  if (n > TO) {{ break }}
  if (is_prime(n)) {{ total = total + 1 }}
  n = n + 2
}}
total
"
    );
    let t_seq = {
        let start = std::time::Instant::now();
        let _ = run_source(&seq).unwrap();
        start.elapsed()
    };
    let tasks = 4usize;
    let par = format!(
        r"
const let TO = {to}
const let STEP = {tasks}
const let ODD_STEP = STEP + STEP
{IS_PRIME}
func worker(id, box, wg) {{
  var n = 3 + id * 2
  var local = 0
  if (id == 0) {{ local = 1 }}
  loop {{
    if (n > TO) {{ break }}
    if (is_prime(n)) {{ local = local + 1 }}
    n = n + ODD_STEP
  }}
  let g = box.lock()
  var rows = g.get()
  rows.append(local)
  g.set(rows)
  g.unlock()
  wg.done()
}}
func start_worker(id, box, wg) {{
  go do {{ worker(id, box, wg) }}
}}
let box = Mutex([])
let wg = WaitGroup(STEP)
var wid = 0
loop (STEP) {{
  start_worker(wid, box, wg)
  wid = wid + 1
}}
wg.wait()
let g = box.lock()
let rows = g.get()
g.unlock()
var total = 0
var i = 0
loop {{
  if (i >= rows.len()) {{ break }}
  total = total + rows[i]
  i = i + 1
}}
total
"
    );
    let (t_par, helpers) = {
        let start = std::time::Instant::now();
        let mut vm = Vm::with_workers(tasks);
        let _ = run_source_in_vm(&mut vm, &par, "<primes-speed>").unwrap();
        (start.elapsed(), vm.helper_runs())
    };
    eprintln!("primes 50001: seq={t_seq:?} par/4={t_par:?} helpers={helpers}");
    assert!(
        helpers >= 2,
        "expected multiple helper task runs, got {helpers}"
    );
}
