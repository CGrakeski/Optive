//! 泛型函数单态化：在 AST 中替换类型参数。

use std::collections::HashMap;

use crate::ast::{
    Block, CallArg, CatchClause, DelTarget, Expr, ExprKind, FStringPart, ForItem, FuncParam,
    LValue, LocatedStmt, MatchCase, Pattern, PatternElem, SelectCase, Stmt,
};
use crate::types::{static_type_value_from_expr, type_value_display, type_values_equal};
use crate::value::Value;

pub fn type_args_from_index_expr(index: &Expr) -> Result<Vec<Value>, String> {
    match &index.kind {
        ExprKind::List(elems) => elems
            .iter()
            .map(|e| {
                static_type_value_from_expr(e)
                    .ok_or_else(|| "expected type name in generic index".to_string())
            })
            .collect(),
        _ => static_type_value_from_expr(index)
            .map(|v| vec![v])
            .ok_or_else(|| "expected type name in generic index".into()),
    }
}

pub fn infer_type_args_from_call_args(
    template: &crate::opcode::GenericFunctionTemplate,
    args: &[CallArg],
) -> Result<Vec<Value>, String> {
    infer_type_args_from_arg_exprs(
        &template.name,
        &template.type_params,
        &template.params,
        args,
        |e| infer_type_from_expr(e, "x"),
    )
}

pub fn infer_type_args_from_arg_exprs(
    func_name: &str,
    type_params: &[(String, Option<Expr>)],
    params: &[FuncParam],
    args: &[CallArg],
    infer_arg: impl Fn(&Expr) -> Option<Value>,
) -> Result<Vec<Value>, String> {
    let mut inferred: HashMap<String, Value> = HashMap::new();
    for (param, arg) in params.iter().zip(args.iter()) {
        let Some(ty_expr) = &param.type_expr else {
            continue;
        };
        let Some(tp) = type_param_in_annotation(ty_expr, type_params) else {
            continue;
        };
        let Some(ty) = infer_arg(&arg.value) else {
            continue;
        };
        if let Some(prev) = inferred.get(&tp) {
            if !type_values_equal(prev, &ty) {
                return Err(format!(
                    "conflicting inferences for type parameter `{tp}` at call to `{func_name}`: {} vs {}",
                    type_value_display(prev),
                    type_value_display(&ty)
                ));
            }
        } else {
            inferred.insert(tp, ty);
        }
    }
    if inferred.is_empty() && type_params.len() == 1 && !args.is_empty() {
        if let Some(ty) = infer_arg(&args[0].value) {
            inferred.insert(type_params[0].0.clone(), ty);
        }
    }
    let mut out = Vec::with_capacity(type_params.len());
    for (name, _) in type_params {
        match inferred.get(name) {
            Some(v) => out.push(v.clone()),
            None => {
                return Err(format!(
                    "cannot infer {} type parameter(s) for `{func_name}`; use {func_name}[...](...)",
                    type_params.len()
                ));
            }
        }
    }
    Ok(out)
}

fn type_param_in_annotation(expr: &Expr, type_params: &[(String, Option<Expr>)]) -> Option<String> {
    match &expr.kind {
        ExprKind::Var(name) if type_params.iter().any(|(p, _)| p == name) => Some(name.clone()),
        _ => None,
    }
}

#[must_use]
pub fn infer_type_from_expr(expr: &Expr, param_name: &str) -> Option<Value> {
    match &expr.kind {
        ExprKind::Number(_) => Some(Value::type_ref("num")),
        ExprKind::String(_) | ExprKind::FString(_) => Some(Value::type_ref("text")),
        ExprKind::Bool(_) => Some(Value::type_ref("bool")),
        ExprKind::None => Some(Value::type_ref("nonetype")),
        ExprKind::List(_) | ExprKind::ListComp { .. } => Some(Value::type_ref("list")),
        ExprKind::Dict(_) | ExprKind::DictComp { .. } => Some(Value::type_ref("dict")),
        ExprKind::Set(_) | ExprKind::SetComp { .. } => Some(Value::type_ref("set")),
        ExprKind::Tuple(_) => Some(Value::type_ref("tuple")),
        ExprKind::Bytes(_) => Some(Value::type_ref("bytes")),
        ExprKind::Unary { op, operand } => match op {
            crate::ast::UnaryOp::Not | crate::ast::UnaryOp::TruthyNot => {
                Some(Value::type_ref("bool"))
            }
            crate::ast::UnaryOp::Neg | crate::ast::UnaryOp::Invert => {
                infer_type_from_expr(operand, param_name).or_else(|| Some(Value::type_ref("num")))
            }
        },
        ExprKind::Binary { op, left, right } => match op {
            crate::ast::BinaryOp::Eq
            | crate::ast::BinaryOp::Ne
            | crate::ast::BinaryOp::Lt
            | crate::ast::BinaryOp::Le
            | crate::ast::BinaryOp::Gt
            | crate::ast::BinaryOp::Ge
            | crate::ast::BinaryOp::In
            | crate::ast::BinaryOp::Is
            | crate::ast::BinaryOp::IsNot
            | crate::ast::BinaryOp::And
            | crate::ast::BinaryOp::Or => Some(Value::type_ref("bool")),
            _ => {
                let l = infer_type_from_expr(left, param_name);
                let r = infer_type_from_expr(right, param_name);
                match (l, r) {
                    (Some(a), Some(b)) if type_values_equal(&a, &b) => Some(a),
                    (Some(a), None) | (None, Some(a)) => Some(a),
                    _ => None,
                }
            }
        },
        ExprKind::IfThenElse {
            then_expr,
            else_expr,
            ..
        } => {
            let t = infer_type_from_expr(then_expr, param_name)?;
            let e = infer_type_from_expr(else_expr, param_name)?;
            if type_values_equal(&t, &e) {
                Some(t)
            } else {
                None
            }
        }
        ExprKind::Var(name) if name == param_name => None,
        ExprKind::Var(_) => None,
        _ => None,
    }
}

pub use crate::types::substitute_type_annotation as substitute_type_expr;
pub use crate::types::substitute_type_value;

#[must_use]
pub fn substitute_func_param(
    param: &FuncParam,
    subs: &HashMap<String, Value>,
    type_names: &HashMap<String, String>,
) -> FuncParam {
    FuncParam {
        name: param.name.clone(),
        is_variadic: param.is_variadic,
        is_kwvariadic: param.is_kwvariadic,
        implicit: param.implicit,
        type_expr: param
            .type_expr
            .as_ref()
            .map(|t| substitute_type_expr(t, subs)),
        type_strong: param.type_strong,
        default_expr: param
            .default_expr
            .as_ref()
            .map(|e| substitute_expr(e, type_names)),
    }
}

#[must_use]
pub fn substitute_block(block: &Block, type_names: &HashMap<String, String>) -> Block {
    block
        .iter()
        .map(|s| substitute_stmt(s, type_names))
        .collect()
}

fn substitute_stmt(stmt: &LocatedStmt, type_names: &HashMap<String, String>) -> LocatedStmt {
    LocatedStmt {
        line: stmt.line,
        column: stmt.column,
        stmt: substitute_stmt_body(&stmt.stmt, type_names),
    }
}

fn substitute_stmt_body(stmt: &Stmt, type_names: &HashMap<String, String>) -> Stmt {
    match stmt {
        Stmt::Return(e) => Stmt::Return(e.as_ref().map(|x| substitute_expr(x, type_names))),
        Stmt::Yield(e) => Stmt::Yield(e.as_ref().map(|x| substitute_expr(x, type_names))),
        Stmt::YieldFrom(e) => Stmt::YieldFrom(substitute_expr(e, type_names)),
        Stmt::Throw(e) => Stmt::Throw(substitute_expr(e, type_names)),
        Stmt::Assign { target, value } => Stmt::Assign {
            target: substitute_lvalue(target, type_names),
            value: substitute_expr(value, type_names),
        },
        Stmt::DestructDecl {
            visibility,
            is_const,
            is_var,
            pattern,
            init,
        } => Stmt::DestructDecl {
            visibility: *visibility,
            is_const: *is_const,
            is_var: *is_var,
            pattern: pattern.clone(),
            init: substitute_expr(init, type_names),
        },
        Stmt::DestructAssign { pattern, value } => Stmt::DestructAssign {
            pattern: pattern.clone(),
            value: substitute_expr(value, type_names),
        },
        Stmt::Expr(e) => Stmt::Expr(substitute_expr(e, type_names)),
        Stmt::If {
            cond,
            then_block,
            elifs,
            else_block,
        } => Stmt::If {
            cond: substitute_expr(cond, type_names),
            then_block: substitute_block(then_block, type_names),
            elifs: elifs
                .iter()
                .map(|(c, b)| {
                    (
                        substitute_expr(c, type_names),
                        substitute_block(b, type_names),
                    )
                })
                .collect(),
            else_block: else_block.as_ref().map(|b| substitute_block(b, type_names)),
        },
        Stmt::While { cond, body } => Stmt::While {
            cond: substitute_expr(cond, type_names),
            body: substitute_block(body, type_names),
        },
        Stmt::Loop { count, body } => Stmt::Loop {
            count: count.as_ref().map(|c| substitute_expr(c, type_names)),
            body: substitute_block(body, type_names),
        },
        Stmt::For { items, body } => Stmt::For {
            items: items
                .iter()
                .map(|it| ForItem {
                    name: it.name.clone(),
                    iterable: substitute_expr(&it.iterable, type_names),
                })
                .collect(),
            body: substitute_block(body, type_names),
        },
        Stmt::Block(b) => Stmt::Block(substitute_block(b, type_names)),
        Stmt::Try {
            body,
            catches,
            else_block,
        } => Stmt::Try {
            body: substitute_block(body, type_names),
            catches: catches
                .iter()
                .map(|c| CatchClause {
                    pattern: c.pattern.clone(),
                    body: substitute_block(&c.body, type_names),
                })
                .collect(),
            else_block: else_block.as_ref().map(|b| substitute_block(b, type_names)),
        },
        Stmt::Match {
            subject,
            cases,
            else_block,
        } => Stmt::Match {
            subject: substitute_expr(subject, type_names),
            cases: cases
                .iter()
                .map(|c| MatchCase {
                    pattern: substitute_pattern(&c.pattern, type_names),
                    body: substitute_block(&c.body, type_names),
                })
                .collect(),
            else_block: else_block.as_ref().map(|b| substitute_block(b, type_names)),
        },
        Stmt::Del(target) => Stmt::Del(substitute_del_target(target, type_names)),
        Stmt::With {
            context,
            alias,
            body,
        } => Stmt::With {
            context: substitute_expr(context, type_names),
            alias: alias.clone(),
            body: substitute_block(body, type_names),
        },
        Stmt::VarDecl {
            visibility,
            is_const,
            is_var,
            name,
            type_expr,
            type_strong,
            init,
        } => Stmt::VarDecl {
            visibility: *visibility,
            is_const: *is_const,
            is_var: *is_var,
            name: name.clone(),
            type_expr: type_expr
                .as_ref()
                .map(|t| substitute_type_operand_expr(t, type_names)),
            type_strong: *type_strong,
            init: init.as_ref().map(|e| substitute_expr(e, type_names)),
        },
        other => other.clone(),
    }
}

fn substitute_lvalue(lv: &LValue, type_names: &HashMap<String, String>) -> LValue {
    match lv {
        LValue::Name(n) => LValue::Name(n.clone()),
        LValue::Member { object, field } => LValue::Member {
            object: Box::new(substitute_expr(object, type_names)),
            field: field.clone(),
        },
        LValue::Index { object, index } => LValue::Index {
            object: Box::new(substitute_expr(object, type_names)),
            index: Box::new(substitute_expr(index, type_names)),
        },
        LValue::Slice {
            object,
            start,
            end,
            step,
        } => LValue::Slice {
            object: Box::new(substitute_expr(object, type_names)),
            start: start
                .as_ref()
                .map(|e| Box::new(substitute_expr(e, type_names))),
            end: end
                .as_ref()
                .map(|e| Box::new(substitute_expr(e, type_names))),
            step: step
                .as_ref()
                .map(|e| Box::new(substitute_expr(e, type_names))),
        },
    }
}

#[must_use]
pub fn substitute_expr(expr: &Expr, type_names: &HashMap<String, String>) -> Expr {
    let kind = match &expr.kind {
        ExprKind::Member { object, field } if field == "__name__" => match &object.kind {
            ExprKind::Var(n) if type_names.contains_key(n) => {
                ExprKind::String(type_names[n].clone())
            }
            // 不满足「类型名.__name__」时，回退到通用 Member 处理（与下方 Member arm 等价）。
            _ => ExprKind::Member {
                object: Box::new(substitute_expr(object, type_names)),
                field: field.clone(),
            },
        },
        ExprKind::Unary { op, operand } => ExprKind::Unary {
            op: *op,
            operand: Box::new(substitute_expr(operand, type_names)),
        },
        ExprKind::Binary { op, left, right } => ExprKind::Binary {
            op: *op,
            left: Box::new(substitute_expr(left, type_names)),
            right: Box::new(substitute_expr(right, type_names)),
        },
        ExprKind::Call { callee, args } => ExprKind::Call {
            callee: Box::new(substitute_expr(callee, type_names)),
            args: args
                .iter()
                .map(|a| CallArg {
                    name: a.name.clone(),
                    is_splat: a.is_splat,
                    is_kwsplat: a.is_kwsplat,
                    value: substitute_expr(&a.value, type_names),
                })
                .collect(),
        },
        ExprKind::Member { object, field } => ExprKind::Member {
            object: Box::new(substitute_expr(object, type_names)),
            field: field.clone(),
        },
        ExprKind::Index { object, index } => ExprKind::Index {
            object: Box::new(substitute_expr(object, type_names)),
            index: Box::new(substitute_expr(index, type_names)),
        },
        ExprKind::List(elems) => ExprKind::List(
            elems
                .iter()
                .map(|e| substitute_expr(e, type_names))
                .collect(),
        ),
        ExprKind::ListComp {
            elem,
            items,
            guards,
        } => ExprKind::ListComp {
            elem: Box::new(substitute_expr(elem, type_names)),
            items: substitute_for_items(items, type_names),
            guards: substitute_guards(guards, type_names),
        },
        ExprKind::SetComp {
            elem,
            items,
            guards,
        } => ExprKind::SetComp {
            elem: Box::new(substitute_expr(elem, type_names)),
            items: substitute_for_items(items, type_names),
            guards: substitute_guards(guards, type_names),
        },
        ExprKind::GeneratorExp {
            elem,
            items,
            guards,
        } => ExprKind::GeneratorExp {
            elem: Box::new(substitute_expr(elem, type_names)),
            items: substitute_for_items(items, type_names),
            guards: substitute_guards(guards, type_names),
        },
        ExprKind::Dict(entries) => ExprKind::Dict(
            entries
                .iter()
                .map(|(k, v)| {
                    (
                        substitute_expr(k, type_names),
                        substitute_expr(v, type_names),
                    )
                })
                .collect(),
        ),
        ExprKind::DictComp {
            key,
            value,
            items,
            guards,
        } => ExprKind::DictComp {
            key: Box::new(substitute_expr(key, type_names)),
            value: Box::new(substitute_expr(value, type_names)),
            items: substitute_for_items(items, type_names),
            guards: substitute_guards(guards, type_names),
        },
        ExprKind::Set(elems) => ExprKind::Set(
            elems
                .iter()
                .map(|e| substitute_expr(e, type_names))
                .collect(),
        ),
        ExprKind::Tuple(elems) => ExprKind::Tuple(
            elems
                .iter()
                .map(|e| substitute_expr(e, type_names))
                .collect(),
        ),
        ExprKind::TypeConvert { type_expr, value } => ExprKind::TypeConvert {
            type_expr: Box::new(substitute_type_operand_expr(type_expr, type_names)),
            value: Box::new(substitute_expr(value, type_names)),
        },
        // 类型参数作值使用：`return T` → `return num`
        ExprKind::Var(name) if type_names.contains_key(name) => {
            ExprKind::Var(type_names[name].clone())
        }
        ExprKind::Bytes(b) => ExprKind::Bytes(b.clone()),
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => ExprKind::Slice {
            object: Box::new(substitute_expr(object, type_names)),
            start: start
                .as_ref()
                .map(|e| Box::new(substitute_expr(e, type_names))),
            end: end
                .as_ref()
                .map(|e| Box::new(substitute_expr(e, type_names))),
            step: step
                .as_ref()
                .map(|e| Box::new(substitute_expr(e, type_names))),
        },
        ExprKind::Pipeline {
            left,
            right,
            pipe_name,
        } => ExprKind::Pipeline {
            left: Box::new(substitute_expr(left, type_names)),
            right: Box::new(substitute_expr(right, type_names)),
            pipe_name: pipe_name.clone(),
        },
        ExprKind::Match {
            subject,
            cases,
            else_block,
        } => ExprKind::Match {
            subject: Box::new(substitute_expr(subject, type_names)),
            cases: cases
                .iter()
                .map(|c| MatchCase {
                    pattern: substitute_pattern(&c.pattern, type_names),
                    body: substitute_block(&c.body, type_names),
                })
                .collect(),
            else_block: else_block.as_ref().map(|b| substitute_block(b, type_names)),
        },
        ExprKind::FString(parts) => ExprKind::FString(
            parts
                .iter()
                .map(|p| match p {
                    FStringPart::Text(t) => FStringPart::Text(t.clone()),
                    FStringPart::Expr(e) => {
                        FStringPart::Expr(Box::new(substitute_expr(e, type_names)))
                    }
                })
                .collect(),
        ),
        ExprKind::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => ExprKind::IfThenElse {
            cond: Box::new(substitute_expr(cond, type_names)),
            then_expr: Box::new(substitute_expr(then_expr, type_names)),
            else_expr: Box::new(substitute_expr(else_expr, type_names)),
        },
        ExprKind::Handle { operand } => ExprKind::Handle {
            operand: Box::new(substitute_expr(operand, type_names)),
        },
        ExprKind::Go { operand } => ExprKind::Go {
            operand: Box::new(substitute_expr(operand, type_names)),
        },
        ExprKind::ParFor { items, body } => ExprKind::ParFor {
            items: items
                .iter()
                .map(|i| ForItem {
                    name: i.name.clone(),
                    iterable: substitute_expr(&i.iterable, type_names),
                })
                .collect(),
            body: substitute_block(body, type_names),
        },
        ExprKind::ParBlock { exprs } => ExprKind::ParBlock {
            exprs: exprs
                .iter()
                .map(|e| substitute_expr(e, type_names))
                .collect(),
        },
        ExprKind::Snap { operand } => ExprKind::Snap {
            operand: Box::new(substitute_expr(operand, type_names)),
        },
        ExprKind::Await { operand } => ExprKind::Await {
            operand: Box::new(substitute_expr(operand, type_names)),
        },
        ExprKind::Suspend => ExprKind::Suspend,
        ExprKind::Select { cases, else_block } => ExprKind::Select {
            cases: cases
                .iter()
                .map(|c| SelectCase {
                    event: substitute_expr(&c.event, type_names),
                    bind: c.bind.clone(),
                    body: substitute_block(&c.body, type_names),
                })
                .collect(),
            else_block: else_block.as_ref().map(|b| substitute_block(b, type_names)),
        },
        ExprKind::NamedAssign { name, value } => ExprKind::NamedAssign {
            name: name.clone(),
            value: Box::new(substitute_expr(value, type_names)),
        },
        ExprKind::DoFunc {
            params,
            return_type,
            return_strong,
            return_wrapper,
            body,
        } => ExprKind::DoFunc {
            params: params
                .iter()
                .map(|p| FuncParam {
                    name: p.name.clone(),
                    is_variadic: p.is_variadic,
                    is_kwvariadic: p.is_kwvariadic,
                    implicit: p.implicit,
                    type_expr: p
                        .type_expr
                        .as_ref()
                        .map(|t| substitute_type_operand_expr(t, type_names)),
                    type_strong: p.type_strong,
                    default_expr: p
                        .default_expr
                        .as_ref()
                        .map(|e| substitute_expr(e, type_names)),
                })
                .collect(),
            return_type: return_type
                .as_ref()
                .map(|t| Box::new(substitute_type_operand_expr(t, type_names))),
            return_strong: *return_strong,
            return_wrapper: return_wrapper
                .as_ref()
                .map(|e| Box::new(substitute_expr(e, type_names))),
            body: substitute_block(body, type_names),
        },
        ExprKind::Quote {
            hygienic_names,
            bindings,
            body,
        } => ExprKind::Quote {
            hygienic_names: hygienic_names.clone(),
            bindings: bindings
                .iter()
                .map(|b| substitute_expr(b, type_names))
                .collect(),
            body: substitute_block(body, type_names),
        },
        other => other.clone(),
    };
    Expr::new(expr.loc, kind)
}

fn substitute_pattern(pat: &Pattern, type_names: &HashMap<String, String>) -> Pattern {
    match pat {
        Pattern::Bind(n) => Pattern::Bind(n.clone()),
        Pattern::Value(e) => Pattern::Value(Box::new(substitute_expr(e, type_names))),
        Pattern::List(elems) => Pattern::List(
            elems
                .iter()
                .map(|el| match el {
                    PatternElem::Bind(n) => PatternElem::Bind(n.clone()),
                    PatternElem::Nested(p) => {
                        PatternElem::Nested(substitute_pattern(p, type_names))
                    }
                    PatternElem::Value(e) => {
                        PatternElem::Value(Box::new(substitute_expr(e, type_names)))
                    }
                })
                .collect(),
        ),
        Pattern::Tuple(elems) => Pattern::Tuple(
            elems
                .iter()
                .map(|el| match el {
                    PatternElem::Bind(n) => PatternElem::Bind(n.clone()),
                    PatternElem::Nested(p) => {
                        PatternElem::Nested(substitute_pattern(p, type_names))
                    }
                    PatternElem::Value(e) => {
                        PatternElem::Value(Box::new(substitute_expr(e, type_names)))
                    }
                })
                .collect(),
        ),
        Pattern::Struct { type_name, fields } => Pattern::Struct {
            type_name: type_name.clone(),
            fields: fields.clone(),
        },
        Pattern::Or(alts) => Pattern::Or(
            alts.iter()
                .map(|p| substitute_pattern(p, type_names))
                .collect(),
        ),
        Pattern::Call { type_name, args } => Pattern::Call {
            type_name: type_name.clone(),
            args: args
                .iter()
                .map(|a| substitute_pattern(a, type_names))
                .collect(),
        },
    }
}

fn substitute_del_target(target: &DelTarget, type_names: &HashMap<String, String>) -> DelTarget {
    match target {
        DelTarget::Name(n) => DelTarget::Name(n.clone()),
        DelTarget::Member { object, field } => DelTarget::Member {
            object: Box::new(substitute_expr(object, type_names)),
            field: field.clone(),
        },
        DelTarget::Index { object, index } => DelTarget::Index {
            object: Box::new(substitute_expr(object, type_names)),
            index: Box::new(substitute_expr(index, type_names)),
        },
    }
}

fn substitute_type_operand_expr(expr: &Expr, type_names: &HashMap<String, String>) -> Expr {
    match &expr.kind {
        ExprKind::Var(name) if type_names.contains_key(name) => {
            Expr::new(expr.loc, ExprKind::Var(type_names[name].clone()))
        }
        ExprKind::Member { object, field } => Expr::new(
            expr.loc,
            ExprKind::Member {
                object: Box::new(substitute_type_operand_expr(object, type_names)),
                field: field.clone(),
            },
        ),
        _ => substitute_expr(expr, type_names),
    }
}

fn substitute_for_items(items: &[ForItem], type_names: &HashMap<String, String>) -> Vec<ForItem> {
    items
        .iter()
        .map(|it| ForItem {
            name: it.name.clone(),
            iterable: substitute_expr(&it.iterable, type_names),
        })
        .collect()
}

fn substitute_guards(guards: &[Expr], type_names: &HashMap<String, String>) -> Vec<Expr> {
    guards
        .iter()
        .map(|g| substitute_expr(g, type_names))
        .collect()
}

#[must_use]
pub fn type_name_map(
    type_params: &[(String, Option<Expr>)],
    type_args: &[Value],
) -> HashMap<String, String> {
    type_params
        .iter()
        .zip(type_args.iter())
        .map(|((name, _), arg)| (name.clone(), type_value_display(arg)))
        .collect()
}

#[must_use]
pub fn type_substitution_map(
    type_params: &[(String, Option<Expr>)],
    type_args: &[Value],
) -> HashMap<String, Value> {
    type_params
        .iter()
        .zip(type_args.iter())
        .map(|((name, _), arg)| (name.clone(), arg.clone()))
        .collect()
}
