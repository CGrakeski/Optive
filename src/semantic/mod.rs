//! 共享静态语义检查：名字/`std.*`/arity + 硬注解、未使用、不可达、`go` 共享可变。
//!
//! CLI `check` 与 LSP 诊断走同一入口，避免两套规则漂移。

pub mod names;

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Block, Expr, ExprKind, LValue, LocatedStmt, ModuleRef, Program, Stmt, Visibility,
};
use crate::parser::Parser;

pub type Diagnostic = names::Diag;

/// 解析并分析；语法错误变成一条诊断。
#[must_use]
pub fn analyze_source(source: &str) -> Vec<Diagnostic> {
    match Parser::parse(source) {
        Ok(program) => analyze(&program),
        Err(crate::error::ParseError::Message {
            line,
            column,
            message,
        }) => vec![(line, column, message)],
    }
}

#[must_use]
pub fn analyze(program: &Program) -> Vec<Diagnostic> {
    analyze_in(program, "", &HashMap::new())
}

/// `file` 用于解析相对 `import`/`use`；`docs` 为已打开的 URI→源码。
#[must_use]
pub fn analyze_in(
    program: &Program,
    file: &str,
    docs: &HashMap<String, String>,
) -> Vec<Diagnostic> {
    let mut diags = names::analyze_program(program);
    extra_pass(program, &mut diags);
    if !file.is_empty() {
        cross_module_exports(program, file, docs, &mut diags);
    }
    diags
}

fn extra_pass(program: &Program, diags: &mut Vec<Diagnostic>) {
    let declared_types = collect_type_names(&program.stmts);
    unused_imports(&program.stmts, diags);
    unreachable_in_block(&program.stmts, diags);
    walk_block_extra(&program.stmts, &HashSet::new(), &declared_types, diags);
}

fn collect_type_names(stmts: &Block) -> HashSet<String> {
    let mut names = HashSet::new();
    for n in crate::type_registry::global_type_names() {
        names.insert(n.to_string());
    }
    for st in stmts {
        match &st.stmt {
            Stmt::StructDecl { name, .. }
            | Stmt::EnumDecl { name, .. }
            | Stmt::VariantDecl { name, .. }
            | Stmt::ProtocolDecl { name, .. } => {
                names.insert(name.clone());
            }
            _ => {}
        }
    }
    names
}

fn unused_imports(stmts: &Block, diags: &mut Vec<Diagnostic>) {
    let mut imports: Vec<(String, usize, usize)> = Vec::new();
    for st in stmts {
        match &st.stmt {
            Stmt::Import { path, alias, .. } => {
                let key = alias.as_deref().unwrap_or(path.as_str());
                let short = key.rsplit('.').next().unwrap_or(key);
                imports.push((short.to_string(), st.line, st.column));
            }
            Stmt::Use { items, .. } => {
                for it in items {
                    let local = it.alias.as_deref().unwrap_or(it.name.as_str());
                    imports.push((local.to_string(), st.line, st.column));
                }
            }
            _ => {}
        }
    }
    if imports.is_empty() {
        return;
    }
    let mut used = HashSet::new();
    walk_block_uses(stmts, &mut used);
    for (name, line, col) in imports {
        if name.starts_with('_') {
            continue;
        }
        if !used.contains(&name) {
            diags.push((line, col, format!("unused import `{name}`")));
        }
    }
}

fn walk_block_uses(stmts: &Block, used: &mut HashSet<String>) {
    for st in stmts {
        walk_stmt_uses(&st.stmt, used);
    }
}

fn walk_stmt_uses(stmt: &Stmt, used: &mut HashSet<String>) {
    match stmt {
        Stmt::VarDecl {
            init, type_expr, ..
        } => {
            if let Some(t) = type_expr {
                walk_expr_uses(t, used);
            }
            if let Some(e) = init {
                walk_expr_uses(e, used);
            }
        }
        Stmt::DestructDecl { init, .. } => walk_expr_uses(init, used),
        Stmt::Assign { target, value } => {
            walk_lvalue_uses(target, used);
            walk_expr_uses(value, used);
        }
        Stmt::DestructAssign { value, .. } => walk_expr_uses(value, used),
        Stmt::FuncDecl {
            params,
            body,
            return_type,
            return_wrapper,
            type_params,
            ..
        } => {
            for (_, bound) in type_params {
                if let Some(b) = bound {
                    walk_expr_uses(b, used);
                }
            }
            for p in params {
                if let Some(t) = &p.type_expr {
                    walk_expr_uses(t, used);
                }
                if let Some(d) = &p.default_expr {
                    walk_expr_uses(d, used);
                }
            }
            if let Some(t) = return_type {
                walk_expr_uses(t, used);
            }
            if let Some(t) = return_wrapper {
                walk_expr_uses(t, used);
            }
            walk_block_uses(body, used);
        }
        Stmt::FriendFuncDecl {
            params,
            body,
            return_type,
            return_wrapper,
            ..
        } => {
            if let Some(ps) = params {
                for p in ps {
                    if let Some(t) = &p.type_expr {
                        walk_expr_uses(t, used);
                    }
                }
            }
            if let Some(t) = return_type {
                walk_expr_uses(t, used);
            }
            if let Some(t) = return_wrapper {
                walk_expr_uses(t, used);
            }
            if let Some(b) = body {
                walk_block_uses(b, used);
            }
        }
        Stmt::Return(e) | Stmt::Yield(e) => {
            if let Some(e) = e {
                walk_expr_uses(e, used);
            }
        }
        Stmt::YieldFrom(e) | Stmt::Throw(e) | Stmt::Expr(e) => walk_expr_uses(e, used),
        Stmt::If {
            cond,
            then_block,
            elifs,
            else_block,
        } => {
            walk_expr_uses(cond, used);
            walk_block_uses(then_block, used);
            for (c, b) in elifs {
                walk_expr_uses(c, used);
                walk_block_uses(b, used);
            }
            if let Some(b) = else_block {
                walk_block_uses(b, used);
            }
        }
        Stmt::While { cond, body } => {
            walk_expr_uses(cond, used);
            walk_block_uses(body, used);
        }
        Stmt::Loop { count, body } => {
            if let Some(c) = count {
                walk_expr_uses(c, used);
            }
            walk_block_uses(body, used);
        }
        Stmt::For { items, body } => {
            for it in items {
                walk_expr_uses(&it.iterable, used);
            }
            walk_block_uses(body, used);
        }
        Stmt::Try {
            body,
            catches,
            else_block,
        } => {
            walk_block_uses(body, used);
            for c in catches {
                walk_block_uses(&c.body, used);
            }
            if let Some(b) = else_block {
                walk_block_uses(b, used);
            }
        }
        Stmt::Match {
            subject,
            cases,
            else_block,
        } => {
            walk_expr_uses(subject, used);
            for c in cases {
                walk_block_uses(&c.body, used);
            }
            if let Some(b) = else_block {
                walk_block_uses(b, used);
            }
        }
        Stmt::With { context, body, .. } => {
            walk_expr_uses(context, used);
            walk_block_uses(body, used);
        }
        Stmt::StructDecl {
            fields,
            methods,
            layout,
            type_params,
            ..
        } => {
            for (_, bound) in type_params {
                if let Some(b) = bound {
                    walk_expr_uses(b, used);
                }
            }
            if let Some(l) = layout {
                walk_expr_uses(l, used);
            }
            for f in fields {
                if let Some(t) = &f.type_expr {
                    walk_expr_uses(t, used);
                }
                if let Some(d) = &f.default_expr {
                    walk_expr_uses(d, used);
                }
            }
            for m in methods {
                walk_block_uses(&m.body, used);
            }
        }
        Stmt::EnumDecl {
            methods, members, ..
        } => {
            for mem in members {
                if let Some(v) = &mem.value {
                    walk_expr_uses(v, used);
                }
            }
            for m in methods {
                walk_block_uses(&m.body, used);
            }
        }
        Stmt::VariantDecl { type_params, .. } => {
            for (_, bound) in type_params {
                if let Some(b) = bound {
                    walk_expr_uses(b, used);
                }
            }
        }
        Stmt::MacroDecl { body, .. } => walk_block_uses(body, used),
        Stmt::Block(b) => walk_block_uses(b, used),
        Stmt::Del(t) => match t {
            crate::ast::DelTarget::Name(n) => {
                used.insert(n.clone());
            }
            crate::ast::DelTarget::Member { object, .. }
            | crate::ast::DelTarget::Index { object, .. } => walk_expr_uses(object, used),
        },
        Stmt::Import { .. }
        | Stmt::Use { .. }
        | Stmt::ProtocolDecl { .. }
        | Stmt::Break
        | Stmt::Continue
        | Stmt::Comment { .. } => {}
    }
}

fn walk_lvalue_uses(lv: &LValue, used: &mut HashSet<String>) {
    match lv {
        LValue::Name(_) => {}
        LValue::Member { object, .. } => walk_expr_uses(object, used),
        LValue::Index { object, index } => {
            walk_expr_uses(object, used);
            walk_expr_uses(index, used);
        }
        LValue::Slice {
            object,
            start,
            end,
            step,
        } => {
            walk_expr_uses(object, used);
            if let Some(s) = start {
                walk_expr_uses(s, used);
            }
            if let Some(e) = end {
                walk_expr_uses(e, used);
            }
            if let Some(s) = step {
                walk_expr_uses(s, used);
            }
        }
    }
}

fn walk_expr_uses(expr: &Expr, used: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Var(n) => {
            used.insert(n.clone());
        }
        ExprKind::Unary { operand, .. }
        | ExprKind::Handle { operand }
        | ExprKind::Go { operand }
        | ExprKind::Snap { operand }
        | ExprKind::Await { operand } => walk_expr_uses(operand, used),
        ExprKind::TypeConvert { type_expr, value } => {
            walk_expr_uses(type_expr, used);
            walk_expr_uses(value, used);
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Pipeline { left, right, .. } => {
            walk_expr_uses(left, used);
            walk_expr_uses(right, used);
        }
        ExprKind::Call { callee, args } => {
            walk_expr_uses(callee, used);
            for a in args {
                walk_expr_uses(&a.value, used);
            }
        }
        ExprKind::MacroCall { callee, .. } => walk_expr_uses(callee, used),
        ExprKind::Member { object, .. } => walk_expr_uses(object, used),
        ExprKind::Index { object, index } => {
            walk_expr_uses(object, used);
            walk_expr_uses(index, used);
        }
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => {
            walk_expr_uses(object, used);
            if let Some(s) = start {
                walk_expr_uses(s, used);
            }
            if let Some(e) = end {
                walk_expr_uses(e, used);
            }
            if let Some(s) = step {
                walk_expr_uses(s, used);
            }
        }
        ExprKind::List(xs) | ExprKind::Tuple(xs) | ExprKind::Set(xs) => {
            for e in xs {
                walk_expr_uses(e, used);
            }
        }
        ExprKind::Dict(pairs) => {
            for (k, v) in pairs {
                walk_expr_uses(k, used);
                walk_expr_uses(v, used);
            }
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
            walk_expr_uses(elem, used);
            for it in items {
                walk_expr_uses(&it.iterable, used);
            }
            for g in guards {
                walk_expr_uses(g, used);
            }
        }
        ExprKind::DictComp {
            key,
            value,
            items,
            guards,
        } => {
            walk_expr_uses(key, used);
            walk_expr_uses(value, used);
            for it in items {
                walk_expr_uses(&it.iterable, used);
            }
            for g in guards {
                walk_expr_uses(g, used);
            }
        }
        ExprKind::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            walk_expr_uses(cond, used);
            walk_expr_uses(then_expr, used);
            walk_expr_uses(else_expr, used);
        }
        ExprKind::DoFunc { body, params, .. } => {
            for p in params {
                if let Some(t) = &p.type_expr {
                    walk_expr_uses(t, used);
                }
            }
            walk_block_uses(body, used);
        }
        ExprKind::ParFor { items, body } => {
            for it in items {
                walk_expr_uses(&it.iterable, used);
            }
            walk_block_uses(body, used);
        }
        ExprKind::ParBlock { exprs } => {
            for e in exprs {
                walk_expr_uses(e, used);
            }
        }
        ExprKind::Select { cases, else_block } => {
            for c in cases {
                walk_expr_uses(&c.event, used);
                walk_block_uses(&c.body, used);
            }
            if let Some(b) = else_block {
                walk_block_uses(b, used);
            }
        }
        ExprKind::Quote { bindings, body, .. } => {
            for e in bindings {
                walk_expr_uses(e, used);
            }
            walk_block_uses(body, used);
        }
        ExprKind::Match {
            subject,
            cases,
            else_block,
        } => {
            walk_expr_uses(subject, used);
            for c in cases {
                walk_block_uses(&c.body, used);
            }
            if let Some(b) = else_block {
                walk_block_uses(b, used);
            }
        }
        ExprKind::NamedAssign { value, .. } => walk_expr_uses(value, used),
        ExprKind::FString(parts) => {
            for p in parts {
                if let crate::ast::FStringPart::Expr(e) = p {
                    walk_expr_uses(e, used);
                }
            }
        }
        ExprKind::Placeholder
        | ExprKind::Suspend
        | ExprKind::Number(_)
        | ExprKind::String(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Bytes(_) => {}
    }
}

fn unreachable_in_block(stmts: &Block, diags: &mut Vec<Diagnostic>) {
    let mut dead = false;
    for st in stmts {
        if matches!(&st.stmt, Stmt::Comment { .. }) {
            continue;
        }
        if dead {
            diags.push((st.line, st.column, "unreachable code".into()));
            continue;
        }
        match &st.stmt {
            Stmt::Return(_) | Stmt::Throw(_) | Stmt::Break | Stmt::Continue => dead = true,
            Stmt::If {
                then_block,
                elifs,
                else_block,
                ..
            } => {
                unreachable_in_block(then_block, diags);
                for (_, b) in elifs {
                    unreachable_in_block(b, diags);
                }
                if let Some(b) = else_block {
                    unreachable_in_block(b, diags);
                }
            }
            Stmt::While { body, .. } | Stmt::Loop { body, .. } | Stmt::For { body, .. } => {
                unreachable_in_block(body, diags);
            }
            Stmt::FuncDecl { body, .. } | Stmt::MacroDecl { body, .. } => {
                unreachable_in_block(body, diags);
            }
            Stmt::FriendFuncDecl { body, .. } => {
                if let Some(b) = body {
                    unreachable_in_block(b, diags);
                }
            }
            Stmt::Try {
                body,
                catches,
                else_block,
            } => {
                unreachable_in_block(body, diags);
                for c in catches {
                    unreachable_in_block(&c.body, diags);
                }
                if let Some(b) = else_block {
                    unreachable_in_block(b, diags);
                }
            }
            Stmt::Match {
                cases, else_block, ..
            } => {
                for c in cases {
                    unreachable_in_block(&c.body, diags);
                }
                if let Some(b) = else_block {
                    unreachable_in_block(b, diags);
                }
            }
            Stmt::With { body, .. } | Stmt::Block(body) => unreachable_in_block(body, diags),
            Stmt::StructDecl { methods, .. } => {
                for m in methods {
                    unreachable_in_block(&m.body, diags);
                }
            }
            Stmt::EnumDecl { methods, .. } => {
                for m in methods {
                    unreachable_in_block(&m.body, diags);
                }
            }
            _ => {}
        }
    }
}

fn walk_block_extra(
    stmts: &Block,
    outer: &HashSet<String>,
    types: &HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    let mut scope = outer.clone();
    for st in stmts {
        bind_stmt_names(&st.stmt, &mut scope);
    }
    for st in stmts {
        walk_stmt_extra(st, &scope, types, diags);
    }
}

fn bind_stmt_names(stmt: &Stmt, scope: &mut HashSet<String>) {
    match stmt {
        Stmt::VarDecl { name, .. } => {
            scope.insert(name.clone());
        }
        Stmt::FuncDecl { name, .. }
        | Stmt::FriendFuncDecl { name, .. }
        | Stmt::StructDecl { name, .. }
        | Stmt::EnumDecl { name, .. }
        | Stmt::VariantDecl { name, .. }
        | Stmt::ProtocolDecl { name, .. }
        | Stmt::MacroDecl { name, .. } => {
            scope.insert(name.clone());
        }
        Stmt::Import { path, alias, .. } => {
            let key = alias.as_deref().unwrap_or(path.as_str());
            scope.insert(key.rsplit('.').next().unwrap_or(key).to_string());
        }
        Stmt::Use { items, .. } => {
            for it in items {
                scope.insert(it.alias.as_deref().unwrap_or(&it.name).to_string());
            }
        }
        _ => {}
    }
}

fn walk_stmt_extra(
    st: &LocatedStmt,
    scope: &HashSet<String>,
    types: &HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    match &st.stmt {
        Stmt::VarDecl {
            type_expr,
            type_strong,
            init,
            ..
        } => {
            if *type_strong {
                if let (Some(ty), Some(init)) = (type_expr, init) {
                    check_hard_assign(ty, init, diags);
                }
            }
            if let Some(init) = init {
                walk_expr_extra(init, scope, types, diags);
            }
        }
        Stmt::Assign { value, .. } => walk_expr_extra(value, scope, types, diags),
        Stmt::FuncDecl {
            params,
            body,
            return_type,
            return_strong,
            type_params,
            ..
        } => {
            check_type_params(type_params, types, st.line, st.column, diags);
            let mut inner = scope.clone();
            for p in params {
                inner.insert(p.name.clone());
            }
            unused_in_function(params, body, diags);
            if *return_strong {
                if let Some(rt) = return_type {
                    check_hard_returns(rt, body, diags);
                }
            }
            walk_block_extra(body, &inner, types, diags);
        }
        Stmt::Return(Some(e)) => walk_expr_extra(e, scope, types, diags),
        Stmt::Throw(e) | Stmt::Expr(e) => walk_expr_extra(e, scope, types, diags),
        Stmt::If {
            cond,
            then_block,
            elifs,
            else_block,
        } => {
            walk_expr_extra(cond, scope, types, diags);
            walk_block_extra(then_block, scope, types, diags);
            for (c, b) in elifs {
                walk_expr_extra(c, scope, types, diags);
                walk_block_extra(b, scope, types, diags);
            }
            if let Some(b) = else_block {
                walk_block_extra(b, scope, types, diags);
            }
        }
        Stmt::While { cond, body } => {
            walk_expr_extra(cond, scope, types, diags);
            walk_block_extra(body, scope, types, diags);
        }
        Stmt::Loop { count, body } => {
            if let Some(c) = count {
                walk_expr_extra(c, scope, types, diags);
            }
            walk_block_extra(body, scope, types, diags);
        }
        Stmt::For { body, .. } => walk_block_extra(body, scope, types, diags),
        Stmt::Try {
            body,
            catches,
            else_block,
        } => {
            walk_block_extra(body, scope, types, diags);
            for c in catches {
                walk_block_extra(&c.body, scope, types, diags);
            }
            if let Some(b) = else_block {
                walk_block_extra(b, scope, types, diags);
            }
        }
        Stmt::Match {
            subject,
            cases,
            else_block,
        } => {
            walk_expr_extra(subject, scope, types, diags);
            for c in cases {
                walk_block_extra(&c.body, scope, types, diags);
            }
            if let Some(b) = else_block {
                walk_block_extra(b, scope, types, diags);
            }
        }
        Stmt::With { context, body, .. } => {
            walk_expr_extra(context, scope, types, diags);
            walk_block_extra(body, scope, types, diags);
        }
        Stmt::Block(b) => walk_block_extra(b, scope, types, diags),
        Stmt::StructDecl {
            type_params,
            methods,
            ..
        } => {
            check_type_params(type_params, types, st.line, st.column, diags);
            for m in methods {
                walk_block_extra(&m.body, scope, types, diags);
            }
        }
        Stmt::VariantDecl { type_params, .. } => {
            check_type_params(type_params, types, st.line, st.column, diags);
        }
        Stmt::MacroDecl { body, .. } => walk_block_extra(body, scope, types, diags),
        _ => {}
    }
}

fn check_type_params(
    type_params: &[(String, Option<Expr>)],
    types: &HashSet<String>,
    line: usize,
    col: usize,
    diags: &mut Vec<Diagnostic>,
) {
    for (name, bound) in type_params {
        let Some(bound) = bound else { continue };
        let Some(tn) = type_ann_name(bound) else {
            continue;
        };
        if !types.contains(&tn) {
            diags.push((
                bound.loc.line.max(line),
                bound.loc.column.max(col),
                format!("unknown protocol or type bound `{tn}` on `{name}`"),
            ));
        }
    }
}

fn unused_in_function(params: &[crate::ast::FuncParam], body: &Block, diags: &mut Vec<Diagnostic>) {
    let mut used = HashSet::new();
    walk_block_uses(body, &mut used);
    for p in params {
        if p.name.starts_with('_') || p.name == "self" {
            continue;
        }
        if !used.contains(&p.name) {
            diags.push((
                body.first().map(|s| s.line).unwrap_or(1),
                1,
                format!("unused variable `{0}`", p.name),
            ));
        }
    }
    let mut locals: Vec<(String, usize, usize, bool)> = Vec::new();
    collect_local_lets(body, &mut locals);
    for (name, line, col, exported) in locals {
        if exported || name.starts_with('_') {
            continue;
        }
        if !used.contains(&name) {
            diags.push((line, col, format!("unused variable `{name}`")));
        }
    }
}

fn collect_local_lets(body: &Block, out: &mut Vec<(String, usize, usize, bool)>) {
    for st in body {
        if let Stmt::VarDecl {
            name, visibility, ..
        } = &st.stmt
        {
            out.push((
                name.clone(),
                st.line,
                st.column,
                matches!(visibility, Visibility::Exported),
            ));
        }
    }
}

fn check_hard_assign(ty: &Expr, init: &Expr, diags: &mut Vec<Diagnostic>) {
    let Some(ann) = type_ann_name(ty) else { return };
    let Some(lit) = literal_type_name(init) else {
        return;
    };
    if !types_compatible(&ann, lit) {
        diags.push((
            init.loc.line,
            init.loc.column,
            format!("cannot assign `{lit}` to hard type `{ann}`"),
        ));
    }
}

fn check_hard_returns(ty: &Expr, body: &Block, diags: &mut Vec<Diagnostic>) {
    let Some(ann) = type_ann_name(ty) else { return };
    for st in body {
        if let Stmt::Return(Some(e)) = &st.stmt {
            if let Some(lit) = literal_type_name(e) {
                if !types_compatible(&ann, lit) {
                    diags.push((
                        e.loc.line,
                        e.loc.column,
                        format!("cannot return `{lit}` from hard type `{ann}`"),
                    ));
                }
            }
        }
    }
}

fn type_ann_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Var(n) => Some(n.clone()),
        _ => None,
    }
}

fn literal_type_name(expr: &Expr) -> Option<&'static str> {
    match &expr.kind {
        ExprKind::Number(_) => Some("num"),
        ExprKind::String(_) | ExprKind::FString(_) => Some("text"),
        ExprKind::Bool(_) => Some("bool"),
        ExprKind::None => Some("nonetype"),
        ExprKind::Bytes(_) => Some("bytes"),
        ExprKind::List(_) => Some("list"),
        ExprKind::Dict(_) => Some("dict"),
        ExprKind::Set(_) => Some("set"),
        ExprKind::Tuple(_) => Some("tuple"),
        _ => None,
    }
}

fn types_compatible(ann: &str, lit: &str) -> bool {
    if ann == lit {
        return true;
    }
    matches!((ann, lit), ("none", "nonetype") | ("nonetype", "none"))
}

fn walk_expr_extra(
    expr: &Expr,
    scope: &HashSet<String>,
    types: &HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    if let ExprKind::Go { operand } = &expr.kind {
        check_go_shared(operand, scope, diags);
        walk_expr_extra(operand, scope, types, diags);
        return;
    }
    match &expr.kind {
        ExprKind::DoFunc { body, params, .. } => {
            let mut inner = scope.clone();
            for p in params {
                inner.insert(p.name.clone());
            }
            walk_block_extra(body, &inner, types, diags);
        }
        ExprKind::Unary { operand, .. }
        | ExprKind::Handle { operand }
        | ExprKind::Snap { operand }
        | ExprKind::Await { operand } => walk_expr_extra(operand, scope, types, diags),
        ExprKind::Binary { left, right, .. } | ExprKind::Pipeline { left, right, .. } => {
            walk_expr_extra(left, scope, types, diags);
            walk_expr_extra(right, scope, types, diags);
        }
        ExprKind::Call { callee, args } => {
            walk_expr_extra(callee, scope, types, diags);
            for a in args {
                walk_expr_extra(&a.value, scope, types, diags);
            }
        }
        ExprKind::Member { object, .. } => walk_expr_extra(object, scope, types, diags),
        ExprKind::List(xs)
        | ExprKind::Tuple(xs)
        | ExprKind::Set(xs)
        | ExprKind::ParBlock { exprs: xs } => {
            for e in xs {
                walk_expr_extra(e, scope, types, diags);
            }
        }
        ExprKind::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            walk_expr_extra(cond, scope, types, diags);
            walk_expr_extra(then_expr, scope, types, diags);
            walk_expr_extra(else_expr, scope, types, diags);
        }
        _ => {}
    }
}

fn check_go_shared(operand: &Expr, outer: &HashSet<String>, diags: &mut Vec<Diagnostic>) {
    let mut locals = HashSet::new();
    let mut assigned = Vec::new();
    collect_go_assigns(operand, &mut locals, &mut assigned);
    for (line, col, name) in assigned {
        if outer.contains(&name) && !locals.contains(&name) {
            diags.push((
                line,
                col,
                format!("`go` captures and assigns shared `{name}`; use Channel, Mutex, or Atomic"),
            ));
        }
    }
}

fn collect_go_assigns(
    expr: &Expr,
    locals: &mut HashSet<String>,
    assigned: &mut Vec<(usize, usize, String)>,
) {
    match &expr.kind {
        ExprKind::DoFunc { body, params, .. } => {
            for p in params {
                locals.insert(p.name.clone());
            }
            collect_go_assigns_block(body, locals, assigned);
        }
        ExprKind::NamedAssign { name, value } => {
            assigned.push((expr.loc.line, expr.loc.column, name.clone()));
            collect_go_assigns(value, locals, assigned);
        }
        ExprKind::Call { callee, args } => {
            collect_go_assigns(callee, locals, assigned);
            for a in args {
                collect_go_assigns(&a.value, locals, assigned);
            }
        }
        ExprKind::Unary { operand, .. }
        | ExprKind::Handle { operand }
        | ExprKind::Go { operand }
        | ExprKind::Snap { operand }
        | ExprKind::Await { operand } => collect_go_assigns(operand, locals, assigned),
        ExprKind::Binary { left, right, .. } | ExprKind::Pipeline { left, right, .. } => {
            collect_go_assigns(left, locals, assigned);
            collect_go_assigns(right, locals, assigned);
        }
        _ => {}
    }
}

fn collect_go_assigns_block(
    body: &Block,
    locals: &mut HashSet<String>,
    assigned: &mut Vec<(usize, usize, String)>,
) {
    for st in body {
        match &st.stmt {
            Stmt::VarDecl { name, .. } => {
                locals.insert(name.clone());
            }
            Stmt::Assign {
                target: LValue::Name(n),
                ..
            } => {
                assigned.push((st.line, st.column, n.clone()));
            }
            Stmt::Block(b) => collect_go_assigns_block(b, locals, assigned),
            Stmt::If {
                then_block,
                elifs,
                else_block,
                ..
            } => {
                collect_go_assigns_block(then_block, locals, assigned);
                for (_, b) in elifs {
                    collect_go_assigns_block(b, locals, assigned);
                }
                if let Some(b) = else_block {
                    collect_go_assigns_block(b, locals, assigned);
                }
            }
            Stmt::While { body, .. } | Stmt::Loop { body, .. } | Stmt::For { body, .. } => {
                collect_go_assigns_block(body, locals, assigned);
            }
            Stmt::Expr(e) => collect_go_assigns(e, locals, assigned),
            _ => {}
        }
    }
}

fn cross_module_exports(
    program: &Program,
    file: &str,
    docs: &HashMap<String, String>,
    diags: &mut Vec<Diagnostic>,
) {
    let uri = if file.starts_with("file:") {
        file.to_string()
    } else {
        crate::lsp::workspace::path_to_uri(std::path::Path::new(file))
    };
    for st in &program.stmts {
        let Stmt::Use { module, items } = &st.stmt else {
            continue;
        };
        let spec = match module {
            ModuleRef::FilePath { path, .. } => path.clone(),
            ModuleRef::Qualified(parts) => {
                if parts.first().map(String::as_str) == Some("std") {
                    continue;
                }
                parts.join(".")
            }
        };
        if crate::lsp::workspace::is_std_spec(&spec) {
            continue;
        }
        let Some((_, idx)) = crate::lsp::workspace::load_index(&uri, &spec, docs) else {
            continue;
        };
        let exports: HashSet<String> = idx.exports().into_iter().map(|s| s.name.clone()).collect();
        for it in items {
            if !exports.contains(&it.name) {
                diags.push((
                    st.line,
                    st.column,
                    format!("unknown export `{}` from `{spec}`", it.name),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(src: &str) -> Vec<String> {
        analyze_source(src).into_iter().map(|(_, _, m)| m).collect()
    }

    #[test]
    fn hard_type_mismatch() {
        let d = msgs("let x :: num = \"hi\"\n");
        assert!(d.iter().any(|m| m.contains("cannot assign")), "{d:?}");
    }

    #[test]
    fn unused_import_reported() {
        let d = msgs("use std.math.{ sin }\nlet x = 1\nprint(x)\n");
        assert!(d.iter().any(|m| m.contains("unused import `sin`")), "{d:?}");
    }

    #[test]
    fn unused_func_local() {
        let d = msgs("func f() {\n  let z = 1\n  2\n}\nf()\n");
        assert!(d.iter().any(|m| m.contains("unused variable `z`")), "{d:?}");
    }

    #[test]
    fn unreachable_after_return() {
        let d = msgs("func f() {\n  return 1\n  2\n}\n");
        assert!(d.iter().any(|m| m.contains("unreachable")), "{d:?}");
    }

    #[test]
    fn go_shared_assign() {
        let d = msgs("var x = 1\ngo do { x = x + 1 }\n");
        assert!(
            d.iter()
                .any(|m| m.contains("shared `x`") && m.contains("Mutex")),
            "{d:?}"
        );
    }

    #[test]
    fn unknown_generic_bound() {
        let d = msgs("func f[T: NoSuchProto](x) { x }\n");
        assert!(d.iter().any(|m| m.contains("unknown protocol")), "{d:?}");
    }
}
