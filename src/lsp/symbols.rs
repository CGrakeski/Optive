//! 从 AST / 词法收集符号，供补全、悬停、跳转、引用、大纲使用。

use std::path::Path;

use crate::ast::{
    BinaryOp, Block, CatchPattern, DelTarget, DestructElem, DestructPattern, Expr, ExprKind,
    LValue, LocatedStmt, ModuleRef, Pattern, PatternElem, Program, Stmt, UnaryOp, Visibility,
};
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::token::TokenKind;

pub const KIND_FUNC: u8 = 3;
pub const KIND_VAR: u8 = 6;
pub const KIND_CLASS: u8 = 7;
pub const KIND_INTERFACE: u8 = 8;
pub const KIND_MODULE: u8 = 9;
pub const KIND_FIELD: u8 = 5;
pub const KIND_METHOD: u8 = 2;
pub const KIND_KEYWORD: u8 = 14;
pub const KIND_SNIPPET: u8 = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Unknown,
    Num,
    Text,
    Bool,
    None,
    Bytes,
    List,
    Dict,
    Set,
    Tuple,
    Struct(String),
    Module,
    Func,
}

impl Ty {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Unknown => "unknown".into(),
            Self::Num => "num".into(),
            Self::Text => "text".into(),
            Self::Bool => "bool".into(),
            Self::None => "none".into(),
            Self::Bytes => "bytes".into(),
            Self::List => "list".into(),
            Self::Dict => "dict".into(),
            Self::Set => "set".into(),
            Self::Tuple => "tuple".into(),
            Self::Struct(n) => n.clone(),
            Self::Module => "module".into(),
            Self::Func => "func".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: u8,
    pub detail: String,
    pub line: usize,
    pub col: usize,
    pub scope_lo: usize,
    pub scope_hi: usize,
    pub container: Option<String>,
    pub ty: Ty,
    /// `use spec.{ export as name }` → `(spec, export)`
    pub imported_from: Option<(String, String)>,
    /// `import spec` 绑定的模块路径
    pub module_spec: Option<String>,
    /// 顶层且非 `intern`：可被其他文件 `import` / `use`
    pub exported: bool,
}

#[derive(Debug, Clone)]
pub struct FileIndex {
    pub symbols: Vec<Symbol>,
    pub uses: Vec<(String, usize, usize)>,
}

impl FileIndex {
    #[must_use]
    pub fn in_scope(&self, line_1: usize) -> Vec<&Symbol> {
        self.symbols
            .iter()
            .filter(|s| s.container.is_none() && line_1 >= s.scope_lo && line_1 <= s.scope_hi)
            .collect()
    }

    #[must_use]
    pub fn members_of(&self, name: &str) -> Vec<&Symbol> {
        self.symbols
            .iter()
            .filter(|s| s.container.as_deref() == Some(name))
            .collect()
    }

    #[must_use]
    pub fn def_of(&self, name: &str, line_1: usize) -> Option<&Symbol> {
        self.in_scope(line_1)
            .into_iter()
            .filter(|s| s.name == name)
            .max_by_key(|s| s.line)
    }

    #[must_use]
    pub fn any_def(&self, name: &str) -> Option<&Symbol> {
        self.symbols
            .iter()
            .filter(|s| s.name == name && s.container.is_none())
            .min_by_key(|s| s.line)
    }

    #[must_use]
    pub fn exports(&self) -> Vec<&Symbol> {
        self.symbols
            .iter()
            .filter(|s| s.exported && s.container.is_none() && s.imported_from.is_none())
            .collect()
    }
}

#[must_use]
pub fn index_source(source: &str) -> FileIndex {
    let hi = source.lines().count().max(1).saturating_add(1);
    if let Some(program) = parse_for_lsp(source) {
        index_program(&program, hi)
    } else {
        token_index(source, hi)
    }
}

#[must_use]
pub fn index_program(program: &Program, hi: usize) -> FileIndex {
    let mut idx = FileIndex {
        symbols: Vec::new(),
        uses: Vec::new(),
    };
    walk_block(&program.stmts, 1, hi, &mut idx);
    idx
}

/// 完整解析，失败则补全未写完的 `.` / `(` / 括号后再试。
#[must_use]
pub fn parse_for_lsp(source: &str) -> Option<Program> {
    if let Ok(p) = Parser::parse(source) {
        return Some(p);
    }
    Parser::parse(&close_incomplete(source)).ok()
}

/// 补全/签名时源码常不完整。用不会进用户符号表的合法语法补全，再解析。
const INCOMPLETE_MEMBER: &str = "__lsp";
const INCOMPLETE_ARG: &str = "none)";

fn close_incomplete(source: &str) -> String {
    let mut lines: Vec<String> = source.lines().map(str::to_string).collect();
    for line in &mut lines {
        let trimmed = line.trim_end();
        if trimmed.ends_with('.') {
            line.push_str(INCOMPLETE_MEMBER);
        } else if trimmed.ends_with('(') {
            line.push_str(INCOMPLETE_ARG);
        } else if trimmed.ends_with(',') && trimmed.contains('(') {
            line.push(' ');
            line.push_str(INCOMPLETE_ARG);
        }
    }
    let mut out = lines.join("\n");
    if source.ends_with('\n') {
        out.push('\n');
    }
    close_unbalanced(&mut out);
    out
}

fn close_unbalanced(s: &mut String) {
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    let mut in_str = false;
    let mut quote = '\0';
    let mut escape = false;
    for c in s.chars() {
        if in_str {
            if escape {
                escape = false;
                continue;
            }
            if c == '\\' {
                escape = true;
                continue;
            }
            if c == quote {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                in_str = true;
                quote = c;
            }
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            '{' => brace += 1,
            '}' => brace -= 1,
            _ => {}
        }
    }
    if in_str {
        s.push(quote);
    }
    for _ in 0..paren.max(0) {
        s.push(')');
    }
    for _ in 0..bracket.max(0) {
        s.push(']');
    }
    for _ in 0..brace.max(0) {
        s.push('}');
    }
}

fn token_index(source: &str, hi: usize) -> FileIndex {
    let tokens = Lexer::new(source).tokenize().unwrap_or_default();
    let mut symbols = Vec::new();
    let mut uses = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        let bind = matches!(
            t.kind,
            TokenKind::KwLet
                | TokenKind::KwVar
                | TokenKind::KwConst
                | TokenKind::KwFunc
                | TokenKind::KwGen
                | TokenKind::KwStruct
                | TokenKind::KwEnum
                | TokenKind::KwVariant
                | TokenKind::KwFor
                | TokenKind::KwCatch
        );
        if bind {
            if let Some(n) = tokens.get(i + 1) {
                if n.kind == TokenKind::Identifier {
                    let kind = match t.kind {
                        TokenKind::KwFunc | TokenKind::KwGen => KIND_FUNC,
                        TokenKind::KwStruct | TokenKind::KwEnum | TokenKind::KwVariant => {
                            KIND_CLASS
                        }
                        _ => KIND_VAR,
                    };
                    let mut params = Vec::new();
                    if kind == KIND_FUNC
                        && tokens.get(i + 2).map(|x| x.kind) == Some(TokenKind::LParen)
                    {
                        let mut j = i + 3;
                        while j < tokens.len() && tokens[j].kind != TokenKind::RParen {
                            if tokens[j].kind == TokenKind::Identifier {
                                params.push((
                                    tokens[j].value.clone(),
                                    tokens[j].line,
                                    tokens[j].column,
                                ));
                            }
                            if tokens[j].kind == TokenKind::LBrace {
                                break;
                            }
                            j += 1;
                        }
                    }
                    let detail = if kind == KIND_FUNC {
                        format!(
                            "func {}({})",
                            n.value,
                            params
                                .iter()
                                .map(|(p, _, _)| p.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    } else {
                        n.value.clone()
                    };
                    for (p, pl, pc) in &params {
                        symbols.push(Symbol {
                            name: p.clone(),
                            kind: KIND_VAR,
                            detail: format!("param {p}"),
                            line: *pl,
                            col: *pc,
                            scope_lo: n.line,
                            scope_hi: hi,
                            container: None,
                            ty: Ty::Unknown,
                            imported_from: None,
                            module_spec: None,
                            exported: false,
                        });
                    }
                    symbols.push(Symbol {
                        name: n.value.clone(),
                        kind,
                        detail,
                        line: n.line,
                        col: n.column,
                        scope_lo: 1,
                        scope_hi: hi,
                        container: None,
                        ty: if kind == KIND_FUNC {
                            Ty::Func
                        } else {
                            Ty::Unknown
                        },
                        imported_from: None,
                        module_spec: None,
                        exported: kind == KIND_FUNC || kind == KIND_CLASS,
                    });
                }
            }
        }
        if t.kind == TokenKind::Identifier {
            uses.push((t.value.clone(), t.line, t.column));
        }
        i += 1;
    }
    FileIndex { symbols, uses }
}

fn walk_block(stmts: &Block, lo: usize, hi: usize, idx: &mut FileIndex) {
    for st in stmts {
        walk_stmt(st, lo, hi, idx);
    }
}

fn block_hi(stmts: &Block, fallback: usize) -> usize {
    stmts
        .iter()
        .map(|s| s.line)
        .max()
        .unwrap_or(fallback)
        .max(fallback)
}

/// 空函数体（含补全 `}` 后的未写完缓冲）把参数范围扩到外层 `hi`。
fn body_hi(body: &Block, start: usize, enclose_hi: usize) -> usize {
    if body.is_empty() {
        enclose_hi.max(start)
    } else {
        block_hi(body, start)
    }
}

#[allow(clippy::too_many_arguments)]
fn push_sym(
    idx: &mut FileIndex,
    name: String,
    kind: u8,
    detail: String,
    line: usize,
    col: usize,
    lo: usize,
    hi: usize,
    container: Option<String>,
) {
    idx.symbols.push(Symbol {
        name,
        kind,
        detail,
        line,
        col,
        scope_lo: lo,
        scope_hi: hi,
        container,
        ty: Ty::Unknown,
        imported_from: None,
        module_spec: None,
        exported: false,
    });
}

fn format_params(params: &[crate::ast::FuncParam]) -> String {
    params
        .iter()
        .map(|p| {
            let mut s = String::new();
            if p.is_kwvariadic {
                s.push_str("**");
            } else if p.is_variadic {
                s.push('*');
            }
            s.push_str(&p.name);
            if let Some(ty) = p.type_expr.as_ref().and_then(ty_from_ann) {
                s.push_str(": ");
                s.push_str(&ty.label());
            }
            s
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn last_sym(idx: &mut FileIndex) -> Option<&mut Symbol> {
    idx.symbols.last_mut()
}

fn mark_export(idx: &mut FileIndex, vis: Visibility) {
    if vis != Visibility::Internal {
        if let Some(s) = last_sym(idx) {
            s.exported = true;
        }
    }
}

fn walk_stmt(st: &LocatedStmt, lo: usize, hi: usize, idx: &mut FileIndex) {
    match &st.stmt {
        Stmt::VarDecl { .. } => walk_var_decl(st, hi, idx),
        Stmt::DestructDecl { .. } => walk_destruct_decl(st, hi, idx),
        Stmt::FuncDecl { .. } => walk_func(st, lo, hi, idx),
        Stmt::FriendFuncDecl { .. } => walk_friend_func(st, lo, hi, idx),
        Stmt::StructDecl { .. } => walk_struct(st, lo, hi, idx),
        Stmt::EnumDecl { .. } => walk_enum(st, lo, hi, idx),
        Stmt::VariantDecl { .. } => walk_variant(st, lo, hi, idx),
        Stmt::ProtocolDecl { .. } => walk_protocol(st, lo, hi, idx),
        Stmt::MacroDecl { .. } => walk_macro(st, lo, hi, idx),
        Stmt::Import { .. } => walk_import(st, lo, hi, idx),
        Stmt::Use { .. } => walk_use(st, lo, hi, idx),
        Stmt::If { .. } => walk_if(st, hi, idx),
        Stmt::While { cond, body } => {
            walk_expr(cond, idx);
            walk_block(body, st.line, block_hi(body, hi), idx);
        }
        Stmt::Loop { count, body } => {
            if let Some(c) = count {
                walk_expr(c, idx);
            }
            walk_block(body, st.line, block_hi(body, hi), idx);
        }
        Stmt::For { .. } => walk_for(st, hi, idx),
        Stmt::Try { .. } => walk_try(st, hi, idx),
        Stmt::Match { .. } => walk_match(st, hi, idx),
        Stmt::With { .. } => walk_with(st, hi, idx),
        Stmt::Return(e) | Stmt::Yield(e) => {
            if let Some(e) = e {
                walk_expr(e, idx);
            }
        }
        Stmt::YieldFrom(e) | Stmt::Throw(e) | Stmt::Expr(e) => walk_expr(e, idx),
        Stmt::Assign { target, value } => {
            walk_lvalue(target, idx);
            walk_expr(value, idx);
        }
        Stmt::DestructAssign { value, .. } => walk_expr(value, idx),
        Stmt::Del(t) => walk_del(t, idx),
        Stmt::Block(b) => walk_block(b, st.line, block_hi(b, hi), idx),
        Stmt::Break | Stmt::Continue | Stmt::Comment { .. } => {}
    }
}

fn walk_var_decl(st: &LocatedStmt, hi: usize, idx: &mut FileIndex) {
    let Stmt::VarDecl {
        visibility,
        name,
        is_var,
        is_const,
        type_expr,
        init,
        ..
    } = &st.stmt
    else {
        return;
    };

    let kw = if *is_const {
        "const"
    } else if *is_var {
        "var"
    } else {
        "let"
    };
    let ty = type_expr
        .as_ref()
        .and_then(ty_from_ann)
        .or_else(|| init.as_ref().map(|e| infer_expr_in(e, Some(idx))))
        .unwrap_or(Ty::Unknown);
    let mut detail = format!("{kw} {name}");
    if ty != Ty::Unknown {
        detail.push_str(&format!(": {}", ty.label()));
    }
    push_sym(
        idx,
        name.clone(),
        KIND_VAR,
        detail,
        st.line,
        st.column,
        st.line,
        hi,
        None,
    );
    if let Some(s) = last_sym(idx) {
        s.ty = ty;
    }
    mark_export(idx, *visibility);
    if let Some(e) = init {
        walk_expr(e, idx);
    }
}

fn walk_destruct_decl(st: &LocatedStmt, hi: usize, idx: &mut FileIndex) {
    let Stmt::DestructDecl { pattern, init, .. } = &st.stmt else {
        return;
    };

    for n in destruct_names(pattern) {
        push_sym(
            idx,
            n.clone(),
            KIND_VAR,
            format!("let {n}"),
            st.line,
            st.column,
            st.line,
            hi,
            None,
        );
    }
    walk_expr(init, idx);
}

fn walk_func(st: &LocatedStmt, lo: usize, hi: usize, idx: &mut FileIndex) {
    let Stmt::FuncDecl {
        visibility,
        name,
        params,
        body,
        decorators,
        ..
    } = &st.stmt
    else {
        return;
    };

    let sig = format!("func {name}({})", format_params(params));
    push_sym(
        idx,
        name.clone(),
        KIND_FUNC,
        sig,
        st.line,
        st.column,
        lo,
        hi,
        None,
    );
    if let Some(s) = last_sym(idx) {
        s.ty = Ty::Func;
    }
    mark_export(idx, *visibility);
    let inner_hi = body_hi(body, st.line, hi);
    for p in params {
        let ty = p
            .type_expr
            .as_ref()
            .and_then(ty_from_ann)
            .unwrap_or(Ty::Unknown);
        let mut detail = format!("param {}", p.name);
        if ty != Ty::Unknown {
            detail.push_str(&format!(": {}", ty.label()));
        }
        push_sym(
            idx,
            p.name.clone(),
            KIND_VAR,
            detail,
            st.line,
            st.column,
            st.line,
            inner_hi,
            None,
        );
        if let Some(s) = last_sym(idx) {
            s.ty = ty;
        }
    }
    for d in decorators {
        walk_expr(d, idx);
    }
    walk_block(body, st.line, inner_hi, idx);
}

fn walk_friend_func(st: &LocatedStmt, lo: usize, hi: usize, idx: &mut FileIndex) {
    let Stmt::FriendFuncDecl {
        visibility,
        name,
        params,
        body,
        ..
    } = &st.stmt
    else {
        return;
    };

    let ps = params.as_deref().unwrap_or(&[]);
    push_sym(
        idx,
        name.clone(),
        KIND_FUNC,
        format!("func {name}({})", format_params(ps)),
        st.line,
        st.column,
        lo,
        hi,
        None,
    );
    if let Some(s) = last_sym(idx) {
        s.ty = Ty::Func;
    }
    mark_export(idx, *visibility);
    if let Some(body) = body {
        let inner_hi = body_hi(body, st.line, hi);
        for p in ps {
            push_sym(
                idx,
                p.name.clone(),
                KIND_VAR,
                format!("param {}", p.name),
                st.line,
                st.column,
                st.line,
                inner_hi,
                None,
            );
        }
        walk_block(body, st.line, inner_hi, idx);
    }
}

fn walk_struct(st: &LocatedStmt, lo: usize, hi: usize, idx: &mut FileIndex) {
    let Stmt::StructDecl {
        visibility,
        name,
        fields,
        methods,
        ..
    } = &st.stmt
    else {
        return;
    };

    push_sym(
        idx,
        name.clone(),
        KIND_CLASS,
        format!("struct {name}"),
        st.line,
        st.column,
        lo,
        hi,
        None,
    );
    if let Some(s) = last_sym(idx) {
        s.ty = Ty::Struct(name.clone());
    }
    mark_export(idx, *visibility);
    for f in fields {
        let ty = f
            .type_expr
            .as_ref()
            .and_then(ty_from_ann)
            .unwrap_or(Ty::Unknown);
        let mut detail = format!("{name}.{}", f.name);
        if ty != Ty::Unknown {
            detail.push_str(&format!(": {}", ty.label()));
        }
        push_sym(
            idx,
            f.name.clone(),
            KIND_FIELD,
            detail,
            st.line,
            st.column,
            lo,
            hi,
            Some(name.clone()),
        );
        if let Some(s) = last_sym(idx) {
            s.ty = ty;
        }
    }
    for m in methods {
        push_sym(
            idx,
            m.name.clone(),
            KIND_METHOD,
            format!("func {}({})", m.name, format_params(&m.params)),
            st.line,
            st.column,
            lo,
            hi,
            Some(name.clone()),
        );
        let inner_hi = block_hi(&m.body, st.line);
        for p in &m.params {
            push_sym(
                idx,
                p.name.clone(),
                KIND_VAR,
                format!("param {}", p.name),
                st.line,
                st.column,
                st.line,
                inner_hi,
                None,
            );
        }
        walk_block(&m.body, st.line, inner_hi, idx);
    }
}

fn walk_enum(st: &LocatedStmt, lo: usize, hi: usize, idx: &mut FileIndex) {
    let Stmt::EnumDecl {
        visibility,
        name,
        members,
        methods,
        ..
    } = &st.stmt
    else {
        return;
    };

    push_sym(
        idx,
        name.clone(),
        KIND_CLASS,
        format!("enum {name}"),
        st.line,
        st.column,
        lo,
        hi,
        None,
    );
    if let Some(s) = last_sym(idx) {
        s.ty = Ty::Struct(name.clone());
    }
    mark_export(idx, *visibility);
    for m in members {
        push_sym(
            idx,
            m.name.clone(),
            KIND_FIELD,
            format!("{name}.{}", m.name),
            st.line,
            st.column,
            lo,
            hi,
            Some(name.clone()),
        );
    }
    for m in methods {
        let inner_hi = block_hi(&m.body, st.line);
        walk_block(&m.body, st.line, inner_hi, idx);
    }
}

fn walk_variant(st: &LocatedStmt, lo: usize, hi: usize, idx: &mut FileIndex) {
    let Stmt::VariantDecl {
        visibility,
        name,
        cases,
        ..
    } = &st.stmt
    else {
        return;
    };

    push_sym(
        idx,
        name.clone(),
        KIND_CLASS,
        format!("variant {name}"),
        st.line,
        st.column,
        lo,
        hi,
        None,
    );
    if let Some(s) = last_sym(idx) {
        s.ty = Ty::Struct(name.clone());
    }
    mark_export(idx, *visibility);
    for c in cases {
        push_sym(
            idx,
            c.name.clone(),
            KIND_FIELD,
            format!("{name}.{}", c.name),
            st.line,
            st.column,
            lo,
            hi,
            Some(name.clone()),
        );
    }
}

fn walk_protocol(st: &LocatedStmt, lo: usize, hi: usize, idx: &mut FileIndex) {
    let Stmt::ProtocolDecl {
        visibility,
        name,
        members,
        ..
    } = &st.stmt
    else {
        return;
    };

    push_sym(
        idx,
        name.clone(),
        KIND_INTERFACE,
        format!("protocol {name}"),
        st.line,
        st.column,
        lo,
        hi,
        None,
    );
    mark_export(idx, *visibility);
    for m in members {
        match m {
            crate::ast::ProtocolMember::Method { name: mn, params } => {
                push_sym(
                    idx,
                    mn.clone(),
                    KIND_METHOD,
                    format!("func {mn}({})", format_params(params)),
                    st.line,
                    st.column,
                    lo,
                    hi,
                    Some(name.clone()),
                );
            }
            crate::ast::ProtocolMember::Field { name: fnm, .. } => {
                push_sym(
                    idx,
                    fnm.clone(),
                    KIND_FIELD,
                    format!("{name}.{fnm}"),
                    st.line,
                    st.column,
                    lo,
                    hi,
                    Some(name.clone()),
                );
            }
        }
    }
}

fn walk_macro(st: &LocatedStmt, lo: usize, hi: usize, idx: &mut FileIndex) {
    let Stmt::MacroDecl {
        visibility,
        name,
        body,
        ..
    } = &st.stmt
    else {
        return;
    };

    push_sym(
        idx,
        name.clone(),
        KIND_FUNC,
        format!("macro {name}"),
        st.line,
        st.column,
        lo,
        hi,
        None,
    );
    mark_export(idx, *visibility);
    let inner_hi = block_hi(body, st.line);
    walk_block(body, st.line, inner_hi, idx);
}

fn walk_import(st: &LocatedStmt, lo: usize, hi: usize, idx: &mut FileIndex) {
    let Stmt::Import {
        path,
        alias,
        path_is_string,
        ..
    } = &st.stmt
    else {
        return;
    };

    let key = alias.as_deref().unwrap_or(path.as_str());
    let key = if *path_is_string {
        Path::new(key)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(key)
    } else {
        key.rsplit('.').next().unwrap_or(key)
    };
    push_sym(
        idx,
        key.to_string(),
        KIND_MODULE,
        format!("import {path}"),
        st.line,
        st.column,
        lo,
        hi,
        None,
    );
    if let Some(s) = last_sym(idx) {
        s.ty = Ty::Module;
        s.module_spec = Some(path.clone());
    }
}

fn walk_use(st: &LocatedStmt, lo: usize, hi: usize, idx: &mut FileIndex) {
    let Stmt::Use { module, items } = &st.stmt else {
        return;
    };

    let mod_s = match module {
        ModuleRef::Qualified(p) => p.join("."),
        ModuleRef::FilePath { path, .. } => path.clone(),
    };
    for it in items {
        let name = it.alias.as_deref().unwrap_or(it.name.as_str());
        push_sym(
            idx,
            name.to_string(),
            KIND_FUNC,
            format!("use {mod_s}.{}", it.name),
            st.line,
            st.column,
            lo,
            hi,
            None,
        );
        if let Some(s) = last_sym(idx) {
            s.imported_from = Some((mod_s.clone(), it.name.clone()));
            s.ty = Ty::Func;
        }
    }
}

fn walk_if(st: &LocatedStmt, hi: usize, idx: &mut FileIndex) {
    let Stmt::If {
        cond,
        then_block,
        elifs,
        else_block,
    } = &st.stmt
    else {
        return;
    };

    walk_expr(cond, idx);
    walk_block(then_block, st.line, block_hi(then_block, hi), idx);
    for (c, b) in elifs {
        walk_expr(c, idx);
        walk_block(b, st.line, block_hi(b, hi), idx);
    }
    if let Some(b) = else_block {
        walk_block(b, st.line, block_hi(b, hi), idx);
    }
}

fn walk_for(st: &LocatedStmt, hi: usize, idx: &mut FileIndex) {
    let Stmt::For { items, body } = &st.stmt else {
        return;
    };

    let inner_hi = block_hi(body, hi);
    for it in items {
        walk_expr(&it.iterable, idx);
        push_sym(
            idx,
            it.name.clone(),
            KIND_VAR,
            format!("for {}", it.name),
            st.line,
            st.column,
            st.line,
            inner_hi,
            None,
        );
    }
    walk_block(body, st.line, inner_hi, idx);
}

fn walk_try(st: &LocatedStmt, hi: usize, idx: &mut FileIndex) {
    let Stmt::Try {
        body,
        catches,
        else_block,
    } = &st.stmt
    else {
        return;
    };

    walk_block(body, st.line, block_hi(body, hi), idx);
    for c in catches {
        let inner_hi = block_hi(&c.body, hi);
        if let CatchPattern::Bind { name, .. } = &c.pattern {
            push_sym(
                idx,
                name.clone(),
                KIND_VAR,
                format!("catch {name}"),
                st.line,
                st.column,
                st.line,
                inner_hi,
                None,
            );
        }
        walk_block(&c.body, st.line, inner_hi, idx);
    }
    if let Some(b) = else_block {
        walk_block(b, st.line, block_hi(b, hi), idx);
    }
}

fn walk_match(st: &LocatedStmt, hi: usize, idx: &mut FileIndex) {
    let Stmt::Match {
        subject,
        cases,
        else_block,
    } = &st.stmt
    else {
        return;
    };

    walk_expr(subject, idx);
    for c in cases {
        let inner_hi = block_hi(&c.body, hi);
        for n in pattern_names(&c.pattern) {
            push_sym(
                idx,
                n.clone(),
                KIND_VAR,
                format!("case {n}"),
                st.line,
                st.column,
                st.line,
                inner_hi,
                None,
            );
        }
        walk_block(&c.body, st.line, inner_hi, idx);
    }
    if let Some(b) = else_block {
        walk_block(b, st.line, block_hi(b, hi), idx);
    }
}

fn walk_with(st: &LocatedStmt, hi: usize, idx: &mut FileIndex) {
    let Stmt::With {
        context,
        body,
        alias,
        ..
    } = &st.stmt
    else {
        return;
    };

    walk_expr(context, idx);
    let inner_hi = block_hi(body, hi);
    if let Some(a) = alias {
        push_sym(
            idx,
            a.clone(),
            KIND_VAR,
            format!("with {a}"),
            st.line,
            st.column,
            st.line,
            inner_hi,
            None,
        );
    }
    walk_block(body, st.line, inner_hi, idx);
}

fn walk_lvalue(lv: &LValue, idx: &mut FileIndex) {
    match lv {
        LValue::Name(_) => {}
        LValue::Member { object, .. } => walk_expr(object, idx),
        LValue::Index { object, index } => {
            walk_expr(object, idx);
            walk_expr(index, idx);
        }
        LValue::Slice {
            object,
            start,
            end,
            step,
        } => {
            walk_expr(object, idx);
            if let Some(e) = start {
                walk_expr(e, idx);
            }
            if let Some(e) = end {
                walk_expr(e, idx);
            }
            if let Some(e) = step {
                walk_expr(e, idx);
            }
        }
    }
}

fn walk_del(t: &DelTarget, idx: &mut FileIndex) {
    match t {
        DelTarget::Name(_) => {}
        DelTarget::Member { object, .. } => walk_expr(object, idx),
        DelTarget::Index { object, index } => {
            walk_expr(object, idx);
            walk_expr(index, idx);
        }
    }
}

fn walk_expr(expr: &Expr, idx: &mut FileIndex) {
    match &expr.kind {
        ExprKind::Var(n) => idx.uses.push((n.clone(), expr.loc.line, expr.loc.column)),
        ExprKind::Unary { operand, .. }
        | ExprKind::Handle { operand }
        | ExprKind::Go { operand }
        | ExprKind::Snap { operand }
        | ExprKind::Await { operand } => walk_expr(operand, idx),
        ExprKind::Binary { left, right, .. } => {
            walk_expr(left, idx);
            walk_expr(right, idx);
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, idx);
            for a in args {
                walk_expr(&a.value, idx);
            }
        }
        ExprKind::Member { object, field } => {
            walk_expr(object, idx);
            if let Some(base) = expr_path(object) {
                idx.uses
                    .push((format!("{base}.{field}"), expr.loc.line, expr.loc.column));
            }
        }
        ExprKind::Index { object, index } => {
            walk_expr(object, idx);
            walk_expr(index, idx);
        }
        ExprKind::List(xs) | ExprKind::Set(xs) | ExprKind::Tuple(xs) => {
            for e in xs {
                walk_expr(e, idx);
            }
        }
        ExprKind::Dict(ents) => {
            for (k, v) in ents {
                walk_expr(k, idx);
                walk_expr(v, idx);
            }
        }
        ExprKind::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            walk_expr(cond, idx);
            walk_expr(then_expr, idx);
            walk_expr(else_expr, idx);
        }
        ExprKind::DoFunc { params, body, .. } => {
            let hi = block_hi(body, expr.loc.line);
            for p in params {
                push_sym(
                    idx,
                    p.name.clone(),
                    KIND_VAR,
                    format!("param {}", p.name),
                    expr.loc.line,
                    expr.loc.column,
                    expr.loc.line,
                    hi,
                    None,
                );
            }
            walk_block(body, expr.loc.line, hi, idx);
        }
        ExprKind::FString(parts) => {
            for p in parts {
                if let crate::ast::FStringPart::Expr(e) = p {
                    walk_expr(e, idx);
                }
            }
        }
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => {
            walk_expr(object, idx);
            if let Some(e) = start {
                walk_expr(e, idx);
            }
            if let Some(e) = end {
                walk_expr(e, idx);
            }
            if let Some(e) = step {
                walk_expr(e, idx);
            }
        }
        ExprKind::TypeConvert { type_expr, value } => {
            walk_expr(type_expr, idx);
            walk_expr(value, idx);
        }
        ExprKind::ListComp {
            elem,
            items,
            guards,
        }
        | ExprKind::SetComp {
            elem,
            items,
            guards,
        }
        | ExprKind::GeneratorExp {
            elem,
            items,
            guards,
        } => {
            walk_expr(elem, idx);
            for it in items {
                walk_expr(&it.iterable, idx);
            }
            for g in guards {
                walk_expr(g, idx);
            }
        }
        ExprKind::DictComp {
            key,
            value,
            items,
            guards,
        } => {
            walk_expr(key, idx);
            walk_expr(value, idx);
            for it in items {
                walk_expr(&it.iterable, idx);
            }
            for g in guards {
                walk_expr(g, idx);
            }
        }
        ExprKind::NamedAssign { value, .. } => walk_expr(value, idx),
        ExprKind::Pipeline { left, right, .. } => {
            walk_expr(left, idx);
            walk_expr(right, idx);
        }
        ExprKind::Match {
            subject,
            cases,
            else_block,
        } => {
            walk_expr(subject, idx);
            for c in cases {
                walk_block(
                    &c.body,
                    expr.loc.line,
                    block_hi(&c.body, expr.loc.line),
                    idx,
                );
            }
            if let Some(b) = else_block {
                walk_block(b, expr.loc.line, block_hi(b, expr.loc.line), idx);
            }
        }
        ExprKind::ParFor { items, body } => {
            for it in items {
                walk_expr(&it.iterable, idx);
            }
            walk_block(body, expr.loc.line, block_hi(body, expr.loc.line), idx);
        }
        ExprKind::ParBlock { exprs } => {
            for e in exprs {
                walk_expr(e, idx);
            }
        }
        ExprKind::Select { cases, else_block } => {
            for c in cases {
                walk_expr(&c.event, idx);
                walk_block(
                    &c.body,
                    expr.loc.line,
                    block_hi(&c.body, expr.loc.line),
                    idx,
                );
            }
            if let Some(b) = else_block {
                walk_block(b, expr.loc.line, block_hi(b, expr.loc.line), idx);
            }
        }
        ExprKind::Quote { bindings, body, .. } => {
            for e in bindings {
                walk_expr(e, idx);
            }
            walk_block(body, expr.loc.line, block_hi(body, expr.loc.line), idx);
        }
        ExprKind::MacroCall { callee, .. } => walk_expr(callee, idx),
        ExprKind::Placeholder | ExprKind::Suspend => {}
        _ => {}
    }
}

fn expr_path(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Var(n) => Some(n.clone()),
        ExprKind::Member { object, field } => Some(format!("{}.{}", expr_path(object)?, field)),
        _ => None,
    }
}

fn destruct_names(p: &DestructPattern) -> Vec<String> {
    match p {
        DestructPattern::Name(n) => vec![n.clone()],
        DestructPattern::Discard => Vec::new(),
        DestructPattern::Tuple(xs) | DestructPattern::List(xs) => {
            xs.iter().flat_map(destruct_elem_names).collect()
        }
    }
}

fn destruct_elem_names(e: &DestructElem) -> Vec<String> {
    match e {
        DestructElem::Pat(p) => destruct_names(p),
        DestructElem::Rest(n) => vec![n.clone()],
        DestructElem::RestDiscard => Vec::new(),
    }
}

fn pattern_names(p: &Pattern) -> Vec<String> {
    match p {
        Pattern::Bind(n) => vec![n.clone()],
        Pattern::List(xs) | Pattern::Tuple(xs) => xs
            .iter()
            .flat_map(|e| match e {
                PatternElem::Bind(n) => vec![n.clone()],
                PatternElem::Nested(inner) => pattern_names(inner),
                PatternElem::Value(_) => Vec::new(),
            })
            .collect(),
        Pattern::Struct { fields, .. } => fields.clone(),
        Pattern::Or(xs) => xs.iter().flat_map(pattern_names).collect(),
        Pattern::Call { args, .. } => args.iter().flat_map(pattern_names).collect(),
        Pattern::Value(_) => Vec::new(),
    }
}

#[must_use]
pub fn infer_receiver_from_index(idx: &FileIndex, recv: &str) -> Option<String> {
    if !idx.members_of(recv).is_empty() {
        return Some(recv.to_string());
    }
    match idx.any_def(recv).map(|s| &s.ty) {
        Some(Ty::Struct(n)) => Some(n.clone()),
        Some(Ty::Module) => None,
        _ => None,
    }
}

#[must_use]
pub fn ty_from_ann(expr: &Expr) -> Option<Ty> {
    match &expr.kind {
        ExprKind::Var(n) => Some(match n.as_str() {
            "num" | "int" | "rat" => Ty::Num,
            "text" | "str" | "string" => Ty::Text,
            "bool" => Ty::Bool,
            "none" => Ty::None,
            "bytes" => Ty::Bytes,
            "list" => Ty::List,
            "dict" => Ty::Dict,
            "set" => Ty::Set,
            "tuple" => Ty::Tuple,
            "func" => Ty::Func,
            other => Ty::Struct(other.to_string()),
        }),
        _ => None,
    }
}

#[must_use]
pub fn infer_expr_in(expr: &Expr, idx: Option<&FileIndex>) -> Ty {
    match &expr.kind {
        ExprKind::Number(_) => Ty::Num,
        ExprKind::String(_) | ExprKind::FString(_) => Ty::Text,
        ExprKind::Bool(_) => Ty::Bool,
        ExprKind::None => Ty::None,
        ExprKind::Bytes(_) => Ty::Bytes,
        ExprKind::List(_) | ExprKind::ListComp { .. } => Ty::List,
        ExprKind::Dict(_) | ExprKind::DictComp { .. } => Ty::Dict,
        ExprKind::Set(_) | ExprKind::SetComp { .. } => Ty::Set,
        ExprKind::Tuple(_) => Ty::Tuple,
        ExprKind::Var(n) => idx
            .and_then(|i| i.any_def(n))
            .map(|s| s.ty.clone())
            .filter(|t| *t != Ty::Unknown)
            .unwrap_or(Ty::Unknown),
        ExprKind::TypeConvert { type_expr, .. } => ty_from_ann(type_expr).unwrap_or(Ty::Unknown),
        ExprKind::Call { callee, .. } => {
            if let Some(path) = expr_path(callee) {
                if let Some(ty) = crate::api_registry::std_call_type(&path) {
                    return Ty::Struct(ty.to_string());
                }
            }
            if let ExprKind::Member { object, field } = &callee.kind {
                let recv = infer_expr_in(object, idx);
                if let Ty::Struct(n) = &recv {
                    if let Some(ty) = crate::api_registry::handle_method_result(n, field) {
                        return Ty::Struct(ty.to_string());
                    }
                }
            }
            match &callee.kind {
                ExprKind::Var(n) if n.chars().next().is_some_and(|c| c.is_uppercase()) => {
                    Ty::Struct(n.clone())
                }
                _ => Ty::Unknown,
            }
        }
        ExprKind::Unary { op, operand } => match op {
            UnaryOp::Not | UnaryOp::TruthyNot => Ty::Bool,
            UnaryOp::Neg | UnaryOp::Invert => infer_expr_in(operand, idx),
        },
        ExprKind::Binary { op, left, right } => {
            infer_binary(*op, infer_expr_in(left, idx), infer_expr_in(right, idx))
        }
        ExprKind::IfThenElse {
            then_expr,
            else_expr,
            ..
        } => {
            let t = infer_expr_in(then_expr, idx);
            let e = infer_expr_in(else_expr, idx);
            if t == e {
                t
            } else {
                Ty::Unknown
            }
        }
        ExprKind::DoFunc { .. } => Ty::Func,
        _ => Ty::Unknown,
    }
}

fn infer_binary(op: BinaryOp, left: Ty, right: Ty) -> Ty {
    match op {
        BinaryOp::Eq
        | BinaryOp::Ne
        | BinaryOp::Lt
        | BinaryOp::Le
        | BinaryOp::Gt
        | BinaryOp::Ge
        | BinaryOp::In
        | BinaryOp::Is
        | BinaryOp::IsNot
        | BinaryOp::And
        | BinaryOp::Or => Ty::Bool,
        BinaryOp::Add if left == Ty::Text || right == Ty::Text => Ty::Text,
        BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::Div
        | BinaryOp::Mod
        | BinaryOp::Pow
            if left == Ty::Num && right == Ty::Num =>
        {
            Ty::Num
        }
        BinaryOp::BitAnd
        | BinaryOp::BitOr
        | BinaryOp::BitXor
        | BinaryOp::LShift
        | BinaryOp::RShift
            if left == Ty::Num && right == Ty::Num =>
        {
            Ty::Num
        }
        _ => Ty::Unknown,
    }
}

pub fn import_path_for_name<'a>(program: &'a Program, name: &str) -> Option<&'a str> {
    for st in &program.stmts {
        if let Stmt::Import {
            path,
            alias,
            path_is_string,
            ..
        } = &st.stmt
        {
            let key = alias.as_deref().unwrap_or(path.as_str());
            let key = if *path_is_string {
                Path::new(key)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(key)
            } else {
                key.rsplit('.').next().unwrap_or(key)
            };
            if key == name {
                return Some(path.as_str());
            }
        }
    }
    None
}
