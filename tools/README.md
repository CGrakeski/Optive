# `tools/`

本目录存放**不属于解释器本体**、但服务于开发与发布流程的辅助工具。

## 现有

| 路径 | 作用 |
|------|------|
| [`bench-compare.sh`](bench-compare.sh) | 在 base 分支与当前工作树上各跑一次 criterion 基准并对比，用于检测 VM 性能回归。 |
| [`syntax/tive.tmLanguage.json`](syntax/tive.tmLanguage.json) | `.tive` 文件的 TextMate 语法，供 VS Code / Sublime / shiki 等做语法高亮。 |

> **与 REPL 的区别**：交互式 REPL 的输入着色在解释器内（`src/cli/repl_highlight.rs`，Lexer + ANSI），不读本目录 TextMate。编辑器高亮用下面步骤；关 REPL 高亮用 `OPTIVE_REPL_HIGHLIGHT=0`。

## 语法高亮怎么用

### VS Code（无需发布插件，本机即可用）

1. 在 VS Code 里 `Ctrl/Cmd+Shift+P` → `Preferences: Open Settings (JSON)`，加入：
   ```json
   "files.associations": { "*.tive": "optive" }
   ```
2. 安装任意支持 TextMate 语法加载的扩展（如 *TextMate Languages*），把 `tive.tmLanguage.json` 注册为 `source.tive`；或直接用 `code --install-extension` 装 [extension generator](https://code.visualstudio.com/api) 打包的最小扩展。

### shiki（文档站 / 网页高亮）

```js
import { loadLanguages } from "shiki";
const tm = require("./tools/syntax/tive.tmLanguage.json");
// 注册为 shiki 语言后即可在 markdown 代码块用 ```tive
```

## 路线图（按需添加，不预先创建）

下列是「将来值得放进 `tools/`」的候选项，**当前未实现**，避免空目录与死代码：

- `tools/gen-opcodes.py` —— 从单一来源生成 `Instruction` 枚举与 `HotCode` 的 `H_*` 常量映射，防止两者漂移。
- `tools/update-snapshots.sh` —— 集成测试快照更新助手（当输出格式有意变更时批量 `--update`）。
- `tools/release.sh` —— 本地复刻 `release.yml` 的构建+打包流程，便于发版前手测。
- `tools/vscode-extension/` —— 把 `syntax/` 打包成可发布的 VS Code 扩展（含 REPL 启动、LSP 占位）。
- `tools/fuzz/` —— `cargo-fuzz` 目标，针对 lexer/parser 喂随机输入找 panic（呼应 OPTIVE_REVIEW §3 的 UB 收敛）。
- `tools/check-stdlib-coverage.py` —— 核对 `docs/stdlib.md` 列出的每个 API 是否在 `stdlib/` 注册。

> 原则：`tools/` 只放**有人会用**的东西；纯占位的空目录/脚本不预先创建，需要时再补。
