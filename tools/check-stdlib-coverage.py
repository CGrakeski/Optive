#!/usr/bin/env python3
"""核对 runtime API registry 的 std 模块/导出是否出现在 docs/stdlib.md。"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
API_REGISTRY = ROOT / "src" / "lsp" / "catalog.rs"
STDLIB_DOC = ROOT / "docs" / "stdlib.md"


def parse_modules(text: str) -> list[str]:
    block = re.search(r"pub const STD_MODULES:.*?=\s*&\[(.*?)\];", text, re.S)
    if not block:
        sys.exit("STD_MODULES not found")
    return re.findall(r'"([^"]+)"', block.group(1))


def parse_exports(text: str) -> list[tuple[str, str]]:
    block = re.search(r"pub const STD_EXPORTS:.*?=\s*&\[(.*?)\];", text, re.S)
    if not block:
        sys.exit("STD_EXPORTS not found")
    return re.findall(r'\("([^"]*)",\s*"([^"]+)"\)', block.group(1))


def main() -> int:
    cat = API_REGISTRY.read_text(encoding="utf-8")
    doc = STDLIB_DOC.read_text(encoding="utf-8")
    missing: list[str] = []
    for mod in parse_modules(cat):
        heading = f"`std.{mod}`"
        if heading not in doc and f"## `std.{mod}`" not in doc:
            missing.append(f"module {mod} (need heading `std.{mod}`)")
    for mod, exp in parse_exports(cat):
        if mod == "":
            if f"`{exp}`" not in doc and f"`std.{exp}`" not in doc:
                missing.append(f"root export {exp}")
            continue
        # 模块文档段里至少出现导出名
        if not re.search(
            rf"`[^`]*(?<![A-Za-z0-9_]){re.escape(exp)}(?![A-Za-z0-9_])[^`]*`",
            doc,
        ):
            missing.append(f"std.{mod}.{exp}")
    if missing:
        print("stdlib coverage gaps:")
        for m in missing:
            print(f"  - {m}")
        return 1
    print(
        f"ok: {len(parse_modules(cat))} modules, "
        f"{len(parse_exports(cat))} registry exports mentioned in docs/stdlib.md"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
