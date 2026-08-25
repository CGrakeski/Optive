use std::collections::HashSet;

use crate::ast::{Block, DestructElem, DestructPattern, Expr, ExprKind, LValue, Stmt};
use crate::error::RuntimeError;
use crate::value::{Num, Value};

pub(super) fn collect_assigned_names(body: &Block, out: &mut HashSet<String>) {
    for s in body {
        match &s.stmt {
            Stmt::Assign {
                target: LValue::Name(n),
                ..
            } => {
                out.insert(n.clone());
            }
            Stmt::VarDecl {
                name, is_var: true, ..
            } => {
                // 块内新建 var 无妨；跨任务写的是 Assign。
                let _ = name;
            }
            Stmt::If {
                then_block,
                elifs,
                else_block,
                ..
            } => {
                collect_assigned_names(then_block, out);
                for (_, b) in elifs {
                    collect_assigned_names(b, out);
                }
                if let Some(b) = else_block {
                    collect_assigned_names(b, out);
                }
            }
            Stmt::While { body, .. }
            | Stmt::Loop { body, .. }
            | Stmt::For { body, .. }
            | Stmt::With { body, .. }
            | Stmt::Block(body) => collect_assigned_names(body, out),
            Stmt::Try {
                body,
                catches,
                else_block,
            } => {
                collect_assigned_names(body, out);
                for c in catches {
                    collect_assigned_names(&c.body, out);
                }
                if let Some(b) = else_block {
                    collect_assigned_names(b, out);
                }
            }
            Stmt::Match {
                cases, else_block, ..
            } => {
                for c in cases {
                    collect_assigned_names(&c.body, out);
                }
                if let Some(b) = else_block {
                    collect_assigned_names(b, out);
                }
            }
            _ => {}
        }
    }
}

pub(super) type DestructSplit<'a> = (
    Vec<&'a DestructPattern>,
    Option<&'a DestructElem>,
    Vec<&'a DestructPattern>,
);

pub(super) fn split_destruct_elems(
    elems: &[DestructElem],
) -> std::result::Result<DestructSplit<'_>, RuntimeError> {
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut rest = None;
    for elem in elems {
        match elem {
            DestructElem::Pat(p) => {
                if rest.is_some() {
                    after.push(p);
                } else {
                    before.push(p);
                }
            }
            DestructElem::Rest(_) | DestructElem::RestDiscard => {
                if rest.is_some() {
                    return Err(RuntimeError::msg("multiple *rest in destructuring pattern"));
                }
                rest = Some(elem);
            }
        }
    }
    Ok((before, rest, after))
}

pub(super) fn destruct_bound_names(pattern: &DestructPattern) -> Vec<String> {
    let mut out = Vec::new();
    collect_destruct_names(pattern, &mut out);
    out
}

fn collect_destruct_names(pattern: &DestructPattern, out: &mut Vec<String>) {
    match pattern {
        DestructPattern::Name(n) => out.push(n.clone()),
        DestructPattern::Discard => {}
        DestructPattern::Tuple(elems) | DestructPattern::List(elems) => {
            for el in elems {
                match el {
                    DestructElem::Pat(p) => collect_destruct_names(p, out),
                    DestructElem::Rest(n) => out.push(n.clone()),
                    DestructElem::RestDiscard => {}
                }
            }
        }
    }
}

pub(super) fn const_default_value(expr: &Expr) -> Option<Value> {
    match &expr.kind {
        ExprKind::None => Some(Value::None),
        ExprKind::Bool(b) => Some(Value::Bool(*b)),
        ExprKind::String(s) => Some(Value::Text(s.clone())),
        ExprKind::Number(s) => Num::from_literal(s).ok().map(Value::Num),
        _ => None,
    }
}

/// 识别 `sleep(secs)` / `std.time.sleep(secs)` 等单参数 sleep 调用，返回秒数表达式。
pub(super) fn select_sleep_seconds_expr(event: &Expr) -> Option<&Expr> {
    let ExprKind::Call { callee, args } = &event.kind else {
        return None;
    };
    if args.len() != 1 || args[0].name.is_some() || args[0].is_splat || args[0].is_kwsplat {
        return None;
    }
    if call_ends_with_sleep(callee) {
        Some(&args[0].value)
    } else {
        None
    }
}

fn call_ends_with_sleep(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Var(name) => name == "sleep",
        ExprKind::Member { field, .. } => field == "sleep",
        _ => false,
    }
}
