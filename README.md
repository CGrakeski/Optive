# Optive

[![License: MulanPSL-2.0](https://img.shields.io/badge/license-MulanPSL--2.0-blue.svg)](LICENSE)

Optive 是一门动态、表达式优先的脚本语言，使用 `.tive` 作为源文件扩展名，解释器以 Rust 实现。当前版本为 **0.2.0**。

语言提供渐进式类型、精确数值、支持继承的 `struct`、`variant`、宏、模块与 Git 包依赖；并发使用 `go` / `await`，标准库包含 HTTP、网络和 SQLite 等模块。`struct` 承担类的角色，但语言不使用 `class` 关键字，也没有 `async func` 语法。

## 快速开始

从 Release 获取可执行文件并加入 `PATH`，或者从源码构建：

```powershell
cargo build --release --bin Optive
./target/release/Optive.exe --version
```

运行一段代码、启动 REPL，或创建项目：

```powershell
Optive -c "print(1 + 2)"
Optive
Optive new my_app
cd my_app
Optive run
```

第一次使用请继续阅读[上手教程](docs/user/tutorial.md)；按主题查阅时从[文档中心](docs/README.md)开始。

## 常用命令

| 场景 | 命令 |
| --- | --- |
| 运行 | `Optive`、`Optive <file.tive>`、`Optive -c <code>` |
| 项目 | `new`、`run`、`build`、`up`、`test`、`check` |
| 依赖 | `add`、`remove`、`update`、`deps`、`search`、`index` |
| 工具 | `fmt`、`debug`、`lsp`、`dap` |

运行用户代码的命令支持 `--sandbox`、`--no-network`、`--no-ffi`、`--allow-ffi` 和 `--allow-path` 等能力选项。完整参数以 `Optive --help` 为准，概念和示例见[包与能力](docs/user/packages.md)。

## 文档导航

- [语言用户指南](docs/user/README.md)：教程、语法、模块、包、并发、调试与编辑器集成；
- [设计沿革](docs/design-history.md)：项目起源、命名变化和主要语言取舍；
- [解释器开发手册](docs/architecture.md)：前端、编译器、VM、GC、调度器和扩展流程；
- [贡献与测试](docs/contributing.md)：开发工作流、测试选择和提交前检查；
- [工具说明](tools/README.md)：仓库内辅助工具。

本地生成文档站点需要 `mdbook` 和 `mdbook-mermaid`：

```powershell
mdbook build docs
```

Optive 仍处于 0.x 阶段。文档与实现冲突时，以当前版本的 `Optive --help`、源码和测试为准。

## 开发

```powershell
cargo test
cargo test -- --ignored
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

CI 在 Windows、Linux 和 macOS 上执行格式化、Clippy 与测试；`v*` 标签触发发布流程。

## 许可证

本项目使用 [MulanPSL-2.0](LICENSE)。
