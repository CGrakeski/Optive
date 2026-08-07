use crate::ast::*;
use crate::error::ParseError;
use crate::lexer::Lexer;
use crate::runtime_ast;
use crate::token::{Token, TokenKind};
use std::sync::Arc;

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    paren_depth: usize,
    bracket_depth: usize,
    brace_depth: usize,
    /// 仅在管道右侧与返回包装器中为 true；其余位置的 `_` 在解析期报错。
    allow_placeholder: bool,
    next_pipe_id: usize,
}

impl Parser {
    pub fn parse(source: &str) -> Result<Program, ParseError> {
        let tokens = Self::lex(source)?;
        let mut p = Self {
            tokens,
            pos: 0,
            paren_depth: 0,
            bracket_depth: 0,
            brace_depth: 0,
            allow_placeholder: false,
            next_pipe_id: 0,
        };
        let mut stmts = Vec::new();
        p.skip_newlines_only();
        while !p.is_at_end() {
            stmts.push(p.parse_stmt()?);
            p.skip_newlines_only();
        }
        Ok(Program { stmts })
    }

    /// 解析单个表达式（供 f-string `{...}` 片段）。
    pub fn parse_expr_from_source(source: &str) -> Result<Expr, ParseError> {
        let tokens = Self::lex(source)?;
        let mut p = Self {
            tokens,
            pos: 0,
            paren_depth: 0,
            bracket_depth: 0,
            brace_depth: 0,
            allow_placeholder: false,
            next_pipe_id: 0,
        };
        let expr = p.parse_expr()?;
        if !p.is_at_end() {
            return Err(ParseError::here(
                p.current().line,
                p.current().column,
                "unexpected tokens after expression",
            ));
        }
        Ok(expr)
    }

    fn lex(source: &str) -> Result<Vec<Token>, ParseError> {
        Lexer::new(source)
            .tokenize()
            .map_err(|e| match e {
                crate::error::LexError::Message {
                    line,
                    column,
                    message,
                } => ParseError::Message {
                    line,
                    column,
                    message: format!("lex error: {message}"),
                },
            })
    }

    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn is_at_end(&self) -> bool {
        self.current().kind == TokenKind::End
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if t.kind != TokenKind::End {
            self.pos += 1;
        }
        t
    }

    fn check(&self, kind: TokenKind) -> bool {
        self.current().kind == kind
    }

    fn match_kind(&mut self, kind: TokenKind) -> bool {
        // 表达式中的行/块注释在匹配前丢弃；语句级注释由 parse_stmt 单独收取。
        if !matches!(kind, TokenKind::LineComment | TokenKind::BlockComment) {
            while matches!(
                self.current().kind,
                TokenKind::LineComment | TokenKind::BlockComment
            ) {
                self.advance();
            }
        }
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind, msg: &str) -> Result<Token, ParseError> {
        while matches!(
            self.current().kind,
            TokenKind::LineComment | TokenKind::BlockComment
        ) {
            self.advance();
        }
        if self.check(kind) {
            Ok(self.advance())
        } else {
            Err(self.error(msg))
        }
    }

    fn error(&self, msg: &str) -> ParseError {
        ParseError::here(self.current().line, self.current().column, msg)
    }

    fn loc_here(&self) -> SourceLoc {
        SourceLoc::new(self.current().line, self.current().column)
    }

    fn skip_newlines(&mut self) {
        // 表达式等上下文：换行与注释均可跳过（注释不进中间 AST）。
        while matches!(
            self.current().kind,
            TokenKind::Newline | TokenKind::LineComment | TokenKind::BlockComment
        ) {
            self.advance();
        }
    }

    /// 仅跳过换行，保留注释 token（语句边界用，以便收成 `Stmt::Comment`）。
    fn skip_newlines_only(&mut self) {
        while self.match_kind_raw(TokenKind::Newline) {}
    }

    fn match_kind_raw(&mut self, kind: TokenKind) -> bool {
        if self.current().kind == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    /// 语句位置的 `{...}`：字典/集合字面量 vs 语句块。
    ///
    /// - `{}` / `{k: v}` / `{a, b}` → 字面量
    /// - `{ let x = 1 }` 等以语句关键字开头 → 块
    /// - `{ expr }` 单表达式（含 `{{}}`）→ 字面量，与表达式位置一致
    /// - `{ expr1 \n expr2 }` 深度 1 上多条语句 → 块
    fn brace_starts_dict(&self) -> bool {
        let mut i = self.pos;
        if self.tokens.get(i).map(|t| t.kind) != Some(TokenKind::LBrace) {
            return false;
        }
        i += 1;
        while i < self.tokens.len()
            && matches!(
                self.tokens[i].kind,
                TokenKind::Newline | TokenKind::LineComment | TokenKind::BlockComment
            )
        {
            i += 1;
        }
        if i >= self.tokens.len() {
            return false;
        }
        if self.tokens[i].kind == TokenKind::RBrace {
            return true;
        }
        match self.tokens[i].kind {
            TokenKind::KwLet
            | TokenKind::KwVar
            | TokenKind::KwFunc
            | TokenKind::KwIf
            | TokenKind::KwWhile
            | TokenKind::KwFor
            | TokenKind::KwLoop
            | TokenKind::KwReturn
            | TokenKind::KwBreak
            | TokenKind::KwContinue
            | TokenKind::KwThrow
            | TokenKind::KwTry
            | TokenKind::KwStruct
            | TokenKind::KwProtocol
            | TokenKind::KwTyped
            | TokenKind::KwMacro
            | TokenKind::KwEnum
            | TokenKind::KwVariant
            | TokenKind::KwMatch
            | TokenKind::KwWith
            | TokenKind::KwUse
            | TokenKind::KwImport
            | TokenKind::KwDel => return false,
            _ => {}
        }
        let mut brace_depth: usize = 1;
        let mut paren_depth: usize = 0;
        let mut bracket_depth: usize = 0;
        let mut j = i;
        let mut saw_collection_marker = false;
        let mut saw_stmt_break = false;
        while j < self.tokens.len() && brace_depth > 0 {
            match self.tokens[j].kind {
                TokenKind::LBrace => brace_depth += 1,
                TokenKind::RBrace => brace_depth -= 1,
                TokenKind::LParen => paren_depth += 1,
                TokenKind::RParen => paren_depth = paren_depth.saturating_sub(1),
                TokenKind::LBracket => bracket_depth += 1,
                TokenKind::RBracket => bracket_depth = bracket_depth.saturating_sub(1),
                // 只把花括号顶层、且不在 ()/[] 内的 `,`/`:` 当作集合/字典标记。
                TokenKind::Colon | TokenKind::Comma
                    if brace_depth == 1 && paren_depth == 0 && bracket_depth == 0 =>
                {
                    saw_collection_marker = true;
                }
                TokenKind::Newline
                    if brace_depth == 1 && paren_depth == 0 && bracket_depth == 0 =>
                {
                    let mut k = j + 1;
                    while k < self.tokens.len() && self.tokens[k].kind == TokenKind::Newline {
                        k += 1;
                    }
                    if k < self.tokens.len() && self.tokens[k].kind != TokenKind::RBrace {
                        // 深度 1 上换行后又有内容：多语句块（`{ a \n b }`），
                        // 除非已见 `,`/`:`（多行集合/字典字面量）。
                        // 在 () / [] 内的换行（如 `{ foo(\n  x\n) }`）不算语句分隔。
                        if !saw_collection_marker {
                            saw_stmt_break = true;
                        }
                    }
                }
                _ => {}
            }
            j += 1;
        }
        if saw_collection_marker {
            return true;
        }
        // 单表达式花括号：与 `a = {expr}` 一致，按集合/字典字面量解析。
        !saw_stmt_break
    }

    fn parse_stmt(&mut self) -> Result<LocatedStmt, ParseError> {
        self.skip_newlines_only();
        let line = self.current().line;
        let column = self.current().column;
        if matches!(
            self.current().kind,
            TokenKind::LineComment | TokenKind::BlockComment
        ) {
            let tok = self.advance();
            return Ok(LocatedStmt {
                line,
                column,
                stmt: Stmt::Comment {
                    is_block: tok.kind == TokenKind::BlockComment,
                    text: tok.value,
                },
            });
        }
        Ok(LocatedStmt {
            line,
            column,
            stmt: self.parse_stmt_body()?,
        })
    }

    fn parse_stmt_body(&mut self) -> Result<Stmt, ParseError> {
        self.skip_newlines();
        let vis = self.parse_visibility();
        let is_const = self.match_kind(TokenKind::KwConst);

        if self.match_kind(TokenKind::KwWith) && self.check(TokenKind::LParen) {
            return self.parse_with_stmt();
        }

        let mut decorators = self.parse_decorator_prefix()?;
        if self.match_kind(TokenKind::KwWith) {
            decorators.extend(self.parse_decorator_prefix()?);
            self.expect(TokenKind::KwMake, "expected 'make' after with")?;
            if self.match_kind(TokenKind::KwFunc) {
                return self.parse_func_decl(vis, decorators, false);
            }
            if self.match_kind(TokenKind::KwGen) {
                return self.parse_func_decl(vis, decorators, true);
            }
            if self.check(TokenKind::KwDo) {
                let loc = self.loc_here();
                self.advance();
                let do_expr = self.parse_do_func_expr(loc)?;
                return Ok(Stmt::Expr(self.apply_decorators_to_expr(decorators, do_expr)?));
            }
            return Err(self.error("expected 'func', 'gen', or 'do' after with make"));
        }
        if self.match_kind(TokenKind::KwFunc) {
            return self.parse_func_decl(vis, decorators, false);
        }
        if self.match_kind(TokenKind::KwGen) {
            return self.parse_func_decl(vis, decorators, true);
        }

        if is_const {
            // `const` 只能修饰 let/var；否则静默落到赋值会得到错误语义。
            if self.match_kind(TokenKind::KwLet) {
                return self.parse_var_or_destruct_decl(vis, true, false);
            }
            if self.match_kind(TokenKind::KwVar) {
                return self.parse_var_or_destruct_decl(vis, true, true);
            }
            return Err(self.error("expected 'let' or 'var' after 'const'"));
        }
        if self.match_kind(TokenKind::KwLet) {
            return self.parse_var_or_destruct_decl(vis, false, false);
        }
        if self.match_kind(TokenKind::KwVar) {
            return self.parse_var_or_destruct_decl(vis, false, true);
        }
        if !decorators.is_empty() {
            if self.check(TokenKind::KwDo) {
                let loc = self.loc_here();
                self.advance();
                let do_expr = self.parse_do_func_expr(loc)?;
                return Ok(Stmt::Expr(self.apply_decorators_to_expr(decorators, do_expr)?));
            }
            return Err(self.error("decorators must precede func or do"));
        }
        if self.match_kind(TokenKind::KwMacro) {
            return self.parse_macro_decl(vis);
        }
        if self.match_kind(TokenKind::KwFriend) {
            return self.parse_friend_func_decl(vis);
        }
        if self.match_kind(TokenKind::KwReturn) {
            let expr = if self.is_expr_start() {
                Some(self.parse_expr()?)
            } else {
                None
            };
            return Ok(Stmt::Return(expr));
        }
        if self.match_kind(TokenKind::KwYield) {
            // `yield from expr`
            if self.check(TokenKind::Identifier) && self.current().value == "from" {
                self.advance();
                let expr = self.parse_expr()?;
                return Ok(Stmt::YieldFrom(expr));
            }
            let expr = if self.is_expr_start() {
                Some(self.parse_expr()?)
            } else {
                None
            };
            return Ok(Stmt::Yield(expr));
        }
        if self.check(TokenKind::KwIf) {
            if self.tokens.get(self.pos + 1).map(|t| t.kind) == Some(TokenKind::LParen) {
                return self.parse_if();
            }
            let saved = self.pos;
            self.advance();
            let cond = self.parse_is_ex(false)?;
            if self.check(TokenKind::LBrace) {
                let then_block = self.parse_block()?;
                let mut elifs = Vec::new();
                loop {
                    if self.match_kind(TokenKind::KwElif) {
                        self.expect(TokenKind::LParen, "expected '(' after elif")?;
                        let c = self.parse_is_ex(false)?;
                        self.expect(TokenKind::RParen, "expected ')'")?;
                        elifs.push((c, self.parse_block()?));
                        continue;
                    }
                    if self.match_kind(TokenKind::KwElse) {
                        if self.match_kind(TokenKind::KwIf) {
                            let c = self.parse_is_ex(false)?;
                            if !self.check(TokenKind::LBrace) {
                                return Err(self.error("expected '{' after else-if condition"));
                            }
                            elifs.push((c, self.parse_block()?));
                            continue;
                        }
                        return Ok(Stmt::If {
                            cond,
                            then_block,
                            elifs,
                            else_block: Some(self.parse_block()?),
                        });
                    }
                    break;
                }
                return Ok(Stmt::If {
                    cond,
                    then_block,
                    elifs,
                    else_block: None,
                });
            }
            self.pos = saved;
            return Ok(Stmt::Expr(self.parse_expr()?));
        }
        if self.match_kind(TokenKind::KwWhile) {
            self.expect(TokenKind::LParen, "expected '(' after while")?;
            let cond = self.parse_expr()?;
            self.expect(TokenKind::RParen, "expected ')'")?;
            let body = self.parse_block()?;
            return Ok(Stmt::While { cond, body });
        }
        if self.match_kind(TokenKind::KwLoop) {
            let count = if self.check(TokenKind::LParen) {
                self.advance();
                let e = self.parse_expr()?;
                self.expect(TokenKind::RParen, "expected ')'")?;
                Some(e)
            } else {
                None
            };
            let body = self.parse_block()?;
            return Ok(Stmt::Loop { count, body });
        }
        if self.match_kind(TokenKind::KwFor) {
            return self.parse_for();
        }
        if self.match_kind(TokenKind::KwBreak) {
            return Ok(Stmt::Break);
        }
        if self.match_kind(TokenKind::KwContinue) {
            return Ok(Stmt::Continue);
        }
        if self.match_kind(TokenKind::KwThrow) {
            return Ok(Stmt::Throw(self.parse_expr()?));
        }
        if self.match_kind(TokenKind::KwTry) {
            return self.parse_try();
        }
        if self.match_kind(TokenKind::KwMatch) {
            return self.parse_match();
        }
        if self.match_kind(TokenKind::KwDel) {
            return Ok(Stmt::Del(self.parse_del_target()?));
        }
        if self.match_kind(TokenKind::KwImport) {
            return self.parse_import();
        }
        if self.match_kind(TokenKind::KwUse) {
            return self.parse_use();
        }
        if self.match_kind(TokenKind::KwTyped) {
            self.expect(TokenKind::KwStruct, "expected struct after typed")?;
            return self.parse_struct(vis, true);
        }
        if self.match_kind(TokenKind::KwStruct) {
            return self.parse_struct(vis, false);
        }
        if self.match_kind(TokenKind::KwEnum) {
            return self.parse_enum(vis);
        }
        if self.match_kind(TokenKind::KwVariant) {
            return self.parse_variant(vis);
        }
        if self.match_kind(TokenKind::KwProtocol) {
            return self.parse_protocol_decl(vis);
        }
        if self.check(TokenKind::LBrace) && self.brace_starts_dict() {
            let expr = self.parse_expr()?;
            return Ok(Stmt::Expr(expr));
        }
        // `parse_block_inner` 自己消费 `{`，此处不可先 match。
        if self.check(TokenKind::LBrace) {
            let body = self.parse_block_inner()?;
            return Ok(Stmt::Block(body));
        }

        // 解构赋值：`(x, y) = ...` / `[a, *rest] = ...`
        if self.check(TokenKind::LParen) || self.check(TokenKind::LBracket) {
            let saved = self.pos;
            if let Ok(pattern) = self.parse_destruct_pattern() {
                if self.match_kind(TokenKind::Assign) {
                    let value = self.parse_expr()?;
                    return Ok(Stmt::DestructAssign { pattern, value });
                }
            }
            self.pos = saved;
        }

        // 赋值或表达式语句
        let expr = self.parse_expr()?;
        if self.match_kind(TokenKind::Assign) {
            let value = self.parse_expr()?;
            let target = expr_to_lvalue(expr)?;
            return Ok(Stmt::Assign { target, value });
        }
        Ok(Stmt::Expr(expr))
    }

    fn parse_del_target(&mut self) -> Result<DelTarget, ParseError> {
        let expr = self.parse_postfix()?;
        match &expr.kind {
            ExprKind::Var(name) => Ok(DelTarget::Name(name.clone())),
            ExprKind::Member { object, field } => Ok(DelTarget::Member {
                object: object.clone(),
                field: field.clone(),
            }),
            ExprKind::Index { object, index } => Ok(DelTarget::Index {
                object: object.clone(),
                index: index.clone(),
            }),
            _ => Err(self.error("invalid del target")),
        }
    }

    fn parse_visibility(&mut self) -> Visibility {
        if self.match_kind(TokenKind::KwIntern) {
            Visibility::Internal
        } else if self.match_kind(TokenKind::KwExport) {
            Visibility::Exported
        } else {
            Visibility::Default
        }
    }

    fn parse_var_or_destruct_decl(
        &mut self,
        visibility: Visibility,
        is_const: bool,
        is_var_kw: bool,
    ) -> Result<Stmt, ParseError> {
        if self.check(TokenKind::LParen)
            || self.check(TokenKind::LBracket)
            || self.check(TokenKind::Placeholder)
        {
            let pattern = self.parse_destruct_pattern()?;
            self.expect(TokenKind::Assign, "expected '=' in destructuring declaration")?;
            let init = self.parse_expr()?;
            return Ok(Stmt::DestructDecl {
                visibility,
                is_const,
                is_var: is_var_kw,
                pattern,
                init,
            });
        }
        self.parse_var_decl(visibility, is_const, is_var_kw)
    }

    fn parse_var_decl(
        &mut self,
        visibility: Visibility,
        is_const: bool,
        is_var_kw: bool,
    ) -> Result<Stmt, ParseError> {
        let name = self
            .expect(TokenKind::Identifier, "expected variable name")?
            .value;
        let (type_expr, type_strong) = self.parse_optional_type()?;
        self.expect(TokenKind::Assign, "expected '=' in variable declaration")?;
        let init = self.parse_expr()?;
        Ok(Stmt::VarDecl {
            visibility,
            is_const,
            is_var: is_var_kw,
            name,
            type_expr,
            type_strong,
            init: Some(init),
        })
    }

    /// `(a, [b, *rest], _)` / `[x, y]` / `name` / `_`
    fn parse_destruct_pattern(&mut self) -> Result<DestructPattern, ParseError> {
        if self.match_kind(TokenKind::Placeholder) {
            return Ok(DestructPattern::Discard);
        }
        if self.match_kind(TokenKind::LParen) {
            let elems = self.parse_destruct_elem_list(TokenKind::RParen, "')'")?;
            return Ok(DestructPattern::Tuple(elems));
        }
        if self.match_kind(TokenKind::LBracket) {
            let elems = self.parse_destruct_elem_list(TokenKind::RBracket, "']'")?;
            return Ok(DestructPattern::List(elems));
        }
        if self.check(TokenKind::Identifier) {
            return Ok(DestructPattern::Name(self.advance().value));
        }
        Err(self.error("expected destructuring pattern"))
    }

    fn parse_destruct_elem_list(
        &mut self,
        end: TokenKind,
        end_label: &str,
    ) -> Result<Vec<DestructElem>, ParseError> {
        let mut elems = Vec::new();
        let mut saw_rest = false;
        self.skip_newlines();
        if self.check(end) {
            self.advance();
            return Ok(elems);
        }
        loop {
            self.skip_newlines();
            elems.push(self.parse_destruct_elem(&mut saw_rest)?);
            self.skip_newlines();
            if self.match_kind(TokenKind::Comma) {
                self.skip_newlines();
                if self.check(end) {
                    break;
                }
                continue;
            }
            break;
        }
        self.skip_newlines();
        self.expect(end, &format!("expected {end_label} in destructuring pattern"))?;
        Ok(elems)
    }

    fn parse_destruct_elem(&mut self, saw_rest: &mut bool) -> Result<DestructElem, ParseError> {
        if self.match_kind(TokenKind::Star) {
            if *saw_rest {
                return Err(self.error("multiple *rest in destructuring pattern"));
            }
            *saw_rest = true;
            if self.match_kind(TokenKind::Placeholder) {
                return Ok(DestructElem::RestDiscard);
            }
            let name = self
                .expect(TokenKind::Identifier, "expected name after '*' in destructuring")?
                .value;
            return Ok(DestructElem::Rest(name));
        }
        Ok(DestructElem::Pat(self.parse_destruct_pattern()?))
    }

    /// `: T` / `:: T` — 注解是普通表达式（`num`、`list[num]`、`type(do(){})`、`C.types.int`…）。
    fn parse_optional_type(&mut self) -> Result<(Option<Expr>, bool), ParseError> {
        let strong = if self.match_kind(TokenKind::ColonColon) {
            true
        } else if self.match_kind(TokenKind::Colon) {
            false
        } else {
            return Ok((None, false));
        };
        Ok((Some(self.parse_type_annotation_expr()?), strong))
    }

    /// 类型注解位置的表达式：与 postfix 同级，覆盖 Call / Index / Member。
    fn parse_type_annotation_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_postfix_no_macro()
    }

    fn parse_decorator_expr(&mut self) -> Result<Expr, ParseError> {
        let loc = self.loc_here();
        let name = self
            .expect(TokenKind::Identifier, "expected decorator name")?
            .value;
        let mut expr = Expr::new(loc, ExprKind::Var(name));
        loop {
            self.skip_newlines();
            if self.match_kind(TokenKind::LParen) {
                let args = self.parse_call_args(true)?;
                self.expect(TokenKind::RParen, "expected ')' after decorator arguments")?;
                let call_loc = expr.loc;
                expr = Expr::new(
                    call_loc,
                    ExprKind::Call {
                        callee: Box::new(expr),
                        args,
                    },
                );
            } else if self.check(TokenKind::Dot) {
                // `Name.(value)` 是类型转换，不是装饰器属性链；留给表达式解析。
                if self.tokens.get(self.pos + 1).map(|t| t.kind) == Some(TokenKind::LParen) {
                    break;
                }
                self.advance();
                let field = self.parse_member_name()?;
                let mem_loc = expr.loc;
                expr = Expr::new(
                    mem_loc,
                    ExprKind::Member {
                        object: Box::new(expr),
                        field,
                    },
                );
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_decorator_prefix(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut decos = Vec::new();
        loop {
            let pos = self.pos;
            if !self.check(TokenKind::Identifier) || self.is_stmt_keyword_at_decorator_boundary() {
                break;
            }
            let deco = self.parse_decorator_expr()?;
            let after_deco = self.pos;
            self.skip_newlines();
            // 仅当后续确实是装饰器链并落到 func/do/with 时才提交；
            // 避免 `foo()\nbar.(1)` 把上一行表达式误判为装饰器。
            if !self.decorator_chain_leads_to_target() {
                self.pos = pos;
                break;
            }
            self.pos = after_deco;
            self.skip_newlines();
            decos.push(deco);
        }
        Ok(decos)
    }

    /// 从当前位置起，能否看到 `decorator* (func|do|with)`（允许装饰器间换行）。
    fn decorator_chain_leads_to_target(&self) -> bool {
        let mut i = self.pos;
        while matches!(
            self.tokens.get(i).map(|t| t.kind),
            Some(TokenKind::Newline | TokenKind::LineComment | TokenKind::BlockComment)
        ) {
            i += 1;
        }
        loop {
            match self.tokens.get(i).map(|t| t.kind) {
                Some(TokenKind::KwFunc | TokenKind::KwDo) => return true,
                Some(TokenKind::KwWith) => {
                    // 仅 `with make` 是装饰器目标；`with (` 是 with 语句。
                    let mut j = i + 1;
                    while matches!(
                        self.tokens.get(j).map(|t| t.kind),
                        Some(
                            TokenKind::Newline
                                | TokenKind::LineComment
                                | TokenKind::BlockComment
                        )
                    ) {
                        j += 1;
                    }
                    return self.tokens.get(j).map(|t| t.kind) == Some(TokenKind::KwMake);
                }
                Some(TokenKind::Identifier) => {
                    if self.is_stmt_keyword_token_at(i) {
                        return false;
                    }
                    i += 1;
                    loop {
                        while matches!(
                            self.tokens.get(i).map(|t| t.kind),
                            Some(
                                TokenKind::Newline
                                    | TokenKind::LineComment
                                    | TokenKind::BlockComment
                            )
                        ) {
                            i += 1;
                        }
                        match self.tokens.get(i).map(|t| t.kind) {
                            Some(TokenKind::Dot) => {
                                if self.tokens.get(i + 1).map(|t| t.kind) == Some(TokenKind::LParen)
                                {
                                    // `Name.(...)` 是类型转换，不是装饰器。
                                    return false;
                                }
                                i += 1;
                                if self.tokens.get(i).map(|t| t.kind) != Some(TokenKind::Identifier)
                                {
                                    return false;
                                }
                                i += 1;
                            }
                            Some(TokenKind::LParen) => match Self::skip_balanced_tokens(
                                &self.tokens,
                                i,
                                TokenKind::LParen,
                                TokenKind::RParen,
                            ) {
                                Some(next) => i = next,
                                None => return false,
                            },
                            _ => break,
                        }
                    }
                }
                _ => return false,
            }
        }
    }

    fn is_stmt_keyword_token_at(&self, index: usize) -> bool {
        matches!(
            self.tokens.get(index).map(|t| t.kind),
            Some(
                TokenKind::KwFunc
                    | TokenKind::KwMacro
                    | TokenKind::KwFriend
                    | TokenKind::KwStruct
                    | TokenKind::KwProtocol
                    | TokenKind::KwTyped
                    | TokenKind::KwDo
                    | TokenKind::KwWith
                    | TokenKind::KwLet
                    | TokenKind::KwVar
                    | TokenKind::KwReturn
                    | TokenKind::KwIf
                    | TokenKind::KwWhile
                    | TokenKind::KwFor
                    | TokenKind::KwLoop
                    | TokenKind::KwBreak
                    | TokenKind::KwContinue
                    | TokenKind::KwThrow
                    | TokenKind::KwTry
                    | TokenKind::KwMatch
                    | TokenKind::KwDel
                    | TokenKind::KwImport
                    | TokenKind::KwUse
            )
        )
    }

    fn skip_balanced_tokens(
        tokens: &[Token],
        start: usize,
        open: TokenKind,
        close: TokenKind,
    ) -> Option<usize> {
        if tokens.get(start).map(|t| t.kind) != Some(open) {
            return None;
        }
        let mut depth = 0usize;
        let mut i = start;
        while i < tokens.len() {
            let kind = tokens[i].kind;
            if kind == open {
                depth += 1;
            } else if kind == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            } else if kind == TokenKind::End {
                return None;
            }
            i += 1;
        }
        None
    }

    fn is_stmt_keyword_at_decorator_boundary(&self) -> bool {
        self.is_stmt_keyword_token_at(self.pos)
    }

    fn apply_decorators_to_expr(
        &self,
        decorators: Vec<Expr>,
        inner: Expr,
    ) -> Result<Expr, ParseError> {
        let mut expr = inner;
        for deco in decorators.into_iter().rev() {
            { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::Call {
                callee: Box::new(deco),
                args: vec![CallArg {
                    name: None,
                    is_splat: false,
                    is_kwsplat: false,
                    value: expr,
                }],
            }); }
        }
        Ok(expr)
    }

    fn parse_do_func_expr(&mut self, loc: SourceLoc) -> Result<Expr, ParseError> {
        // `do { ... }` ≈ `do() { ... } ()` — 无参 IIFE 糖。
        if self.check(TokenKind::LBrace) {
            let body = self.parse_block()?;
            let do_func = Expr::new(
                loc,
                ExprKind::DoFunc {
                    params: Vec::new(),
                    return_type: None,
                    return_strong: false,
                    return_wrapper: None,
                    body,
                },
            );
            return Ok(Expr::new(
                loc,
                ExprKind::Call {
                    callee: Box::new(do_func),
                    args: Vec::new(),
                },
            ));
        }
        self.expect(TokenKind::LParen, "expected '(' or '{' after do")?;
        let params = self.parse_param_list()?;
        self.skip_newlines();
        self.expect(TokenKind::RParen, "expected ')'")?;
        let (return_type, return_strong, return_wrapper) = self.parse_optional_return()?;
        let body = self.parse_block()?;
        Ok(Expr::new(
            loc,
            ExprKind::DoFunc {
                params,
                return_type: return_type.map(Box::new),
                return_strong,
                return_wrapper: return_wrapper.map(Box::new),
                body,
            },
        ))
    }

    fn parse_with_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::LParen, "expected '(' after with")?;
        self.skip_newlines();
        let context = self.parse_expr()?;
        let mut alias = None;
        if self.match_kind(TokenKind::KwAs) {
            alias = Some(
                self.expect(TokenKind::Identifier, "expected name after as")?
                    .value,
            );
        }
        self.skip_newlines();
        self.expect(TokenKind::RParen, "expected ')'")?;
        let body = self.parse_block()?;
        Ok(Stmt::With {
            context,
            alias,
            body,
        })
    }

    fn parse_type_param_list(&mut self) -> Result<Vec<(String, Option<Expr>)>, ParseError> {
        let mut params = Vec::new();
        loop {
            let pname = self
                .expect(TokenKind::Identifier, "expected type param name")?
                .value;
            let bound = if self.match_kind(TokenKind::Colon) {
                Some(self.parse_type_annotation_expr()?)
            } else {
                None
            };
            params.push((pname, bound));
            if !self.match_kind(TokenKind::Comma) {
                break;
            }
        }
        Ok(params)
    }

    fn parse_func_decl(
        &mut self,
        visibility: Visibility,
        decorators: Vec<Expr>,
        is_generator: bool,
    ) -> Result<Stmt, ParseError> {
        let name = self
            .expect(TokenKind::Identifier, "expected function name")?
            .value;
        let type_params = if self.match_kind(TokenKind::LBracket) {
            let params = self.parse_type_param_list()?;
            self.expect(TokenKind::RBracket, "expected ']' after type params")?;
            params
        } else {
            Vec::new()
        };
        self.expect(TokenKind::LParen, "expected '('")?;
        let params = self.parse_param_list()?;
        self.skip_newlines();
        self.expect(TokenKind::RParen, "expected ')'")?;
        let (return_type, return_strong, return_wrapper) = self.parse_optional_return()?;
        let body = self.parse_block()?;
        Ok(Stmt::FuncDecl {
            visibility,
            decorators,
            name,
            type_params,
            params,
            return_type,
            return_strong,
            return_wrapper,
            body,
            is_generator,
        })
    }

    fn parse_protocol_decl(&mut self, visibility: Visibility) -> Result<Stmt, ParseError> {
        let name = self
            .expect(TokenKind::Identifier, "expected protocol name")?
            .value;
        self.expect(TokenKind::LBrace, "expected '{' after protocol name")?;
        let mut members = Vec::new();
        self.skip_newlines();
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            members.push(self.parse_protocol_member()?);
            self.skip_newlines();
        }
        self.expect(TokenKind::RBrace, "expected '}' after protocol body")?;
        Ok(Stmt::ProtocolDecl {
            visibility,
            name,
            members,
        })
    }

    fn parse_protocol_member(&mut self) -> Result<ProtocolMember, ParseError> {
        if self.match_kind(TokenKind::KwFunc) {
            let method_name = self
                .expect(TokenKind::Identifier, "expected protocol method name")?
                .value;
            self.expect(TokenKind::LParen, "expected '('")?;
            let params = self.parse_param_list()?;
            self.skip_newlines();
            self.expect(TokenKind::RParen, "expected ')'")?;
            let body = self.parse_block()?;
            if !Self::is_empty_protocol_body(&body) {
                return Err(self.error("protocol method body must be empty"));
            }
            return Ok(ProtocolMember::Method {
                name: method_name,
                params,
            });
        }
        let mutable = if self.match_kind(TokenKind::KwVar) {
            true
        } else {
            self.expect(TokenKind::KwLet, "expected 'func', 'var', or 'let' in protocol")?;
            false
        };
        let field_name = self
            .expect(TokenKind::Identifier, "expected field name")?
            .value;
        let _ = self.parse_optional_type()?;
        Ok(ProtocolMember::Field {
            name: field_name,
            mutable,
        })
    }

    fn is_empty_protocol_body(body: &Block) -> bool {
        body.is_empty()
            || body.iter().all(|s| match &s.stmt {
                Stmt::Block(b) => Self::is_empty_protocol_body(b),
                _ => false,
            })
    }

    fn parse_macro_decl(&mut self, visibility: Visibility) -> Result<Stmt, ParseError> {
        let name = self
            .expect(TokenKind::Identifier, "expected macro name")?
            .value;
        self.expect(TokenKind::LParen, "expected '('")?;
        let params = self.parse_macro_param_list()?;
        self.expect(TokenKind::RParen, "expected ')'")?;
        let body = self.parse_block()?;
        Ok(Stmt::MacroDecl {
            visibility,
            name,
            params,
            body,
        })
    }

    /// 逗号分隔列表，允许尾逗号；在 `end` token 处结束。
    fn parse_comma_list_until<T>(
        &mut self,
        end: TokenKind,
        parse_item: fn(&mut Self) -> Result<T, ParseError>,
    ) -> Result<Vec<T>, ParseError> {
        self.skip_newlines();
        if self.check(end) {
            return Ok(Vec::new());
        }
        let mut items = vec![parse_item(self)?];
        while self.match_kind(TokenKind::Comma) {
            self.skip_newlines();
            if self.check(end) {
                break;
            }
            items.push(parse_item(self)?);
        }
        // 无尾逗号时也跳过换行，允许右括号/结束符落在下一行。
        self.skip_newlines();
        Ok(items)
    }

    fn parse_macro_param_list(&mut self) -> Result<Vec<MacroParam>, ParseError> {
        self.parse_comma_list_until(TokenKind::RParen, Self::parse_macro_param)
    }

    fn parse_macro_param(&mut self) -> Result<MacroParam, ParseError> {
        let is_variadic = self.match_kind(TokenKind::Star);
        let name = self
            .expect(TokenKind::Identifier, "expected parameter name")?
            .value;
        let (type_expr, type_strong) = self.parse_optional_type()?;
        Ok(MacroParam {
            name,
            is_variadic,
            type_expr,
            type_strong,
        })
    }

    fn parse_friend_func_decl(&mut self, visibility: Visibility) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwFunc, "expected 'func' after friend")?;
        let name = self
            .expect(TokenKind::Identifier, "expected friend func name")?
            .value;
        if self.check(TokenKind::LParen) {
            self.expect(TokenKind::LParen, "expected '('")?;
            let params = self.parse_param_list()?;
            self.skip_newlines();
            self.expect(TokenKind::RParen, "expected ')'")?;
            let (return_type, return_strong, return_wrapper) = self.parse_optional_return()?;
            let body = self.parse_block()?;
            Ok(Stmt::FriendFuncDecl {
                visibility,
                name,
                params: Some(params),
                return_type,
                return_strong,
                return_wrapper,
                body: Some(body),
            })
        } else {
            Ok(Stmt::FriendFuncDecl {
                visibility,
                name,
                params: None,
                return_type: None,
                return_strong: false,
                return_wrapper: None,
                body: None,
            })
        }
    }

    fn parse_quote_expr(&mut self) -> Result<Expr, ParseError> {
        let loc = self.loc_here();
        self.advance(); // quote 关键字
        let mut hygienic = Vec::new();
        if self.match_kind(TokenKind::LParen) {
            self.skip_newlines();
            if !self.check(TokenKind::RParen) {
                hygienic.push(
                    self.expect(TokenKind::Identifier, "expected hygienic name")?
                        .value,
                );
                while self.match_kind(TokenKind::Comma) {
                    self.skip_newlines();
                    hygienic.push(
                        self.expect(TokenKind::Identifier, "expected hygienic name")?
                            .value,
                    );
                }
            }
            self.skip_newlines();
            self.expect(TokenKind::RParen, "expected ')'")?;
        }
        let mut bindings = Vec::new();
        if self.match_kind(TokenKind::KwWith) {
            self.expect(TokenKind::LParen, "expected '(' after with")?;
            self.skip_newlines();
            if !self.check(TokenKind::RParen) {
                bindings.push(self.parse_expr()?);
                while self.match_kind(TokenKind::Comma) {
                    self.skip_newlines();
                    bindings.push(self.parse_expr()?);
                }
            }
            self.skip_newlines();
            self.expect(TokenKind::RParen, "expected ')'")?;
        }
        let body = self.parse_block()?;
        Ok(Expr::new(
            loc,
            ExprKind::Quote {
                hygienic_names: hygienic,
                bindings,
                body,
            },
        ))
    }

    fn parse_optional_return(
        &mut self,
    ) -> Result<(Option<Expr>, bool, Option<Expr>), ParseError> {
        // `-> T` 软返回；`=> T` 强返回；可选 `: Wrap(_)` 包装器（`_` 为返回值洞，先包装再按返回类型检查）。
        let (return_type, return_strong) = if self.match_kind(TokenKind::FatArrow) {
            (Some(self.parse_type_annotation_expr()?), true)
        } else if self.match_kind(TokenKind::Arrow) {
            (Some(self.parse_type_annotation_expr()?), false)
        } else {
            (None, false)
        };
        let return_wrapper = if self.match_kind(TokenKind::Colon) {
            let prev = self.allow_placeholder;
            self.allow_placeholder = true;
            let wrapper = self.parse_postfix_no_macro()?;
            self.allow_placeholder = prev;
            let repl = Expr::at(0, 1, ExprKind::Var(RET_WRAPPER_VAL.into()));
            Some(fill_placeholders(&wrapper, &repl))
        } else {
            None
        };
        Ok((return_type, return_strong, return_wrapper))
    }

    fn parse_param_list(&mut self) -> Result<Vec<FuncParam>, ParseError> {
        self.skip_newlines();
        if self.check(TokenKind::RParen) {
            return Ok(Vec::new());
        }
        let mut params = vec![self.parse_param()?];
        while self.match_kind(TokenKind::Comma) {
            self.skip_newlines();
            if self.check(TokenKind::RParen) {
                break;
            }
            params.push(self.parse_param()?);
        }
        self.validate_param_list(&params)?;
        Ok(params)
    }

    fn validate_param_list(&self, params: &[FuncParam]) -> Result<(), ParseError> {
        let mut seen_default = false;
        let mut seen_var = false;
        let mut seen_kwvar = false;
        for p in params {
            if p.is_kwvariadic {
                if seen_kwvar {
                    return Err(self.error("duplicate **kwargs parameter"));
                }
                seen_kwvar = true;
                continue;
            }
            if seen_kwvar {
                return Err(self.error("parameter after **kwargs"));
            }
            if p.is_variadic {
                if seen_var {
                    return Err(self.error("duplicate *args parameter"));
                }
                seen_var = true;
                continue;
            }
            if seen_var {
                return Err(self.error("positional parameter after *args"));
            }
            if p.default_expr.is_some() {
                seen_default = true;
            } else if seen_default {
                return Err(self.error(
                    "non-default parameter follows parameter with default",
                ));
            }
        }
        Ok(())
    }

    fn parse_param(&mut self) -> Result<FuncParam, ParseError> {
        let is_kwvariadic = self.match_kind(TokenKind::StarStar);
        let is_variadic = if is_kwvariadic {
            false
        } else {
            self.match_kind(TokenKind::Star)
        };
        let implicit = if self.check(TokenKind::Identifier) && self.current().value == "implicit" {
            self.advance();
            true
        } else {
            false
        };
        let name = self
            .expect(TokenKind::Identifier, "expected parameter name")?
            .value;
        let (type_expr, type_strong) = self.parse_optional_type()?;
        let default_expr = if !is_variadic && !is_kwvariadic && self.match_kind(TokenKind::Assign)
        {
            Some(self.parse_expr()?)
        } else {
            None
        };
        if (is_variadic || is_kwvariadic) && default_expr.is_some() {
            return Err(self.error("*args/**kwargs cannot have defaults"));
        }
        if implicit && type_expr.is_none() {
            return Err(self.error("implicit parameter requires a type annotation"));
        }
        Ok(FuncParam {
            name,
            is_variadic,
            is_kwvariadic,
            implicit,
            type_expr,
            type_strong,
            default_expr,
        })
    }

    fn parse_block(&mut self) -> Result<Block, ParseError> {
        // 裸 `...` 等价于空块 `{}`（不是花括号内的语句）。
        let bf_pos = self.pos;
        (*self).skip_newlines();
        if (*self).check(TokenKind::Ellipsis) {
            self.pos += 1;
            return Ok(Block::new());
        } else {
            self.pos = bf_pos;
        }
        self.parse_block_inner()
    }

    fn parse_block_inner(&mut self) -> Result<Block, ParseError> {
        let prev_placeholder = self.allow_placeholder;
        self.allow_placeholder = false;
        self.expect(TokenKind::LBrace, "expected '{'")?;
        self.brace_depth += 1;
        let mut stmts = Vec::new();
        self.skip_newlines_only();
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            stmts.push(self.parse_stmt()?);
            self.skip_newlines_only();
        }
        self.expect(TokenKind::RBrace, "expected '}'")?;
        self.brace_depth -= 1;
        self.allow_placeholder = prev_placeholder;
        Ok(stmts)
    }

    fn parse_if_paren_cond(&mut self) -> Result<Expr, ParseError> {
        self.expect(TokenKind::LParen, "expected '(' after if")?;
        let cond = self.parse_expr()?;
        self.expect(TokenKind::RParen, "expected ')'")?;
        Ok(cond)
    }

    fn parse_if(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwIf, "expected 'if'")?;
        let cond = self.parse_if_paren_cond()?;
        let then_block = self.parse_block()?;
        let mut elifs = Vec::new();
        loop {
            if self.match_kind(TokenKind::KwElif) {
                elifs.push((self.parse_if_paren_cond()?, self.parse_block()?));
                continue;
            }
            if self.match_kind(TokenKind::KwElse) {
                if self.match_kind(TokenKind::KwIf) {
                    elifs.push((self.parse_if_paren_cond()?, self.parse_block()?));
                    continue;
                }
                return Ok(Stmt::If {
                    cond,
                    then_block,
                    elifs,
                    else_block: Some(self.parse_block()?),
                });
            }
            break;
        }
        Ok(Stmt::If {
            cond,
            then_block,
            elifs,
            else_block: None,
        })
    }

    fn parse_for_items_in_parens(&mut self) -> Result<Vec<ForItem>, ParseError> {
        self.expect(TokenKind::LParen, "expected '(' after for")?;
        let mut items = Vec::new();
        loop {
            let name = if self.match_kind(TokenKind::Placeholder) {
                "_".into()
            } else {
                self.expect(TokenKind::Identifier, "expected loop variable")?
                    .value
            };
            self.expect(TokenKind::KwIn, "expected 'in'")?;
            let iterable = self.parse_expr()?;
            items.push(ForItem { name, iterable });
            if !self.match_kind(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen, "expected ')'")?;
        Ok(items)
    }

    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        let items = self.parse_for_items_in_parens()?;
        let body = self.parse_block()?;
        Ok(Stmt::For { items, body })
    }

    fn parse_try(&mut self) -> Result<Stmt, ParseError> {
        let body = self.parse_block()?;
        let mut catches = Vec::new();
        while self.match_kind(TokenKind::KwCatch) {
            self.expect(TokenKind::LParen, "expected '(' after catch")?;
            // `catch (...)` 通配；`catch (e)` / `catch (e: T)` / `catch (e: ...)` 绑定。
            let pattern = if self.match_kind(TokenKind::Ellipsis) {
                CatchPattern::Wildcard
            } else if self.match_kind(TokenKind::Placeholder) {
                return Err(self.error(
                    "'_' only valid in pipeline step or return wrapper; use '...' for catch wildcard",
                ));
            } else if self.check(TokenKind::Identifier) {
                let name = self.advance().value;
                let type_name = if self.match_kind(TokenKind::Colon) {
                    if self.match_kind(TokenKind::Ellipsis) {
                        None
                    } else if self.match_kind(TokenKind::Placeholder) {
                        return Err(self.error(
                            "'_' only valid in pipeline step or return wrapper; use '...' to omit catch type",
                        ));
                    } else {
                        Some(
                            self.expect(TokenKind::Identifier, "expected type name")?
                                .value,
                        )
                    }
                } else {
                    None
                };
                CatchPattern::Bind { name, type_name }
            } else {
                return Err(self.error("expected catch pattern"));
            };
            self.expect(TokenKind::RParen, "expected ')'")?;
            catches.push(CatchClause {
                pattern,
                body: self.parse_block()?,
            });
        }
        let else_block = if self.match_kind(TokenKind::KwElse) {
            Some(self.parse_block()?)
        } else {
            None
        };
        Ok(Stmt::Try {
            body,
            catches,
            else_block,
        })
    }

    fn parse_match(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::LParen, "expected '(' after match")?;
        let subject = self.parse_expr()?;
        self.expect(TokenKind::RParen, "expected ')' after match subject")?;
        self.expect(TokenKind::LBrace, "expected '{' after match")?;
        let mut cases = Vec::new();
        self.skip_newlines();
        let mut else_block = None;
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            if self.match_kind(TokenKind::KwCase) {
                let pattern = self.parse_pattern()?;
                let body = self.parse_block()?;
                cases.push(MatchCase { pattern, body });
                self.skip_newlines();
            } else if self.match_kind(TokenKind::KwElse) {
                else_block = Some(self.parse_block()?);
                self.skip_newlines();
            } else {
                return Err(self.error("expected 'case' or 'else' in match"));
            }
        }
        self.expect(TokenKind::RBrace, "expected '}' after match cases")?;
        if else_block.is_none()
            && self.match_kind(TokenKind::KwElse) {
                else_block = Some(self.parse_block()?);
            }
        Ok(Stmt::Match {
            subject,
            cases,
            else_block,
        })
    }

    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        let first = self.parse_pattern_unit()?;
        let mut alts = vec![first];
        while self.match_kind(TokenKind::Bar) {
            alts.push(self.parse_pattern_unit()?);
        }
        if alts.len() == 1 {
            Ok(alts.into_iter().next().unwrap())
        } else {
            Ok(Pattern::Or(alts))
        }
    }

    fn parse_pattern_unit(&mut self) -> Result<Pattern, ParseError> {
        if self.match_kind(TokenKind::LBracket) {
            let mut elems = Vec::new();
            self.skip_newlines();
            while !self.check(TokenKind::RBracket) && !self.is_at_end() {
                elems.push(self.parse_pattern_elem()?);
                if !self.match_kind(TokenKind::Comma) {
                    break;
                }
                self.skip_newlines();
            }
            self.skip_newlines();
            self.expect(TokenKind::RBracket, "expected ']' in list pattern")?;
            return Ok(Pattern::List(elems));
        }
        if self.check(TokenKind::Identifier) {
            let name = self.current().value.clone();
            if self.tokens.get(self.pos + 1).map(|t| t.kind) == Some(TokenKind::LBrace) {
                self.advance();
                self.advance();
                let mut fields = Vec::new();
                self.skip_newlines();
                while !self.check(TokenKind::RBrace) && !self.is_at_end() {
                    fields.push(
                        self.expect(TokenKind::Identifier, "expected struct field in pattern")?
                            .value,
                    );
                    if !self.match_kind(TokenKind::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
                self.skip_newlines();
                self.expect(TokenKind::RBrace, "expected '}' in struct pattern")?;
                return Ok(Pattern::Struct {
                    type_name: name,
                    fields,
                });
            }
            if matches!(
                self.tokens.get(self.pos + 1).map(|t| t.kind),
                Some(TokenKind::LParen | TokenKind::Dot)
            ) {
                let expr = self.parse_postfix_no_macro()?;
                return expr_to_pattern(expr);
            }
            // 裸名字：值比较（与当前绑定比较），不是字段 Bind。
            self.advance();
            return Ok(Pattern::Value(Box::new(Expr::new(
                self.loc_here(),
                ExprKind::Var(name),
            ))));
        }
        if self.match_kind(TokenKind::LParen) {
            self.skip_newlines();
            if self.match_kind(TokenKind::RParen) {
                return Ok(Pattern::Tuple(vec![]));
            }
            let mut elems = Vec::new();
            let mut saw_comma = false;
            loop {
                elems.push(self.parse_pattern_elem()?);
                self.skip_newlines();
                if !self.match_kind(TokenKind::Comma) {
                    break;
                }
                saw_comma = true;
                self.skip_newlines();
                if self.check(TokenKind::RParen) {
                    break;
                }
            }
            self.skip_newlines();
            self.expect(TokenKind::RParen, "expected ')' in tuple pattern")?;
            // 单元素无逗号：`(x)` 仍按值模式（与表达式一致）；`(x,)` 为一元组。
            if elems.len() == 1 && !saw_comma {
                match elems.into_iter().next().unwrap() {
                    PatternElem::Bind(name) => {
                        return Ok(Pattern::Value(Box::new(Expr::new(
                            self.loc_here(),
                            ExprKind::Var(name),
                        ))));
                    }
                    PatternElem::Nested(p) => return Ok(p),
                    PatternElem::Value(e) => return Ok(Pattern::Value(e)),
                }
            }
            return Ok(Pattern::Tuple(elems));
        }
        Err(self.error("expected match pattern"))
    }

    fn parse_pattern_elem(&mut self) -> Result<PatternElem, ParseError> {
        if self.check(TokenKind::LBracket)
            || (self.check(TokenKind::Identifier)
                && self.tokens.get(self.pos + 1).map(|t| t.kind) == Some(TokenKind::LBrace))
            || self.check(TokenKind::LParen)
        {
            return Ok(PatternElem::Nested(self.parse_pattern_unit()?));
        }
        // 点访问 / 调用（`Color.Green`、`Some(1)`）：按值/构造模式解析，而非 Bind。
        if self.check(TokenKind::Identifier)
            && matches!(
                self.tokens.get(self.pos + 1).map(|t| t.kind),
                Some(TokenKind::Dot | TokenKind::LParen)
            )
        {
            let expr = self.parse_postfix_no_macro()?;
            return match expr_to_pattern(expr)? {
                Pattern::Value(e) => Ok(PatternElem::Value(e)),
                p => Ok(PatternElem::Nested(p)),
            };
        }
        if self.check(TokenKind::Identifier) {
            return Ok(PatternElem::Bind(self.advance().value));
        }
        let expr = self.parse_expr()?;
        Ok(PatternElem::Value(Box::new(expr)))
    }

    fn parse_enum(&mut self, visibility: Visibility) -> Result<Stmt, ParseError> {
        let name = self
            .expect(TokenKind::Identifier, "expected enum name")?
            .value;
        self.expect(TokenKind::LBrace, "expected '{' after enum name")?;
        let mut members = Vec::new();
        let mut methods = Vec::new();
        self.skip_newlines();
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            if self.match_kind(TokenKind::KwFunc) {
                let mname = self
                    .expect(TokenKind::Identifier, "expected enum method name")?
                    .value;
                self.expect(TokenKind::LParen, "expected '('")?;
                let params = self.parse_param_list()?;
                self.skip_newlines();
                self.expect(TokenKind::RParen, "expected ')'")?;
                let body = self.parse_block()?;
                methods.push(EnumMethodDecl {
                    name: mname,
                    params,
                    body,
                });
            } else {
                let mname = self
                    .expect(TokenKind::Identifier, "expected enum member name")?
                    .value;
                let value = if self.match_kind(TokenKind::Assign) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                members.push(EnumMemberDecl { name: mname, value });
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RBrace, "expected '}' after enum body")?;
        Ok(Stmt::EnumDecl {
            visibility,
            name,
            members,
            methods,
        })
    }

    fn parse_variant(&mut self, visibility: Visibility) -> Result<Stmt, ParseError> {
        let name = self
            .expect(TokenKind::Identifier, "expected variant name")?
            .value;
        let type_params = if self.match_kind(TokenKind::LBracket) {
            let params = self.parse_type_param_list()?;
            self.expect(TokenKind::RBracket, "expected ']' after type params")?;
            params
        } else {
            Vec::new()
        };
        self.expect(TokenKind::LBrace, "expected '{' after variant name")?;
        let mut cases = Vec::new();
        self.skip_newlines();
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            cases.push(self.parse_variant_case()?);
            self.skip_newlines();
        }
        self.expect(TokenKind::RBrace, "expected '}' after variant body")?;
        Ok(Stmt::VariantDecl {
            visibility,
            name,
            type_params,
            cases,
        })
    }

    fn parse_variant_case(&mut self) -> Result<VariantCaseDecl, ParseError> {
        if self.match_kind(TokenKind::KwTyped) {
            let case_name = self
                .expect(TokenKind::Identifier, "expected case name")?
                .value;
            self.expect(TokenKind::LParen, "expected '(' after case name")?;
            let fields = self.parse_variant_field_list()?;
            self.expect(TokenKind::RParen, "expected ')' after case fields")?;
            return Ok(VariantCaseDecl { name: case_name, fields });
        }
        let case_name = self
            .expect(TokenKind::Identifier, "expected case name")?
            .value;
        self.expect(TokenKind::Assign, "expected '=' in variant case")?;
        // `Case = struct { let x }` 与 `Case = typed struct { x: T }` 都合法；
        // 不强制 typed——载荷形状就是一个 struct 声明体。
        let typed = self.match_kind(TokenKind::KwTyped);
        self.expect(
            TokenKind::KwStruct,
            "expected 'struct' or 'typed struct' after '='",
        )?;
        self.expect(TokenKind::LBrace, "expected '{' after struct")?;
        let fields = if typed {
            self.parse_variant_field_list_in_brace()?
        } else {
            self.parse_struct_field_list_in_brace()?
        };
        Ok(VariantCaseDecl {
            name: case_name,
            fields,
        })
    }

    fn parse_variant_field(&mut self) -> Result<StructField, ParseError> {
        let fname = self
            .expect(TokenKind::Identifier, "expected field name")?
            .value;
        let (type_expr, type_strong) = self.parse_optional_type()?;
        Ok(StructField {
            mutable: false,
            name: fname,
            type_expr,
            type_strong,
            default_expr: None,
        })
    }

    fn parse_variant_field_list(&mut self) -> Result<Vec<StructField>, ParseError> {
        self.parse_comma_list_until(TokenKind::RParen, Self::parse_variant_field)
    }

    fn parse_variant_field_list_in_brace(&mut self) -> Result<Vec<StructField>, ParseError> {
        self.skip_newlines();
        let mut fields = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            fields.push(self.parse_variant_field()?);
            self.skip_newlines();
        }
        self.expect(TokenKind::RBrace, "expected '}' after case struct fields")?;
        Ok(fields)
    }

    /// 普通 struct 字段体：`let`/`var` 名 [类型] [= 默认值]，供 variant case 与命名 struct 共用。
    fn parse_struct_field_list_in_brace(&mut self) -> Result<Vec<StructField>, ParseError> {
        self.skip_newlines();
        let mut fields = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            let mutable = self.match_kind(TokenKind::KwVar);
            if !mutable {
                self.expect(TokenKind::KwLet, "expected let/var field")?;
            }
            let fname = self
                .expect(TokenKind::Identifier, "expected field name")?
                .value;
            let (type_expr, type_strong) = self.parse_optional_type()?;
            let default_expr = if self.match_kind(TokenKind::Assign) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            fields.push(StructField {
                mutable,
                name: fname,
                type_expr,
                type_strong,
                default_expr,
            });
            self.skip_newlines();
        }
        self.expect(TokenKind::RBrace, "expected '}' after case struct fields")?;
        Ok(fields)
    }

    fn parse_struct(&mut self, visibility: Visibility, typed: bool) -> Result<Stmt, ParseError> {
        let name = self
            .expect(TokenKind::Identifier, "expected struct name")?
            .value;
        let type_params = if self.match_kind(TokenKind::LBracket) {
            let params = self.parse_type_param_list()?;
            self.expect(TokenKind::RBracket, "expected ']' after type params")?;
            params
        } else {
            Vec::new()
        };
        let base = if self.match_kind(TokenKind::Colon) {
            Some(
                self.expect(TokenKind::Identifier, "expected base type")?
                    .value,
            )
        } else {
            None
        };
        self.expect(TokenKind::LBrace, "expected '{'")?;
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        self.skip_newlines();
        while !self.check(TokenKind::RBrace) {
            if self.match_kind(TokenKind::KwFunc) {
                let mname = self
                    .expect(TokenKind::Identifier, "expected method name")?
                    .value;
                self.expect(TokenKind::LParen, "expected '('")?;
                let params = self.parse_param_list()?;
                self.skip_newlines();
                self.expect(TokenKind::RParen, "expected ')'")?;
                let outside = self.match_kind(TokenKind::KwOutside);
                let overload = self.match_kind(TokenKind::KwOverload);
                let (return_type, return_strong, return_wrapper) = self.parse_optional_return()?;
                let body = self.parse_block()?;
                methods.push(StructMethod {
                    name: mname,
                    params,
                    outside,
                    overload,
                    return_type,
                    return_strong,
                    return_wrapper,
                    body,
                });
            } else {
                let mutable = self.match_kind(TokenKind::KwVar);
                if !mutable {
                    self.expect(TokenKind::KwLet, "expected let/var field")?;
                }
                let fname = self
                    .expect(TokenKind::Identifier, "expected field name")?
                    .value;
                let (type_expr, type_strong) = self.parse_optional_type()?;
                let default_expr = if self.match_kind(TokenKind::Assign) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                fields.push(StructField {
                    mutable,
                    name: fname,
                    type_expr,
                    type_strong,
                    default_expr,
                });
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RBrace, "expected '}'")?;
        let layout = if self.match_kind(TokenKind::Colon) {
            Some(self.parse_type_annotation_expr()?)
        } else {
            None
        };
        Ok(Stmt::StructDecl {
            visibility,
            typed,
            name,
            type_params,
            base,
            fields,
            methods,
            layout,
        })
    }

    fn parse_import(&mut self) -> Result<Stmt, ParseError> {
        let (path, path_is_string) = if self.check(TokenKind::StringLiteral) {
            let tok = self.advance();
            (tok.value, true)
        } else {
            (self.parse_qualified_name()?.join("."), false)
        };
        let alias = if self.match_kind(TokenKind::KwAs) {
            Some(
                self.expect(TokenKind::Identifier, "expected alias after 'as'")?
                    .value,
            )
        } else {
            None
        };
        Ok(Stmt::Import {
            path,
            path_is_string,
            alias,
        })
    }

    fn parse_use(&mut self) -> Result<Stmt, ParseError> {
        let module = if self.check(TokenKind::StringLiteral) {
            let tok = self.advance();
            let mut attrs = Vec::new();
            while self.match_kind(TokenKind::Dot) {
                if self.check(TokenKind::LBrace) {
                    break;
                }
                attrs.push(
                    self.parse_member_name()?,
                );
            }
            ModuleRef::FilePath {
                path: tok.value,
                attrs,
            }
        } else {
            let mut module_path = vec![self
                .expect(TokenKind::Identifier, "expected module name in use")?
                .value];
            while self.match_kind(TokenKind::Dot) {
                if self.check(TokenKind::LBrace) {
                    break;
                }
                module_path.push(
                    self.expect(TokenKind::Identifier, "expected identifier after '.' in use")?
                        .value,
                );
            }
            ModuleRef::Qualified(module_path)
        };
        self.expect(TokenKind::LBrace, "expected '{' in use statement")?;
        let items = self.parse_use_list()?;
        self.expect(TokenKind::RBrace, "expected '}' in use statement")?;
        Ok(Stmt::Use { module, items })
    }

    fn parse_qualified_name(&mut self) -> Result<Vec<String>, ParseError> {
        let mut parts = vec![self
            .expect(TokenKind::Identifier, "expected module path")?
            .value];
        while self.match_kind(TokenKind::Dot) {
            parts.push(
                self.expect(TokenKind::Identifier, "expected identifier in module path")?
                    .value,
            );
        }
        Ok(parts)
    }

    fn parse_use_list(&mut self) -> Result<Vec<UseItem>, ParseError> {
        self.skip_newlines();
        let mut items = vec![self.parse_use_item()?];
        while self.match_kind(TokenKind::Comma) {
            self.skip_newlines();
            if self.check(TokenKind::RBrace) {
                break;
            }
            items.push(self.parse_use_item()?);
        }
        Ok(items)
    }

    fn parse_use_item(&mut self) -> Result<UseItem, ParseError> {
        let name = self.parse_member_name()?;
        let alias = if self.match_kind(TokenKind::KwAs) {
            Some(
                self.expect(TokenKind::Identifier, "expected alias after 'as'")?
                    .value,
            )
        } else {
            None
        };
        Ok(UseItem { name, alias })
    }

    fn parse_fstring(&self, loc: SourceLoc, raw: &str) -> Result<Expr, ParseError> {
        let mut parts: Vec<FStringPart> = Vec::new();
        let mut lit = String::new();
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            if ch == '{' {
                if i + 1 < chars.len() && chars[i + 1] == '{' {
                    lit.push('{');
                    i += 2;
                    continue;
                }
                if !lit.is_empty() {
                    parts.push(FStringPart::Text(std::mem::take(&mut lit)));
                }
                i += 1;
                let start = i;
                let mut depth = 1usize;
                while i < chars.len() && depth > 0 {
                    let c = chars[i];
                    if c == '{' {
                        depth += 1;
                    } else if c == '}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    i += 1;
                }
                if depth != 0 {
                    return Err(ParseError::Message {
                        line: 0,
                        column: 0,
                        message: "unclosed '{' in f-string".into(),
                    });
                }
                let expr_src: String = chars[start..i].iter().collect();
                let expr = Self::parse_expr_from_source(expr_src.trim())?;
                parts.push(FStringPart::Expr(Box::new(expr)));
                i += 1;
                continue;
            }
            if ch == '}' && i + 1 < chars.len() && chars[i + 1] == '}' {
                lit.push('}');
                i += 2;
                continue;
            }
            lit.push(ch);
            i += 1;
        }
        if !lit.is_empty() {
            parts.push(FStringPart::Text(lit));
        }
        if parts.is_empty() {
            parts.push(FStringPart::Text(String::new()));
        }
        Ok(Expr::new(loc, ExprKind::FString(parts)))
    }

    fn is_expr_start(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::NumLiteral
                | TokenKind::StringLiteral
                | TokenKind::FStringLiteral
                | TokenKind::BytesLiteral
                | TokenKind::Identifier
                | TokenKind::KwDo
                | TokenKind::KwQuote
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
                | TokenKind::Minus
                | TokenKind::Bang
                | TokenKind::KwNot
                | TokenKind::KwMatch
                | TokenKind::KwSelect
                | TokenKind::KwHandle
                | TokenKind::KwGo
                | TokenKind::KwAwait
                | TokenKind::KwIf
                | TokenKind::Placeholder
        )
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_named_assign()
    }

    /// 海象赋值：`name := expr`（右结合于本层；优先级低于三元 / 管道）。
    fn parse_named_assign(&mut self) -> Result<Expr, ParseError> {
        if self.check(TokenKind::Identifier)
            && self.tokens.get(self.pos + 1).map(|t| t.kind) == Some(TokenKind::ColonEq)
        {
            let loc = self.loc_here();
            let name = self.advance().value;
            self.advance(); // :=
            let value = self.parse_named_assign()?;
            return Ok(Expr::new(
                loc,
                ExprKind::NamedAssign {
                    name,
                    value: Box::new(value),
                },
            ));
        }
        self.parse_ternary()
    }

    fn parse_ternary(&mut self) -> Result<Expr, ParseError> {
        if self.check(TokenKind::KwIf) {
            let saved = self.pos;
            let loc = self.loc_here();
            self.advance();
            let cond = self.parse_or()?;
            if self.match_kind(TokenKind::KwThen) {
                let then_expr = self.parse_ternary()?;
                self.expect(TokenKind::KwElse, "expected 'else' in if-then-else expression")?;
                let else_expr = self.parse_ternary()?;
                return Ok(Expr::new(
                    loc,
                    ExprKind::IfThenElse {
                        cond: Box::new(cond),
                        then_expr: Box::new(then_expr),
                        else_expr: Box::new(else_expr),
                    },
                ));
            }
            self.pos = saved;
        }
        self.parse_pipeline()
    }

    fn parse_pipeline(&mut self) -> Result<Expr, ParseError> {
        let pos = self.pos;
        let decos = self.parse_decorator_prefix()?;
        if self.check(TokenKind::KwDo) {
            let loc = self.loc_here();
            self.advance();
            let inner = self.parse_do_func_expr(loc)?;
            return self.apply_decorators_to_expr(decos, inner);
        }
        if !decos.is_empty() {
            self.pos = pos;
        }
        let mut expr = self.parse_or()?;
        while self.match_kind(TokenKind::Pipe) {
            let prev = self.allow_placeholder;
            self.allow_placeholder = true;
            let right = self.parse_or()?;
            self.allow_placeholder = prev;
            let pipe_name = format!("__pipe_{}", self.next_pipe_id);
            self.next_pipe_id += 1;
            let repl = Expr::at(0, 1, ExprKind::Var(pipe_name.clone()));
            let right = fill_placeholders(&right, &repl);
            let __loc = expr.loc;
            expr = Expr::new(
                __loc,
                ExprKind::Pipeline {
                    left: Box::new(expr),
                    right: Box::new(right),
                    pipe_name,
                },
            );
        }
        Ok(expr)
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        self.parse_left_assoc_simple(TokenKind::KwOr, BinaryOp::Or, Self::parse_and)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        self.parse_left_assoc_simple(TokenKind::KwAnd, BinaryOp::And, Self::parse_not)
    }

    fn parse_not(&mut self) -> Result<Expr, ParseError> {
        if self.check(TokenKind::KwNot) {
            let loc = self.loc_here();
            self.advance();
            return Ok(Expr::new(
                loc,
                ExprKind::Unary {
                    op: UnaryOp::TruthyNot,
                    operand: Box::new(self.parse_not()?),
                },
            ));
        }
        self.parse_membership()
    }

    fn parse_membership(&mut self) -> Result<Expr, ParseError> {
        self.parse_left_assoc_simple(TokenKind::KwIn, BinaryOp::In, Self::parse_is)
    }

    /// 左结合二元：`a OP b OP c`，无跳过换行。
    fn parse_left_assoc_simple(
        &mut self,
        token: TokenKind,
        op: BinaryOp,
        next: fn(&mut Self) -> Result<Expr, ParseError>,
    ) -> Result<Expr, ParseError> {
        let mut expr = next(self)?;
        while self.match_kind(token) {
            let loc = expr.loc;
            let right = next(self)?;
            expr = Expr::new(
                loc,
                ExprKind::Binary {
                    op,
                    left: Box::new(expr),
                    right: Box::new(right),
                },
            );
        }
        Ok(expr)
    }

    fn parse_is(&mut self) -> Result<Expr, ParseError> {
        self.parse_is_ex(true)
    }

    fn parse_is_ex(&mut self, allow_macro: bool) -> Result<Expr, ParseError> {
        let mut expr = self.parse_comparison_ex(allow_macro)?;
        while self.match_kind(TokenKind::KwIs) {
            let neg = self.match_kind(TokenKind::KwNot);
            let right = self.parse_comparison_ex(allow_macro)?;
            { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::Binary {
                op: if neg { BinaryOp::IsNot } else { BinaryOp::Is },
                left: Box::new(expr),
                right: Box::new(right),
            }); }
        }
        Ok(expr)
    }

    fn parse_comparison_ex(&mut self, allow_macro: bool) -> Result<Expr, ParseError> {
        let mut expr = self.parse_bitor_ex(allow_macro)?;
        loop {
            self.skip_newlines();
            let op = match self.current().kind {
                TokenKind::EqEq => BinaryOp::Eq,
                TokenKind::Ne => BinaryOp::Ne,
                TokenKind::Lt => BinaryOp::Lt,
                TokenKind::Le => BinaryOp::Le,
                TokenKind::Gt => BinaryOp::Gt,
                TokenKind::Ge => BinaryOp::Ge,
                _ => break,
            };
            self.advance();
            { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::Binary {
                op,
                left: Box::new(expr),
                right: Box::new(self.parse_bitor_ex(allow_macro)?),
            }); }
        }
        Ok(expr)
    }

    /// `|` 按位或（表达式中复用 `TokenKind::Bar`；模式解析不走此层）。
    fn parse_bitor_ex(&mut self, allow_macro: bool) -> Result<Expr, ParseError> {
        self.parse_left_assoc_token_skip_nl(
            TokenKind::Bar,
            BinaryOp::BitOr,
            allow_macro,
            Self::parse_bitxor_ex,
        )
    }

    fn parse_bitxor_ex(&mut self, allow_macro: bool) -> Result<Expr, ParseError> {
        self.parse_left_assoc_token_skip_nl(
            TokenKind::Caret,
            BinaryOp::BitXor,
            allow_macro,
            Self::parse_bitand_ex,
        )
    }

    fn parse_bitand_ex(&mut self, allow_macro: bool) -> Result<Expr, ParseError> {
        self.parse_left_assoc_token_skip_nl(
            TokenKind::Ampersand,
            BinaryOp::BitAnd,
            allow_macro,
            Self::parse_shift_ex,
        )
    }

    /// 左结合二元：跳过换行后匹配单一 token。
    fn parse_left_assoc_token_skip_nl(
        &mut self,
        token: TokenKind,
        op: BinaryOp,
        allow_macro: bool,
        next: fn(&mut Self, bool) -> Result<Expr, ParseError>,
    ) -> Result<Expr, ParseError> {
        let mut expr = next(self, allow_macro)?;
        loop {
            self.skip_newlines();
            if !self.check(token) {
                break;
            }
            self.advance();
            self.skip_newlines();
            let loc = expr.loc;
            let right = next(self, allow_macro)?;
            expr = Expr::new(
                loc,
                ExprKind::Binary {
                    op,
                    left: Box::new(expr),
                    right: Box::new(right),
                },
            );
        }
        Ok(expr)
    }

    fn parse_shift_ex(&mut self, allow_macro: bool) -> Result<Expr, ParseError> {
        let mut expr = self.parse_additive_ex(allow_macro)?;
        loop {
            self.skip_newlines();
            let op = match self.current().kind {
                TokenKind::LtLt => BinaryOp::LShift,
                TokenKind::GtGt => BinaryOp::RShift,
                _ => break,
            };
            self.advance();
            self.skip_newlines();
            { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::Binary {
                op,
                left: Box::new(expr),
                right: Box::new(self.parse_additive_ex(allow_macro)?),
            }); }
        }
        Ok(expr)
    }

    fn parse_additive_ex(&mut self, allow_macro: bool) -> Result<Expr, ParseError> {
        let mut expr = self.parse_multiplicative_ex(allow_macro)?;
        loop {
            self.skip_newlines();
            let op = match self.current().kind {
                TokenKind::Plus => BinaryOp::Add,
                TokenKind::Minus => BinaryOp::Sub,
                _ => break,
            };
            self.advance();
            self.skip_newlines();
            { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::Binary {
                op,
                left: Box::new(expr),
                right: Box::new(self.parse_multiplicative_ex(allow_macro)?),
            }); }
        }
        Ok(expr)
    }

    fn parse_multiplicative_ex(&mut self, allow_macro: bool) -> Result<Expr, ParseError> {
        let mut expr = self.parse_unary_ex(allow_macro)?;
        loop {
            self.skip_newlines();
            let op = match self.current().kind {
                TokenKind::Star => BinaryOp::Mul,
                TokenKind::Slash => BinaryOp::Div,
                TokenKind::Percent => BinaryOp::Mod,
                _ => break,
            };
            self.advance();
            self.skip_newlines();
            { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::Binary {
                op,
                left: Box::new(expr),
                right: Box::new(self.parse_unary_ex(allow_macro)?),
            }); }
        }
        Ok(expr)
    }

    fn parse_unary_ex(&mut self, allow_macro: bool) -> Result<Expr, ParseError> {
        self.skip_newlines();
        if self.check(TokenKind::KwHandle) {
            let loc = self.loc_here();
            self.advance();
            // 捕获比较及以下优先级的运算（含 `1/0`），但紧于 `is`/`and`/`or`：
            // `handle boom() is none` → `(handle boom()) is none`
            // `handle 1/0` → `handle (1/0)`
            let operand = self.parse_comparison_ex(allow_macro)?;
            return Ok(Expr::new(loc, ExprKind::Handle { operand: Box::new(operand) }));
        }
        if self.check(TokenKind::KwGo) {
            let loc = self.loc_here();
            self.advance();
            let operand = self.parse_comparison_ex(allow_macro)?;
            return Ok(Expr::new(loc, ExprKind::Go { operand: Box::new(operand) }));
        }
        if self.check(TokenKind::KwAwait) {
            let loc = self.loc_here();
            self.advance();
            self.skip_newlines();
            if self.check(TokenKind::KwYield) {
                return Err(self.error(
                    "'await yield' was removed; use 'suspend' to yield to the scheduler",
                ));
            }
            let operand = self.parse_comparison_ex(allow_macro)?;
            return Ok(Expr::new(loc, ExprKind::Await { operand: Box::new(operand) }));
        }
        if self.check(TokenKind::KwSuspend) {
            let loc = self.loc_here();
            self.advance();
            return Ok(Expr::new(loc, ExprKind::Suspend));
        }
        if self.check(TokenKind::Minus) {
            let loc = self.loc_here();
            self.advance();
            return Ok(Expr::new(
                loc,
                ExprKind::Unary {
                    op: UnaryOp::Neg,
                    operand: Box::new(self.parse_unary_ex(allow_macro)?),
                },
            ));
        }
        if self.check(TokenKind::Bang) {
            let loc = self.loc_here();
            self.advance();
            return Ok(Expr::new(
                loc,
                ExprKind::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(self.parse_unary_ex(allow_macro)?),
                },
            ));
        }
        if self.check(TokenKind::Tilde) {
            let loc = self.loc_here();
            self.advance();
            return Ok(Expr::new(
                loc,
                ExprKind::Unary {
                    op: UnaryOp::Invert,
                    operand: Box::new(self.parse_unary_ex(allow_macro)?),
                },
            ));
        }
        self.parse_power_ex(allow_macro)
    }

    /// 幂运算：右结合，优先级高于一元以外的二元运算；`-1**2` → `-(1**2)`。
    fn parse_power_ex(&mut self, allow_macro: bool) -> Result<Expr, ParseError> {
        let expr = if allow_macro {
            self.parse_postfix()?
        } else {
            self.parse_postfix_no_macro()?
        };
        self.skip_newlines();
        if !self.match_kind(TokenKind::StarStar) {
            return Ok(expr);
        }
        self.skip_newlines();
        let right = self.parse_unary_ex(allow_macro)?;
        let loc = expr.loc;
        Ok(Expr::new(
            loc,
            ExprKind::Binary {
                op: BinaryOp::Pow,
                left: Box::new(expr),
                right: Box::new(right),
            },
        ))
    }

    fn parse_postfix_no_macro(&mut self) -> Result<Expr, ParseError> {
        self.parse_postfix_inner(false)
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        self.parse_postfix_inner(true)
    }

    fn parse_member_name(&mut self) -> Result<String, ParseError> {
        if self.check(TokenKind::Identifier) {
            return Ok(self.advance().value);
        }
        let tok = self.current();
        // 允许部分关键字作成员名：`obj.match`、`std.sync.yield` 等。
        match tok.kind {
            TokenKind::KwMatch => {
                self.advance();
                Ok("match".into())
            }
            TokenKind::KwYield => {
                self.advance();
                Ok("yield".into())
            }
            TokenKind::KwDo => {
                self.advance();
                Ok("do".into())
            }
            _ => Err(ParseError::here(
                tok.line,
                tok.column,
                "expected field name",
            )),
        }
    }

    fn parse_postfix_inner(&mut self, allow_macro: bool) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.match_kind(TokenKind::LParen) {
                let args = self.parse_call_args(true)?;
                self.expect(TokenKind::RParen, "expected ')'")?;
                { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::Call {
                    callee: Box::new(expr),
                    args,
                }); }
            } else if allow_macro && self.match_kind(TokenKind::LBrace) {
                let args = self.parse_macro_call_args()?;
                self.expect(TokenKind::RBrace, "expected '}'")?;
                { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::MacroCall {
                    callee: Box::new(expr),
                    args,
                }); }
            } else if self.match_kind(TokenKind::Dot) {
                // `Type.(value)` 类型转换；否则为成员访问。
                if self.check(TokenKind::LParen) {
                    self.advance();
                    let value = if self.check(TokenKind::RParen) {
                        Expr::at(self.current().line, self.current().column, ExprKind::None)
                    } else {
                        self.parse_expr()?
                    };
                    self.expect(TokenKind::RParen, "expected ')'")?;
                    { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::TypeConvert {
                        type_expr: Box::new(expr),
                        value: Box::new(value),
                    }); }
                } else {
                    let field = self.parse_member_name()?;
                    { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::Member {
                        object: Box::new(expr),
                        field,
                    }); }
                }
            } else if self.check(TokenKind::LBracket) {
                self.match_kind(TokenKind::LBracket);
                if self.check(TokenKind::Colon) {
                    self.expect(TokenKind::Colon, "expected ':' in slice")?;
                    let end = if self.check(TokenKind::Colon) || self.check(TokenKind::RBracket) {
                        None
                    } else {
                        Some(Box::new(self.parse_expr()?))
                    };
                    let step = if self.match_kind(TokenKind::Colon) {
                        if self.check(TokenKind::RBracket) {
                            None
                        } else {
                            Some(Box::new(self.parse_expr()?))
                        }
                    } else {
                        None
                    };
                    self.expect(TokenKind::RBracket, "expected ']'")?;
                    { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::Slice {
                        object: Box::new(expr),
                        start: None,
                        end,
                        step,
                    }); }
                } else {
                    let first = self.parse_expr()?;
                    if self.match_kind(TokenKind::Colon) {
                        let end = if self.check(TokenKind::Colon) || self.check(TokenKind::RBracket) {
                            None
                        } else {
                            Some(Box::new(self.parse_expr()?))
                        };
                        let step = if self.match_kind(TokenKind::Colon) {
                            if self.check(TokenKind::RBracket) {
                                None
                            } else {
                                Some(Box::new(self.parse_expr()?))
                            }
                        } else {
                            None
                        };
                        self.expect(TokenKind::RBracket, "expected ']'")?;
                        { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::Slice {
                            object: Box::new(expr),
                            start: Some(Box::new(first)),
                            end,
                            step,
                        }); }
                    } else {
                        // `a[i]`；多实参 `dict[text, num]` / `Union[num, text]` 收成 List 下标。
                        let index = if self.check(TokenKind::Comma) {
                            let mut items = vec![first];
                            while self.match_kind(TokenKind::Comma) {
                                if self.check(TokenKind::RBracket) {
                                    break;
                                }
                                items.push(self.parse_expr()?);
                            }
                            let loc = items[0].loc;
                            Expr::new(loc, ExprKind::List(items))
                        } else {
                            first
                        };
                        self.expect(TokenKind::RBracket, "expected ']'")?;
                        { let __loc = expr.loc; expr = Expr::new(__loc, ExprKind::Index {
                            object: Box::new(expr),
                            index: Box::new(index),
                        }); }
                    }
                }
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_macro_call_args(&mut self) -> Result<Vec<MacroCallArg>, ParseError> {
        self.parse_comma_list_until(TokenKind::RBrace, Self::parse_macro_call_arg)
    }

    fn parse_macro_call_arg(&mut self) -> Result<MacroCallArg, ParseError> {
        let is_splat = self.match_kind(TokenKind::Star);
        // 解析期将实参表达式冻结为 AST；宏实参不是运行时 Expr。
        let expr = self.parse_expr()?;
        let node = Arc::new(runtime_ast::ast_from_expr(&expr));
        Ok(MacroCallArg { is_splat, node })
    }

    fn parse_call_args(&mut self, allow_named: bool) -> Result<Vec<CallArg>, ParseError> {
        self.skip_newlines();
        if self.check(TokenKind::RParen) || self.check(TokenKind::RBrace) {
            return Ok(Vec::new());
        }
        let mut args = vec![self.parse_call_arg(allow_named)?];
        while self.match_kind(TokenKind::Comma) {
            self.skip_newlines();
            if self.check(TokenKind::RParen) || self.check(TokenKind::RBrace) {
                break;
            }
            args.push(self.parse_call_arg(allow_named)?);
        }
        Ok(args)
    }

    fn parse_call_arg(&mut self, allow_named: bool) -> Result<CallArg, ParseError> {
        if self.match_kind(TokenKind::StarStar) {
            return Ok(CallArg {
                name: None,
                is_splat: false,
                is_kwsplat: true,
                value: self.parse_expr()?,
            });
        }
        let is_splat = self.match_kind(TokenKind::Star);
        if allow_named && !is_splat && self.check(TokenKind::Identifier) {
            let name = self.current().value.clone();
            let next_is_assign =
                self.tokens.get(self.pos + 1).map(|t| t.kind) == Some(TokenKind::Assign);
            if next_is_assign {
                self.advance();
                self.expect(TokenKind::Assign, "expected '='")?;
                return Ok(CallArg {
                    name: Some(name),
                    is_splat: false,
                    is_kwsplat: false,
                    value: self.parse_expr()?,
                });
            }
        }
        Ok(CallArg {
            name: None,
            is_splat,
            is_kwsplat: false,
            value: self.parse_expr()?,
        })
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        match self.current().kind {
            TokenKind::KwIf => {
                let loc = self.loc_here();
                self.advance();
                let cond = self.parse_or()?;
                self.expect(TokenKind::KwThen, "expected 'then' in if-then-else expression")?;
                let then_expr = self.parse_expr()?;
                self.expect(TokenKind::KwElse, "expected 'else' in if-then-else expression")?;
                let else_expr = self.parse_expr()?;
                Ok(Expr::new(
                    loc,
                    ExprKind::IfThenElse {
                        cond: Box::new(cond),
                        then_expr: Box::new(then_expr),
                        else_expr: Box::new(else_expr),
                    },
                ))
            }
            TokenKind::NumLiteral => {
                let loc = self.loc_here();
                let v = self.advance().value;
                Ok(Expr::new(loc, ExprKind::Number(v)))
            }
            TokenKind::StringLiteral => {
                let loc = self.loc_here();
                let v = self.advance().value;
                Ok(Expr::new(loc, ExprKind::String(v)))
            }
            TokenKind::FStringLiteral => {
                let loc = self.loc_here();
                let v = self.advance().value;
                self.parse_fstring(loc, &v)
            }
            TokenKind::BytesLiteral => {
                let loc = self.loc_here();
                let v = self.advance().value;
                let bytes: Vec<u8> = v.chars().map(|c| c as u8).collect();
                Ok(Expr::new(loc, ExprKind::Bytes(bytes)))
            }
            TokenKind::Identifier => {
                let loc = self.loc_here();
                let v = self.advance().value;
                match v.as_str() {
                    "true" => Ok(Expr::new(loc, ExprKind::Bool(true))),
                    "false" => Ok(Expr::new(loc, ExprKind::Bool(false))),
                    "none" => Ok(Expr::new(loc, ExprKind::None)),
                    _ => Ok(Expr::new(loc, ExprKind::Var(v))),
                }
            }
            TokenKind::Placeholder => {
                if !self.allow_placeholder {
                    return Err(self.error(
                        "'_' only valid in pipeline step or return wrapper",
                    ));
                }
                let loc = self.loc_here();
                self.advance();
                Ok(Expr::new(loc, ExprKind::Placeholder))
            }
            TokenKind::KwDo => {
                let loc = self.loc_here();
                self.advance();
                self.parse_do_func_expr(loc)
            }
            TokenKind::KwQuote => self.parse_quote_expr(),
            TokenKind::KwMatch => self.parse_match_expr(),
            TokenKind::KwSelect => self.parse_select_expr(),
            TokenKind::LParen => {
                let loc = self.loc_here();
                self.paren_depth += 1;
                self.advance();
                self.skip_newlines();
                if self.check(TokenKind::RParen) {
                    self.advance();
                    self.paren_depth -= 1;
                    return Ok(Expr::new(loc, ExprKind::Tuple(Vec::new())));
                }
                let first = self.parse_expr()?;
                self.skip_newlines();
                if self.match_kind(TokenKind::KwFor) {
                    let (items, guards) = self.parse_comp_clauses()?;
                    self.skip_newlines();
                    self.expect(TokenKind::RParen, "expected ')' after generator expression")?;
                    self.paren_depth -= 1;
                    return Ok(Expr::new(
                        loc,
                        ExprKind::GeneratorExp {
                            elem: Box::new(first),
                            items,
                            guards,
                        },
                    ));
                }
                if self.match_kind(TokenKind::Comma) {
                    let mut elems = vec![first];
                    self.skip_newlines();
                    if !self.check(TokenKind::RParen) {
                        loop {
                            elems.push(self.parse_expr()?);
                            self.skip_newlines();
                            if !self.match_kind(TokenKind::Comma) {
                                break;
                            }
                            self.skip_newlines();
                            if self.check(TokenKind::RParen) {
                                break;
                            }
                        }
                    }
                    self.expect(TokenKind::RParen, "expected ')'")?;
                    self.paren_depth -= 1;
                    return Ok(Expr::new(loc, ExprKind::Tuple(elems)));
                }
                self.expect(TokenKind::RParen, "expected ')'")?;
                self.paren_depth -= 1;
                Ok(first)
            }
            TokenKind::LBracket => {
                let loc = self.loc_here();
                self.bracket_depth += 1;
                self.advance();
                self.skip_newlines();
                if self.check(TokenKind::RBracket) {
                    self.advance();
                    self.bracket_depth -= 1;
                    return Ok(Expr::new(loc, ExprKind::List(Vec::new())));
                }
                let first = self.parse_expr()?;
                if self.match_kind(TokenKind::KwFor) {
                    let (items, guards) = self.parse_comp_clauses()?;
                    self.skip_newlines();
                    self.expect(TokenKind::RBracket, "expected ']'")?;
                    self.bracket_depth -= 1;
                    return Ok(Expr::new(
                        loc,
                        ExprKind::ListComp {
                            elem: Box::new(first),
                            items,
                            guards,
                        },
                    ));
                }
                let mut elems = vec![first];
                while self.match_kind(TokenKind::Comma) {
                    self.skip_newlines();
                    if self.check(TokenKind::RBracket) {
                        break;
                    }
                    elems.push(self.parse_expr()?);
                }
                self.skip_newlines();
                self.expect(TokenKind::RBracket, "expected ']'")?;
                self.bracket_depth -= 1;
                Ok(Expr::new(loc, ExprKind::List(elems)))
            }
            TokenKind::LBrace => {
                let loc = self.loc_here();
                self.brace_depth += 1;
                self.advance();
                self.skip_newlines();
                if self.check(TokenKind::RBrace) {
                    self.advance();
                    self.brace_depth -= 1;
                    // `{}` 为空字典；空集合用 `set()`。
                    return Ok(Expr::new(loc, ExprKind::Dict(Vec::new())));
                }
                let first = self.parse_expr()?;
                if self.match_kind(TokenKind::Colon) {
                    let value = self.parse_expr()?;
                    if self.match_kind(TokenKind::KwFor) {
                        let (items, guards) = self.parse_comp_clauses()?;
                        self.skip_newlines();
                        self.expect(TokenKind::RBrace, "expected '}'")?;
                        self.brace_depth -= 1;
                        return Ok(Expr::new(
                            loc,
                            ExprKind::DictComp {
                                key: Box::new(first),
                                value: Box::new(value),
                                items,
                                guards,
                            },
                        ));
                    }
                    let mut entries = vec![(first, value)];
                    while self.match_kind(TokenKind::Comma) {
                        self.skip_newlines();
                        if self.check(TokenKind::RBrace) {
                            break;
                        }
                        let k = self.parse_expr()?;
                        self.expect(TokenKind::Colon, "expected ':' in dict entry")?;
                        let v = self.parse_expr()?;
                        entries.push((k, v));
                    }
                    self.skip_newlines();
                    self.expect(TokenKind::RBrace, "expected '}'")?;
                    self.brace_depth -= 1;
                    Ok(Expr::new(loc, ExprKind::Dict(entries)))
                } else if self.match_kind(TokenKind::KwFor) {
                    let (items, guards) = self.parse_comp_clauses()?;
                    self.skip_newlines();
                    self.expect(TokenKind::RBrace, "expected '}'")?;
                    self.brace_depth -= 1;
                    Ok(Expr::new(
                        loc,
                        ExprKind::SetComp {
                            elem: Box::new(first),
                            items,
                            guards,
                        },
                    ))
                } else {
                    let mut elems = vec![first];
                    while self.match_kind(TokenKind::Comma) {
                        self.skip_newlines();
                        if self.check(TokenKind::RBrace) {
                            break;
                        }
                        elems.push(self.parse_expr()?);
                    }
                    self.skip_newlines();
                    self.expect(TokenKind::RBrace, "expected '}'")?;
                    self.brace_depth -= 1;
                    Ok(Expr::new(loc, ExprKind::Set(elems)))
                }
            }
            _ => Err(self.error("expected expression")),
        }
    }

    /// `for (x in xs, ...) if (cond) ...` — 推导式共用尾部。
    fn parse_comp_clauses(&mut self) -> Result<(Vec<ForItem>, Vec<Expr>), ParseError> {
        let items = self.parse_for_items_in_parens()?;
        let mut guards = Vec::new();
        while self.match_kind(TokenKind::KwIf) {
            self.expect(TokenKind::LParen, "expected '(' after if")?;
            guards.push(self.parse_expr()?);
            self.expect(TokenKind::RParen, "expected ')' after guard")?;
        }
        Ok((items, guards))
    }

    fn parse_match_expr(&mut self) -> Result<Expr, ParseError> {
        let loc = self.loc_here();
        self.advance();
        self.expect(TokenKind::LParen, "expected '(' after match")?;
        let subject = self.parse_expr()?;
        self.expect(TokenKind::RParen, "expected ')' after match subject")?;
        self.expect(TokenKind::LBrace, "expected '{' after match")?;
        let mut cases = Vec::new();
        self.skip_newlines();
        let mut else_block = None;
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            if self.match_kind(TokenKind::KwCase) {
                let pattern = self.parse_pattern()?;
                let body = self.parse_block()?;
                cases.push(MatchCase { pattern, body });
                self.skip_newlines();
            } else if self.match_kind(TokenKind::KwElse) {
                else_block = Some(self.parse_block()?);
                self.skip_newlines();
            } else {
                return Err(self.error("expected 'case' or 'else' in match"));
            }
        }
        self.expect(TokenKind::RBrace, "expected '}' after match cases")?;
        if else_block.is_none()
            && self.match_kind(TokenKind::KwElse) {
                else_block = Some(self.parse_block()?);
            }
        Ok(Expr::new(
            loc,
            ExprKind::Match {
                subject: Box::new(subject),
                cases,
                else_block,
            },
        ))
    }

    fn parse_select_expr(&mut self) -> Result<Expr, ParseError> {
        let loc = self.loc_here();
        self.advance(); // select
        self.expect(TokenKind::LBrace, "expected '{' after select")?;
        let mut cases = Vec::new();
        self.skip_newlines();
        let mut else_block = None;
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            if self.match_kind(TokenKind::KwCase) {
                let event = self.parse_expr()?;
                self.skip_newlines();
                let bind = if self.match_kind(TokenKind::KwAs) {
                    // `_` 是 Placeholder token，不是 Identifier；codegen 对 `_` 丢弃绑定值。
                    if self.match_kind(TokenKind::Placeholder) {
                        Some("_".into())
                    } else {
                        let name = self
                            .expect(TokenKind::Identifier, "expected name after 'as'")?
                            .value;
                        Some(name)
                    }
                } else {
                    None
                };
                let body = self.parse_block()?;
                cases.push(SelectCase { event, bind, body });
                self.skip_newlines();
            } else if self.match_kind(TokenKind::KwElse) {
                else_block = Some(self.parse_block()?);
                self.skip_newlines();
            } else {
                return Err(self.error("expected 'case' or 'else' in select"));
            }
        }
        self.expect(TokenKind::RBrace, "expected '}' after select cases")?;
        if else_block.is_none() && self.match_kind(TokenKind::KwElse) {
            else_block = Some(self.parse_block()?);
        }
        Ok(Expr::new(
            loc,
            ExprKind::Select {
                cases,
                else_block,
            },
        ))
    }
}

fn expr_to_pattern(expr: Expr) -> Result<Pattern, ParseError> {
    let loc = expr.loc;
    match expr.kind {
        ExprKind::Call { callee, args } => {
            let type_name = callee_to_pattern_name(*callee)?;
            let mut pat_args = Vec::new();
            for arg in args {
                pat_args.push(call_arg_to_pattern(arg.value)?);
            }
            Ok(Pattern::Call {
                type_name,
                args: pat_args,
            })
        }
        // `(x)` / 值位置上的裸名字：与变量当前值比较。
        kind => Ok(Pattern::Value(Box::new(Expr::new(loc, kind)))),
    }
}

/// 构造器实参如 `Point(a, b)`：裸标识符视为字段绑定。
fn call_arg_to_pattern(expr: Expr) -> Result<Pattern, ParseError> {
    let loc = expr.loc;
    match expr.kind {
        ExprKind::Var(name) => Ok(Pattern::Bind(name)),
        other => expr_to_pattern(Expr::new(loc, other)),
    }
}

fn callee_to_pattern_name(callee: Expr) -> Result<String, ParseError> {
    match callee.kind {
        ExprKind::Var(name) => Ok(name),
        ExprKind::Member { object, field } => {
            let base = callee_to_pattern_name(*object)?;
            Ok(format!("{base}.{field}"))
        }
        _ => Err(ParseError::Message {
            line: 0,
            column: 0,
            message: "expected constructor name in match pattern".into(),
        }),
    }
}

fn expr_to_lvalue(expr: Expr) -> Result<LValue, ParseError> {
    match expr.kind {
        ExprKind::Var(name) => Ok(LValue::Name(name)),
        ExprKind::Member { object, field } => Ok(LValue::Member { object, field }),
        ExprKind::Index { object, index } => Ok(LValue::Index { object, index }),
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => Ok(LValue::Slice {
            object,
            start,
            end,
            step,
        }),
        _ => Err(ParseError::Message {
            line: 0,
            column: 0,
            message: "invalid assignment target".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_addition() {
        let p = Parser::parse("1 + 2").unwrap();
        assert_eq!(p.stmts.len(), 1);
    }

    #[test]
    fn parse_func() {
        let p = Parser::parse("func f(x) { return x + 1 }").unwrap();
        assert_eq!(p.stmts.len(), 1);
    }
}
