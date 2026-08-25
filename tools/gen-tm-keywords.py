#!/usr/bin/env python3
"""Regenerate TextMate keyword matches from src/frontend/token.rs KEYWORDS."""

from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TOKEN_RS = ROOT / "src" / "frontend" / "token.rs"
TM = ROOT / "tools" / "syntax" / "tive.tmLanguage.json"

CONTROL = {
    "if",
    "elif",
    "else",
    "then",
    "loop",
    "while",
    "for",
    "in",
    "break",
    "continue",
    "return",
    "match",
    "case",
    "do",
    "handle",
    "try",
    "catch",
    "throw",
    "outside",
    "go",
    "par",
    "snap",
    "await",
    "select",
    "yield",
    "suspend",
    "gen",
}
LOGICAL = {"and", "or", "not", "is"}
DECLARATION = {
    "let",
    "var",
    "const",
    "func",
    "friend",
    "struct",
    "enum",
    "variant",
    "protocol",
    "macro",
    "quote",
    "typed",
    "overload",
    "import",
    "use",
    "as",
    "intern",
    "export",
    "with",
    "make",
    "del",
}


def keywords_from_token_rs() -> list[str]:
    text = TOKEN_RS.read_text(encoding="utf-8")
    m = re.search(r"pub const KEYWORDS: &\[&str\] = &\[(.*?)\];", text, re.S)
    if not m:
        raise SystemExit("KEYWORDS not found in token.rs")
    return re.findall(r'"([^"]+)"', m.group(1))


def alt(words: list[str]) -> str:
    return "|".join(sorted(words, key=lambda w: (-len(w), w)))


def main() -> None:
    kws = keywords_from_token_rs()
    control = [w for w in kws if w in CONTROL]
    logical = [w for w in kws if w in LOGICAL]
    decl = [w for w in kws if w in DECLARATION or w not in CONTROL | LOGICAL]
    data = json.loads(TM.read_text(encoding="utf-8"))
    patterns = data["repository"]["keywords"]["patterns"]
    by_name = {p["name"]: p for p in patterns}
    by_name["keyword.control.tive"]["match"] = rf"\b({alt(control)})\b"
    by_name["keyword.operator.logical.tive"]["match"] = rf"\b({alt(logical)})\b"
    by_name["keyword.declaration.tive"]["match"] = rf"\b({alt(decl)})\b"
    TM.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"updated {TM.relative_to(ROOT)} from {len(kws)} KEYWORDS")


if __name__ == "__main__":
    main()
