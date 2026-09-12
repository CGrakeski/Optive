//! REPL 语法高亮：Lexer span + ANSI，热路径只 tokenize、带行缓存。

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Style {
    None,
    Kw,
    TypeKw,
    Num,
    Str,
    Comment,
    Op,
}

const fn style_for(kind: TokenKind) -> Style {
    use TokenKind::{
        Ampersand, Arrow, Assign, Bang, Bar, BlockComment, BytesLiteral, Caret, Colon, ColonColon,
        ColonEq, Comma, Dot, Ellipsis, EqEq, FStringLiteral, FatArrow, Ge, Gt, GtGt, KwAnd, KwAs,
        KwAwait, KwBreak, KwCase, KwCatch, KwConst, KwContinue, KwDel, KwDo, KwElif, KwElse,
        KwEnum, KwExport, KwFor, KwFriend, KwFunc, KwGen, KwGo, KwHandle, KwIf, KwImport, KwIn,
        KwIntern, KwIs, KwLet, KwLoop, KwMacro, KwMake, KwMatch, KwNot, KwOr, KwOutside,
        KwOverload, KwPar, KwProtocol, KwQuote, KwReturn, KwSelect, KwSnap, KwStruct, KwSuspend,
        KwThen, KwThrow, KwTry, KwTyped, KwUse, KwVar, KwVariant, KwWhile, KwWith, KwYield, Le,
        LineComment, Lt, LtLt, Minus, Ne, NumLiteral, Percent, Pipe, Placeholder, Plus, Slash,
        Star, StarStar, StringLiteral, Tilde,
    };
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

const fn ansi_prefix(style: Style) -> &'static str {
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

fn highlight_setting(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let value = value.to_ascii_lowercase();
            !matches!(value.as_str(), "0" | "false" | "off" | "no")
        }
        None => true,
    }
}

/// 是否启用输入行高亮（尊重 `--color` / `NO_COLOR`）。
///
/// 默认开启，所有平台都可用 `OPTIVE_REPL_HIGHLIGHT=0` 关闭。
pub fn highlight_enabled() -> bool {
    if !color::enabled() {
        return false;
    }
    let configured = env::var("OPTIVE_REPL_HIGHLIGHT").ok();
    highlight_setting(configured.as_deref())
}

fn trailing_style(line: &str) -> Style {
    Lexer::new(line)
        .tokenize_spans()
        .into_iter()
        .rev()
        .find_map(|(_, end, kind)| (end == line.len()).then(|| style_for(kind)))
        .unwrap_or(Style::None)
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
        if matches!(style, Style::None) {
            out.push_str(slice)
        } else {
            out.push_str(ansi_prefix(style));
            out.push_str(slice);
            out.push_str(RESET);
        }
        cursor = end;
    }
    if cursor < line.len() {
        out.push_str(&line[cursor..]);
    }
    out
}

/// 行级缓存：rustyline 的 `highlight` 只有 `&self`。
pub struct LineHighlightCache {
    inner: RefCell<Option<(String, String)>>,
    terminal_style: Cell<Style>,
    preserve_trailing_style: Cell<bool>,
}

impl Default for LineHighlightCache {
    fn default() -> Self {
        Self {
            inner: RefCell::new(None),
            terminal_style: Cell::new(Style::None),
            preserve_trailing_style: Cell::new(true),
        }
    }
}

impl LineHighlightCache {
    /// rustyline 在 Windows 上通过清空并重画整行来更新高亮。只在行尾 token 的
    /// 样式发生变化时请求重画；同一 token 内的普通输入沿用当前终端样式。
    pub fn should_refresh(
        &self,
        line: &str,
        at: usize,
        kind: rustyline::highlight::CmdKind,
    ) -> bool {
        if !highlight_enabled() {
            self.terminal_style.set(Style::None);
            return false;
        }
        self.should_refresh_when_enabled(line, at, kind)
    }

    fn should_refresh_when_enabled(
        &self,
        line: &str,
        at: usize,
        kind: rustyline::highlight::CmdKind,
    ) -> bool {
        if kind == rustyline::highlight::CmdKind::ForcedRefresh {
            // Enter/最终刷新必须恢复默认终端样式，避免后续输出继承输入颜色。
            self.preserve_trailing_style.set(false);
            self.terminal_style.set(Style::None);
            return false;
        }
        self.preserve_trailing_style.set(true);
        if at != line.len() {
            return false;
        }
        trailing_style(line) != self.terminal_style.get()
    }

    pub fn get_or_highlight<'l>(&self, line: &'l str) -> Cow<'l, str> {
        if !highlight_enabled() {
            return Cow::Borrowed(line);
        }
        let trailing = if self.preserve_trailing_style.get() {
            trailing_style(line)
        } else {
            Style::None
        };
        {
            let guard = self.inner.borrow();
            if let Some((src, painted)) = guard.as_ref() {
                if src == line {
                    let mut painted = painted.clone();
                    if trailing != Style::None {
                        painted.push_str(ansi_prefix(trailing));
                    }
                    self.terminal_style.set(trailing);
                    return Cow::Owned(painted);
                }
            }
        }
        let painted = highlight_tive_line(line);
        *self.inner.borrow_mut() = Some((line.to_string(), painted.clone()));
        let mut output = painted;
        if trailing != Style::None {
            output.push_str(ansi_prefix(trailing));
        }
        self.terminal_style.set(trailing);
        Cow::Owned(output)
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

    #[test]
    fn highlighting_is_enabled_by_default() {
        assert!(highlight_setting(None));
    }

    #[test]
    fn explicit_highlight_setting_overrides_platform_default() {
        assert!(highlight_setting(Some("1")));
        assert!(highlight_setting(Some("true")));
        assert!(!highlight_setting(Some("0")));
        assert!(!highlight_setting(Some("OFF")));
    }

    #[test]
    fn repaint_is_requested_only_when_trailing_style_changes() {
        use rustyline::highlight::CmdKind;

        let cache = LineHighlightCache::default();
        assert!(!cache.should_refresh_when_enabled("l", 1, CmdKind::Other));
        assert!(!cache.should_refresh_when_enabled("le", 2, CmdKind::Other));
        assert!(cache.should_refresh_when_enabled("let", 3, CmdKind::Other));
        cache.terminal_style.set(Style::Kw);
        assert!(cache.should_refresh_when_enabled("letx", 4, CmdKind::Other));
        cache.terminal_style.set(Style::None);
        assert!(!cache.should_refresh_when_enabled("letxy", 5, CmdKind::Other));
    }

    #[test]
    fn forced_refresh_restores_default_terminal_style() {
        use rustyline::highlight::CmdKind;

        let cache = LineHighlightCache::default();
        assert!(cache.should_refresh_when_enabled("42", 2, CmdKind::Other));
        cache.terminal_style.set(Style::Num);
        cache.preserve_trailing_style.set(true);
        assert!(!cache.should_refresh_when_enabled("42", 2, CmdKind::ForcedRefresh));
        assert_eq!(cache.terminal_style.get(), Style::None);
        assert!(!cache.preserve_trailing_style.get());
    }
}
