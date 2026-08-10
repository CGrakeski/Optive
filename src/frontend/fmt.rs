//! Optive 源码格式化（`Optive fmt`）。
//!
//! 风格约定：
//! - 缩进 4 空格；K&R 花括号；行宽 100
//! - 宽松空格：`a + b`、`f(x, y)`、`{ a: 1 }`
//! - 调用：尽量保留原单行/多行结构（用实参源码行号判断）；超行宽则竖排
//! - 控制结构强制 `if (cond) { ... }`
//! - 顶层声明之间固定 1 空行；块内不加空行
//! - 注释以 `Stmt::Comment` 保留

use crate::ast::{Program, LocatedStmt, Stmt, ProtocolMember, CatchPattern, DelTarget, Visibility, Expr, FuncParam, RET_WRAPPER_VAL, ModuleRef, DestructPattern, DestructElem, LValue, Pattern, PatternElem, ExprKind, FStringPart, ForItem, CallArg, UnaryOp, BinaryOp};
use crate::error::ParseError;
use crate::parser::Parser;
use crate::runtime_ast;

const INDENT: &str = "    ";
const MAX_WIDTH: usize = 100;

pub fn format_source(source: &str) -> Result<String, ParseError> {
    let program = Parser::parse(source)?;
    Ok(format_program(&program))
}

#[must_use]
pub fn format_program(program: &Program) -> String {
    let mut out = Formatter::new();
    out.emit_block_stmts(&program.stmts, 0, true);
    let mut s = out.buf;
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

struct Formatter {
    buf: String,
}

impl Formatter {
    const fn new() -> Self {
        Self { buf: String::new() }
    }

    fn indent(&mut self, depth: usize) {
        for _ in 0..depth {
            self.buf.push_str(INDENT);
        }
    }

    fn emit_block_stmts(&mut self, stmts: &[LocatedStmt], depth: usize, top_level: bool) {
        let mut i = 0;
        while i < stmts.len() {
            // 前置注释贴着下一条声明，中间不空行
            while i < stmts.len() && matches!(stmts[i].stmt, Stmt::Comment { .. }) {
                self.indent(depth);
                self.emit_comment(&stmts[i].stmt);
                self.buf.push('\n');
                i += 1;
            }
            if i >= stmts.len() {
                break;
            }
            self.indent(depth);
            self.emit_stmt(&stmts[i].stmt, depth);
            self.buf.push('\n');
            i += 1;
            if top_level && i < stmts.len() {
                // 跳过后续纯注释前仍插空行：注释属于下一条
                let next_is_only_trailing_comments = false;
                let _ = next_is_only_trailing_comments;
                self.buf.push('\n');
            }
        }
        // 顶层末尾可能多一个空行：去掉连续尾部空行，保留单个换行由调用方保证
        if top_level {
            while self.buf.ends_with("\n\n") {
                self.buf.pop();
            }
        }
    }

    fn emit_comment(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Comment { is_block: false, text } => {
                self.buf.push_str("//");
                self.buf.push_str(text);
            }
            Stmt::Comment { is_block: true, text } => {
                self.buf.push_str("/*");
                self.buf.push_str(text);
                self.buf.push_str("*/");
            }
            _ => {}
        }
    }

    fn emit_stmt(&mut self, stmt: &Stmt, depth: usize) {
        match stmt {
            Stmt::Comment { .. } => self.emit_comment(stmt),
            Stmt::VarDecl {
                visibility,
                is_const,
                is_var,
                name,
                type_expr,
                type_strong,
                init,
            } => {
                self.emit_visibility(*visibility);
                if *is_const {
                    self.buf.push_str("const ");
                }
                self.buf.push_str(if *is_var { "var " } else { "let " });
                self.buf.push_str(name);
                if let Some(t) = type_expr {
                    self.buf.push_str(if *type_strong { " :: " } else { ": " });
                    self.emit_type(t);
                }
                if let Some(e) = init {
                    self.buf.push_str(" = ");
                    self.emit_expr(e, depth);
                }
            }
            Stmt::DestructDecl {
                visibility,
                is_const,
                is_var,
                pattern,
                init,
            } => {
                self.emit_visibility(*visibility);
                if *is_const {
                    self.buf.push_str("const ");
                }
                self.buf.push_str(if *is_var { "var " } else { "let " });
                self.emit_destruct(pattern);
                self.buf.push_str(" = ");
                self.emit_expr(init, depth);
            }
            Stmt::Assign { target, value } => {
                self.emit_lvalue(target, depth);
                self.buf.push_str(" = ");
                self.emit_expr(value, depth);
            }
            Stmt::DestructAssign { pattern, value } => {
                self.emit_destruct(pattern);
                self.buf.push_str(" = ");
                self.emit_expr(value, depth);
            }
            Stmt::FuncDecl {
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
            } => {
                for d in decorators {
                    self.emit_expr(d, depth);
                    self.buf.push('\n');
                    self.indent(depth);
                }
                self.emit_visibility(*visibility);
                if *is_generator {
                    self.buf.push_str("gen ");
                } else {
                    self.buf.push_str("func ");
                }
                self.buf.push_str(name);
                self.emit_type_params(type_params);
                self.emit_param_list(params, depth);
                self.emit_return_sig(return_type.as_ref(), *return_strong, return_wrapper.as_ref(), depth);
                self.buf.push(' ');
                self.emit_block(body, depth);
            }
            Stmt::ProtocolDecl {
                visibility,
                name,
                members,
            } => {
                self.emit_visibility(*visibility);
                self.buf.push_str("protocol ");
                self.buf.push_str(name);
                self.buf.push_str(" {");
                if !members.is_empty() {
                    self.buf.push('\n');
                    for m in members {
                        self.indent(depth + 1);
                        match m {
                            ProtocolMember::Method { name, params } => {
                                self.buf.push_str("func ");
                                self.buf.push_str(name);
                                self.emit_param_list(params, depth + 1);
                            }
                            ProtocolMember::Field { name, mutable } => {
                                self.buf.push_str(if *mutable { "var " } else { "let " });
                                self.buf.push_str(name);
                            }
                        }
                        self.buf.push('\n');
                    }
                    self.indent(depth);
                }
                self.buf.push('}');
            }
            Stmt::Return(e) => {
                self.buf.push_str("return");
                if let Some(e) = e {
                    self.buf.push(' ');
                    self.emit_expr(e, depth);
                }
            }
            Stmt::Yield(e) => {
                self.buf.push_str("yield");
                if let Some(e) = e {
                    self.buf.push(' ');
                    self.emit_expr(e, depth);
                }
            }
            Stmt::YieldFrom(e) => {
                self.buf.push_str("yield from ");
                self.emit_expr(e, depth);
            }
            Stmt::Throw(e) => {
                self.buf.push_str("throw ");
                self.emit_expr(e, depth);
            }
            Stmt::If {
                cond,
                then_block,
                elifs,
                else_block,
            } => {
                self.buf.push_str("if (");
                self.emit_expr(cond, depth);
                self.buf.push_str(") ");
                self.emit_block(then_block, depth);
                for (c, b) in elifs {
                    self.buf.push_str(" elif (");
                    self.emit_expr(c, depth);
                    self.buf.push_str(") ");
                    self.emit_block(b, depth);
                }
                if let Some(b) = else_block {
                    self.buf.push_str(" else ");
                    self.emit_block(b, depth);
                }
            }
            Stmt::While { cond, body } => {
                self.buf.push_str("while (");
                self.emit_expr(cond, depth);
                self.buf.push_str(") ");
                self.emit_block(body, depth);
            }
            Stmt::Loop { count, body } => {
                self.buf.push_str("loop");
                if let Some(c) = count {
                    self.buf.push_str(" (");
                    self.emit_expr(c, depth);
                    self.buf.push(')');
                }
                self.buf.push(' ');
                self.emit_block(body, depth);
            }
            Stmt::For { items, body } => {
                self.buf.push_str("for (");
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.buf.push_str(&it.name);
                    self.buf.push_str(" in ");
                    self.emit_expr(&it.iterable, depth);
                }
                self.buf.push_str(") ");
                self.emit_block(body, depth);
            }
            Stmt::Break => self.buf.push_str("break"),
            Stmt::Continue => self.buf.push_str("continue"),
            Stmt::Try {
                body,
                catches,
                else_block,
            } => {
                self.buf.push_str("try ");
                self.emit_block(body, depth);
                for c in catches {
                    self.buf.push_str(" catch (");
                    match &c.pattern {
                        CatchPattern::Wildcard => self.buf.push_str("..."),
                        CatchPattern::Bind { name, type_name } => {
                            self.buf.push_str(name);
                            if let Some(t) = type_name {
                                self.buf.push_str(": ");
                                self.buf.push_str(t);
                            }
                        }
                    }
                    self.buf.push_str(") ");
                    self.emit_block(&c.body, depth);
                }
                if let Some(b) = else_block {
                    self.buf.push_str(" else ");
                    self.emit_block(b, depth);
                }
            }
            Stmt::Match {
                subject,
                cases,
                else_block,
            } => {
                self.buf.push_str("match (");
                self.emit_expr(subject, depth);
                self.buf.push_str(") {");
                self.buf.push('\n');
                for case in cases {
                    self.indent(depth + 1);
                    self.buf.push_str("case ");
                    self.emit_pattern(&case.pattern, depth + 1);
                    self.buf.push(' ');
                    self.emit_block(&case.body, depth + 1);
                    self.buf.push('\n');
                }
                if let Some(b) = else_block {
                    self.indent(depth + 1);
                    self.buf.push_str("else ");
                    self.emit_block(b, depth + 1);
                    self.buf.push('\n');
                }
                self.indent(depth);
                self.buf.push('}');
            }
            Stmt::Del(target) => {
                self.buf.push_str("del ");
                match target {
                    DelTarget::Name(n) => self.buf.push_str(n),
                    DelTarget::Member { object, field } => {
                        self.emit_expr(object, depth);
                        self.buf.push('.');
                        self.buf.push_str(field);
                    }
                    DelTarget::Index { object, index } => {
                        self.emit_expr(object, depth);
                        self.buf.push('[');
                        self.emit_expr(index, depth);
                        self.buf.push(']');
                    }
                }
            }
            Stmt::With {
                context,
                alias,
                body,
            } => {
                self.buf.push_str("with (");
                self.emit_expr(context, depth);
                self.buf.push(')');
                if let Some(a) = alias {
                    self.buf.push_str(" as ");
                    self.buf.push_str(a);
                }
                self.buf.push(' ');
                self.emit_block(body, depth);
            }
            Stmt::Import {
                path,
                path_is_string,
                alias,
            } => {
                self.buf.push_str("import ");
                if *path_is_string {
                    self.emit_string_lit(path);
                } else {
                    self.buf.push_str(path);
                }
                if let Some(a) = alias {
                    self.buf.push_str(" as ");
                    self.buf.push_str(a);
                }
            }
            Stmt::Use { module, items } => {
                self.buf.push_str("use ");
                self.emit_module_ref(module);
                self.buf.push_str(".{ ");
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.buf.push_str(&it.name);
                    if let Some(a) = &it.alias {
                        self.buf.push_str(" as ");
                        self.buf.push_str(a);
                    }
                }
                self.buf.push_str(" }");
            }
            Stmt::StructDecl {
                visibility,
                typed,
                name,
                type_params,
                base,
                fields,
                methods,
                layout,
            } => {
                self.emit_visibility(*visibility);
                if *typed {
                    self.buf.push_str("typed ");
                }
                self.buf.push_str("struct ");
                self.buf.push_str(name);
                self.emit_type_params(type_params);
                if let Some(b) = base {
                    self.buf.push_str(": ");
                    self.buf.push_str(b);
                }
                self.buf.push_str(" {");
                self.buf.push('\n');
                for f in fields {
                    self.indent(depth + 1);
                    self.buf.push_str(if f.mutable { "var " } else { "let " });
                    self.buf.push_str(&f.name);
                    if let Some(t) = &f.type_expr {
                        self.buf.push_str(if f.type_strong { " :: " } else { ": " });
                        self.emit_type(t);
                    }
                    if let Some(d) = &f.default_expr {
                        self.buf.push_str(" = ");
                        self.emit_expr(d, depth + 1);
                    }
                    self.buf.push('\n');
                }
                for m in methods {
                    self.indent(depth + 1);
                    if m.outside {
                        self.buf.push_str("outside ");
                    }
                    if m.overload {
                        self.buf.push_str("overload ");
                    }
                    self.buf.push_str("func ");
                    self.buf.push_str(&m.name);
                    self.emit_param_list(&m.params, depth + 1);
                    self.emit_return_sig(
                        m.return_type.as_ref(),
                        m.return_strong,
                        m.return_wrapper.as_ref(),
                        depth + 1,
                    );
                    self.buf.push(' ');
                    self.emit_block(&m.body, depth + 1);
                    self.buf.push('\n');
                }
                self.indent(depth);
                self.buf.push('}');
                if let Some(l) = layout {
                    self.buf.push_str(": ");
                    self.emit_type(l);
                }
            }
            Stmt::MacroDecl {
                visibility,
                name,
                params,
                body,
            } => {
                self.emit_visibility(*visibility);
                self.buf.push_str("macro ");
                self.buf.push_str(name);
                self.buf.push('(');
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    if p.is_variadic {
                        self.buf.push('*');
                    }
                    self.buf.push_str(&p.name);
                    if let Some(t) = &p.type_expr {
                        self.buf.push_str(if p.type_strong { " :: " } else { ": " });
                        self.emit_type(t);
                    }
                }
                self.buf.push_str(") ");
                self.emit_block(body, depth);
            }
            Stmt::FriendFuncDecl {
                visibility,
                name,
                params,
                return_type,
                return_strong,
                return_wrapper,
                body,
            } => {
                self.emit_visibility(*visibility);
                self.buf.push_str("friend func ");
                self.buf.push_str(name);
                if let Some(ps) = params {
                    self.emit_param_list(ps, depth);
                }
                self.emit_return_sig(return_type.as_ref(), *return_strong, return_wrapper.as_ref(), depth);
                if let Some(b) = body {
                    self.buf.push(' ');
                    self.emit_block(b, depth);
                }
            }
            Stmt::EnumDecl {
                visibility,
                name,
                members,
                methods,
            } => {
                self.emit_visibility(*visibility);
                self.buf.push_str("enum ");
                self.buf.push_str(name);
                self.buf.push_str(" {");
                self.buf.push('\n');
                for m in members {
                    self.indent(depth + 1);
                    self.buf.push_str(&m.name);
                    if let Some(v) = &m.value {
                        self.buf.push_str(" = ");
                        self.emit_expr(v, depth + 1);
                    }
                    self.buf.push('\n');
                }
                for m in methods {
                    self.indent(depth + 1);
                    self.buf.push_str("func ");
                    self.buf.push_str(&m.name);
                    self.emit_param_list(&m.params, depth + 1);
                    self.buf.push(' ');
                    self.emit_block(&m.body, depth + 1);
                    self.buf.push('\n');
                }
                self.indent(depth);
                self.buf.push('}');
            }
            Stmt::VariantDecl {
                visibility,
                name,
                type_params,
                cases,
            } => {
                self.emit_visibility(*visibility);
                self.buf.push_str("variant ");
                self.buf.push_str(name);
                self.emit_type_params(type_params);
                self.buf.push_str(" {");
                self.buf.push('\n');
                for c in cases {
                    self.indent(depth + 1);
                    self.buf.push_str(&c.name);
                    if !c.fields.is_empty() {
                        self.buf.push('(');
                        for (i, f) in c.fields.iter().enumerate() {
                            if i > 0 {
                                self.buf.push_str(", ");
                            }
                            self.buf.push_str(if f.mutable { "var " } else { "let " });
                            self.buf.push_str(&f.name);
                            if let Some(t) = &f.type_expr {
                                self.buf
                                    .push_str(if f.type_strong { " :: " } else { ": " });
                                self.emit_type(t);
                            }
                        }
                        self.buf.push(')');
                    }
                    self.buf.push('\n');
                }
                self.indent(depth);
                self.buf.push('}');
            }
            Stmt::Expr(e) => self.emit_expr(e, depth),
            Stmt::Block(b) => self.emit_block(b, depth),
        }
    }

    fn emit_visibility(&mut self, vis: Visibility) {
        match vis {
            Visibility::Default => {}
            Visibility::Exported => self.buf.push_str("export "),
            Visibility::Internal => self.buf.push_str("intern "),
        }
    }

    fn emit_block(&mut self, block: &[LocatedStmt], depth: usize) {
        self.buf.push('{');
        if block.is_empty() {
            self.buf.push('}');
            return;
        }
        self.buf.push('\n');
        self.emit_block_stmts(block, depth + 1, false);
        self.indent(depth);
        self.buf.push('}');
    }

    fn emit_type_params(&mut self, params: &[(String, Option<Expr>)]) {
        if params.is_empty() {
            return;
        }
        self.buf.push('[');
        for (i, (name, bound)) in params.iter().enumerate() {
            if i > 0 {
                self.buf.push_str(", ");
            }
            self.buf.push_str(name);
            if let Some(b) = bound {
                self.buf.push_str(": ");
                self.emit_type(b);
            }
        }
        self.buf.push(']');
    }

    fn emit_param_list(&mut self, params: &[FuncParam], depth: usize) {
        self.buf.push('(');
        let multiline = params_multiline(params);
        if multiline {
            self.buf.push('\n');
            for p in params {
                self.indent(depth + 1);
                self.emit_func_param(p, depth + 1);
                self.buf.push(',');
                self.buf.push('\n');
            }
            self.indent(depth);
            self.buf.push(')');
        } else {
            for (i, p) in params.iter().enumerate() {
                if i > 0 {
                    self.buf.push_str(", ");
                }
                self.emit_func_param(p, depth);
            }
            self.buf.push(')');
        }
    }

    fn emit_func_param(&mut self, p: &FuncParam, depth: usize) {
        if p.implicit {
            self.buf.push_str("implicit ");
        }
        if p.is_kwvariadic {
            self.buf.push_str("**");
        } else if p.is_variadic {
            self.buf.push('*');
        }
        self.buf.push_str(&p.name);
        if let Some(t) = &p.type_expr {
            self.buf.push_str(if p.type_strong { " :: " } else { ": " });
            self.emit_type(t);
        }
        if let Some(d) = &p.default_expr {
            self.buf.push_str(" = ");
            self.emit_expr(d, depth);
        }
    }

    fn emit_return_sig(
        &mut self,
        return_type: Option<&Expr>,
        return_strong: bool,
        return_wrapper: Option<&Expr>,
        depth: usize,
    ) {
        if let Some(t) = return_type {
            self.buf
                .push_str(if return_strong { " :: " } else { ": " });
            self.emit_type(t);
        }
        if let Some(w) = return_wrapper {
            self.buf.push_str(" -> ");
            // 包装器里的 `__ret_wrapper_val` 印回 `_`
            self.emit_expr_replacing(w, depth, RET_WRAPPER_VAL, "_");
        }
    }

    fn emit_type(&mut self, t: &Expr) {
        self.emit_expr(t, 0);
    }

    fn emit_module_ref(&mut self, m: &ModuleRef) {
        match m {
            ModuleRef::Qualified(parts) => self.buf.push_str(&parts.join(".")),
            ModuleRef::FilePath { path, attrs } => {
                self.emit_string_lit(path);
                for a in attrs {
                    self.buf.push('.');
                    self.buf.push_str(a);
                }
            }
        }
    }

    fn emit_destruct(&mut self, p: &DestructPattern) {
        match p {
            DestructPattern::Name(n) => self.buf.push_str(n),
            DestructPattern::Discard => self.buf.push('_'),
            DestructPattern::Tuple(elems) => {
                self.buf.push('(');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_destruct_elem(e);
                }
                self.buf.push(')');
            }
            DestructPattern::List(elems) => {
                self.buf.push('[');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_destruct_elem(e);
                }
                self.buf.push(']');
            }
        }
    }

    fn emit_destruct_elem(&mut self, e: &DestructElem) {
        match e {
            DestructElem::Pat(p) => self.emit_destruct(p),
            DestructElem::Rest(n) => {
                self.buf.push('*');
                self.buf.push_str(n);
            }
            DestructElem::RestDiscard => self.buf.push_str("*_"),
        }
    }

    fn emit_lvalue(&mut self, lv: &LValue, depth: usize) {
        match lv {
            LValue::Name(n) => self.buf.push_str(n),
            LValue::Member { object, field } => {
                self.emit_expr(object, depth);
                self.buf.push('.');
                self.buf.push_str(field);
            }
            LValue::Index { object, index } => {
                self.emit_expr(object, depth);
                self.buf.push('[');
                self.emit_expr(index, depth);
                self.buf.push(']');
            }
            LValue::Slice {
                object,
                start,
                end,
                step,
            } => {
                self.emit_expr(object, depth);
                self.buf.push('[');
                if let Some(s) = start {
                    self.emit_expr(s, depth);
                }
                self.buf.push(':');
                if let Some(e) = end {
                    self.emit_expr(e, depth);
                }
                if let Some(s) = step {
                    self.buf.push(':');
                    self.emit_expr(s, depth);
                }
                self.buf.push(']');
            }
        }
    }

    fn emit_pattern(&mut self, p: &Pattern, depth: usize) {
        match p {
            Pattern::Bind(n) => self.buf.push_str(n),
            Pattern::Value(e) => self.emit_expr(e, depth),
            Pattern::List(elems) => {
                self.buf.push('[');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    match e {
                        PatternElem::Bind(n) => self.buf.push_str(n),
                        PatternElem::Nested(np) => self.emit_pattern(np, depth),
                        PatternElem::Value(v) => self.emit_expr(v, depth),
                    }
                }
                self.buf.push(']');
            }
            Pattern::Tuple(elems) => {
                self.buf.push('(');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    match e {
                        PatternElem::Bind(n) => self.buf.push_str(n),
                        PatternElem::Nested(np) => self.emit_pattern(np, depth),
                        PatternElem::Value(v) => self.emit_expr(v, depth),
                    }
                }
                if elems.len() == 1 {
                    self.buf.push(',');
                }
                self.buf.push(')');
            }
            Pattern::Struct { type_name, fields } => {
                self.buf.push_str(type_name);
                self.buf.push('(');
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.buf.push_str(f);
                }
                self.buf.push(')');
            }
            Pattern::Or(ps) => {
                for (i, p) in ps.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(" | ");
                    }
                    self.emit_pattern(p, depth);
                }
            }
            Pattern::Call { type_name, args } => {
                self.buf.push_str(type_name);
                self.buf.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_pattern(a, depth);
                }
                self.buf.push(')');
            }
        }
    }

    fn emit_expr(&mut self, expr: &Expr, depth: usize) {
        self.emit_expr_replacing(expr, depth, "", "");
    }

    fn emit_expr_replacing(&mut self, expr: &Expr, depth: usize, replace_var: &str, with: &str) {
        match &expr.kind {
            ExprKind::Number(n) => self.buf.push_str(n),
            ExprKind::String(s) => self.emit_string_lit(s),
            ExprKind::FString(parts) => {
                self.buf.push_str("f\"");
                for p in parts {
                    match p {
                        FStringPart::Text(t) => self.push_escaped_fstring_text(t),
                        FStringPart::Expr(e) => {
                            self.buf.push('{');
                            self.emit_expr_replacing(e, depth, replace_var, with);
                            self.buf.push('}');
                        }
                    }
                }
                self.buf.push('"');
            }
            ExprKind::Bool(b) => self.buf.push_str(if *b { "true" } else { "false" }),
            ExprKind::None => self.buf.push_str("none"),
            ExprKind::Var(n) => {
                if !replace_var.is_empty() && n == replace_var {
                    self.buf.push_str(with);
                } else {
                    self.buf.push_str(n);
                }
            }
            ExprKind::Placeholder => self.buf.push('_'),
            ExprKind::Unary { op, operand } => {
                self.buf.push_str(unary_op_str(*op));
                let need_paren = matches!(operand.kind, ExprKind::Binary { .. });
                if need_paren {
                    self.buf.push('(');
                }
                self.emit_expr_replacing(operand, depth, replace_var, with);
                if need_paren {
                    self.buf.push(')');
                }
            }
            ExprKind::Binary { op, left, right } => {
                self.emit_expr_replacing(left, depth, replace_var, with);
                self.buf.push(' ');
                self.buf.push_str(binary_op_str(*op));
                self.buf.push(' ');
                self.emit_expr_replacing(right, depth, replace_var, with);
            }
            ExprKind::Call { callee, args } => {
                self.emit_expr_replacing(callee, depth, replace_var, with);
                self.emit_call_args(args, depth, |this, a, d| {
                    this.emit_call_arg(a, d, replace_var, with);
                });
            }
            ExprKind::MacroCall { callee, args } => {
                self.emit_expr_replacing(callee, depth, replace_var, with);
                self.buf.push('{');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    if a.is_splat {
                        self.buf.push('*');
                    }
                    self.buf.push_str(&runtime_ast::ast_to_source(&a.node));
                }
                self.buf.push('}');
            }
            ExprKind::Member { object, field } => {
                self.emit_expr_replacing(object, depth, replace_var, with);
                self.buf.push('.');
                self.buf.push_str(field);
            }
            ExprKind::Index { object, index } => {
                self.emit_expr_replacing(object, depth, replace_var, with);
                self.buf.push('[');
                self.emit_expr_replacing(index, depth, replace_var, with);
                self.buf.push(']');
            }
            ExprKind::Slice {
                object,
                start,
                end,
                step,
            } => {
                self.emit_expr_replacing(object, depth, replace_var, with);
                self.buf.push('[');
                if let Some(s) = start {
                    self.emit_expr_replacing(s, depth, replace_var, with);
                }
                self.buf.push(':');
                if let Some(e) = end {
                    self.emit_expr_replacing(e, depth, replace_var, with);
                }
                if let Some(s) = step {
                    self.buf.push(':');
                    self.emit_expr_replacing(s, depth, replace_var, with);
                }
                self.buf.push(']');
            }
            ExprKind::TypeConvert { type_expr, value } => {
                self.emit_expr_replacing(type_expr, depth, replace_var, with);
                self.buf.push_str(".(");
                self.emit_expr_replacing(value, depth, replace_var, with);
                self.buf.push(')');
            }
            ExprKind::List(elems) => {
                self.buf.push('[');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr_replacing(e, depth, replace_var, with);
                }
                self.buf.push(']');
            }
            ExprKind::Dict(entries) => {
                self.buf.push('{');
                if !entries.is_empty() {
                    self.buf.push(' ');
                    for (i, (k, v)) in entries.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.emit_expr_replacing(k, depth, replace_var, with);
                        self.buf.push_str(": ");
                        self.emit_expr_replacing(v, depth, replace_var, with);
                    }
                    self.buf.push(' ');
                }
                self.buf.push('}');
            }
            ExprKind::Set(elems) => {
                if elems.is_empty() {
                    self.buf.push_str("{,}");
                } else {
                    self.buf.push('{');
                    for (i, e) in elems.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.emit_expr_replacing(e, depth, replace_var, with);
                    }
                    self.buf.push('}');
                }
            }
            ExprKind::Tuple(elems) => {
                self.buf.push('(');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr_replacing(e, depth, replace_var, with);
                }
                if elems.len() == 1 {
                    self.buf.push(',');
                }
                self.buf.push(')');
            }
            ExprKind::Bytes(b) => {
                self.buf.push_str("b\"");
                for &byte in b {
                    match byte {
                        b'\\' => self.buf.push_str("\\\\"),
                        b'"' => self.buf.push_str("\\\""),
                        b'\n' => self.buf.push_str("\\n"),
                        b'\r' => self.buf.push_str("\\r"),
                        b'\t' => self.buf.push_str("\\t"),
                        0x20..=0x7e => self.buf.push(byte as char),
                        _ => self.buf.push_str(&format!("\\x{byte:02x}")),
                    }
                }
                self.buf.push('"');
            }
            ExprKind::ListComp { elem, items, guards } => {
                self.buf.push('[');
                self.emit_expr_replacing(elem, depth, replace_var, with);
                self.emit_comp_clauses(items, guards, depth, replace_var, with);
                self.buf.push(']');
            }
            ExprKind::DictComp {
                key,
                value,
                items,
                guards,
            } => {
                self.buf.push('{');
                self.emit_expr_replacing(key, depth, replace_var, with);
                self.buf.push_str(": ");
                self.emit_expr_replacing(value, depth, replace_var, with);
                self.emit_comp_clauses(items, guards, depth, replace_var, with);
                self.buf.push('}');
            }
            ExprKind::SetComp { elem, items, guards } => {
                self.buf.push('{');
                self.emit_expr_replacing(elem, depth, replace_var, with);
                self.emit_comp_clauses(items, guards, depth, replace_var, with);
                self.buf.push('}');
            }
            ExprKind::GeneratorExp { elem, items, guards } => {
                self.buf.push('(');
                self.emit_expr_replacing(elem, depth, replace_var, with);
                self.emit_comp_clauses(items, guards, depth, replace_var, with);
                self.buf.push(')');
            }
            ExprKind::IfThenElse {
                cond,
                then_expr,
                else_expr,
            } => {
                self.buf.push_str("if ");
                self.emit_expr_replacing(cond, depth, replace_var, with);
                self.buf.push_str(" then ");
                self.emit_expr_replacing(then_expr, depth, replace_var, with);
                self.buf.push_str(" else ");
                self.emit_expr_replacing(else_expr, depth, replace_var, with);
            }
            ExprKind::Handle { operand } => {
                self.buf.push_str("handle ");
                self.emit_expr_replacing(operand, depth, replace_var, with);
            }
            ExprKind::Go { operand } => {
                self.buf.push_str("go ");
                self.emit_expr_replacing(operand, depth, replace_var, with);
            }
            ExprKind::ParFor { items, body } => {
                self.buf.push_str("par for (");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.buf.push_str(&item.name);
                    self.buf.push_str(" in ");
                    self.emit_expr_replacing(&item.iterable, depth, replace_var, with);
                }
                self.buf.push_str(") ");
                self.emit_block(body, depth);
            }
            ExprKind::ParBlock { exprs } => {
                self.buf.push_str("par {");
                self.buf.push('\n');
                for e in exprs {
                    self.indent(depth + 1);
                    self.emit_expr_replacing(e, depth + 1, replace_var, with);
                    self.buf.push('\n');
                }
                self.indent(depth);
                self.buf.push('}');
            }
            ExprKind::Snap { operand } => {
                self.buf.push_str("snap ");
                self.emit_expr_replacing(operand, depth, replace_var, with);
            }
            ExprKind::Await { operand } => {
                self.buf.push_str("await ");
                self.emit_expr_replacing(operand, depth, replace_var, with);
            }
            ExprKind::Suspend => self.buf.push_str("suspend"),
            ExprKind::Select { cases, else_block } => {
                self.buf.push_str("select {");
                self.buf.push('\n');
                for c in cases {
                    self.indent(depth + 1);
                    self.buf.push_str("case ");
                    self.emit_expr_replacing(&c.event, depth + 1, replace_var, with);
                    if let Some(b) = &c.bind {
                        self.buf.push_str(" as ");
                        self.buf.push_str(b);
                    }
                    self.buf.push(' ');
                    self.emit_block(&c.body, depth + 1);
                    self.buf.push('\n');
                }
                if let Some(b) = else_block {
                    self.indent(depth + 1);
                    self.buf.push_str("else ");
                    self.emit_block(b, depth + 1);
                    self.buf.push('\n');
                }
                self.indent(depth);
                self.buf.push('}');
            }
            ExprKind::NamedAssign { name, value } => {
                self.buf.push_str(name);
                self.buf.push_str(" := ");
                self.emit_expr_replacing(value, depth, replace_var, with);
            }
            ExprKind::DoFunc {
                params,
                return_type,
                return_strong,
                return_wrapper,
                body,
            } => {
                self.buf.push_str("do");
                self.emit_param_list(params, depth);
                self.emit_return_sig(
                    return_type.as_deref(),
                    *return_strong,
                    return_wrapper.as_deref(),
                    depth,
                );
                self.buf.push(' ');
                self.emit_block(body, depth);
            }
            ExprKind::Pipeline {
                left,
                right,
                pipe_name,
            } => {
                self.emit_expr_replacing(left, depth, replace_var, with);
                self.buf.push_str(" |> ");
                self.emit_expr_replacing(right, depth, pipe_name, "_");
            }
            ExprKind::Quote {
                hygienic_names,
                bindings,
                body,
            } => {
                self.buf.push_str("quote");
                if !hygienic_names.is_empty() || !bindings.is_empty() {
                    self.buf.push('(');
                    for (i, n) in hygienic_names.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.buf.push_str(n);
                    }
                    for (i, b) in bindings.iter().enumerate() {
                        if i > 0 || !hygienic_names.is_empty() {
                            self.buf.push_str(", ");
                        }
                        self.emit_expr_replacing(b, depth, replace_var, with);
                    }
                    self.buf.push(')');
                }
                self.buf.push(' ');
                self.emit_block(body, depth);
            }
            ExprKind::Match {
                subject,
                cases,
                else_block,
            } => {
                self.buf.push_str("match (");
                self.emit_expr_replacing(subject, depth, replace_var, with);
                self.buf.push_str(") {");
                self.buf.push('\n');
                for case in cases {
                    self.indent(depth + 1);
                    self.buf.push_str("case ");
                    self.emit_pattern(&case.pattern, depth + 1);
                    self.buf.push(' ');
                    self.emit_block(&case.body, depth + 1);
                    self.buf.push('\n');
                }
                if let Some(b) = else_block {
                    self.indent(depth + 1);
                    self.buf.push_str("else ");
                    self.emit_block(b, depth + 1);
                    self.buf.push('\n');
                }
                self.indent(depth);
                self.buf.push('}');
            }
        }
    }

    fn emit_comp_clauses(
        &mut self,
        items: &[ForItem],
        guards: &[Expr],
        depth: usize,
        replace_var: &str,
        with: &str,
    ) {
        for it in items {
            self.buf.push_str(" for (");
            self.buf.push_str(&it.name);
            self.buf.push_str(" in ");
            self.emit_expr_replacing(&it.iterable, depth, replace_var, with);
            self.buf.push(')');
        }
        for g in guards {
            self.buf.push_str(" if (");
            self.emit_expr_replacing(g, depth, replace_var, with);
            self.buf.push(')');
        }
    }

    fn emit_call_args<F>(&mut self, args: &[CallArg], depth: usize, mut emit_one: F)
    where
        F: FnMut(&mut Self, &CallArg, usize),
    {
        let multiline = call_args_multiline(args);
        self.buf.push('(');
        if args.is_empty() {
            self.buf.push(')');
            return;
        }
        if multiline {
            self.buf.push('\n');
            for a in args {
                self.indent(depth + 1);
                emit_one(self, a, depth + 1);
                self.buf.push(',');
                self.buf.push('\n');
            }
            self.indent(depth);
            self.buf.push(')');
        } else {
            let start = self.buf.len();
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    self.buf.push_str(", ");
                }
                emit_one(self, a, depth);
            }
            // 超行宽则改为竖排
            let line_start = self.buf[..start].rfind('\n').map_or(0, |i| i + 1);
            let width = self.buf.len() - line_start + 1; // + ')'
            if width > MAX_WIDTH {
                self.buf.truncate(start);
                self.buf.push('\n');
                for a in args {
                    self.indent(depth + 1);
                    emit_one(self, a, depth + 1);
                    self.buf.push(',');
                    self.buf.push('\n');
                }
                self.indent(depth);
            }
            self.buf.push(')');
        }
    }

    fn emit_call_arg(&mut self, a: &CallArg, depth: usize, replace_var: &str, with: &str) {
        if a.is_kwsplat {
            self.buf.push_str("**");
        } else if a.is_splat {
            self.buf.push('*');
        }
        if let Some(n) = &a.name {
            self.buf.push_str(n);
            self.buf.push_str(" = ");
        }
        self.emit_expr_replacing(&a.value, depth, replace_var, with);
    }

    fn emit_string_lit(&mut self, s: &str) {
        self.buf.push('"');
        for ch in s.chars() {
            match ch {
                '\\' => self.buf.push_str("\\\\"),
                '"' => self.buf.push_str("\\\""),
                '\n' => self.buf.push_str("\\n"),
                '\r' => self.buf.push_str("\\r"),
                '\t' => self.buf.push_str("\\t"),
                c if c.is_control() => self.buf.push_str(&format!("\\u{{{:x}}}", c as u32)),
                c => self.buf.push(c),
            }
        }
        self.buf.push('"');
    }

    fn push_escaped_fstring_text(&mut self, t: &str) {
        for ch in t.chars() {
            match ch {
                '\\' => self.buf.push_str("\\\\"),
                '"' => self.buf.push_str("\\\""),
                '{' => self.buf.push_str("{{"),
                '}' => self.buf.push_str("}}"),
                '\n' => self.buf.push_str("\\n"),
                c => self.buf.push(c),
            }
        }
    }
}

const fn unary_op_str(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not | UnaryOp::TruthyNot => "not ",
        UnaryOp::Invert => "~",
    }
}

const fn binary_op_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::Pow => "**",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::LShift => "<<",
        BinaryOp::RShift => ">>",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::In => "in",
        BinaryOp::Is => "is",
        BinaryOp::IsNot => "is not",
        BinaryOp::And => "and",
        BinaryOp::Or => "or",
    }
}

/// 5C：任一实参与首参不在同一行 → 竖排。
fn call_args_multiline(args: &[CallArg]) -> bool {
    if args.len() < 2 {
        return false;
    }
    let first_line = args[0].value.loc.line;
    args.iter().any(|a| a.value.loc.line != first_line)
}

fn params_multiline(params: &[FuncParam]) -> bool {
    if params.len() < 2 {
        return false;
    }
    // 形参无独立 loc；若有默认表达式且跨行则竖排
    let lines: Vec<usize> = params
        .iter()
        .filter_map(|p| p.default_expr.as_ref().map(|e| e.loc.line))
        .collect();
    if lines.len() >= 2 {
        let first = lines[0];
        if lines.iter().any(|&l| l != first) {
            return true;
        }
    }
    false
}
