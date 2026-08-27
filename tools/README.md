# `tools/`

本目录存放**不属于解释器本体**、但服务于开发与发布流程的辅助工具。

## 现有

| 路径 | 作用 |
|------|------|
| [`bench-compare.sh`](bench-compare.sh) | 在 base 分支与当前工作树上各跑一次 criterion 基准并对比。 |
| [`syntax/tive.tmLanguage.json`](syntax/tive.tmLanguage.json) | `.tive` 的 TextMate 语法。 |
| [`gen-tm-keywords.py`](gen-tm-keywords.py) | 从 `src/frontend/token.rs` 的 `KEYWORDS` 重写 TextMate 关键字。 |
| [`fuzz/`](fuzz/README.md) | 可选 `cargo-fuzz`（不进默认 workspace）。CI 冒烟是根目录 `tests/fuzz_frontend.rs`。 |

> **与 REPL 的区别**：交互式 REPL 的输入着色在解释器内（`src/cli/repl_highlight.rs`，Lexer + ANSI），不读本目录 TextMate。编辑器高亮用下面步骤；关 REPL 高亮用 `OPTIVE_REPL_HIGHLIGHT=0`。

## 语法高亮怎么用

### VS Code / Cursor（推荐）

使用旁路扩展仓 **`OptivePlugin`**（本机常见路径 `D:\OptivePlugin`）：打包 VSIX 后安装。需求与验收见 [`docs/vscode-extension.md`](../docs/vscode-extension.md)。扩展内 `syntax/tive.tmLanguage.json` 应与本目录文件保持同步（以本仓库为准）。

### VS Code（无需扩展，本机临时）

1. 在 VS Code 里 `Ctrl/Cmd+Shift+P` → `Preferences: Open Settings (JSON)`，加入：
   ```json
   {
     "files.associations": { "*.tive": "optive" }
   }
   ```
2. 安装任意支持 TextMate 语法加载的扩展（如 *TextMate Languages*），把 `tive.tmLanguage.json` 注册为 `source.tive`。

### shiki（文档站 / 网页高亮）

```js
import { loadLanguages } from "shiki";
const tm = require("./tools/syntax/tive.tmLanguage.json");
// 注册为 shiki 语言后即可在 markdown 代码块用 ```tive
```

## 路线图（按需添加，不预先创建）

下列是「将来值得放进 `tools/`」的候选项，**当前未实现**，避免空目录与死代码：

- `tools/gen-opcodes.py` —— 从单一来源生成 `Instruction` 枚举与 `HotCode` 的 `H_*` 常量映射。
- `tools/update-snapshots.sh` —— 集成测试快照更新助手。
- `tools/release.sh` —— 本地复刻 `release.yml`。
> 原则：`tools/` 只放**有人会用**的东西；纯占位的空目录/脚本不预先创建，需要时再补。VS Code 扩展在旁路仓 `OptivePlugin`，不在本目录创建 `vscode-extension/`。手册站用本仓 `docs/` + mdBook（`mdbook build docs`），不要再手抄一份 Sphinx API。
