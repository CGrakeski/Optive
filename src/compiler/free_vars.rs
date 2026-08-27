//! 闭包捕获的自由变量分析。

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Block, CatchPattern, DelTarget, DestructElem, DestructPattern, Expr, ExprKind, ForItem,
    FuncParam, LValue, Pattern, PatternElem, Program, Stmt,
};

#[must_use]
pub fn free_vars_in_block(body: &Block, params: &HashSet<String>) -> Vec<String> {
    let mut locals = params.clone();
    let mut free = HashSet::new();
    for located in body {
        collect_stmt(&located.stmt, &mut locals, &mut free);
    }
    let mut names: Vec<String> = free.into_iter().collect();
    names.sort();
    names
}

#[must_use]
pub fn free_vars_in_expr(expr: &Expr, params: &HashSet<String>) -> Vec<String> {
    let locals: HashMap<String, ()> = params.iter().map(|n| (n.clone(), ())).collect();
    let mut free = HashSet::new();
    collect_expr(expr, &locals, &mut free);
    let mut names: Vec<String> = free.into_iter().collect();
    names.sort();
    names
}

fn collect_block(body: &Block, locals: &mut HashMap<String, ()>, free: &mut HashSet<String>) {
    let mut scoped = locals.clone();
    for located in body {
        collect_stmt_scoped(&located.stmt, &mut scoped, free);
    }
}

fn collect_stmt(stmt: &Stmt, locals: &mut HashSet<String>, free: &mut HashSet<String>) {
    let mut map: HashMap<String, ()> = locals.iter().map(|n| (n.clone(), ())).collect();
    collect_stmt_scoped(stmt, &mut map, free);
    *locals = map.into_keys().collect();
}

fn collect_stmt_scoped(stmt: &Stmt, locals: &mut HashMap<String, ()>, free: &mut HashSet<String>) {
    match stmt {
        Stmt::VarDecl {
            name,
            init,
            type_expr,
            ..
        } => {
            if let Some(ty) = type_expr {
                collect_expr(ty, locals, free);
            }
            if let Some(e) = init {
                collect_expr(e, locals, free);
            }
            locals.insert(name.clone(), ());
        }
        Stmt::DestructDecl { pattern, init, .. } => {
            collect_expr(init, locals, free);
            bind_destruct_pattern(pattern, locals);
        }
        Stmt::Assign { target, value } => {
            collect_lvalue(target, locals, free);
            collect_expr(value, locals, free);
            if let LValue::Name(n) = target {
                locals.insert(n.clone(), ());
            }
        }
        Stmt::DestructAssign { pattern, value } => {
            collect_expr(value, locals, free);
            bind_destruct_pattern(pattern, locals);
        }
        Stmt::ProtocolDecl { .. } => {}
        Stmt::FuncDecl {
            params,
            body,
            return_type,
            ..
        } => {
            let mut scoped = locals.clone();
            for p in params {
                if let Some(ty) = &p.type_expr {
                    collect_expr(ty, locals, free);
                }
                if let Some(d) = &p.default_expr {
                    collect_expr(d, locals, free);
                }
                scoped.insert(p.name.clone(), ());
            }
            if let Some(rt) = return_type {
                collect_expr(rt, locals, free);
            }
            collect_block(body, &mut scoped, free);
        }
        Stmt::MacroDecl { params, body, .. } => {
            let mut scoped = locals.clone();
            for p in params {
                scoped.insert(p.name.clone(), ());
            }
            collect_block(body, &mut scoped, free);
        }
        Stmt::FriendFuncDecl { params, body, .. } => {
            if let Some(b) = body {
                let mut scoped = locals.clone();
                if let Some(params) = params {
                    for p in params {
                        scoped.insert(p.name.clone(), ());
                    }
                }
                collect_block(b, &mut scoped, free);
            }
        }
        Stmt::Return(e) | Stmt::Yield(e) => {
            if let Some(e) = e {
                collect_expr(e, locals, free);
            }
        }
        Stmt::YieldFrom(e) => collect_expr(e, locals, free),
        Stmt::Throw(e) => collect_expr(e, locals, free),
        Stmt::Expr(e) => collect_expr(e, locals, free),
        Stmt::If {
            cond,
            then_block,
            elifs,
            else_block,
        } => {
            collect_expr(cond, locals, free);
            collect_block(then_block, locals, free);
            for (c, b) in elifs {
                collect_expr(c, locals, free);
                collect_block(b, locals, free);
            }
            if let Some(b) = else_block {
                collect_block(b, locals, free);
            }
        }
        Stmt::While { cond, body } => {
            collect_expr(cond, locals, free);
            collect_block(body, locals, free);
        }
        Stmt::Loop { count, body } => {
            if let Some(c) = count {
                collect_expr(c, locals, free);
            }
            collect_block(body, locals, free);
        }
        Stmt::For { items, body } => {
            let mut scoped = locals.clone();
            for item in items {
                collect_expr(&item.iterable, locals, free);
                scoped.insert(item.name.clone(), ());
            }
            for located in body {
                collect_stmt_scoped(&located.stmt, &mut scoped, free);
            }
        }
        Stmt::Break | Stmt::Continue => {}
        Stmt::Comment { .. } => {}
        Stmt::Try {
            body,
            catches,
            else_block,
        } => {
            collect_block(body, locals, free);
            for c in catches {
                let mut scoped = locals.clone();
                if let CatchPattern::Bind { name, .. } = &c.pattern {
                    scoped.insert(name.clone(), ());
                }
                collect_block(&c.body, &mut scoped, free);
            }
            if let Some(b) = else_block {
                collect_block(b, locals, free);
            }
        }
        Stmt::Match {
            subject,
            cases,
            else_block,
        } => {
            collect_expr(subject, locals, free);
            for case in cases {
                let mut scoped = locals.clone();
                bind_pattern(&case.pattern, &mut scoped, free);
                collect_block(&case.body, &mut scoped, free);
            }
            if let Some(b) = else_block {
                let mut scoped = locals.clone();
                collect_block(b, &mut scoped, free);
            }
        }
        Stmt::Del(target) => collect_del(target, locals, free),
        Stmt::With {
            alias,
            context,
            body,
        } => {
            collect_expr(context, locals, free);
            let mut scoped = locals.clone();
            if let Some(a) = alias {
                scoped.insert(a.clone(), ());
            }
            collect_block(body, &mut scoped, free);
        }
        Stmt::Import { .. } | Stmt::Use { .. } => {}
        Stmt::StructDecl { .. } => {}
        Stmt::Block(body) => collect_block(body, locals, free),
        Stmt::EnumDecl { .. } | Stmt::VariantDecl { .. } => {}
    }
}

fn bind_pattern(pat: &Pattern, locals: &mut HashMap<String, ()>, free: &mut HashSet<String>) {
    match pat {
        Pattern::Bind(n) => {
            locals.insert(n.clone(), ());
        }
        Pattern::Value(e) => collect_expr(e, locals, free),
        Pattern::List(elems) | Pattern::Tuple(elems) => {
            for el in elems {
                match el {
                    PatternElem::Bind(n) => {
                        locals.insert(n.clone(), ());
                    }
                    PatternElem::Nested(p) => bind_pattern(p, locals, free),
                    PatternElem::Value(e) => collect_expr(e, locals, free),
                }
            }
        }
        Pattern::Struct { fields, .. } => {
            for f in fields {
                locals.insert(f.clone(), ());
            }
        }
        Pattern::Or(alts) => {
            for p in alts {
                bind_pattern(p, locals, free);
            }
        }
        Pattern::Call { args, .. } => {
            for arg in args {
                bind_pattern(arg, locals, free);
            }
        }
    }
}

fn bind_destruct_pattern(pat: &DestructPattern, locals: &mut HashMap<String, ()>) {
    match pat {
        DestructPattern::Name(n) => {
            locals.insert(n.clone(), ());
        }
        DestructPattern::Discard => {}
        DestructPattern::Tuple(elems) | DestructPattern::List(elems) => {
            for el in elems {
                match el {
                    DestructElem::Pat(p) => bind_destruct_pattern(p, locals),
                    DestructElem::Rest(n) => {
                        locals.insert(n.clone(), ());
                    }
                    DestructElem::RestDiscard => {}
                }
            }
        }
    }
}

fn collect_del(target: &DelTarget, locals: &HashMap<String, ()>, free: &mut HashSet<String>) {
    match target {
        DelTarget::Name(n) => note_var(n, locals, free),
        DelTarget::Index { object, index } => {
            collect_expr(object, locals, free);
            collect_expr(index, locals, free);
        }
        DelTarget::Member { object, .. } => collect_expr(object, locals, free),
    }
}

fn collect_lvalue(lv: &LValue, locals: &HashMap<String, ()>, free: &mut HashSet<String>) {
    match lv {
        LValue::Name(n) => note_var(n, locals, free),
        LValue::Member { object, .. } | LValue::Index { object, .. } => {
            collect_expr(object, locals, free);
        }
        LValue::Slice { object, .. } => collect_expr(object, locals, free),
    }
}

fn collect_expr(expr: &Expr, locals: &HashMap<String, ()>, free: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Var(n) => note_var(n, locals, free),
        ExprKind::Number(_)
        | ExprKind::String(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Placeholder => {}
        ExprKind::FString(parts) => {
            for part in parts {
                if let crate::ast::FStringPart::Expr(e) = part {
                    collect_expr(e, locals, free);
                }
            }
        }
        ExprKind::Unary { operand, .. } => collect_expr(operand, locals, free),
        ExprKind::Binary { left, right, .. } => {
            collect_expr(left, locals, free);
            collect_expr(right, locals, free);
        }
        ExprKind::Call { callee, args } => {
            collect_expr(callee, locals, free);
            for a in args {
                collect_expr(&a.value, locals, free);
            }
        }
        ExprKind::Member { object, .. } => collect_expr(object, locals, free),
        ExprKind::Index { object, index } => {
            collect_expr(object, locals, free);
            collect_expr(index, locals, free);
        }
        ExprKind::List(elems) => {
            for e in elems {
                collect_expr(e, locals, free);
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
            for item in items {
                collect_expr(&item.iterable, locals, free);
            }
            let mut scoped = locals.clone();
            for item in items {
                scoped.insert(item.name.clone(), ());
            }
            collect_expr(elem, &scoped, free);
            for guard in guards {
                collect_expr(guard, &scoped, free);
            }
        }
        ExprKind::DictComp {
            key,
            value,
            items,
            guards,
        } => {
            for item in items {
                collect_expr(&item.iterable, locals, free);
            }
            let mut scoped = locals.clone();
            for item in items {
                scoped.insert(item.name.clone(), ());
            }
            collect_expr(key, &scoped, free);
            collect_expr(value, &scoped, free);
            for guard in guards {
                collect_expr(guard, &scoped, free);
            }
        }
        ExprKind::Dict(entries) => {
            for (k, v) in entries {
                collect_expr(k, locals, free);
                collect_expr(v, locals, free);
            }
        }
        ExprKind::Set(elems) | ExprKind::Tuple(elems) => {
            for e in elems {
                collect_expr(e, locals, free);
            }
        }
        ExprKind::Bytes(_) => {}
        ExprKind::DoFunc {
            params,
            body,
            return_type,
            ..
        } => {
            let mut scoped = locals.clone();
            for p in params {
                if let Some(ty) = &p.type_expr {
                    collect_expr(ty, locals, free);
                }
                if let Some(d) = &p.default_expr {
                    collect_expr(d, locals, free);
                }
                scoped.insert(p.name.clone(), ());
            }
            if let Some(rt) = return_type {
                collect_expr(rt, locals, free);
            }
            collect_block(body, &mut scoped, free);
        }
        ExprKind::Pipeline {
            left,
            right,
            pipe_name,
        } => {
            collect_expr(left, locals, free);
            let mut scoped = locals.clone();
            scoped.insert(pipe_name.clone(), ());
            collect_expr(right, &scoped, free);
        }
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => {
            collect_expr(object, locals, free);
            if let Some(e) = start {
                collect_expr(e, locals, free);
            }
            if let Some(e) = end {
                collect_expr(e, locals, free);
            }
            if let Some(e) = step {
                collect_expr(e, locals, free);
            }
        }
        ExprKind::TypeConvert { type_expr, value } => {
            collect_expr(type_expr, locals, free);
            collect_expr(value, locals, free);
        }
        ExprKind::MacroCall { callee, args } => {
            collect_expr(callee, locals, free);
            for a in args {
                if a.node.kind == crate::runtime_ast::AstNodeKind::VarRef {
                    note_var(&a.node.text, locals, free);
                }
            }
        }
        ExprKind::Quote { bindings, body, .. } => {
            for b in bindings {
                collect_expr(b, locals, free);
            }
            let mut scoped = locals.clone();
            collect_block(body, &mut scoped, free);
        }
        ExprKind::Match {
            subject,
            cases,
            else_block,
        } => {
            collect_expr(subject, locals, free);
            for case in cases {
                let mut scoped = locals.clone();
                bind_pattern(&case.pattern, &mut scoped, free);
                collect_block(&case.body, &mut scoped, free);
            }
            if let Some(b) = else_block {
                let mut scoped = locals.clone();
                collect_block(b, &mut scoped, free);
            }
        }
        ExprKind::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_expr(cond, locals, free);
            collect_expr(then_expr, locals, free);
            collect_expr(else_expr, locals, free);
        }
        ExprKind::Handle { operand } => collect_expr(operand, locals, free),
        ExprKind::Go { operand } | ExprKind::Await { operand } | ExprKind::Snap { operand } => {
            collect_expr(operand, locals, free);
        }
        ExprKind::ParFor { items, body } => {
            for item in items {
                collect_expr(&item.iterable, locals, free);
            }
            let mut scoped = locals.clone();
            for item in items {
                scoped.insert(item.name.clone(), ());
            }
            collect_block(body, &mut scoped, free);
        }
        ExprKind::ParBlock { exprs } => {
            for e in exprs {
                collect_expr(e, locals, free);
            }
        }
        ExprKind::Suspend => {}
        ExprKind::Select { cases, else_block } => {
            for case in cases {
                collect_expr(&case.event, locals, free);
                let mut scoped = locals.clone();
                if let Some(name) = &case.bind {
                    scoped.insert(name.clone(), ());
                }
                collect_block(&case.body, &mut scoped, free);
            }
            if let Some(b) = else_block {
                let mut scoped = locals.clone();
                collect_block(b, &mut scoped, free);
            }
        }
        ExprKind::NamedAssign { name, value } => {
            let mut scoped = locals.clone();
            scoped.insert(name.clone(), ());
            collect_expr(value, &scoped, free);
        }
    }
}

fn note_var(name: &str, locals: &HashMap<String, ()>, free: &mut HashSet<String>) {
    if !locals.contains_key(name) {
        free.insert(name.to_string());
    }
}

/// 顶层 `let`/`var`（非 `const`）中，从未被本单元任意函数/`do` 以自由变量引用的名字。
/// 这些名字可编成脚本快局部；被引用的仍走全局，供闭包与模块导出看见中间状态。
#[must_use]
pub fn unescaped_script_var_names(program: &Program) -> Vec<String> {
    let mut declared = Vec::new();
    let mut seen = HashSet::new();
    for located in &program.stmts {
        collect_top_level_var_names(&located.stmt, &mut declared, &mut seen);
    }
    if declared.is_empty() {
        return declared;
    }
    let escaped = nested_callable_free_names(program);
    declared.retain(|n| !escaped.contains(n));
    declared
}

fn collect_top_level_var_names(stmt: &Stmt, out: &mut Vec<String>, seen: &mut HashSet<String>) {
    if let Stmt::VarDecl {
        is_const: false,
        name,
        ..
    } = stmt
    {
        if seen.insert(name.clone()) {
            out.push(name.clone());
        }
    }
}

fn nested_callable_free_names(program: &Program) -> HashSet<String> {
    let mut free = HashSet::new();
    for located in &program.stmts {
        walk_stmt_callables(&located.stmt, &mut free);
    }
    free
}

fn add_func_frees(params: &[FuncParam], body: &Block, free: &mut HashSet<String>) {
    let param_names: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
    for p in params {
        if let Some(ty) = &p.type_expr {
            walk_expr_callables(ty, free);
            for n in free_vars_in_expr(ty, &HashSet::new()) {
                free.insert(n);
            }
        }
        if let Some(d) = &p.default_expr {
            walk_expr_callables(d, free);
            for n in free_vars_in_expr(d, &param_names) {
                free.insert(n);
            }
        }
    }
    for n in free_vars_in_block(body, &param_names) {
        free.insert(n);
    }
}

fn walk_stmt_callables(stmt: &Stmt, free: &mut HashSet<String>) {
    match stmt {
        Stmt::VarDecl {
            init, type_expr, ..
        } => {
            if let Some(ty) = type_expr {
                walk_expr_callables(ty, free);
            }
            if let Some(e) = init {
                walk_expr_callables(e, free);
            }
        }
        Stmt::DestructDecl { init, .. } => walk_expr_callables(init, free),
        Stmt::Assign { target, value } => {
            walk_lvalue_callables(target, free);
            walk_expr_callables(value, free);
        }
        Stmt::DestructAssign { value, .. } => walk_expr_callables(value, free),
        Stmt::FuncDecl {
            params,
            body,
            return_type,
            decorators,
            ..
        } => {
            for d in decorators {
                walk_expr_callables(d, free);
            }
            add_func_frees(params, body, free);
            for s in body {
                walk_stmt_callables(&s.stmt, free);
            }
            if let Some(rt) = return_type {
                walk_expr_callables(rt, free);
            }
        }
        Stmt::MacroDecl { params, body, .. } => {
            let param_names: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
            for n in free_vars_in_block(body, &param_names) {
                free.insert(n);
            }
            walk_block_callables(body, free);
        }
        Stmt::FriendFuncDecl {
            params,
            body,
            return_type,
            ..
        } => {
            if let Some(rt) = return_type {
                walk_expr_callables(rt, free);
            }
            if let (Some(params), Some(body)) = (params, body) {
                add_func_frees(params, body, free);
                walk_block_callables(body, free);
            }
        }
        Stmt::Del(target) => match target {
            DelTarget::Name(_) => {}
            DelTarget::Member { object, .. } => walk_expr_callables(object, free),
            DelTarget::Index { object, index } => {
                walk_expr_callables(object, free);
                walk_expr_callables(index, free);
            }
        },
        Stmt::Return(e) | Stmt::Yield(e) => {
            if let Some(e) = e {
                walk_expr_callables(e, free);
            }
        }
        Stmt::YieldFrom(e) | Stmt::Throw(e) | Stmt::Expr(e) => walk_expr_callables(e, free),
        Stmt::If {
            cond,
            then_block,
            elifs,
            else_block,
        } => {
            walk_expr_callables(cond, free);
            walk_block_callables(then_block, free);
            for (c, b) in elifs {
                walk_expr_callables(c, free);
                walk_block_callables(b, free);
            }
            if let Some(b) = else_block {
                walk_block_callables(b, free);
            }
        }
        Stmt::While { cond, body } => {
            walk_expr_callables(cond, free);
            walk_block_callables(body, free);
        }
        Stmt::Loop { count, body } => {
            if let Some(c) = count {
                walk_expr_callables(c, free);
            }
            walk_block_callables(body, free);
        }
        Stmt::For { items, body } => {
            for item in items {
                walk_expr_callables(&item.iterable, free);
            }
            walk_block_callables(body, free);
        }
        Stmt::Try {
            body,
            catches,
            else_block,
        } => {
            walk_block_callables(body, free);
            for c in catches {
                walk_block_callables(&c.body, free);
            }
            if let Some(b) = else_block {
                walk_block_callables(b, free);
            }
        }
        Stmt::Match {
            subject,
            cases,
            else_block,
        } => {
            walk_expr_callables(subject, free);
            for case in cases {
                walk_block_callables(&case.body, free);
            }
            if let Some(b) = else_block {
                walk_block_callables(b, free);
            }
        }
        Stmt::With { context, body, .. } => {
            walk_expr_callables(context, free);
            walk_block_callables(body, free);
        }
        Stmt::StructDecl {
            methods, layout, ..
        } => {
            for m in methods {
                add_func_frees(&m.params, &m.body, free);
                walk_block_callables(&m.body, free);
            }
            if let Some(e) = layout {
                walk_expr_callables(e, free);
            }
        }
        Stmt::EnumDecl { methods, .. } => {
            for m in methods {
                add_func_frees(&m.params, &m.body, free);
                walk_block_callables(&m.body, free);
            }
        }
        Stmt::Block(body) => walk_block_callables(body, free),
        Stmt::ProtocolDecl { .. }
        | Stmt::Break
        | Stmt::Continue
        | Stmt::Import { .. }
        | Stmt::Use { .. }
        | Stmt::VariantDecl { .. }
        | Stmt::Comment { .. } => {}
    }
}

fn walk_block_callables(body: &Block, free: &mut HashSet<String>) {
    for s in body {
        walk_stmt_callables(&s.stmt, free);
    }
}

fn walk_lvalue_callables(lv: &LValue, free: &mut HashSet<String>) {
    match lv {
        LValue::Name(_) => {}
        LValue::Member { object, .. } => walk_expr_callables(object, free),
        LValue::Index { object, index } => {
            walk_expr_callables(object, free);
            walk_expr_callables(index, free);
        }
        LValue::Slice {
            object,
            start,
            end,
            step,
        } => {
            walk_expr_callables(object, free);
            if let Some(e) = start {
                walk_expr_callables(e, free);
            }
            if let Some(e) = end {
                walk_expr_callables(e, free);
            }
            if let Some(e) = step {
                walk_expr_callables(e, free);
            }
        }
    }
}

fn walk_for_items_callables(items: &[ForItem], free: &mut HashSet<String>) {
    for item in items {
        walk_expr_callables(&item.iterable, free);
    }
}

fn walk_comp_callables(
    items: &[ForItem],
    guards: &[Expr],
    extras: &[&Expr],
    free: &mut HashSet<String>,
) {
    walk_for_items_callables(items, free);
    for g in guards {
        walk_expr_callables(g, free);
    }
    for e in extras {
        walk_expr_callables(e, free);
    }
}

fn walk_expr_callables(expr: &Expr, free: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::DoFunc {
            params,
            body,
            return_type,
            ..
        } => {
            add_func_frees(params, body, free);
            walk_block_callables(body, free);
            if let Some(rt) = return_type {
                walk_expr_callables(rt, free);
            }
        }
        ExprKind::ParFor { items, body } => {
            let bound: HashSet<String> = items.iter().map(|i| i.name.clone()).collect();
            walk_for_items_callables(items, free);
            for n in free_vars_in_block(body, &bound) {
                free.insert(n);
            }
            walk_block_callables(body, free);
        }
        ExprKind::GeneratorExp {
            elem,
            items,
            guards,
        } => {
            let bound: HashSet<String> = items.iter().map(|i| i.name.clone()).collect();
            walk_for_items_callables(items, free);
            for n in free_vars_in_expr(elem, &bound) {
                free.insert(n);
            }
            for g in guards {
                for n in free_vars_in_expr(g, &bound) {
                    free.insert(n);
                }
                walk_expr_callables(g, free);
            }
            walk_expr_callables(elem, free);
        }
        ExprKind::Number(_)
        | ExprKind::String(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Var(_)
        | ExprKind::Placeholder
        | ExprKind::Bytes(_)
        | ExprKind::Suspend => {}
        ExprKind::FString(parts) => {
            for part in parts {
                if let crate::ast::FStringPart::Expr(e) = part {
                    walk_expr_callables(e, free);
                }
            }
        }
        ExprKind::Unary { operand, .. }
        | ExprKind::Member {
            object: operand, ..
        }
        | ExprKind::Handle { operand }
        | ExprKind::Go { operand }
        | ExprKind::Await { operand }
        | ExprKind::Snap { operand } => walk_expr_callables(operand, free),
        ExprKind::Binary { left, right, .. } | ExprKind::Pipeline { left, right, .. } => {
            walk_expr_callables(left, free);
            walk_expr_callables(right, free);
        }
        ExprKind::Call { callee, args } => {
            walk_expr_callables(callee, free);
            for a in args {
                walk_expr_callables(&a.value, free);
            }
        }
        ExprKind::MacroCall { callee, .. } => walk_expr_callables(callee, free),
        ExprKind::Index { object, index } => {
            walk_expr_callables(object, free);
            walk_expr_callables(index, free);
        }
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => {
            walk_expr_callables(object, free);
            if let Some(e) = start {
                walk_expr_callables(e, free);
            }
            if let Some(e) = end {
                walk_expr_callables(e, free);
            }
            if let Some(e) = step {
                walk_expr_callables(e, free);
            }
        }
        ExprKind::TypeConvert { type_expr, value } => {
            walk_expr_callables(type_expr, free);
            walk_expr_callables(value, free);
        }
        ExprKind::List(elems) | ExprKind::Set(elems) | ExprKind::Tuple(elems) => {
            for e in elems {
                walk_expr_callables(e, free);
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
        } => walk_comp_callables(items, guards, &[elem.as_ref()], free),
        ExprKind::DictComp {
            key,
            value,
            items,
            guards,
        } => walk_comp_callables(items, guards, &[key.as_ref(), value.as_ref()], free),
        ExprKind::Dict(entries) => {
            for (k, v) in entries {
                walk_expr_callables(k, free);
                walk_expr_callables(v, free);
            }
        }
        ExprKind::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            walk_expr_callables(cond, free);
            walk_expr_callables(then_expr, free);
            walk_expr_callables(else_expr, free);
        }
        ExprKind::ParBlock { exprs } => {
            for e in exprs {
                walk_expr_callables(e, free);
            }
        }
        ExprKind::Select { cases, else_block } => {
            for case in cases {
                walk_expr_callables(&case.event, free);
                walk_block_callables(&case.body, free);
            }
            if let Some(b) = else_block {
                walk_block_callables(b, free);
            }
        }
        ExprKind::NamedAssign { value, .. } => walk_expr_callables(value, free),
        ExprKind::Quote { bindings, body, .. } => {
            for b in bindings {
                walk_expr_callables(b, free);
            }
            walk_block_callables(body, free);
        }
        ExprKind::Match {
            subject,
            cases,
            else_block,
        } => {
            walk_expr_callables(subject, free);
            for case in cases {
                walk_block_callables(&case.body, free);
            }
            if let Some(b) = else_block {
                walk_block_callables(b, free);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn unescaped(src: &str) -> Vec<String> {
        let program = Parser::parse(src).expect("parse");
        unescaped_script_var_names(&program)
    }

    #[test]
    fn arith_loop_counter_is_unescaped() {
        assert_eq!(
            unescaped("let sum = 0\nloop (10) { sum = sum + 1 }\nsum\n"),
            vec!["sum".to_string()]
        );
    }

    #[test]
    fn nested_func_escapes_top_level_name() {
        assert_eq!(
            unescaped("let n = 1\nfunc f() { return n }\nn = 2\nf()\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn module_internal_prefix_is_escaped() {
        let src = include_str!("../../tests/import_fixtures/module_internal_global.tive");
        let names = unescaped(src);
        assert!(
            !names.iter().any(|s| s == "PREFIX"),
            "PREFIX must stay global, got {names:?}"
        );
        let prog = crate::compile(src).expect("compile");
        assert!(
            prog.global_names.iter().any(|n| n == "PREFIX"),
            "PREFIX missing from global_names: {:?}",
            prog.global_names
        );
        let has_store_global = prog
            .code
            .iter()
            .any(|ins| matches!(ins, crate::opcode::Instruction::StoreGlobal(_)));
        assert!(
            has_store_global,
            "script should StoreGlobal PREFIX, code={:?}",
            prog.code
        );
    }

    #[test]
    fn go_do_escapes_captured_name() {
        let names = unescaped("let n = 1\ngo do { n + 1 }\n");
        assert!(
            !names.iter().any(|s| s == "n"),
            "n must stay global for go-do, got {names:?}"
        );
    }

    #[test]
    fn sibling_let_not_escaped_by_unrelated_func() {
        assert_eq!(
            unescaped("let sum = 0\nfunc id(x) { return x }\nsum = sum + 1\nsum\n"),
            vec!["sum".to_string()]
        );
    }

    #[test]
    fn go_do_assignment_escapes_progressed() {
        let names = unescaped(
            r"
var progressed = 0
go do {
    var i = 0
    while (i < 50) {
        progressed = progressed + 1
        i = i + 1
        suspend
    }
    return progressed
}
",
        );
        assert!(
            !names.iter().any(|s| s == "progressed"),
            "progressed must stay global for go-do, got {names:?}"
        );
    }
}
