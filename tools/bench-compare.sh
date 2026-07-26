#!/usr/bin/env bash
# 性能回归对比：在 base 分支与当前工作树上各跑一次 criterion 基准，输出对比。
#
# 用法：
#   tools/bench-compare.sh [base-ref]      # 默认 base = origin/main
#
# 依赖：已安装 cargo + criterion，且工作树干净（脚本会临时 stash）。
# 输出：criterion 的 --save-baseline / --baseline 对比报告，回归项会高亮。

set -euo pipefail

BASE="${1:-origin/main}"
BENCHES="${BENCHES:-}"   # 可选：限定 bench 名，如 "fib(30)"

echo "==> 性能回归对比：base=$BASE  HEAD=工作树"

# 1. 在 base 上跑一次，存为 baseline。
echo "==> [1/2] 在 $BASE 上构建并跑基准（保存为 baseline）..."
git stash push --include-untracked -q -m "bench-compare stash" || true
git checkout "$BASE" -- .
cargo bench --bench optive -- $BENCHES --save-baseline base
git checkout - -- .
git stash pop -q || true

# 2. 在工作树上跑一次，对比 baseline。
echo "==> [2/2] 在工作树上跑基准（对比 base）..."
cargo bench --bench optive -- $BENCHES --baseline base

echo "==> 完成。回归（regressed）项会在上方报告中标记。"
