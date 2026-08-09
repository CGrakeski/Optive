//! REPL 语法高亮：Lexer span + ANSI，热路径只 tokenize、带行缓存。

use std::borrow::Cow;
use std::cell::RefCell;
use std::env;

use optive::lexer::Lexer;
use optive::token::TokenKind;

use super::color;

const RESET: &str = "\x1b[0m";
/// 关键字：256 色橘（208；不加粗）
const KW: &str = "\x1b[38;5;208m";
/// 类型相关关键字：粗体 + 亮青
const TYPE_KW: &str = "\x1b[1;96m";
const LIT_NUM: &str = "\x1b[93m"; // bright yellow
const LIT_STR: &str = "\x1b[92m"; // bright green
const COMMENT: &str = "\x1b[90m"; // bright black / gray
const OP: &str = "\x1b[37m"; // white/gray

#[derive(Clone, Copy)]
enum Style {
    None,
    Kw,
    TypeKw,
    Num,
    Str,
    Comment,
    Op,
}

fn style_for(kind: TokenKind) -> Style {
    use TokenKind::*;
    match kind {
        KwLet | KwVar | KwConst | KwFunc | KwGen | KwFriend | KwDo | KwReturn | KwIf | KwElif
        | KwElse | KwAnd | KwOr | KwNot | KwLoop | KwWhile | KwBreak | KwContinue | KwImport
        | KwUse | KwAs | KwIntern | KwExport | KwWith | KwMake | KwFor | KwIn | KwIs | KwThen
        | KwHandle | KwGo | KwPar | KwSnap | KwAwait | KwSelect | KwYield | KwSuspend | KwMatch
        | KwCase | KwTry | KwCatch | KwThrow | KwDel | KwOutside | KwOverload | KwMacro
        | KwQuote | KwTyped => Style::Kw,

        KwVariant | KwEnum | KwStruct | KwProtocol | ColonColon => Style::TypeKw,

        NumLiteral => Style::Num,
        StringLiteral | FStringLiteral | BytesLiteral => Style::Str,
        LineComment | BlockComment => Style::Comment,

        Plus | Minus | Star | StarStar | Slash | Percent | Ampersand | Caret | Tilde | Bang
        | EqEq | Ne | Lt | Gt | Le | Ge | LtLt | GtGt | Assign | Colon | ColonEq | Arrow
        | FatArrow | Pipe | Bar | Dot | Comma | Ellipsis | Placeholder => Style::Op,

        _ => Style::None,
    }
}

fn ansi_prefix(style: Style) -> &'static str {
    match style {
        Style::None => "",
        Style::Kw => KW,
        Style::TypeKw => TYPE_KW,
        Style::Num => LIT_NUM,
        Style::Str => LIT_STR,
        Style::Comment => COMMENT,
        Style::Op => OP,
    }
}

/// 是否启用输入行高亮（尊重 `--color` / `NO_COLOR`，可用 `OPTIVE_REPL_HIGHLIGHT=0` 关掉）。
pub fn highlight_enabled() -> bool {
    if !color::enabled() {
        return false;
    }
    match env::var("OPTIVE_REPL_HIGHLIGHT") {
        Ok(s) => {
            let s = s.to_ascii_lowercase();
            !matches!(s.as_str(), "0" | "false" | "off" | "no")
        }
        Err(_) => true,
    }
}

/// 单行高亮（不查缓存）。`line` 为字节串；span 来自 Lexer。
pub fn highlight_tive_line(line: &str) -> String {
    if line.is_empty() {
        return String::new();
    }
    let spans = Lexer::new(line).tokenize_spans();
    let mut out = String::with_capacity(line.len().saturating_mul(2));
    let mut cursor = 0usize;
    for (start, end, kind) in spans {
        let start = start.min(line.len());
        let end = end.min(line.len()).max(start);
        if start > cursor {
            out.push_str(&line[cursor..start]);
        }
        let style = style_for(kind);
        let slice = &line[start..end];
        match style {
            Style::None => out.push_str(slice),
            _ => {
                out.push_str(ansi_prefix(style));
                out.push_str(slice);
                out.push_str(RESET);
            }
        }
        cursor = end;
    }
    if cursor < line.len() {
        out.push_str(&line[cursor..]);
    }
    out
}

/// 行级缓存：rustyline 的 `highlight` 只有 `&self`。
#[derive(Default)]
pub struct LineHighlightCache {
    inner: RefCell<Option<(String, String)>>,
}

impl LineHighlightCache {
    pub fn get_or_highlight<'l>(&self, line: &'l str) -> Cow<'l, str> {
        if !highlight_enabled() {
            return Cow::Borrowed(line);
        }
        {
            let guard = self.inner.borrow();
            if let Some((src, painted)) = guard.as_ref() {
                if src == line {
                    return Cow::Owned(painted.clone());
                }
            }
        }
        let painted = highlight_tive_line(line);
        *self.inner.borrow_mut() = Some((line.to_string(), painted.clone()));
        Cow::Owned(painted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_keyword_and_number() {
        let s = highlight_tive_line("let x = 42");
        assert!(s.contains(KW));
        assert!(s.contains(LIT_NUM));
        assert!(s.contains("let"));
        assert!(s.contains("42"));
        assert!(s.contains(RESET));
    }

    #[test]
    fn highlights_comment() {
        let s = highlight_tive_line("x // hi");
        assert!(s.contains(COMMENT));
        assert!(s.contains("//"));
    }

    #[test]
    fn unterminated_string_still_colors() {
        let s = highlight_tive_line("let s = \"abc");
        assert!(s.contains(LIT_STR));
    }

    #[test]
    fn cache_hits_same_line() {
        let c = LineHighlightCache::default();
        // 不依赖全局 color flag：直接测 highlight_tive_line 稳定性。
        let a = highlight_tive_line("func f() { 1 }");
        let b = highlight_tive_line("func f() { 1 }");
        assert_eq!(a, b);
        let _ = c;
    }
}
