# Optive

[![License: MulanPSL-2.0](https://img.shields.io/badge/license-MulanPSL--2.0-blue.svg)](LICENSE)

**Optive** 是一门动态、表达式丰富的脚本语言，用 Rust 实现的解释器：词法分析 → 语法分析 → 字节码 VM。支持软/硬类型注解、泛型、协议、管道、模式匹配、宏，以及通过 `extern` 调用 C ABI 动态库；并自带基于 Git 的依赖管理（`Optive.toml` / `Optive.lock` / CAS `pack/`）。

源文件扩展名：`.tive`。当前版本：**0.2.0**。

---

## 特性

- **表达式优先**：几乎所有构造都是表达式（`if` / `loop` / `match` / 块都有值），顶层结果非 `none` 自动打印（`Optive test` 除外）。
- **渐进式类型**：可写软类型注解（解释期检查）或硬类型注解（编译期单态化），也可完全不写。泛型可从字面量、容器、算术与多参数注解推断。
- **泛型与协议**：`protocol` 定义接口，`struct`/`enum`/`variant` 实现之；编译器按需单态化。
- **元编程**：`macro` / `quote` 宏系统，编译期展开。
- **C 互操作**：`extern` 加载 `.dll`/`.so`/`.dylib`；护照指针、`typed struct ... : <layout>`（内建一等对象 `C.layout`，**可按值传/返回结构体**）、字符串编组、沙箱 FFI 门禁（见 [`docs/ffi.md`](docs/ffi.md)）。
- **依赖管理**：Git URL 或索引上的 semver 约束（`^` / `~` / `>=` 等），CAS + SQLite，`Optive.lock` 保证可复现构建。
- **REPL**：交互式多行输入，历史持久化；TTY 下默认 Lexer 语法高亮（`OPTIVE_REPL_HIGHLIGHT=0` 可关）。
- **并发**：Go 风格 `go` / `await` / `Channel` / `select` / `par`（含多迭代器 `par for`）；默认 M:1，设 `OPTIVE_WORKERS` 可升至 M:N。

---

## 快速开始

教程从 [`docs/tutorial/01-quickstart.md`](docs/tutorial/01-quickstart.md) 起。下面是最短路径。

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

# 3) 项目 + 依赖 + 测试
Optive new my_app
cd my_app
Optive add file:///D:/some/local/repo   # Git 依赖
Optive add greeter@0.1.2                # 索引包名@version
Optive search greet
Optive run                                # 严格按 Optive.lock 跑
Optive test                               # tests/**/*.tive
Optive up                                 # update + run（跟随 tip）
```

---

## 依赖管理三件套

| 文件 | 角色 | 提交到 Git？ |
|------|------|:---:|
| `Optive.toml` | **意图**：声明依赖（URL 或索引版本约束） | ✅ |
| `Optive.lock` | **复现**：锁定每个依赖到具体 commit | ✅ |
| `Optive.cache` | **本地**：依赖 tip/id 小抄（**不是**源码/字节码缓存） | ❌（加入 `.gitignore`） |
| `Custom.toml` | **定制**：项目选用的定制包链（`use = [...]`） | 可选 |

依赖本体存在全局 `pack/`（内容寻址），由 `index.db` 索引。包名版本如 `greeter = "^0.1.0"` 从 `index.json` 查 Git URL，再按 tag 选版本。详见 [`docs/deps.md`](docs/deps.md)。

---

## 命令一览

| 命令 | 作用 |
|------|------|
| `Optive` | 启动交互式 REPL |
| `Optive <script.tive>` | 运行脚本 |
| `Optive -c <code>` | 运行参数中的源码（可多行） |
| `Optive new <Name>` | 新建项目脚手架 |
| `Optive run [path] [-- args...]` | 严格按 lock 确保依赖 + 运行入口 |
| `Optive up [path] [-- args...]` | `update` + `run` |
| `Optive test [path]` | 运行 `tests/**/*.tive` |
| `Optive add <git-url\|pack[@ver]> [...]` | 添加 Git 或索引包依赖 |
| `Optive search [query]` | 搜索包索引中的包名 |
| `Optive remove <name>` | 移除依赖 |
| `Optive update [name] [--dry-run] [-v]` | 更新依赖，写 lock |
| `Optive publish <version>` | 打 annotated tag（`vX.Y.Z`）并可选推送 |
| `Optive deps [-v]` | 列出项目依赖 |
| `Optive deps doctor [-v]` | 诊断依赖 / lock / 孤儿 pack |
| `Optive cache gc [--dry-run]` | 回收孤儿 pack |
| `Optive env` | 打印 `OPTIVE_HOME` 与各路径 |
| `Optive change track_latest=true\|false` | 切换是否跟随 tip |
| `Optive fmt <file> [-o\|--out]` | 格式化 `.tive` |
| `Optive debug [file\|path]` | 调试器 |
| `Optive index sync` | 同步包索引（默认：Gitee 官方 Optindex） |
| `Optive index change <url>` | 设置索引 Git 远程并同步 |
| `Optive custom show\|all\|use\|add` | 定制包 |
| `Optive -V` / `--version` | 版本 |
| `Optive -h` / `--help` | 帮助 |

能力开关（`run` / `up` / `debug` / `test` / 脚本 / `-c`）：`--sandbox[=DIR]` `--no-network` `--no-ffi` `--allow-ffi` `--allow-path DIR`。

### 环境变量

| 变量 | 作用 |
|------|------|
| `OPTIVE_HOME` | 全局 `pack/` + `custom/` + `index.db` + `index.url` 根目录 |
| `OPTIVE_INDEX` | 包索引本地目录（`index.json`） |
| `OPTIVE_INDEX_URL` | 覆盖索引 Git 远程（默认 `https://gitee.com/CGrakeski/optindex.git`） |
| `OPTIVE_CUSTOM=a,b` | 整链覆盖激活的定制包 |
| `OPTIVE_USE_LOCAL_DEPS=1` | 调试：把依赖装进项目内 `deps/` |
| `OPTIVE_HISTORY` | REPL 历史文件路径 |
| `OPTIVE_PATH` | 模块搜索路径（`:` / `;` 分隔） |
| `OPTIVE_WORKERS` | OS worker 数（默认 `1`；`0`=核数；`>1` 真并行） |
| `OPTIVE_SUSPEND_BUDGET` | 自动协作挂起字节码预算（默认 8192） |
| `OPTIVE_MAX_CALL_DEPTH` | 调用深度上限（默认 10000） |
| `OPTIVE_STW_TIMEOUT_MS` | STW 握手时限 ms（默认 2000） |
| `OPTIVE_GC_COOLDOWN_MS` | STW 失败后冷却 ms（默认 50） |
| `OPTIVE_GC_MODE` | `concurrent`（默认）或 `stw` |
| `OPTIVE_GC_MARKERS` | concurrent 并行标记线程数 |
| `OPTIVE_GC_THRESHOLD` | 自动 GC 跟踪表阈值（默认 8192） |
| `OPTIVE_FFI_SERIAL` | `1` 时 FFI 全局串行 |
| `OPTIVE_FFI_THREADS` | FFI 卸荷线程数（默认 0） |
| `OPTIVE_REPL_HIGHLIGHT` | `0`/`off` 关闭 REPL 输入语法高亮 |

完整表见 [`docs/cli.md`](docs/cli.md)。

---

## 项目结构

```
src/
├── main.rs              CLI 入口 + REPL
├── lib.rs               库入口（run_source / run_source_in_vm）
├── frontend/            词法 + 语法 + 诊断 + 格式化
├── compiler/            AST → 字节码（含泛型单态化）
├── runtime/             字节码 VM + GC + FFI + 并发
├── stdlib/              内置标准库
├── custom/              定制包（文案 + 排版；内嵌 en-US）
└── cli/                 包管理 / debug / test / index
```

---

## 文档

索引：[`docs/README.md`](docs/README.md)。

**参考（描述整门语言与系统）**

- [`docs/language.md`](docs/language.md) — 语言
- [`docs/stdlib.md`](docs/stdlib.md) — 标准库
- [`docs/cli.md`](docs/cli.md) — 命令行
- [`docs/deps.md`](docs/deps.md) — 依赖
- [`docs/index.md`](docs/index.md) — 包索引
- [`docs/concurrency.md`](docs/concurrency.md) — 并发
- [`docs/ffi.md`](docs/ffi.md) — FFI
- [`docs/debug.md`](docs/debug.md) — 调试器
- [`docs/custom-packs.md`](docs/custom-packs.md) — 定制包

**教程**（教学性质）：[`docs/tutorial/`](docs/tutorial/README.md)

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
