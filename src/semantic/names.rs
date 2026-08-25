//! 名字 / `std.*` 导出 / 已知 arity。不启动 VM，不做类型推断。

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Block, CatchPattern, DelTarget, DestructElem, DestructPattern, Expr, ExprKind, LValue,
    LocatedStmt, ModuleRef, Pattern, PatternElem, Program, Stmt,
};
use crate::type_registry;

use crate::api_registry::{builtin_arity, std_arity, BUILTINS, STD_EXPORTS, STD_MODULES};

pub type Diag = (usize, usize, String);

pub fn analyze_program(program: &Program) -> Vec<Diag> {
    let mut cx = Cx {
        scopes: vec![global_names()],
        std_alias: HashMap::new(),
        std_mod_alias: HashMap::new(),
        user_defined: HashSet::new(),
        diags: Vec::new(),
    };
    walk_block(&program.stmts, &mut cx);
    cx.diags
}

struct Cx {
    scopes: Vec<HashSet<String>>,
    /// 本地名 → `(std_module, export)`，供 arity。
    std_alias: HashMap<String, (&'static str, &'static str)>,
    /// `use std.{ math }` / `import std.math` → 本地模块名。
    std_mod_alias: HashMap<String, &'static str>,
    /// 用户声明的名字；arity 检查跳过 builtin 表。
    user_defined: HashSet<String>,
    diags: Vec<Diag>,
}

impl Cx {
    fn define(&mut self, name: impl Into<String>) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.into());
        }
    }

    fn define_user(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.user_defined.insert(name.clone());
        self.define(name);
    }

    fn is_defined(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|s| s.contains(name))
    }

    fn push(&mut self) {
        self.scopes.push(HashSet::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }
}

fn global_names() -> HashSet<String> {
    let mut s = HashSet::new();
    for (n, _) in BUILTINS {
        s.insert((*n).to_string());
    }
    for n in type_registry::global_type_names() {
        s.insert(n.to_string());
    }
    s.insert("self".into());
    s.insert("_".into());
    s
}

fn walk_block(stmts: &Block, cx: &mut Cx) {
    for st in stmts {
        hoist_stmt(&st.stmt, cx);
    }
    for st in stmts {
        walk_stmt(st, cx);
    }
}

fn hoist_stmt(stmt: &Stmt, cx: &mut Cx) {
    match stmt {
        Stmt::FuncDecl { name, .. }
        | Stmt::FriendFuncDecl { name, .. }
        | Stmt::StructDecl { name, .. }
        | Stmt::EnumDecl { name, .. }
        | Stmt::VariantDecl { name, .. }
        | Stmt::MacroDecl { name, .. } => cx.define_user(name.clone()),
        Stmt::ProtocolDecl { name, .. } => cx.define(name.clone()),
        Stmt::Import { path, alias, .. } => {
            let key = alias.as_deref().unwrap_or(path.as_str());
            let short = key.rsplit('.').next().unwrap_or(key);
            cx.define(short.to_string());
            if let Some(m) = path.strip_prefix("std.") {
                if !m.contains('.') && STD_MODULES.contains(&m) {
                    cx.std_mod_alias.insert(short.to_string(), intern_mod(m));
                }
            }
        }
        Stmt::Use { module, items } => {
            let parts = match module {
                ModuleRef::Qualified(p) => p.as_slice(),
                ModuleRef::FilePath { .. } => &[],
            };
            for it in items {
                let local = it.alias.as_deref().unwrap_or(it.name.as_str());
                cx.define(local.to_string());
                if parts.first().map(String::as_str) == Some("std") {
                    match parts.len() {
                        1 => {
                            if STD_MODULES.contains(&it.name.as_str()) {
                                cx.std_mod_alias
                                    .insert(local.to_string(), intern_mod(&it.name));
                            } else if let Some((m, e)) = STD_EXPORTS
                                .iter()
                                .find(|(m, e)| m.is_empty() && *e == it.name)
                            {
                                cx.std_alias.insert(local.to_string(), (*m, *e));
                            }
                        }
                        2 => {
                            let mod_name = parts[1].as_str();
                            if STD_EXPORTS
                                .iter()
                                .any(|(m, e)| *m == mod_name && *e == it.name)
                            {
                                cx.std_alias.insert(
                                    local.to_string(),
                                    (intern_mod(mod_name), intern_exp(mod_name, &it.name)),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}

fn intern_mod(m: &str) -> &'static str {
    STD_MODULES.iter().copied().find(|x| *x == m).unwrap_or("")
}

fn intern_exp(module: &str, exp: &str) -> &'static str {
    STD_EXPORTS
        .iter()
        .find(|(m, e)| *m == module && *e == exp)
        .map(|(_, e)| *e)
        .unwrap_or("")
}

fn walk_stmt(st: &LocatedStmt, cx: &mut Cx) {
    match &st.stmt {
        Stmt::VarDecl {
            name,
            type_expr,
            init,
            ..
        } => {
            if let Some(t) = type_expr {
                walk_expr(t, cx);
            }
            if let Some(e) = init {
                walk_expr(e, cx);
            }
            cx.define_user(name.clone());
        }
        Stmt::DestructDecl { pattern, init, .. } => {
            walk_expr(init, cx);
            for n in destruct_names(pattern) {
                cx.define_user(n);
            }
        }
        Stmt::FuncDecl {
            type_params,
            params,
            body,
            decorators,
            return_type,
            return_wrapper,
            ..
        } => {
            for d in decorators {
                walk_expr(d, cx);
            }
            cx.push();
            bind_type_params(type_params, cx);
            for p in params {
                cx.define_user(p.name.clone());
                if let Some(t) = &p.type_expr {
                    walk_expr(t, cx);
                }
                if let Some(d) = &p.default_expr {
                    walk_expr(d, cx);
                }
            }
            if let Some(t) = return_type {
                walk_expr(t, cx);
            }
            if let Some(t) = return_wrapper {
                walk_expr(t, cx);
            }
            walk_block(body, cx);
            cx.pop();
        }
        Stmt::FriendFuncDecl {
            params,
            body,
            return_type,
            return_wrapper,
            ..
        } => {
            cx.push();
            if let Some(ps) = params {
                for p in ps {
                    cx.define_user(p.name.clone());
                    if let Some(t) = &p.type_expr {
                        walk_expr(t, cx);
                    }
                }
            }
            if let Some(t) = return_type {
                walk_expr(t, cx);
            }
            if let Some(t) = return_wrapper {
                walk_expr(t, cx);
            }
            if let Some(b) = body {
                walk_block(b, cx);
            }
            cx.pop();
        }
        Stmt::StructDecl {
            type_params,
            fields,
            methods,
            layout,
            ..
        } => {
            cx.push();
            bind_type_params(type_params, cx);
            if let Some(l) = layout {
                walk_expr(l, cx);
            }
            for f in fields {
                if let Some(t) = &f.type_expr {
                    walk_expr(t, cx);
                }
                if let Some(d) = &f.default_expr {
                    walk_expr(d, cx);
                }
            }
            for m in methods {
                cx.push();
                cx.define("self");
                for p in &m.params {
                    cx.define_user(p.name.clone());
                    if let Some(t) = &p.type_expr {
                        walk_expr(t, cx);
                    }
                }
                if let Some(t) = &m.return_type {
                    walk_expr(t, cx);
                }
                walk_block(&m.body, cx);
                cx.pop();
            }
            cx.pop();
        }
        Stmt::EnumDecl {
            members, methods, ..
        } => {
            for mem in members {
                if let Some(v) = &mem.value {
                    walk_expr(v, cx);
                }
            }
            for m in methods {
                cx.push();
                cx.define("self");
                for p in &m.params {
                    cx.define_user(p.name.clone());
                }
                walk_block(&m.body, cx);
                cx.pop();
            }
        }
        Stmt::VariantDecl {
            type_params, cases, ..
        } => {
            cx.push();
            bind_type_params(type_params, cx);
            for c in cases {
                for f in &c.fields {
                    if let Some(t) = &f.type_expr {
                        walk_expr(t, cx);
                    }
                }
            }
            cx.pop();
        }
        Stmt::If {
            cond,
            then_block,
            elifs,
            else_block,
        } => {
            walk_expr(cond, cx);
            walk_block(then_block, cx);
            for (c, b) in elifs {
                walk_expr(c, cx);
                walk_block(b, cx);
            }
            if let Some(b) = else_block {
                walk_block(b, cx);
            }
        }
        Stmt::While { cond, body } => {
            walk_expr(cond, cx);
            walk_block(body, cx);
        }
        Stmt::Loop { count, body } => {
            if let Some(c) = count {
                walk_expr(c, cx);
            }
            walk_block(body, cx);
        }
        Stmt::For { items, body } => {
            for it in items {
                walk_expr(&it.iterable, cx);
            }
            cx.push();
            for it in items {
                cx.define(it.name.clone());
            }
            walk_block(body, cx);
            cx.pop();
        }
        Stmt::Try {
            body,
            catches,
            else_block,
        } => {
            walk_block(body, cx);
            for c in catches {
                cx.push();
                if let CatchPattern::Bind { name, .. } = &c.pattern {
                    cx.define(name.clone());
                }
                walk_block(&c.body, cx);
                cx.pop();
            }
            if let Some(b) = else_block {
                walk_block(b, cx);
            }
        }
        Stmt::Match {
            subject,
            cases,
            else_block,
        } => {
            walk_expr(subject, cx);
            for c in cases {
                walk_pattern_exprs(&c.pattern, cx);
                cx.push();
                for n in pattern_names(&c.pattern) {
                    cx.define(n);
                }
                walk_block(&c.body, cx);
                cx.pop();
            }
            if let Some(b) = else_block {
                walk_block(b, cx);
            }
        }
        Stmt::With {
            context,
            body,
            alias,
            ..
        } => {
            walk_expr(context, cx);
            cx.push();
            if let Some(a) = alias {
                cx.define(a.clone());
            }
            walk_block(body, cx);
            cx.pop();
        }
        Stmt::Return(e) | Stmt::Yield(e) => {
            if let Some(e) = e {
                walk_expr(e, cx);
            }
        }
        Stmt::YieldFrom(e) | Stmt::Throw(e) | Stmt::Expr(e) => walk_expr(e, cx),
        Stmt::Assign { target, value } => {
            walk_lvalue(target, cx);
            walk_expr(value, cx);
        }
        Stmt::DestructAssign { value, .. } => walk_expr(value, cx),
        Stmt::Del(t) => walk_del_target(t, cx),
        Stmt::Block(b) => walk_block(b, cx),
        Stmt::MacroDecl { params, body, .. } => {
            cx.push();
            for p in params {
                cx.define_user(p.name.clone());
            }
            walk_block(body, cx);
            cx.pop();
        }
        Stmt::ProtocolDecl { .. }
        | Stmt::Import { .. }
        | Stmt::Use { .. }
        | Stmt::Break
        | Stmt::Continue
        | Stmt::Comment { .. } => {}
    }
}

fn walk_expr(expr: &Expr, cx: &mut Cx) {
    match &expr.kind {
        ExprKind::Var(n) => {
            if !cx.is_defined(n) {
                cx.diags.push((
                    expr.loc.line,
                    expr.loc.column,
                    format!("undefined name `{n}`"),
                ));
            }
        }
        ExprKind::Member { object, field } => {
            walk_expr(object, cx);
            if let Some(path) = expr_path(object) {
                check_std_path(&path, field, expr.loc.line, expr.loc.column, cx);
            }
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, cx);
            let splat = args.iter().any(|a| a.is_splat || a.is_kwsplat);
            for a in args {
                walk_expr(&a.value, cx);
            }
            if !splat {
                if let Some(path) = expr_path(callee) {
                    check_arity_mut(&path, args.len(), expr.loc.line, expr.loc.column, cx);
                }
            }
        }
        ExprKind::Unary { operand, .. }
        | ExprKind::Handle { operand }
        | ExprKind::Go { operand }
        | ExprKind::Snap { operand }
        | ExprKind::Await { operand } => walk_expr(operand, cx),
        ExprKind::Binary { left, right, .. } => {
            walk_expr(left, cx);
            walk_expr(right, cx);
        }
        ExprKind::Index { object, index } => {
            walk_expr(object, cx);
            walk_expr(index, cx);
        }
        ExprKind::List(xs) | ExprKind::Set(xs) | ExprKind::Tuple(xs) => {
            for e in xs {
                walk_expr(e, cx);
            }
        }
        ExprKind::Dict(ents) => {
            for (k, v) in ents {
                walk_expr(k, cx);
                walk_expr(v, cx);
            }
        }
        ExprKind::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            walk_expr(cond, cx);
            walk_expr(then_expr, cx);
            walk_expr(else_expr, cx);
        }
        ExprKind::DoFunc {
            params,
            body,
            return_type,
            return_wrapper,
            ..
        } => {
            cx.push();
            for p in params {
                cx.define_user(p.name.clone());
                if let Some(t) = &p.type_expr {
                    walk_expr(t, cx);
                }
            }
            if let Some(t) = return_type {
                walk_expr(t, cx);
            }
            if let Some(t) = return_wrapper {
                walk_expr(t, cx);
            }
            walk_block(body, cx);
            cx.pop();
        }
        ExprKind::FString(parts) => {
            for p in parts {
                if let crate::ast::FStringPart::Expr(e) = p {
                    walk_expr(e, cx);
                }
            }
        }
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => {
            walk_expr(object, cx);
            if let Some(e) = start {
                walk_expr(e, cx);
            }
            if let Some(e) = end {
                walk_expr(e, cx);
            }
            if let Some(e) = step {
                walk_expr(e, cx);
            }
        }
        ExprKind::TypeConvert { type_expr, value } => {
            walk_expr(type_expr, cx);
            walk_expr(value, cx);
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
            for it in items {
                walk_expr(&it.iterable, cx);
            }
            cx.push();
            for it in items {
                cx.define(it.name.clone());
            }
            walk_expr(elem, cx);
            for g in guards {
                walk_expr(g, cx);
            }
            cx.pop();
        }
        ExprKind::DictComp {
            key,
            value,
            items,
            guards,
        } => {
            for it in items {
                walk_expr(&it.iterable, cx);
            }
            cx.push();
            for it in items {
                cx.define(it.name.clone());
            }
            walk_expr(key, cx);
            walk_expr(value, cx);
            for g in guards {
                walk_expr(g, cx);
            }
            cx.pop();
        }
        ExprKind::NamedAssign { value, .. } => walk_expr(value, cx),
        ExprKind::Pipeline { left, right, .. } => {
            walk_expr(left, cx);
            walk_expr(right, cx);
        }
        ExprKind::Match {
            subject,
            cases,
            else_block,
        } => {
            walk_expr(subject, cx);
            for c in cases {
                walk_pattern_exprs(&c.pattern, cx);
                cx.push();
                for n in pattern_names(&c.pattern) {
                    cx.define(n);
                }
                walk_block(&c.body, cx);
                cx.pop();
            }
            if let Some(b) = else_block {
                walk_block(b, cx);
            }
        }
        ExprKind::ParFor { items, body } => {
            for it in items {
                walk_expr(&it.iterable, cx);
            }
            cx.push();
            for it in items {
                cx.define(it.name.clone());
            }
            walk_block(body, cx);
            cx.pop();
        }
        ExprKind::ParBlock { exprs } => {
            for e in exprs {
                walk_expr(e, cx);
            }
        }
        ExprKind::Select { cases, else_block } => {
            for c in cases {
                walk_expr(&c.event, cx);
                cx.push();
                if let Some(n) = &c.bind {
                    cx.define(n.clone());
                }
                walk_block(&c.body, cx);
                cx.pop();
            }
            if let Some(b) = else_block {
                walk_block(b, cx);
            }
        }
        ExprKind::Quote {
            hygienic_names,
            bindings,
            body,
            ..
        } => {
            for e in bindings {
                walk_expr(e, cx);
            }
            cx.push();
            for n in hygienic_names {
                cx.define(n.clone());
            }
            walk_block(body, cx);
            cx.pop();
        }
        ExprKind::MacroCall { callee, .. } => walk_expr(callee, cx),
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

fn check_std_path(base: &str, field: &str, line: usize, col: usize, cx: &mut Cx) {
    if base == "std" {
        if STD_MODULES.contains(&field)
            || STD_EXPORTS.iter().any(|(m, e)| m.is_empty() && *e == field)
        {
            return;
        }
        cx.diags
            .push((line, col, format!("unknown std module or export `{field}`")));
        return;
    }
    let aliased = cx.std_mod_alias.get(base).copied();
    let mod_name = if let Some(m) = aliased {
        Some(m)
    } else {
        base.strip_prefix("std.")
            .filter(|m| !m.contains('.') && STD_MODULES.contains(m))
    };
    let Some(mod_name) = mod_name else {
        return;
    };
    if STD_EXPORTS
        .iter()
        .any(|(m, e)| *m == mod_name && *e == field)
    {
        return;
    }
    cx.diags.push((
        line,
        col,
        format!("unknown export `std.{mod_name}.{field}`"),
    ));
}

fn bind_type_params(params: &[(String, Option<Expr>)], cx: &mut Cx) {
    for (n, bound) in params {
        cx.define(n.clone());
        if let Some(b) = bound {
            walk_expr(b, cx);
        }
    }
}

fn walk_lvalue(lv: &LValue, cx: &mut Cx) {
    match lv {
        LValue::Name(_) => {}
        LValue::Member { object, .. } => walk_expr(object, cx),
        LValue::Index { object, index } => {
            walk_expr(object, cx);
            walk_expr(index, cx);
        }
        LValue::Slice {
            object,
            start,
            end,
            step,
        } => {
            walk_expr(object, cx);
            if let Some(e) = start {
                walk_expr(e, cx);
            }
            if let Some(e) = end {
                walk_expr(e, cx);
            }
            if let Some(e) = step {
                walk_expr(e, cx);
            }
        }
    }
}

fn walk_del_target(t: &DelTarget, cx: &mut Cx) {
    match t {
        DelTarget::Name(_) => {}
        DelTarget::Member { object, .. } => walk_expr(object, cx),
        DelTarget::Index { object, index } => {
            walk_expr(object, cx);
            walk_expr(index, cx);
        }
    }
}

fn walk_pattern_exprs(p: &Pattern, cx: &mut Cx) {
    match p {
        Pattern::Bind(_) | Pattern::Struct { .. } => {}
        Pattern::Value(e) => walk_expr(e, cx),
        Pattern::List(xs) | Pattern::Tuple(xs) => {
            for e in xs {
                match e {
                    PatternElem::Value(v) => walk_expr(v, cx),
                    PatternElem::Nested(n) => walk_pattern_exprs(n, cx),
                    PatternElem::Bind(_) => {}
                }
            }
        }
        Pattern::Or(ps) | Pattern::Call { args: ps, .. } => {
            for n in ps {
                walk_pattern_exprs(n, cx);
            }
        }
    }
}

fn check_arity_mut(path: &str, argc: usize, line: usize, col: usize, cx: &mut Cx) {
    let spec = if let Some((m, e)) = cx.std_alias.get(path) {
        Some((*m, *e))
    } else if let Some(rest) = path.strip_prefix("std.") {
        if let Some((m, e)) = rest.split_once('.') {
            Some((m, e))
        } else {
            Some(("", rest))
        }
    } else if let Some((head, exp)) = path.split_once('.') {
        cx.std_mod_alias.get(head).map(|m| (*m, exp))
    } else {
        None
    };
    let range = if let Some((m, e)) = spec {
        std_arity(m, e)
    } else if cx.user_defined.contains(path) {
        None
    } else {
        builtin_arity(path)
    };
    let Some((min, max)) = range else {
        return;
    };
    if argc >= min && max.is_none_or(|mx| argc <= mx) {
        return;
    }
    let expect = match max {
        Some(mx) if mx == min => format!("{min}"),
        Some(mx) => format!("{min}..{mx}"),
        None => format!("{min}+"),
    };
    cx.diags.push((
        line,
        col,
        format!("`{path}` expects {expect} argument(s), got {argc}"),
    ));
}

fn destruct_names(p: &DestructPattern) -> Vec<String> {
    match p {
        DestructPattern::Name(n) => vec![n.clone()],
        DestructPattern::Discard => vec![],
        DestructPattern::List(xs) | DestructPattern::Tuple(xs) => {
            xs.iter().flat_map(destruct_elem_names).collect()
        }
    }
}

fn destruct_elem_names(e: &DestructElem) -> Vec<String> {
    match e {
        DestructElem::Pat(p) => destruct_names(p),
        DestructElem::Rest(n) => vec![n.clone()],
        DestructElem::RestDiscard => vec![],
    }
}

fn pattern_names(p: &Pattern) -> Vec<String> {
    match p {
        Pattern::Bind(n) => vec![n.clone()],
        Pattern::Value(_) => vec![],
        Pattern::List(xs) | Pattern::Tuple(xs) => xs
            .iter()
            .flat_map(|e| match e {
                PatternElem::Bind(n) => vec![n.clone()],
                PatternElem::Nested(p) => pattern_names(p),
                PatternElem::Value(_) => vec![],
            })
            .collect(),
        Pattern::Struct { fields, .. } => fields.clone(),
        Pattern::Or(ps) => ps.iter().flat_map(pattern_names).collect(),
        Pattern::Call { args, .. } => args.iter().flat_map(pattern_names).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn diags(src: &str) -> Vec<String> {
        let program = Parser::parse(src).expect(src);
        analyze_program(&program)
            .into_iter()
            .map(|(_, _, m)| m)
            .collect()
    }

    #[test]
    fn undefined_name() {
        let d = diags("print(no_such_name)\n");
        assert!(d
            .iter()
            .any(|m| m.contains("undefined name `no_such_name`")));
    }

    #[test]
    fn known_names_ok() {
        assert!(diags("let x = 1\nprint(x)\nprint(len([1]))\n").is_empty());
        assert!(diags("func add(a, b) { a + b }\nadd(1, 2)\n").is_empty());
        assert!(diags("use std.math.{ sin }\nprint(sin(0))\n").is_empty());
        assert!(diags("std.math.sin(0)\n").is_empty());
    }

    #[test]
    fn unknown_std_export() {
        let d = diags("std.math.nope_fn\n");
        assert!(d.iter().any(|m| m.contains("unknown export")));
        let d = diags("std.not_a_module\n");
        assert!(d.iter().any(|m| m.contains("unknown std")));
    }

    #[test]
    fn arity_builtin_and_std() {
        let d = diags("len()\n");
        assert!(d.iter().any(|m| m.contains("expects")));
        let d = diags("std.math.sin()\n");
        assert!(d.iter().any(|m| m.contains("expects")));
        assert!(
            diags("std.http.serve(80, handler, \"0.0.0.0\")\nfunc handler(req) { req }\n")
                .is_empty()
        );
    }
}
