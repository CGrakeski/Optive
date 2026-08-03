# Optive

[![License: MulanPSL-2.0](https://img.shields.io/badge/license-MulanPSL--2.0-blue.svg)](LICENSE)

**Optive** 是一门动态、表达式丰富的脚本语言，用 Rust 实现的解释器：词法分析 → 语法分析 → 字节码 VM。支持软/硬类型注解、泛型、协议、管道、模式匹配、宏，以及通过 `extern` 调用 C ABI 动态库；并自带基于 Git 的依赖管理（`Optive.toml` / `optive.lock` / `Optive.cache`）。

源文件扩展名：`.tive`。当前版本：**0.2.0**。

---

## 特性

- **表达式优先**：几乎所有构造都是表达式（`if` / `loop` / `match` / 块都有值），顶层结果非 `none` 自动打印。
- **渐进式类型**：可写软类型注解（解释期检查）或硬类型注解（编译期单态化），也可完全不写。
- **泛型与协议**：`protocol` 定义接口，`struct`/`enum`/`variant` 实现之；编译器按需单态化生成专用字节码。
- **元编程**：`macro` / `quote` 宏系统，编译期展开。
- **C 互操作**：`extern` 加载 `.dll`/`.so`/`.dylib`；护照指针、`typed struct : C.layout`、字符串编组、沙箱 FFI 门禁（见 [`docs/ffi-c.md`](docs/ffi-c.md)）。
- **依赖管理**：Git URL 作为依赖源，内容寻址存储（CAS）+ SQLite 索引，`optive.lock` 保证可复现构建。
- **REPL**：交互式多行输入，历史记录持久化。

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
Optive run                                # 严格按 optive.lock 跑
Optive up                                 # update + run（跟随 tip）
```

---

## 依赖管理三件套

| 文件 | 角色 | 提交到 Git？ |
|------|------|:---:|
| `Optive.toml` | **意图**：声明依赖（URL、版本约束、是否跟随 tip） | ✅ |
| `optive.lock` | **复现**：锁定每个依赖到具体 commit，全平台一致 | ✅ |
| `Optive.cache` | **本地**：路径指针 + 校验和，加速冷启动 | ❌（加入 `.gitignore`） |

依赖本体存在全局 `pack/` 目录（内容寻址），由 `index.db`（SQLite）索引，多个项目共享。详见 [`docs/deps-strategy.md`](docs/deps-strategy.md) 与 [`docs/deps-tutorial.md`](docs/deps-tutorial.md)。

---

## 命令一览

| 命令 | 作用 |
|------|------|
| `Optive` | 启动交互式 REPL |
| `Optive <script.tive>` | 运行脚本 |
| `Optive new <Name>` | 新建项目脚手架（`Optive.toml` + `src/main.tive` + `.gitignore`） |
| `Optive run [path]` | 严格按 `optive.lock` 确保依赖 + 运行入口 |
| `Optive up [path]` | `update` + `run`（跟随 tip，更新 lock） |
| `Optive add <git-url> [--name N] [--branch B\|--tag T]` | 添加依赖（默认钉到 tip commit） |
| `Optive remove <name>` | 移除依赖 |
| `Optive update [name] [--dry-run] [-v]` | 更新依赖，写 `optive.lock` |
| `Optive deps [-v]` | 列出项目依赖 |
| `Optive deps doctor [-v]` | 诊断依赖 / lock / 孤儿 pack |
| `Optive cache gc [--dry-run]` | 回收未被引用的孤儿 pack |
| `Optive env` | 打印 `OPTIVE_HOME` 与各路径 |
| `Optive change track_latest=true\|false` | 切换某依赖是否跟随 tip（会告警） |
| `Optive fmt <file> [-o\|--out]` | 格式化 `.tive` 源文件（默认写回；`-o` 只打印） |
| `Optive -V` / `--version` | 版本 |
| `Optive -h` / `--help` | 帮助 |

### 环境变量

| 变量 | 作用 |
|------|------|
| `OPTIVE_HOME` | 全局 `pack/` + `index.db` 根目录 |
| `OPTIVE_USE_LOCAL_DEPS=1` | 调试：把依赖装进项目内 `deps/` 而非全局 |
| `OPTIVE_HISTORY` | REPL 历史文件路径 |

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
└── cli/                包管理 CLI（含 caps / debug / fmt 等）
```

---

## 文档

- [`docs/getting-started.md`](docs/getting-started.md) — 入门教程（手把手建项目、加依赖、跑）
- [`docs/language.md`](docs/language.md) — 语言参考（语法、类型、控制流、泛型、宏；§1.1 含 CLI / `Optive fmt`）
- [`docs/stdlib.md`](docs/stdlib.md) — 标准库 API（含 `std.http` 等）
- [`docs/ffi-c.md`](docs/ffi-c.md) — C 互操作
- [`docs/deps-strategy.md`](docs/deps-strategy.md) — 依赖管理设计
- [`docs/deps-tutorial.md`](docs/deps-tutorial.md) — 依赖管理实操
- [`docs/concurrency_like_go.md`](docs/concurrency_like_go.md) — 并发模型（`go` / channel / `select`）

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
