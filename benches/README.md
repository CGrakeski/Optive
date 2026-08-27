# Benchmarks

Criterion harness: `cargo bench --bench optive`.

Tracked kernels: `fib(30)`, empty loop, arithmetic loop, function-call loop,
channel ping-pong, parallel primes.

- `function_call_loop(50_000)`：`id(n+1)` 浅调用。
- `channel_ping_20000`：`workers=1` / `workers=4`，容量 1 的 Channel 乒乓（调度税，不是算术）。
- `parallel_primes_to_10001`：小核，固定 8 个 `go` 切块，每次迭代新建 VM。测启动税，**不要**用来谈加速比。
- `parallel_primes_to_50001`：与文档第 2.1–2.3 节同核（`[2, 50001]`、固定 8 个 `go`，OS worker = 1/2/4/8）。每次迭代新建 VM / 线程池，测启动税；8 worker 可以慢于 4。
- `primes_to_100001`：公平加速比。
  - `sequential`：无 `go`，只扫奇数（2 单独计入），`workers=1`
  - `par/2` `par/4` `par/8`：`go` 个数 = OS worker，奇数轮转 `n = 3+2*id; n += 2N`
  - 不要用 `n = 2+id; n += N`（N 为偶数时一半 worker 只碰到偶数，试除几乎全在奇数任务上，`par/2` 上限约 1×）
  - 采样前编译并建 VM；每次迭代 `reset_script_bindings` + `run`
  - 加速比 = \(T_{\text{sequential}} / T_{\text{par/N}}\)。`N` 不要超过物理核。

M:N 与这组核相关的运行时：独占 CPU `go` 不再每 8192 tick 切纤程；新任务进全局 injector；helper 在任务开始时拷贝脚本全局槽并本地化函数热码，避免热路径抢 `SharedMap` / 跨核 `Arc`；STW 只在预算耗尽时 poll，热回跳不再每条读 `stw_requested`。

本机全量 `cargo bench --bench optive`（2026-08-26 稍后，release，i7-11700K 8 物理核；Criterion 点估计）：

公平核 `primes_to_100001`（复用 VM）：

| 配置 | 时间 | 加速比 |
|---|---|---|
| sequential | 34.8 ms | 1.00× |
| par/2 | 17.6 ms | **1.98×** |
| par/4 | 10.9 ms | **3.20×** |
| par/8 | 7.08 ms | **4.91×** |

切块 `parallel_primes_to_50001`（每次新建线程池；相对 1 worker）：21.7 / 14.1 / 10.6 / 29.5 ms → 1.00× / 1.53× / **2.04×** / 0.73×。

```bash
cargo bench --bench optive -- primes_to_100001
python docs/OptiveDocs/_bench/bench_python_fair.py
```

Python 对照：`bench_python_fair.py`（`timeit`，同一奇数轮转，π=9592）。`bench_python.py` / `bench_python_parallel.py` 仍是 `2..50001` 切块，**不是**同一组核。

## Regression

`tests/bench_regression.rs` runs small kernels under a generous wall-clock ceiling so CI catches catastrophic slowdowns, not micro-jitter.

JIT/AOT is **not** in this release train. Revisit only if this baseline plus `OPTIVE_METRICS=1` opcode/GC/call samples show the interpreter cannot meet a documented target.

```bash
OPTIVE_METRICS=1 cargo run --release --bin Optive -- -c "loop (100000) { }"
```
