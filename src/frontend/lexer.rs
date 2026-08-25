use crate::error::LexError;
use crate::token::{keyword_or_ident, Token, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputStatus {
    Complete,
    Incomplete,
}

impl InputStatus {
    #[must_use]
    pub const fn is_incomplete(self) -> bool {
        matches!(self, Self::Incomplete)
    }
}

/// Classify whether more input can complete the current token/delimiter stream.
///
/// This deliberately does not validate syntax; it only tracks lexical constructs that may span
/// REPL reads: delimiters, block comments, and normal/raw/f/bytes (including triple) literals.
#[must_use]
pub fn input_status(source: &str) -> InputStatus {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        LineComment,
        BlockComment,
        String {
            raw: bool,
            triple: bool,
            escaped: bool,
        },
    }

    let chars: Vec<char> = source.chars().collect();
    let mut state = State::Code;
    let (mut paren, mut bracket, mut brace) = (0usize, 0usize, 0usize);
    let mut i = 0usize;
    while i < chars.len() {
        match state {
            State::LineComment => {
                if chars[i] == '\n' {
                    state = State::Code;
                }
                i += 1;
            }
            State::BlockComment => {
                if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    state = State::Code;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            State::String {
                raw,
                triple,
                escaped,
            } => {
                if triple
                    && chars[i] == '"'
                    && chars.get(i + 1) == Some(&'"')
                    && chars.get(i + 2) == Some(&'"')
                {
                    state = State::Code;
                    i += 3;
                } else if !triple && chars[i] == '"' && (raw || !escaped) {
                    state = State::Code;
                    i += 1;
                } else {
                    let next_escaped = !raw && !escaped && chars[i] == '\\';
                    state = State::String {
                        raw,
                        triple,
                        escaped: next_escaped,
                    };
                    i += 1;
                }
            }
            State::Code => {
                if chars[i] == '#' || (chars[i] == '/' && chars.get(i + 1) == Some(&'/')) {
                    state = State::LineComment;
                    i += usize::from(chars[i] == '/');
                } else if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    state = State::BlockComment;
                    i += 2;
                } else {
                    let at_token_start = i == 0 || !is_ident_continue(chars[i - 1]);
                    let (raw, quote_at) = if at_token_start
                        && matches!(chars[i], 'r' | 'f' | 'b')
                        && chars.get(i + 1) == Some(&'"')
                    {
                        (chars[i] == 'r', i + 1)
                    } else if chars[i] == '"' {
                        (false, i)
                    } else {
                        match chars[i] {
                            '(' => paren += 1,
                            ')' => paren = paren.saturating_sub(1),
                            '[' => bracket += 1,
                            ']' => bracket = bracket.saturating_sub(1),
                            '{' => brace += 1,
                            '}' => brace = brace.saturating_sub(1),
                            _ => {}
                        }
                        i += 1;
                        continue;
                    };
                    let triple = chars.get(quote_at + 1) == Some(&'"')
                        && chars.get(quote_at + 2) == Some(&'"');
                    state = State::String {
                        raw,
                        triple,
                        escaped: false,
                    };
                    i = quote_at + if triple { 3 } else { 1 };
                }
            }
        }
    }

    if matches!(state, State::BlockComment | State::String { .. })
        || paren > 0
        || bracket > 0
        || brace > 0
    {
        InputStatus::Incomplete
    } else {
        InputStatus::Complete
    }
}

pub struct Lexer {
    source: String,
    pos: usize,
    line: usize,
    column: usize,
    last_kind: Option<TokenKind>,
    /// 块注释未闭合时置位，由 `tokenize` 报错。
    unclosed_block_comment: bool,
}

impl Lexer {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            pos: 0,
            line: 1,
            column: 1,
            last_kind: None,
            unclosed_block_comment: false,
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        self.skip_bom();
        while !self.is_at_end() {
            if self.skip_whitespace() {
                continue;
            }
            let start_line = self.line;
            let start_col = self.column;
            if let Some(tok) = self.lex_comment(start_line, start_col) {
                if self.unclosed_block_comment {
                    return Err(LexError::Message {
                        line: start_line,
                        column: start_col,
                        message: "unterminated block comment".into(),
                    });
                }
                self.last_kind = Some(tok.kind);
                tokens.push(tok);
                continue;
            }
            let tok = self.next_token();
            if tok.kind == TokenKind::Mismatch {
                let message = if tok.value.starts_with("unterminated")
                    || tok.value.starts_with("non-ASCII")
                    || tok.value.contains(' ')
                {
                    tok.value
                } else if tok.value.is_empty() {
                    format!("unexpected character {:?}", self.peek_char())
                } else {
                    format!(
                        "unexpected character {:?}",
                        tok.value.chars().next().unwrap_or('?')
                    )
                };
                return Err(LexError::Message {
                    line: start_line,
                    column: start_col,
                    message,
                });
            }
            self.last_kind = Some(tok.kind);
            tokens.push(tok);
        }
        tokens.push(Token::new(TokenKind::End, "", self.line, self.column));
        Ok(tokens)
    }

    /// REPL / 高亮用：尽力产出 `(byte_start, byte_end, kind)`，遇错截断且不失败。
    /// 空白不产出 span（由调用方按间隙原样拷贝）。
    #[must_use]
    pub fn tokenize_spans(mut self) -> Vec<(usize, usize, TokenKind)> {
        let mut spans = Vec::new();
        self.skip_bom();
        while !self.is_at_end() {
            if self.skip_whitespace() {
                continue;
            }
            let start_line = self.line;
            let start_col = self.column;
            let start = self.pos;
            if let Some(tok) = self.lex_comment(start_line, start_col) {
                spans.push((start, self.pos, tok.kind));
                self.last_kind = Some(tok.kind);
                if self.unclosed_block_comment {
                    break;
                }
                continue;
            }
            let tok = self.next_token();
            let end = self.pos;
            if tok.kind == TokenKind::End {
                break;
            }
            if tok.kind == TokenKind::Mismatch {
                let kind = if tok.value.starts_with("unterminated") {
                    // 未闭合字面量：按字面量色扫到行末，便于边敲边看。
                    TokenKind::StringLiteral
                } else {
                    TokenKind::Mismatch
                };
                let end = if end > start { end } else { self.source.len() };
                spans.push((start, end, kind));
                if end < self.source.len() {
                    spans.push((end, self.source.len(), TokenKind::Mismatch));
                }
                break;
            }
            spans.push((start, end, tok.kind));
            self.last_kind = Some(tok.kind);
        }
        spans
    }

    fn skip_bom(&mut self) {
        if self.source.starts_with('\u{feff}') {
            self.pos += '\u{feff}'.len_utf8();
        }
    }

    const fn is_at_end(&self) -> bool {
        self.pos >= self.source.len()
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn consume_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    fn skip_whitespace(&mut self) -> bool {
        let mut skipped = false;
        while matches!(self.peek_char(), Some(' ' | '\t' | '\r')) {
            self.consume_char();
            skipped = true;
        }
        skipped
    }

    /// 识别 `//` / `/* */`；非注释返回 `None`（不消耗输入）。
    fn lex_comment(&mut self, line: usize, column: usize) -> Option<Token> {
        if self.source[self.pos..].starts_with("//") {
            self.consume_char();
            self.consume_char();
            let mut text = String::new();
            while let Some(ch) = self.peek_char() {
                if ch == '\n' {
                    break;
                }
                text.push(ch);
                self.consume_char();
            }
            return Some(Token::new(TokenKind::LineComment, text, line, column));
        }
        if self.source[self.pos..].starts_with("/*") {
            self.consume_char(); // '/'
            self.consume_char(); // '*'
            let mut text = String::new();
            let mut closed = false;
            while !self.is_at_end() {
                if self.source[self.pos..].starts_with("*/") {
                    self.consume_char();
                    self.consume_char();
                    closed = true;
                    break;
                }
                if let Some(ch) = self.consume_char() {
                    text.push(ch);
                }
            }
            if !closed {
                self.unclosed_block_comment = true;
            }
            return Some(Token::new(TokenKind::BlockComment, text, line, column));
        }
        None
    }

    fn next_token(&mut self) -> Token {
        let line = self.line;
        let col = self.column;
        let ch = match self.peek_char() {
            Some(c) => c,
            None => return Token::new(TokenKind::End, "", line, col),
        };

        if ch == '\n' {
            self.consume_char();
            return Token::new(TokenKind::Newline, "\\n", line, col);
        }

        if self.source[self.pos..].starts_with("f\"\"\"") {
            return self.read_fstring_triple(line, col);
        }
        if self.source[self.pos..].starts_with("f\"") {
            return self.read_fstring(line, col);
        }

        if self.source[self.pos..].starts_with("b\"") {
            return self.read_bytes_string(line, col);
        }

        // 原始字符串：`r"..."` / `r"""..."""`（`r` 后必须是引号，否则走标识符）
        if ch == 'r' {
            let rest = &self.source[self.pos + 'r'.len_utf8()..];
            if rest.starts_with("\"\"\"") {
                return self.read_raw_triple(line, col);
            }
            if rest.starts_with('"') {
                return self.read_raw_string(line, col);
            }
        }

        if self.source[self.pos..].starts_with("\"\"\"") {
            return self.read_string_triple(line, col);
        }
        if ch == '"' {
            return self.read_string(line, col);
        }

        if ch == '_' && !self.identifier_continues_at(1) {
            self.consume_char();
            return Token::new(TokenKind::Placeholder, "_", line, col);
        }

        if ch.is_ascii_digit() || (ch == '.' && self.peek_next_is_digit()) {
            return self.read_number(line, col);
        }

        if ch == '-' && self.peek_next_is_number_start() && !self.minus_follows_complete_expr() {
            return self.read_number(line, col);
        }

        if is_ident_start(ch) {
            return self.read_ident(line, col);
        }

        // 三点省略号：保留为 Ellipsis。仅在语法需要块的位置（如 `func f() ...`）
        // 由解析器当成空块 `{}`；`catch (e: ...)` 等处保持原义。
        if self.source[self.pos..].starts_with("...") {
            self.pos += 3;
            self.column += 3;
            return Token::new(TokenKind::Ellipsis, "...", line, col);
        }

        // 双字符运算符（`<<`/`>>` 须先于 `<`/`>`）
        let two = &self.source[self.pos..];
        let (kind, len, text) = if two.starts_with("==") {
            (TokenKind::EqEq, 2, "==")
        } else if two.starts_with("!=") {
            (TokenKind::Ne, 2, "!=")
        } else if two.starts_with("<=") {
            (TokenKind::Le, 2, "<=")
        } else if two.starts_with(">=") {
            (TokenKind::Ge, 2, ">=")
        } else if two.starts_with("<<") {
            (TokenKind::LtLt, 2, "<<")
        } else if two.starts_with(">>") {
            (TokenKind::GtGt, 2, ">>")
        } else if two.starts_with("->") {
            (TokenKind::Arrow, 2, "->")
        } else if two.starts_with("=>") {
            (TokenKind::FatArrow, 2, "=>")
        } else if two.starts_with("|>") {
            (TokenKind::Pipe, 2, "|>")
        } else if two.starts_with("**") {
            (TokenKind::StarStar, 2, "**")
        } else if two.starts_with("::") {
            (TokenKind::ColonColon, 2, "::")
        } else if two.starts_with(":=") {
            (TokenKind::ColonEq, 2, ":=")
        } else {
            (TokenKind::Mismatch, 0, "")
        };
        if len > 0 {
            self.pos += len;
            self.column += len;
            return Token::new(kind, text, line, col);
        }

        let kind = match ch {
            '+' => TokenKind::Plus,
            '-' => TokenKind::Minus,
            '*' => TokenKind::Star,
            '/' => TokenKind::Slash,
            '%' => TokenKind::Percent,
            '&' => TokenKind::Ampersand,
            '^' => TokenKind::Caret,
            '~' => TokenKind::Tilde,
            '!' => TokenKind::Bang,
            '<' => TokenKind::Lt,
            '>' => TokenKind::Gt,
            '=' => TokenKind::Assign,
            ',' => TokenKind::Comma,
            '.' => TokenKind::Dot,
            ':' => TokenKind::Colon,
            '|' => TokenKind::Bar,
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            _ => TokenKind::Mismatch,
        };
        if kind == TokenKind::Mismatch {
            return Token::new(kind, ch.to_string(), line, col);
        }
        self.consume_char();
        Token::new(kind, ch.to_string(), line, col)
    }

    fn peek_next_is_digit(&self) -> bool {
        let rest = self.source[self.pos + '.'.len_utf8()..].chars().next();
        rest.is_some_and(|c| c.is_ascii_digit())
    }

    /// 在已完成的主表达式/后缀后，`-` 开启二元减法，而非负数字面量。
    const fn minus_follows_complete_expr(&self) -> bool {
        matches!(
            self.last_kind,
            Some(
                TokenKind::Identifier
                    | TokenKind::NumLiteral
                    | TokenKind::StringLiteral
                    | TokenKind::FStringLiteral
                    | TokenKind::BytesLiteral
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::RBrace
                    | TokenKind::Placeholder
            )
        )
    }

    fn peek_next_is_number_start(&self) -> bool {
        let mut iter = self.source[self.pos + '-'.len_utf8()..].chars();
        match iter.next() {
            Some(c) if c.is_ascii_digit() => true,
            Some('.') => iter.next().is_some_and(|c| c.is_ascii_digit()),
            _ => false,
        }
    }

    fn read_string(&mut self, line: usize, col: usize) -> Token {
        self.consume_char(); // 起始引号
        match self.read_string_body(false, false) {
            Ok(out) => Token::new(TokenKind::StringLiteral, out, line, col),
            Err(msg) => Token::new(TokenKind::Mismatch, msg, line, col),
        }
    }

    /// 多行字符串 `"""..."""`（支持转义与换行）。
    fn read_string_triple(&mut self, line: usize, col: usize) -> Token {
        self.pos += 3;
        self.column += 3;
        match self.read_string_body(true, false) {
            Ok(out) => Token::new(TokenKind::StringLiteral, out, line, col),
            Err(msg) => Token::new(TokenKind::Mismatch, msg, line, col),
        }
    }

    /// 原始字符串 `r"..."`（不处理转义；不可跨行）。
    fn read_raw_string(&mut self, line: usize, col: usize) -> Token {
        self.pos += 'r'.len_utf8();
        self.column += 1;
        self.consume_char(); // 起始引号
        match self.read_string_body(false, true) {
            Ok(out) => Token::new(TokenKind::StringLiteral, out, line, col),
            Err(msg) => Token::new(TokenKind::Mismatch, msg, line, col),
        }
    }

    /// 原始多行字符串 `r"""..."""`。
    fn read_raw_triple(&mut self, line: usize, col: usize) -> Token {
        self.pos += 'r'.len_utf8();
        self.column += 1;
        self.pos += 3;
        self.column += 3;
        match self.read_string_body(true, true) {
            Ok(out) => Token::new(TokenKind::StringLiteral, out, line, col),
            Err(msg) => Token::new(TokenKind::Mismatch, msg, line, col),
        }
    }

    /// `triple`：以 `"""` 结束并可含换行；`raw`：反斜杠原样保留。
    fn read_string_body(&mut self, triple: bool, raw: bool) -> Result<String, String> {
        let mut out = String::new();
        while let Some(ch) = self.peek_char() {
            if triple {
                if self.source[self.pos..].starts_with("\"\"\"") {
                    self.pos += 3;
                    self.column += 3;
                    return Ok(out);
                }
            } else if ch == '"' {
                self.consume_char();
                return Ok(out);
            } else if ch == '\n' {
                return Err("unterminated string".into());
            }
            if !raw && ch == '\\' {
                self.consume_char();
                match self.consume_char() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('x') => {
                        let b = self.read_hex_byte_escape()?;
                        out.push(char::from(b));
                    }
                    Some(c) => {
                        out.push('\\');
                        out.push(c);
                    }
                    None => return Err("unterminated escape".into()),
                }
                continue;
            }
            out.push(ch);
            self.consume_char();
        }
        Err(if triple {
            "unterminated triple-quoted string".into()
        } else {
            "unterminated string".into()
        })
    }

    /// `\xHH`：两位十六进制 → 一字节（0..=255）。
    fn read_hex_byte_escape(&mut self) -> Result<u8, String> {
        let hi = self.consume_char();
        let lo = self.consume_char();
        match (hi, lo) {
            (Some(h), Some(l)) => match (h.to_digit(16), l.to_digit(16)) {
                (Some(hh), Some(ll)) => Ok(((hh << 4) | ll) as u8),
                _ => Err("invalid \\x escape".into()),
            },
            _ => Err("unterminated \\x escape".into()),
        }
    }

    /// 字节字面量 `b"..."` — token 值为 Latin-1（每字节一个 char）。
    fn read_bytes_string(&mut self, line: usize, col: usize) -> Token {
        self.pos += 'b'.len_utf8();
        self.column += 1;
        self.consume_char(); // 起始引号
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(ch) = self.peek_char() {
            if ch == '"' {
                self.consume_char();
                let encoded: String = bytes
                    .iter()
                    .map(|&b| {
                        char::from_u32(u32::from(b)).expect(
                            "byte value 0-255 is always a valid char (theoretically unreachable)",
                        )
                    })
                    .collect();
                return Token::new(TokenKind::BytesLiteral, encoded, line, col);
            }
            if ch == '\n' {
                return Token::new(TokenKind::Mismatch, "unterminated bytes literal", line, col);
            }
            if ch == '\\' {
                self.consume_char();
                match self.consume_char() {
                    Some('n') => bytes.push(b'\n'),
                    Some('t') => bytes.push(b'\t'),
                    Some('r') => bytes.push(b'\r'),
                    Some('"') => bytes.push(b'"'),
                    Some('\\') => bytes.push(b'\\'),
                    Some('x') => match self.read_hex_byte_escape() {
                        Ok(b) => bytes.push(b),
                        Err(msg) => {
                            return Token::new(TokenKind::Mismatch, msg, line, col);
                        }
                    },
                    Some(c) if c.is_ascii() => bytes.push(c as u8),
                    Some(_) => {
                        return Token::new(
                            TokenKind::Mismatch,
                            "non-ASCII escape in bytes literal",
                            line,
                            col,
                        );
                    }
                    None => {
                        return Token::new(TokenKind::Mismatch, "unterminated escape", line, col);
                    }
                }
                continue;
            }
            if !ch.is_ascii() {
                return Token::new(
                    TokenKind::Mismatch,
                    "non-ASCII byte in bytes literal (use \\xHH)",
                    line,
                    col,
                );
            }
            bytes.push(ch as u8);
            self.consume_char();
        }
        Token::new(TokenKind::Mismatch, "unterminated bytes literal", line, col)
    }

    fn read_fstring(&mut self, line: usize, col: usize) -> Token {
        self.pos += 'f'.len_utf8();
        self.column += 1;
        self.consume_char(); // 起始引号
        match self.read_fstring_body(false) {
            Ok(out) => Token::new(TokenKind::FStringLiteral, out, line, col),
            Err(msg) => Token::new(TokenKind::Mismatch, msg, line, col),
        }
    }

    fn read_fstring_triple(&mut self, line: usize, col: usize) -> Token {
        self.pos += 'f'.len_utf8();
        self.column += 1;
        self.pos += 3;
        self.column += 3;
        match self.read_fstring_body(true) {
            Ok(out) => Token::new(TokenKind::FStringLiteral, out, line, col),
            Err(msg) => Token::new(TokenKind::Mismatch, msg, line, col),
        }
    }

    fn read_fstring_body(&mut self, triple: bool) -> Result<String, String> {
        let mut out = String::new();
        while let Some(ch) = self.peek_char() {
            if triple {
                if self.source[self.pos..].starts_with("\"\"\"") {
                    self.pos += 3;
                    self.column += 3;
                    return Ok(out);
                }
            } else if ch == '"' {
                self.consume_char();
                return Ok(out);
            } else if ch == '\n' {
                return Err("unterminated f-string".into());
            }
            if ch == '\\' {
                self.consume_char();
                match self.consume_char() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('{') => out.push('{'),
                    Some('}') => out.push('}'),
                    Some('x') => {
                        let b = self.read_hex_byte_escape()?;
                        out.push(char::from(b));
                    }
                    Some(c) => {
                        out.push('\\');
                        out.push(c);
                    }
                    None => return Err("unterminated escape".into()),
                }
                continue;
            }
            out.push(ch);
            self.consume_char();
        }
        Err(if triple {
            "unterminated triple-quoted f-string".into()
        } else {
            "unterminated f-string".into()
        })
    }

    fn read_number(&mut self, line: usize, col: usize) -> Token {
        let start = self.pos;
        let negative = if self.peek_char() == Some('-') {
            self.consume_char();
            true
        } else if self.peek_char() == Some('+') {
            self.consume_char();
            false
        } else {
            false
        };
        // 0x / 0b 前缀；其它 `0字母` 给出明确词法错误（勿拆成 `0` + 标识符误入装饰器）。
        if self.peek_char() == Some('0') {
            let next = self.source[self.pos + 1..].chars().next();
            match next {
                Some('x' | 'X') => {
                    self.consume_char();
                    self.consume_char();
                    let hex_start = self.pos;
                    while matches!(self.peek_char(), Some(c) if c.is_ascii_hexdigit()) {
                        self.consume_char();
                    }
                    if self.pos == hex_start {
                        return Token::new(
                            TokenKind::Mismatch,
                            "hex literal needs at least one digit after 0x",
                            line,
                            col,
                        );
                    }
                    let digits = &self.source[hex_start..self.pos];
                    return match i64::from_str_radix(digits, 16) {
                        Ok(n) => {
                            let n = if negative {
                                match n.checked_neg() {
                                    Some(v) => v,
                                    None => {
                                        return Token::new(
                                            TokenKind::Mismatch,
                                            format!("invalid hex literal: -0x{digits}"),
                                            line,
                                            col,
                                        );
                                    }
                                }
                            } else {
                                n
                            };
                            Token::new(TokenKind::NumLiteral, n.to_string(), line, col)
                        }
                        Err(_) => Token::new(
                            TokenKind::Mismatch,
                            format!("invalid hex literal: 0x{digits}"),
                            line,
                            col,
                        ),
                    };
                }
                Some('b' | 'B') => {
                    self.consume_char();
                    self.consume_char();
                    let bin_start = self.pos;
                    while matches!(self.peek_char(), Some('0' | '1')) {
                        self.consume_char();
                    }
                    if self.pos == bin_start {
                        return Token::new(
                            TokenKind::Mismatch,
                            "binary literal needs at least one digit after 0b",
                            line,
                            col,
                        );
                    }
                    let digits = &self.source[bin_start..self.pos];
                    return match i64::from_str_radix(digits, 2) {
                        Ok(n) => {
                            let n = if negative {
                                match n.checked_neg() {
                                    Some(v) => v,
                                    None => {
                                        return Token::new(
                                            TokenKind::Mismatch,
                                            format!("invalid binary literal: -0b{digits}"),
                                            line,
                                            col,
                                        );
                                    }
                                }
                            } else {
                                n
                            };
                            Token::new(TokenKind::NumLiteral, n.to_string(), line, col)
                        }
                        Err(_) => Token::new(
                            TokenKind::Mismatch,
                            format!("invalid binary literal: 0b{digits}"),
                            line,
                            col,
                        ),
                    };
                }
                Some(c) if c.is_ascii_alphabetic() => {
                    // `0i32` / `0u64` 等定宽后缀合法；仅拒绝真正的非法前缀（如 `0z`）。
                    let after_zero = &self.source[self.pos + 1..];
                    let mut end = 0;
                    for (i, ch) in after_zero.char_indices() {
                        if ch.is_ascii_alphanumeric() {
                            end = i + ch.len_utf8();
                        } else {
                            break;
                        }
                    }
                    let candidate = &after_zero[..end];
                    if !crate::sized::LITERAL_SUFFIXES.contains(&candidate) {
                        return Token::new(
                            TokenKind::Mismatch,
                            format!(
                                "unsupported numeric prefix '0{c}'; use 0x/0b, a sized suffix like 0i32, or a decimal literal"
                            ),
                            line,
                            col,
                        );
                    }
                    // 合法后缀：落入下方数字体 + 后缀扫描。
                }
                _ => {}
            }
        }
        while matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
            self.consume_char();
        }
        let mut is_rational = false;
        if self.peek_char() == Some('.') {
            is_rational = true;
            self.consume_char();
            while matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                self.consume_char();
            }
        }
        if matches!(self.peek_char(), Some('e' | 'E')) {
            is_rational = true;
            self.consume_char();
            if matches!(self.peek_char(), Some('+' | '-')) {
                self.consume_char();
            }
            while matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                self.consume_char();
            }
        }
        let text = self.source[start..self.pos].to_string();
        let _ = is_rational;
        // 定宽后缀：`1i32`、`3.14f64`（较长优先）
        let saved = self.pos;
        let saved_col = self.column;
        if let Some(ch) = self.peek_char() {
            if ch.is_ascii_alphabetic() {
                let suf_start = self.pos;
                while matches!(self.peek_char(), Some(c) if c.is_ascii_alphanumeric()) {
                    self.consume_char();
                }
                let suf = &self.source[suf_start..self.pos];
                if crate::sized::LITERAL_SUFFIXES.contains(&suf) {
                    let full = self.source[start..self.pos].to_string();
                    return Token::new(TokenKind::NumLiteral, full, line, col);
                }
                self.pos = saved;
                self.column = saved_col;
            }
        }
        Token::new(TokenKind::NumLiteral, text, line, col)
    }

    fn read_ident(&mut self, line: usize, col: usize) -> Token {
        let start = self.pos;
        self.consume_char();
        while self.identifier_continues_here() {
            self.consume_char();
        }
        let text = &self.source[start..self.pos];
        let kind = keyword_or_ident(text);
        Token::new(kind, text.to_string(), line, col)
    }

    fn identifier_continues_at(&self, offset: usize) -> bool {
        let idx = self.pos + offset;
        if idx >= self.source.len() {
            return false;
        }
        self.source[idx..]
            .chars()
            .next()
            .is_some_and(is_ident_continue)
    }

    fn identifier_continues_here(&self) -> bool {
        self.peek_char().is_some_and(is_ident_continue)
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::TokenKind;

    #[test]
    fn lex_basic() {
        let tokens = Lexer::new("let x = 42\n").tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::KwLet);
        assert_eq!(tokens[1].value, "x");
        assert_eq!(tokens[2].kind, TokenKind::Assign);
        assert_eq!(tokens[3].value, "42");
    }

    #[test]
    fn lex_string_and_ops() {
        let tokens = Lexer::new(r#""hi" == "hi""#).tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::StringLiteral);
        assert_eq!(tokens[1].kind, TokenKind::EqEq);
    }

    #[test]
    fn lex_subtraction_not_negative_literal() {
        let kinds: Vec<_> = Lexer::new("fib(n-1)")
            .tokenize()
            .unwrap()
            .into_iter()
            .filter(|t| t.kind != TokenKind::End)
            .map(|t| t.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Identifier,
                TokenKind::LParen,
                TokenKind::Identifier,
                TokenKind::Minus,
                TokenKind::NumLiteral,
                TokenKind::RParen,
            ]
        );
    }

    #[test]
    fn lex_unary_minus_still_works() {
        let kinds: Vec<_> = Lexer::new("x = -1")
            .tokenize()
            .unwrap()
            .into_iter()
            .filter(|t| !matches!(t.kind, TokenKind::End | TokenKind::Newline))
            .map(|t| t.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Identifier,
                TokenKind::Assign,
                TokenKind::NumLiteral,
            ]
        );
        assert_eq!(
            Lexer::new("-1").tokenize().unwrap().first().unwrap().value,
            "-1"
        );
    }
}
