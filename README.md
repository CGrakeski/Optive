# Optive

[![License: MulanPSL-2.0](https://img.shields.io/badge/license-MulanPSL--2.0-blue.svg)](LICENSE)

**Optive** 是一门动态、表达式优先的脚本语言（源文件 `.tive`），解释器用 Rust 实现：词法 → 语法 → 字节码 VM。

当前版本 **0.2.0**。

它有渐进式类型（可写可不写）、`struct` / `variant` / 宏、Git 依赖（`Optive.toml` + lock）、以及 `std.http` / `std.net` / `std.sqlite`。没有 `class` 继承、没有 `async func`（并发用 `go` / `await`）。

## 三分钟

```bash
# 安装：Releases 解压进 PATH，或
#   cargo build --release --bin Optive
Optive -V

Optive                         # REPL
Optive -c "print(1 + 2)"
Optive new my_app && cd my_app && Optive run
```

完整上手：[docs/tutorial/](docs/tutorial/README.md)。文档索引：[docs/README.md](docs/README.md)。

## 常用命令

| | |
|--|--|
| `Optive` / `Optive <file.tive>` / `-c` | REPL、跑文件、跑参数 |
| `new` · `run` · `up` · `test` | 项目：脚手架、按 lock 跑、更新后跑、跑 `tests/` |
| `add` · `search` · `index sync` | 依赖与官方索引（Gitee Optindex） |
| `fmt` · `debug` · `lsp` | 格式化、CLI 调试器、语言服务（诊断 + 跳转） |

能力开关：`--sandbox` `--no-network` `--no-ffi` `--allow-ffi` `--allow-path`。`--no-network` 关掉 `std.http` 和 `std.net`。

命令与环境变量全文：[docs/cli.md](docs/cli.md)。依赖模型：[docs/deps.md](docs/deps.md)。

## 文档

- **教程**（按这个读）：[docs/tutorial/](docs/tutorial/README.md)
- **参考**：语言 · 标准库 · CLI · 依赖 · 并发 · FFI · 调试 · LSP — 见 [docs/README.md](docs/README.md)

> 文档与实现冲突时，以源码和测试为准。

## 开发

```bash
cargo test                          # 含并发回归与 lexer/parser fuzz 冒烟
cargo test -- --ignored             # 慢基准冒烟
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

CI：三平台 fmt / clippy / test。`v*` tag 走 `release.yml`。辅助工具：[tools/README.md](tools/README.md)。

## 许可证

MulanPSL-2.0，见 `LICENSE`。
