# Optive

[![License: MulanPSL-2.0](https://img.shields.io/badge/license-MulanPSL--2.0-blue.svg)](LICENSE)

**Optive** 是一门动态、表达式丰富的脚本语言，用 Rust 实现的解释器：词法分析 → 语法分析 → 字节码 VM。支持软/硬类型注解、泛型、协议、管道、模式匹配、宏，以及通过 `extern` 调用 C ABI 动态库；并自带基于 Git 的依赖管理（`Optive.toml` / `Optive.lock` / `Optive.cache`）。

源文件扩展名：`.tive`。当前版本：**0.2.0**。

---

## 特性

- **表达式优先**：几乎所有构造都是表达式（`if` / `loop` / `match` / 块都有值），顶层结果非 `none` 自动打印。
- **渐进式类型**：可写软类型注解（解释期检查）或硬类型注解（编译期单态化），也可完全不写。
- **泛型与协议**：`protocol` 定义接口，`struct`/`enum`/`variant` 实现之；编译器按需单态化生成专用字节码。
- **元编程**：`macro` / `quote` 宏系统，编译期展开。
- **C 互操作**：`extern` 加载 `.dll`/`.so`/`.dylib`；护照指针、`typed struct : C.layout`、字符串编组、沙箱 FFI 门禁（见 [`docs/ffi-c.md`](docs/ffi-c.md)）。
- **依赖管理**：Git URL 作为依赖源，内容寻址存储（CAS）+ SQLite 索引，`Optive.lock` 保证可复现构建。
- **REPL**：交互式多行输入，历史记录持久化。
- **并发**：Go 风格 `go` / `await` / `Channel` / `select` / `par`（无 `async func`）；默认 M:1，设 `OPTIVE_WORKERS` 可升至 M:N；取消与 `std.async` / `std.sync` 见文档。

---

## 快速开始

### 从 Release 安装

到 [Releases](https://github.com/Optive/Optive/releases) 下载对应平台的压缩包，解压后把 `Optive`（或 `Optive.exe`）放进 `PATH`。

### 从源码构建

```bash
git clone https://github.com/Optive/Optive.git
cd Optive
cargo build --release --bin Optive
# 二进制位于 target/release/Optive(.exe)
```

> 需要 Rust stable。`rusqlite` 用 `bundled` 特性自带 SQLite，无需系统依赖。

### 三分钟上手

```bash
# 1) 交互式 REPL
Optive
>>> let x = 1 + 2
>>> x * 3
9
>>> :quit

# 2) 直接跑一个脚本
echo 'print("你好，Optive")' > hello.tive
Optive hello.tive

# 3) 用项目脚手架 + 依赖管理
Optive new my_app
cd my_app
Optive add file:///D:/some/local/repo   # 加一个 Git 依赖
Optive run                                # 严格按 Optive.lock 跑
Optive up                                 # update + run（跟随 tip）
```

---

## 依赖管理三件套

| 文件 | 角色 | 提交到 Git？ |
|------|------|:---:|
| `Optive.toml` | **意图**：声明依赖（URL、版本约束、是否跟随 tip） | ✅ |
| `Optive.lock` | **复现**：锁定每个依赖到具体 commit，全平台一致 | ✅ |
| `Optive.cache` | **本地**：依赖 tip/id 小抄（**不是**源码/字节码缓存；改 `src/*.tive` 下次 `run` 会重新读盘编译） | ❌（加入 `.gitignore`） |
| `Custom.toml` | **定制**：项目选用的定制包链（`use = [...]`） | 可选 |

依赖本体存在全局 `pack/` 目录（内容寻址），由 `index.db`（SQLite）索引，多个项目共享。详见 [`docs/deps-strategy.md`](docs/deps-strategy.md) 与 [`docs/deps-tutorial.md`](docs/deps-tutorial.md)。

---

## 命令一览

| 命令 | 作用 |
|------|------|
| `Optive` | 启动交互式 REPL |
| `Optive <script.tive>` | 运行脚本 |
| `Optive new <Name>` | 新建项目脚手架（`Optive.toml` + `src/main.tive` + `.gitignore`） |
| `Optive run [path]` | 严格按 `Optive.lock` 确保依赖 + 运行入口 |
| `Optive up [path]` | `update` + `run`（跟随 tip，更新 lock） |
| `Optive add <git-url> [--name N] [--branch B\|--tag T]` | 添加依赖（默认钉到 tip commit） |
| `Optive remove <name>` | 移除依赖 |
| `Optive update [name] [--dry-run] [-v]` | 更新依赖，写 `Optive.lock` |
| `Optive deps [-v]` | 列出项目依赖 |
| `Optive deps doctor [-v]` | 诊断依赖 / lock / 孤儿 pack |
| `Optive cache gc [--dry-run]` | 回收未被引用的孤儿 pack |
| `Optive env` | 打印 `OPTIVE_HOME` 与各路径 |
| `Optive change track_latest=true\|false` | 切换某依赖是否跟随 tip（会告警） |
| `Optive fmt <file> [-o\|--out]` | 格式化 `.tive` 源文件（默认写回；`-o` 只打印） |
| `Optive custom show\|all\|use\|add` | 定制包（人读文案 / 排版；不影响语言身份） |
| `Optive -V` / `--version` | 版本 |
| `Optive -h` / `--help` | 帮助 |

### 环境变量

| 变量 | 作用 |
|------|------|
| `OPTIVE_HOME` | 全局 `pack/` + `custom/` + `index.db` 根目录 |
| `OPTIVE_CUSTOM=a,b` | 整链覆盖激活的定制包（忽略项目/全局 `use`） |
| `OPTIVE_USE_LOCAL_DEPS=1` | 调试：把依赖装进项目内 `deps/` 而非全局 |
| `OPTIVE_HISTORY` | REPL 历史文件路径 |
| `OPTIVE_WORKERS` | OS worker 数（默认 `1`；`0`=核数；`>1` 真并行） |
| `OPTIVE_SUSPEND_BUDGET` | 自动协作挂起字节码预算（默认 8192） |
| `OPTIVE_STW_TIMEOUT_MS` | STW GC 等待 helper 时限 ms（默认 2000） |
| `OPTIVE_PATH` | 模块搜索路径（`:` / `;` 分隔） |

---

## 项目结构

```
src/
├── main.rs              CLI 入口 + REPL
├── lib.rs               库入口（run_source / run_source_in_vm）
├── frontend/            词法 + 语法 + 诊断 + 格式化
│   ├── lexer.rs  parser.rs  ast.rs  token.rs  fmt.rs  error.rs  diagnostics.rs
├── compiler/            AST → 字节码
│   ├── codegen.rs  hot_code.rs  opcode.rs  monomorph.rs  specialize.rs
│   ├── protocol.rs  free_vars.rs
├── runtime/            字节码 VM + 运行时
│   ├── vm.rs  module.rs  value.rs  types.rs  type_registry.rs
│   ├── builtins.rs  gc.rs  exceptions.rs  traceback.rs
│   ├── ffi.rs  ffi_extra.rs  ptr_registry.rs  c_types.rs
│   ├── caps.rs  concurrency.rs  enum_variant.rs  runtime_ast.rs  sized.rs
├── stdlib/             内置标准库
├── custom/             定制包（文案 + 排版；内嵌 en-US）
└── cli/                包管理 CLI（含 caps / debug / fmt / custom 等）
```

---

## 文档

完整索引见 [`docs/README.md`](docs/README.md)。

- [`docs/getting-started.md`](docs/getting-started.md) — 入门教程（手把手建项目、加依赖、跑）
- [`docs/language.md`](docs/language.md) — 语言参考（含 `%` / 位运算 / `snap` / `par` / `{,}` / `gen`·`yield`；§1.1 含 CLI）
- [`docs/stdlib.md`](docs/stdlib.md) — 标准库 API（含 `std.async` / `std.macros` / encoding 等）
- [`docs/debug-tutorial.md`](docs/debug-tutorial.md) / [`docs/debug.md`](docs/debug.md) — 调试器上手与命令参考
- [`docs/ffi-c.md`](docs/ffi-c.md) — C 互操作
- [`docs/ffi-parallel.md`](docs/ffi-parallel.md) — 并行 FFI（默认异符号可重叠；可选卸荷池）
- [`docs/custom-packs.md`](docs/custom-packs.md) — 定制包（人读文案 / 排版；第三方包不内嵌）
- [`docs/deps-strategy.md`](docs/deps-strategy.md) — 依赖管理设计
- [`docs/deps-tutorial.md`](docs/deps-tutorial.md) — 依赖管理实操
- [`docs/concurrency.md`](docs/concurrency.md) — 并发文档地图与实现状态（M:1 默认 / M:N 可选）
- [`docs/concurrency_like_go.md`](docs/concurrency_like_go.md) — 并发语言语义（`go` / `par` / channel / `select` / 取消；权威）
- [`docs/Deprecated/`](docs/Deprecated/) — 归档（未采用方案 / 已完成计划 / 历史修复报告 / GC 提案）

> 文档与实现冲突时，以源码与测试为准。

---

## 开发

```bash
cargo build                # 开发构建
cargo test                 # 全部测试（默认排除慢基准）
cargo test -- --ignored    # 跑慢基准（fib(30) 等）作冒烟
cargo bench --bench optive # criterion 性能基准
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

CI（`.github/workflows/ci.yml`）在 ubuntu / windows / macos 三平台上跑 fmt + clippy + build + test；打 `v*` tag 触发 `release.yml`，自动构建并发布三平台二进制。

辅助工具见 [`tools/README.md`](tools/README.md)（性能回归对比、`.tive` 语法高亮等）。

---

## 许可证

MulanPSL-2.0，见 `LICENSE`。
