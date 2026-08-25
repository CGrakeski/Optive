# Benchmarks

Criterion harness: `cargo bench --bench optive`.

Tracked kernels: `fib(30)`, empty loop, arithmetic loop, parallel primes.

- `parallel_primes_to_10001`：小核，固定 8 个 `go` 切块，每次迭代新建 VM。测启动税，**不要**用来谈加速比。
- `primes_to_100001`：公平加速比。
  - `sequential`：无 `go`，只扫奇数（2 单独计入），`workers=1`
  - `par/2` `par/4` `par/8`：`go` 个数 = OS worker，奇数轮转 `n = 3+2*id; n += 2N`
  - 不要用 `n = 2+id; n += N`（N 为偶数时一半 worker 只碰到偶数，试除几乎全在奇数任务上，`par/2` 上限约 1×）
  - 采样前编译并建 VM；每次迭代 `reset_script_bindings` + `run`
  - 加速比 = \(T_{\text{sequential}} / T_{\text{par/N}}\)。`N` 不要超过物理核。

M:N 与这组核相关的运行时：独占 CPU `go` 不再每 8192 tick 切纤程；新任务进全局 injector；helper 在任务开始时拷贝脚本全局槽并本地化函数热码，避免热路径抢 `SharedMap` / 跨核 `Arc`；STW 只在预算耗尽时 poll，热回跳不再每条读 `stw_requested`。

本机 `cargo bench --bench optive -- primes_to_100001` 一例（release，公平核，i7-11700K 8 物理核）：

| 配置 | 时间 | 加速比 |
|---|---|---|
| sequential | ~53.8 ms | 1.00× |
| par/2 | ~29.8 ms | **1.80×** |
| par/4 | ~17.5 ms | **3.07×** |
| par/8 | ~12.0 ms | **4.49×** |

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
