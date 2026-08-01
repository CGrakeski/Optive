//! 闭包捕获的自由变量分析。

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Block, CatchPattern, DelTarget, DestructElem, DestructPattern, Expr, ExprKind, LValue, Pattern,
    PatternElem, Stmt,
};

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
        Stmt::VarDecl { name, init, .. } => {
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
        Stmt::FuncDecl { params, body, .. } => {
            let mut scoped = locals.clone();
            for p in params {
                scoped.insert(p.name.clone(), ());
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
        Stmt::With { alias, context, body } => {
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
        Pattern::List(elems) => {
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
        ExprKind::Number(_) | ExprKind::String(_) | ExprKind::Bool(_) | ExprKind::None | ExprKind::Placeholder => {}
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
        ExprKind::ListComp { elem, items, guards }
        | ExprKind::SetComp { elem, items, guards }
        | ExprKind::GeneratorExp { elem, items, guards } => {
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
        ExprKind::DoFunc { params, body, .. } => {
            let mut scoped = locals.clone();
            for p in params {
                scoped.insert(p.name.clone(), ());
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
        ExprKind::Slice { object, start, end, step } => {
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
        ExprKind::Quote {
            bindings,
            body,
            ..
        } => {
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
        ExprKind::Go { operand } | ExprKind::Await { operand } => {
            collect_expr(operand, locals, free)
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
