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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy)]
pub struct DiagnosticMetadata {
    pub severity: Severity,
    pub code: &'static str,
    pub help: Option<&'static str>,
}

#[must_use]
pub fn diagnostic_metadata(message: &str) -> DiagnosticMetadata {
    let (severity, code, help) = if message.starts_with("unused import") {
        (
            Severity::Warning,
            "W1001",
            Some("remove the import or prefix its local name with '_'"),
        )
    } else if message.starts_with("unused variable") {
        (
            Severity::Warning,
            "W1002",
            Some("remove the binding or prefix its name with '_'"),
        )
    } else if message == "unreachable code" {
        (
            Severity::Warning,
            "W1003",
            Some("remove this code or change the preceding control flow"),
        )
    } else if message.contains("captures and assigns shared") {
        (
            Severity::Warning,
            "W2001",
            Some("use Channel, Mutex, or Atomic for shared mutation"),
        )
    } else if message.starts_with("cannot resolve module") {
        (
            Severity::Error,
            "E2001",
            Some("check the module path and project dependencies"),
        )
    } else if message.starts_with("unknown export") {
        (Severity::Error, "E2002", Some("check the exported name"))
    } else if message.starts_with("undefined name") {
        (
            Severity::Error,
            "E1001",
            Some("declare or import this name before using it"),
        )
    } else if message.contains("expects") && message.contains("argument") {
        (Severity::Error, "E3001", Some("adjust the call arguments"))
    } else if message.contains("duplicate parameter")
        || message.contains("duplicate named argument")
        || message.contains("variadic parameter")
        || message.contains("missing required argument")
        || message.contains("no named argument")
        || message.contains("multiple values for argument")
        || message.contains("positional argument cannot follow")
    {
        (
            Severity::Error,
            "E3002",
            Some("give each parameter or named argument a unique role"),
        )
    } else if message.starts_with("duplicate ") {
        (
            Severity::Error,
            "E3003",
            Some("rename or remove the duplicate declaration"),
        )
    } else if message.contains("hard type") || message.contains("hard variable") {
        (Severity::Error, "E4001", Some("use a compatible value"))
    } else if message.contains("immutable binding") {
        (
            Severity::Error,
            "E4002",
            Some("declare the binding with var if reassignment is required"),
        )
    } else if message.contains("constant zero") || message.contains("out of bounds") {
        (
            Severity::Error,
            "E7001",
            Some("change the constant expression before running the program"),
        )
    } else if message.contains('?') {
        (
            Severity::Error,
            "E5001",
            Some("use ? only with the matching Result or Option return channel"),
        )
    } else if message.contains("only valid") || message.contains("cannot leave a defer") {
        (
            Severity::Error,
            "E6001",
            Some("move this statement to a valid scope"),
        )
    } else {
        (Severity::Error, "E0001", None)
    };
    DiagnosticMetadata {
        severity,
        code,
        help,
    }
}

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
    check_try_block(&program.stmts, None, false, diags);
    check_control_flow(&program.stmts, 0, 0, false, diags);
    check_strong_assignments(&program.stmts, &mut HashMap::new(), diags);
    check_immutable_block(&program.stmts, &HashMap::new(), diags);
}

fn check_immutable_block(
    block: &Block,
    inherited: &HashMap<String, bool>,
    diags: &mut Vec<Diagnostic>,
) {
    let mut bindings = inherited.clone();
    for located in block {
        match &located.stmt {
            Stmt::VarDecl {
                name,
                is_const,
                is_var,
                ..
            } => {
                bindings.insert(name.clone(), *is_const || !*is_var);
            }
            Stmt::DestructDecl {
                pattern,
                is_const,
                is_var,
                ..
            } => {
                for name in destruct_binding_names(pattern) {
                    bindings.insert(name, *is_const || !*is_var);
                }
            }
            _ => {}
        }
    }
    // Ordinary statements only see bindings declared before them.  Keep the
    // pre-collected map above for function bodies, whose names are analyzed as
    // a whole scope and may capture a binding declared later in that scope.
    let mut visible = inherited.clone();
    for located in block {
        match &located.stmt {
            Stmt::Assign {
                target: LValue::Name(name),
                ..
            } => {
                check_immutable_name(name, located.line, located.column, &visible, diags);
            }
            Stmt::DestructAssign { pattern, .. } => {
                for name in destruct_binding_names(pattern) {
                    check_immutable_name(&name, located.line, located.column, &visible, diags);
                }
            }
            Stmt::FuncDecl { params, body, .. } => {
                let mut inner = bindings.clone();
                for param in params {
                    inner.insert(param.name.clone(), false);
                }
                check_immutable_block(body, &inner, diags);
            }
            Stmt::FriendFuncDecl {
                params,
                body: Some(body),
                ..
            } => {
                let mut inner = bindings.clone();
                if let Some(params) = params {
                    for param in params {
                        inner.insert(param.name.clone(), false);
                    }
                }
                check_immutable_block(body, &inner, diags);
            }
            Stmt::If {
                then_block,
                elifs,
                else_block,
                ..
            } => {
                check_immutable_block(then_block, &visible, diags);
                for (_, body) in elifs {
                    check_immutable_block(body, &visible, diags);
                }
                if let Some(body) = else_block {
                    check_immutable_block(body, &visible, diags);
                }
            }
            Stmt::While { body, .. }
            | Stmt::Loop { body, .. }
            | Stmt::With { body, .. }
            | Stmt::Block(body)
            | Stmt::Defer(body) => check_immutable_block(body, &visible, diags),
            Stmt::For { items, body, .. } => {
                let mut inner = visible.clone();
                for item in items {
                    inner.insert(item.name.clone(), false);
                }
                check_immutable_block(body, &inner, diags);
            }
            Stmt::Try {
                body,
                catches,
                else_block,
            } => {
                check_immutable_block(body, &visible, diags);
                for catch in catches {
                    check_immutable_block(&catch.body, &visible, diags);
                }
                if let Some(body) = else_block {
                    check_immutable_block(body, &visible, diags);
                }
            }
            Stmt::Match {
                cases, else_block, ..
            } => {
                for case in cases {
                    check_immutable_block(&case.body, &visible, diags);
                }
                if let Some(body) = else_block {
                    check_immutable_block(body, &visible, diags);
                }
            }
            Stmt::StructDecl { methods, .. } => {
                for method in methods {
                    let mut inner = bindings.clone();
                    inner.insert("self".into(), false);
                    for param in &method.params {
                        inner.insert(param.name.clone(), false);
                    }
                    check_immutable_block(&method.body, &inner, diags);
                }
            }
            Stmt::EnumDecl { methods, .. } => {
                for method in methods {
                    let mut inner = bindings.clone();
                    inner.insert("self".into(), false);
                    for param in &method.params {
                        inner.insert(param.name.clone(), false);
                    }
                    check_immutable_block(&method.body, &inner, diags);
                }
            }
            _ => {}
        }
        match &located.stmt {
            Stmt::VarDecl {
                name,
                is_const,
                is_var,
                ..
            } => {
                visible.insert(name.clone(), *is_const || !*is_var);
            }
            Stmt::DestructDecl {
                pattern,
                is_const,
                is_var,
                ..
            } => {
                for name in destruct_binding_names(pattern) {
                    visible.insert(name, *is_const || !*is_var);
                }
            }
            _ => {}
        }
    }
}

fn check_immutable_name(
    name: &str,
    line: usize,
    column: usize,
    bindings: &HashMap<String, bool>,
    diags: &mut Vec<Diagnostic>,
) {
    if bindings.get(name).copied() == Some(true) {
        diags.push((
            line,
            column,
            format!("cannot assign to immutable binding `{name}`"),
        ));
    }
}

fn destruct_binding_names(pattern: &crate::ast::DestructPattern) -> Vec<String> {
    use crate::ast::{DestructElem, DestructPattern};
    match pattern {
        DestructPattern::Name(name) => vec![name.clone()],
        DestructPattern::Discard => Vec::new(),
        DestructPattern::Tuple(items) | DestructPattern::List(items) => items
            .iter()
            .flat_map(|item| match item {
                DestructElem::Pat(pattern) => destruct_binding_names(pattern),
                DestructElem::Rest(name) => vec![name.clone()],
                DestructElem::RestDiscard => Vec::new(),
            })
            .collect(),
    }
}

fn check_strong_assignments(
    block: &Block,
    bindings: &mut HashMap<String, String>,
    diags: &mut Vec<Diagnostic>,
) {
    for located in block {
        match &located.stmt {
            Stmt::VarDecl {
                name,
                type_expr: Some(ty),
                type_strong: true,
                ..
            } => {
                if let Some(name_ty) = type_ann_name(ty) {
                    bindings.insert(name.clone(), name_ty);
                }
            }
            Stmt::Assign {
                target: LValue::Name(name),
                value,
            } => {
                if let (Some(expected), Some(actual)) =
                    (bindings.get(name), literal_type_name(value))
                {
                    if !types_compatible(expected, actual) {
                        diags.push((
                            value.loc.line,
                            value.loc.column,
                            format!(
                                "cannot assign {actual} to hard variable {name} of type {expected}"
                            ),
                        ));
                    }
                }
            }
            Stmt::FuncDecl { params, body, .. } => {
                let mut inner = HashMap::new();
                bind_strong_params(params, &mut inner);
                check_strong_assignments(body, &mut inner, diags);
            }
            Stmt::FriendFuncDecl {
                params: Some(params),
                body: Some(body),
                ..
            } => {
                let mut inner = HashMap::new();
                bind_strong_params(params, &mut inner);
                check_strong_assignments(body, &mut inner, diags);
            }
            Stmt::If {
                then_block,
                elifs,
                else_block,
                ..
            } => {
                check_strong_child(then_block, bindings, diags);
                for (_, body) in elifs {
                    check_strong_child(body, bindings, diags);
                }
                if let Some(body) = else_block {
                    check_strong_child(body, bindings, diags);
                }
            }
            Stmt::While { body, .. }
            | Stmt::Loop { body, .. }
            | Stmt::For { body, .. }
            | Stmt::With { body, .. }
            | Stmt::Block(body)
            | Stmt::Defer(body) => check_strong_child(body, bindings, diags),
            Stmt::Try {
                body,
                catches,
                else_block,
            } => {
                check_strong_child(body, bindings, diags);
                for catch in catches {
                    check_strong_child(&catch.body, bindings, diags);
                }
                if let Some(body) = else_block {
                    check_strong_child(body, bindings, diags);
                }
            }
            Stmt::Match {
                cases, else_block, ..
            } => {
                for case in cases {
                    check_strong_child(&case.body, bindings, diags);
                }
                if let Some(body) = else_block {
                    check_strong_child(body, bindings, diags);
                }
            }
            Stmt::StructDecl { methods, .. } => {
                for method in methods {
                    let mut inner = HashMap::new();
                    bind_strong_params(&method.params, &mut inner);
                    check_strong_assignments(&method.body, &mut inner, diags);
                }
            }
            Stmt::EnumDecl { methods, .. } => {
                for method in methods {
                    let mut inner = HashMap::new();
                    bind_strong_params(&method.params, &mut inner);
                    check_strong_assignments(&method.body, &mut inner, diags);
                }
            }
            _ => {}
        }
    }
}

fn check_strong_child(body: &Block, parent: &HashMap<String, String>, diags: &mut Vec<Diagnostic>) {
    let mut child = parent.clone();
    check_strong_assignments(body, &mut child, diags);
}

fn bind_strong_params(params: &[crate::ast::FuncParam], bindings: &mut HashMap<String, String>) {
    for param in params {
        if param.type_strong {
            if let Some(ty) = param.type_expr.as_ref().and_then(type_ann_name) {
                bindings.insert(param.name.clone(), ty);
            }
        }
    }
}

fn check_control_flow(
    block: &Block,
    function_depth: usize,
    loop_depth: usize,
    in_defer: bool,
    diags: &mut Vec<Diagnostic>,
) {
    for located in block {
        let invalid_defer_exit = in_defer
            && matches!(
                located.stmt,
                Stmt::Return(_)
                    | Stmt::Yield(_)
                    | Stmt::YieldFrom(_)
                    | Stmt::Break(_)
                    | Stmt::Continue(_)
            );
        if invalid_defer_exit {
            diags.push((
                located.line,
                located.column,
                "control flow cannot leave a defer body".into(),
            ));
            continue;
        }
        match &located.stmt {
            Stmt::Return(_) if function_depth == 0 => diags.push((
                located.line,
                located.column,
                "return is only valid inside a function".into(),
            )),
            Stmt::Yield(_) | Stmt::YieldFrom(_) if function_depth == 0 => diags.push((
                located.line,
                located.column,
                "yield is only valid inside a function".into(),
            )),
            Stmt::Break(_) | Stmt::Continue(_) if loop_depth == 0 => diags.push((
                located.line,
                located.column,
                "loop control is only valid inside a loop".into(),
            )),
            Stmt::FuncDecl { body, .. } | Stmt::MacroDecl { body, .. } => {
                check_control_flow(body, function_depth + 1, 0, false, diags);
            }
            Stmt::FriendFuncDecl {
                body: Some(body), ..
            } => check_control_flow(body, function_depth + 1, 0, false, diags),
            Stmt::If {
                then_block,
                elifs,
                else_block,
                ..
            } => {
                check_control_flow(then_block, function_depth, loop_depth, in_defer, diags);
                for (_, body) in elifs {
                    check_control_flow(body, function_depth, loop_depth, in_defer, diags);
                }
                if let Some(body) = else_block {
                    check_control_flow(body, function_depth, loop_depth, in_defer, diags);
                }
            }
            Stmt::While { body, .. } | Stmt::Loop { body, .. } | Stmt::For { body, .. } => {
                check_control_flow(body, function_depth, loop_depth + 1, in_defer, diags);
            }
            Stmt::Defer(body) => check_control_flow(body, function_depth, loop_depth, true, diags),
            Stmt::Try {
                body,
                catches,
                else_block,
            } => {
                check_control_flow(body, function_depth, loop_depth, in_defer, diags);
                for catch in catches {
                    check_control_flow(&catch.body, function_depth, loop_depth, in_defer, diags);
                }
                if let Some(body) = else_block {
                    check_control_flow(body, function_depth, loop_depth, in_defer, diags);
                }
            }
            Stmt::Match {
                cases, else_block, ..
            } => {
                for case in cases {
                    check_control_flow(&case.body, function_depth, loop_depth, in_defer, diags);
                }
                if let Some(body) = else_block {
                    check_control_flow(body, function_depth, loop_depth, in_defer, diags);
                }
            }
            Stmt::With { body, .. } | Stmt::Block(body) => {
                check_control_flow(body, function_depth, loop_depth, in_defer, diags);
            }
            Stmt::StructDecl { methods, .. } => {
                for method in methods {
                    check_control_flow(&method.body, function_depth + 1, 0, false, diags);
                }
            }
            Stmt::EnumDecl { methods, .. } => {
                for method in methods {
                    check_control_flow(&method.body, function_depth + 1, 0, false, diags);
                }
            }
            _ => {}
        }
    }
}

fn check_try_block(
    block: &Block,
    return_channel: Option<&str>,
    generator: bool,
    diags: &mut Vec<Diagnostic>,
) {
    for located in block {
        match &located.stmt {
            Stmt::FuncDecl {
                return_type,
                body,
                is_generator,
                ..
            } => check_try_block(
                body,
                return_type.as_ref().and_then(try_type_channel),
                *is_generator,
                diags,
            ),
            Stmt::FriendFuncDecl {
                return_type,
                body: Some(body),
                ..
            } => check_try_block(
                body,
                return_type.as_ref().and_then(try_type_channel),
                false,
                diags,
            ),
            Stmt::Return(Some(expr))
            | Stmt::Yield(Some(expr))
            | Stmt::YieldFrom(expr)
            | Stmt::Throw(expr)
            | Stmt::Expr(expr)
            | Stmt::DestructDecl { init: expr, .. }
            | Stmt::DestructAssign { value: expr, .. } => {
                check_try_expr(expr, return_channel, generator, diags);
            }
            Stmt::Assign { target, value } => {
                check_try_lvalue(target, return_channel, generator, diags);
                check_try_expr(value, return_channel, generator, diags);
            }
            Stmt::Del(target) => {
                check_try_del_target(target, return_channel, generator, diags);
            }
            Stmt::VarDecl {
                init: Some(expr), ..
            } => {
                check_try_expr(expr, return_channel, generator, diags);
            }
            Stmt::If {
                cond,
                then_block,
                elifs,
                else_block,
            } => {
                check_try_expr(cond, return_channel, generator, diags);
                check_try_block(then_block, return_channel, generator, diags);
                for (cond, body) in elifs {
                    check_try_expr(cond, return_channel, generator, diags);
                    check_try_block(body, return_channel, generator, diags);
                }
                if let Some(body) = else_block {
                    check_try_block(body, return_channel, generator, diags);
                }
            }
            Stmt::While { cond, body, .. } => {
                check_try_expr(cond, return_channel, generator, diags);
                check_try_block(body, return_channel, generator, diags);
            }
            Stmt::Loop { count, body, .. } => {
                if let Some(count) = count {
                    check_try_expr(count, return_channel, generator, diags);
                }
                check_try_block(body, return_channel, generator, diags);
            }
            Stmt::For { items, body, .. } => {
                for item in items {
                    check_try_expr(&item.iterable, return_channel, generator, diags);
                }
                check_try_block(body, return_channel, generator, diags);
            }
            Stmt::Defer(body) | Stmt::Block(body) => {
                check_try_block(body, return_channel, generator, diags);
            }
            Stmt::Try {
                body,
                catches,
                else_block,
            } => {
                check_try_block(body, return_channel, generator, diags);
                for catch in catches {
                    check_try_block(&catch.body, return_channel, generator, diags);
                }
                if let Some(body) = else_block {
                    check_try_block(body, return_channel, generator, diags);
                }
            }
            Stmt::Match {
                subject,
                cases,
                else_block,
            } => {
                check_try_expr(subject, return_channel, generator, diags);
                for case in cases {
                    check_try_block(&case.body, return_channel, generator, diags);
                }
                if let Some(body) = else_block {
                    check_try_block(body, return_channel, generator, diags);
                }
            }
            Stmt::With { context, body, .. } => {
                check_try_expr(context, return_channel, generator, diags);
                check_try_block(body, return_channel, generator, diags);
            }
            Stmt::StructDecl { methods, .. } => {
                for method in methods {
                    check_try_block(
                        &method.body,
                        method.return_type.as_ref().and_then(try_type_channel),
                        false,
                        diags,
                    );
                }
            }
            Stmt::EnumDecl { methods, .. } => {
                for method in methods {
                    check_try_block(&method.body, None, false, diags);
                }
            }
            Stmt::MacroDecl { body, .. } => check_try_block(body, None, false, diags),
            _ => {}
        }
    }
}

fn check_try_lvalue(
    target: &LValue,
    return_channel: Option<&str>,
    generator: bool,
    diags: &mut Vec<Diagnostic>,
) {
    match target {
        LValue::Name(_) => {}
        LValue::Member { object, .. } => {
            check_try_expr(object, return_channel, generator, diags);
        }
        LValue::Index { object, index } => {
            check_try_expr(object, return_channel, generator, diags);
            check_try_expr(index, return_channel, generator, diags);
        }
        LValue::Slice {
            object,
            start,
            end,
            step,
        } => {
            check_try_expr(object, return_channel, generator, diags);
            for part in [start, end, step].into_iter().flatten() {
                check_try_expr(part, return_channel, generator, diags);
            }
        }
    }
}

fn check_try_del_target(
    target: &crate::ast::DelTarget,
    return_channel: Option<&str>,
    generator: bool,
    diags: &mut Vec<Diagnostic>,
) {
    match target {
        crate::ast::DelTarget::Name(_) => {}
        crate::ast::DelTarget::Member { object, .. } => {
            check_try_expr(object, return_channel, generator, diags);
        }
        crate::ast::DelTarget::Index { object, index } => {
            check_try_expr(object, return_channel, generator, diags);
            check_try_expr(index, return_channel, generator, diags);
        }
    }
}

fn try_type_channel(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Var(name) => {
            let leaf = name.rsplit('.').next().unwrap_or(name);
            matches!(leaf, "Result" | "Option").then_some(leaf)
        }
        ExprKind::Member { field, .. } if matches!(field.as_str(), "Result" | "Option") => {
            Some(field)
        }
        _ => None,
    }
}

fn constructor_channel(expr: &Expr) -> Option<&str> {
    let ExprKind::Call { callee, .. } = &expr.kind else {
        return None;
    };
    let ExprKind::Member { object, .. } = &callee.kind else {
        return None;
    };
    try_type_channel(object)
}

fn check_try_expr(
    expr: &Expr,
    return_channel: Option<&str>,
    generator: bool,
    diags: &mut Vec<Diagnostic>,
) {
    if let ExprKind::TryPropagate { operand } = &expr.kind {
        let message = if generator {
            Some("? is not valid in a generator".to_string())
        } else if return_channel.is_none() {
            Some("? requires an enclosing function returning Result or Option".to_string())
        } else if let Some(actual) = constructor_channel(operand) {
            (Some(actual) != return_channel).then(|| {
                format!(
                    "cannot propagate {actual} from a function returning {}",
                    return_channel.unwrap_or_default()
                )
            })
        } else {
            literal_type_name(operand)
                .map(|actual| format!("? operand has type {actual}; expected Result or Option"))
        };
        if let Some(message) = message {
            diags.push((expr.loc.line, expr.loc.column, message));
        }
    }

    match &expr.kind {
        ExprKind::TryPropagate { operand }
        | ExprKind::Unary { operand, .. }
        | ExprKind::Handle { operand }
        | ExprKind::Go { operand }
        | ExprKind::Snap { operand }
        | ExprKind::Await { operand } => check_try_expr(operand, return_channel, generator, diags),
        ExprKind::Binary { left, right, .. } | ExprKind::Pipeline { left, right, .. } => {
            check_try_expr(left, return_channel, generator, diags);
            check_try_expr(right, return_channel, generator, diags);
        }
        ExprKind::Call { callee, args } => {
            check_try_expr(callee, return_channel, generator, diags);
            for arg in args {
                check_try_expr(&arg.value, return_channel, generator, diags);
            }
        }
        ExprKind::Member { object, .. } => check_try_expr(object, return_channel, generator, diags),
        ExprKind::Index { object, index } => {
            check_try_expr(object, return_channel, generator, diags);
            check_try_expr(index, return_channel, generator, diags);
        }
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => {
            check_try_expr(object, return_channel, generator, diags);
            for part in [start, end, step].into_iter().flatten() {
                check_try_expr(part, return_channel, generator, diags);
            }
        }
        ExprKind::TypeConvert { type_expr, value } => {
            check_try_expr(type_expr, return_channel, generator, diags);
            check_try_expr(value, return_channel, generator, diags);
        }
        ExprKind::List(items)
        | ExprKind::Tuple(items)
        | ExprKind::Set(items)
        | ExprKind::ParBlock { exprs: items } => {
            for item in items {
                check_try_expr(item, return_channel, generator, diags);
            }
        }
        ExprKind::Dict(items) => {
            for (key, value) in items {
                check_try_expr(key, return_channel, generator, diags);
                check_try_expr(value, return_channel, generator, diags);
            }
        }
        ExprKind::FString(parts) => {
            for part in parts {
                if let crate::ast::FStringPart::Expr(value) = part {
                    check_try_expr(value, return_channel, generator, diags);
                }
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
            check_try_expr(elem, return_channel, generator, diags);
            for item in items {
                check_try_expr(&item.iterable, return_channel, generator, diags);
            }
            for guard in guards {
                check_try_expr(guard, return_channel, generator, diags);
            }
        }
        ExprKind::DictComp {
            key,
            value,
            items,
            guards,
        } => {
            check_try_expr(key, return_channel, generator, diags);
            check_try_expr(value, return_channel, generator, diags);
            for item in items {
                check_try_expr(&item.iterable, return_channel, generator, diags);
            }
            for guard in guards {
                check_try_expr(guard, return_channel, generator, diags);
            }
        }
        ExprKind::ParFor { items, body } => {
            for item in items {
                check_try_expr(&item.iterable, return_channel, generator, diags);
            }
            check_try_block(body, return_channel, generator, diags);
        }
        ExprKind::Select { cases, else_block } => {
            for case in cases {
                check_try_expr(&case.event, return_channel, generator, diags);
                check_try_block(&case.body, return_channel, generator, diags);
            }
            if let Some(body) = else_block {
                check_try_block(body, return_channel, generator, diags);
            }
        }
        ExprKind::Quote { bindings, body, .. } => {
            for binding in bindings {
                check_try_expr(binding, return_channel, generator, diags);
            }
            check_try_block(body, None, false, diags);
        }
        ExprKind::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            check_try_expr(cond, return_channel, generator, diags);
            check_try_expr(then_expr, return_channel, generator, diags);
            check_try_expr(else_expr, return_channel, generator, diags);
        }
        ExprKind::NamedAssign { value, .. } => {
            check_try_expr(value, return_channel, generator, diags);
        }
        ExprKind::DoFunc {
            return_type, body, ..
        } => check_try_block(
            body,
            return_type.as_deref().and_then(try_type_channel),
            crate::ast::block_has_yield(body),
            diags,
        ),
        ExprKind::Match {
            subject,
            cases,
            else_block,
        } => {
            check_try_expr(subject, return_channel, generator, diags);
            for case in cases {
                check_try_block(&case.body, return_channel, generator, diags);
            }
            if let Some(body) = else_block {
                check_try_block(body, return_channel, generator, diags);
            }
        }
        _ => {}
    }
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
        Stmt::While { cond, body, .. } => {
            walk_expr_uses(cond, used);
            walk_block_uses(body, used);
        }
        Stmt::Loop { count, body, .. } => {
            if let Some(c) = count {
                walk_expr_uses(c, used);
            }
            walk_block_uses(body, used);
        }
        Stmt::For { items, body, .. } => {
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
        Stmt::Block(b) | Stmt::Defer(b) => walk_block_uses(b, used),
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
        | Stmt::Break(_)
        | Stmt::Continue(_)
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
        | ExprKind::Await { operand }
        | ExprKind::TryPropagate { operand } => walk_expr_uses(operand, used),
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
            Stmt::Return(_) | Stmt::Throw(_) | Stmt::Break(_) | Stmt::Continue(_) => dead = true,
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
            Stmt::FriendFuncDecl { body: Some(b), .. } => {
                unreachable_in_block(b, diags);
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
        if stmt_definitely_exits(&st.stmt) {
            dead = true;
        }
    }
}

fn stmt_definitely_exits(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return(_) | Stmt::Throw(_) | Stmt::Break(_) | Stmt::Continue(_) => true,
        Stmt::Block(body) => block_definitely_exits(body),
        Stmt::If {
            then_block,
            elifs,
            else_block: Some(else_block),
            ..
        } => {
            block_definitely_exits(then_block)
                && elifs.iter().all(|(_, body)| block_definitely_exits(body))
                && block_definitely_exits(else_block)
        }
        Stmt::Match {
            cases,
            else_block: Some(else_block),
            ..
        } => {
            !cases.is_empty()
                && cases.iter().all(|case| block_definitely_exits(&case.body))
                && block_definitely_exits(else_block)
        }
        _ => false,
    }
}

fn block_definitely_exits(body: &Block) -> bool {
    body.iter()
        .rev()
        .find(|located| !matches!(located.stmt, Stmt::Comment { .. }))
        .is_some_and(|located| stmt_definitely_exits(&located.stmt))
}

fn walk_block_extra(
    stmts: &Block,
    outer: &HashSet<String>,
    types: &HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    check_duplicate_block_bindings(stmts, diags);
    let mut scope = outer.clone();
    for st in stmts {
        bind_stmt_names(&st.stmt, &mut scope);
    }
    for st in stmts {
        walk_stmt_extra(st, &scope, types, diags);
    }
}

fn check_duplicate_block_bindings(stmts: &Block, diags: &mut Vec<Diagnostic>) {
    let mut seen = HashSet::new();
    for located in stmts {
        for name in statement_binding_names(&located.stmt) {
            if !seen.insert(name.clone()) {
                diags.push((
                    located.line,
                    located.column,
                    format!("duplicate binding `{name}` in the same scope"),
                ));
            }
        }
    }
}

fn statement_binding_names(stmt: &Stmt) -> Vec<String> {
    match stmt {
        Stmt::VarDecl { name, .. }
        | Stmt::FuncDecl { name, .. }
        | Stmt::FriendFuncDecl { name, .. }
        | Stmt::StructDecl { name, .. }
        | Stmt::EnumDecl { name, .. }
        | Stmt::VariantDecl { name, .. }
        | Stmt::ProtocolDecl { name, .. }
        | Stmt::MacroDecl { name, .. } => vec![name.clone()],
        Stmt::DestructDecl { pattern, .. } => destruct_binding_names(pattern),
        Stmt::Import { path, alias, .. } => {
            let local = alias.as_deref().unwrap_or(path);
            vec![local.rsplit('.').next().unwrap_or(local).to_string()]
        }
        Stmt::Use { items, .. } => items
            .iter()
            .map(|item| item.alias.as_deref().unwrap_or(&item.name).to_string())
            .collect(),
        _ => Vec::new(),
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
            if let Some(type_expr) = type_expr {
                walk_expr_extra(type_expr, scope, types, diags);
            }
        }
        Stmt::DestructDecl { init, .. } | Stmt::DestructAssign { value: init, .. } => {
            walk_expr_extra(init, scope, types, diags);
        }
        Stmt::Assign { target, value } => {
            walk_lvalue_extra(target, scope, types, diags);
            walk_expr_extra(value, scope, types, diags);
        }
        Stmt::FuncDecl {
            decorators,
            params,
            body,
            return_type,
            return_wrapper,
            return_strong,
            type_params,
            ..
        } => {
            check_parameter_list(params, st.line, st.column, diags);
            check_type_params(type_params, types, st.line, st.column, diags);
            let mut inner = scope.clone();
            for p in params {
                inner.insert(p.name.clone());
                if let Some(type_expr) = &p.type_expr {
                    walk_expr_extra(type_expr, scope, types, diags);
                }
                if let Some(default) = &p.default_expr {
                    walk_expr_extra(default, scope, types, diags);
                }
            }
            for decorator in decorators {
                walk_expr_extra(decorator, scope, types, diags);
            }
            if let Some(return_type) = return_type {
                walk_expr_extra(return_type, scope, types, diags);
            }
            if let Some(return_wrapper) = return_wrapper {
                walk_expr_extra(return_wrapper, scope, types, diags);
            }
            unused_in_function(params, body, diags);
            if *return_strong {
                if let Some(rt) = return_type {
                    check_hard_returns(rt, body, diags);
                }
            }
            walk_block_extra(body, &inner, types, diags);
        }
        Stmt::FriendFuncDecl {
            params: Some(params),
            body,
            return_type,
            return_wrapper,
            ..
        } => {
            check_parameter_list(params, st.line, st.column, diags);
            for param in params {
                if let Some(type_expr) = &param.type_expr {
                    walk_expr_extra(type_expr, scope, types, diags);
                }
                if let Some(default) = &param.default_expr {
                    walk_expr_extra(default, scope, types, diags);
                }
            }
            if let Some(return_type) = return_type {
                walk_expr_extra(return_type, scope, types, diags);
            }
            if let Some(return_wrapper) = return_wrapper {
                walk_expr_extra(return_wrapper, scope, types, diags);
            }
            if let Some(body) = body {
                let mut inner = scope.clone();
                for param in params {
                    inner.insert(param.name.clone());
                }
                unused_in_function(params, body, diags);
                walk_block_extra(body, &inner, types, diags);
            }
        }
        Stmt::Return(Some(e)) | Stmt::Yield(Some(e)) => {
            walk_expr_extra(e, scope, types, diags);
        }
        Stmt::YieldFrom(e) | Stmt::Throw(e) | Stmt::Expr(e) => {
            walk_expr_extra(e, scope, types, diags);
        }
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
        Stmt::While { cond, body, .. } => {
            walk_expr_extra(cond, scope, types, diags);
            walk_block_extra(body, scope, types, diags);
        }
        Stmt::Loop { count, body, .. } => {
            if let Some(c) = count {
                walk_expr_extra(c, scope, types, diags);
            }
            walk_block_extra(body, scope, types, diags);
        }
        Stmt::For { items, body, .. } => {
            let mut inner = scope.clone();
            for item in items {
                walk_expr_extra(&item.iterable, scope, types, diags);
                inner.insert(item.name.clone());
            }
            walk_block_extra(body, &inner, types, diags);
        }
        Stmt::Defer(body) => walk_block_extra(body, scope, types, diags),
        Stmt::Try {
            body,
            catches,
            else_block,
        } => {
            walk_block_extra(body, scope, types, diags);
            for c in catches {
                let mut inner = scope.clone();
                if let crate::ast::CatchPattern::Bind { name, .. } = &c.pattern {
                    inner.insert(name.clone());
                }
                walk_block_extra(&c.body, &inner, types, diags);
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
                walk_pattern_extra(&c.pattern, scope, types, diags);
                let mut inner = scope.clone();
                for name in pattern_binding_names(&c.pattern) {
                    inner.insert(name);
                }
                walk_block_extra(&c.body, &inner, types, diags);
            }
            if let Some(b) = else_block {
                walk_block_extra(b, scope, types, diags);
            }
        }
        Stmt::With { context, body, .. } => {
            walk_expr_extra(context, scope, types, diags);
            let mut inner = scope.clone();
            if let Stmt::With {
                alias: Some(pattern),
                ..
            } = &st.stmt
            {
                inner.extend(destruct_binding_names(pattern));
            }
            walk_block_extra(body, &inner, types, diags);
        }
        Stmt::Block(b) => walk_block_extra(b, scope, types, diags),
        Stmt::StructDecl {
            type_params,
            fields,
            methods,
            layout,
            ..
        } => {
            check_type_params(type_params, types, st.line, st.column, diags);
            check_duplicate_names(
                "struct field",
                fields.iter().map(|field| field.name.as_str()),
                st.line,
                st.column,
                diags,
            );
            // Repeated overload methods intentionally form a dispatch set.
            // Only ordinary methods overwrite one another silently.
            check_duplicate_names(
                "struct method",
                methods
                    .iter()
                    .filter(|method| !method.overload)
                    .map(|method| method.name.as_str()),
                st.line,
                st.column,
                diags,
            );
            for (_, bound) in type_params {
                if let Some(bound) = bound {
                    walk_expr_extra(bound, scope, types, diags);
                }
            }
            if let Some(layout) = layout {
                walk_expr_extra(layout, scope, types, diags);
            }
            for field in fields {
                if let Some(type_expr) = &field.type_expr {
                    walk_expr_extra(type_expr, scope, types, diags);
                }
                if let Some(default) = &field.default_expr {
                    walk_expr_extra(default, scope, types, diags);
                }
            }
            for m in methods {
                check_parameter_list(&m.params, st.line, st.column, diags);
                let mut inner = scope.clone();
                inner.insert("self".into());
                for param in &m.params {
                    inner.insert(param.name.clone());
                    if let Some(type_expr) = &param.type_expr {
                        walk_expr_extra(type_expr, scope, types, diags);
                    }
                    if let Some(default) = &param.default_expr {
                        walk_expr_extra(default, scope, types, diags);
                    }
                }
                if let Some(return_type) = &m.return_type {
                    walk_expr_extra(return_type, scope, types, diags);
                }
                if let Some(return_wrapper) = &m.return_wrapper {
                    walk_expr_extra(return_wrapper, scope, types, diags);
                }
                walk_block_extra(&m.body, &inner, types, diags);
            }
        }
        Stmt::EnumDecl {
            members, methods, ..
        } => {
            check_duplicate_names(
                "enum member",
                members
                    .iter()
                    .map(|member| member.name.as_str())
                    .chain(methods.iter().map(|method| method.name.as_str())),
                st.line,
                st.column,
                diags,
            );
            for member in members {
                if let Some(value) = &member.value {
                    walk_expr_extra(value, scope, types, diags);
                }
            }
            for method in methods {
                check_parameter_list(&method.params, st.line, st.column, diags);
                let mut inner = scope.clone();
                inner.insert("self".into());
                for param in &method.params {
                    inner.insert(param.name.clone());
                    if let Some(type_expr) = &param.type_expr {
                        walk_expr_extra(type_expr, scope, types, diags);
                    }
                    if let Some(default) = &param.default_expr {
                        walk_expr_extra(default, scope, types, diags);
                    }
                }
                walk_block_extra(&method.body, &inner, types, diags);
            }
        }
        Stmt::VariantDecl {
            type_params, cases, ..
        } => {
            check_type_params(type_params, types, st.line, st.column, diags);
            check_duplicate_names(
                "variant case",
                cases.iter().map(|case| case.name.as_str()),
                st.line,
                st.column,
                diags,
            );
            for case in cases {
                check_duplicate_names(
                    "variant field",
                    case.fields.iter().map(|field| field.name.as_str()),
                    st.line,
                    st.column,
                    diags,
                );
                for field in &case.fields {
                    if let Some(type_expr) = &field.type_expr {
                        walk_expr_extra(type_expr, scope, types, diags);
                    }
                    if let Some(default) = &field.default_expr {
                        walk_expr_extra(default, scope, types, diags);
                    }
                }
            }
        }
        Stmt::MacroDecl { params, body, .. } => {
            check_duplicate_names(
                "macro parameter",
                params.iter().map(|param| param.name.as_str()),
                st.line,
                st.column,
                diags,
            );
            let mut inner = scope.clone();
            for param in params {
                inner.insert(param.name.clone());
                if let Some(type_expr) = &param.type_expr {
                    walk_expr_extra(type_expr, scope, types, diags);
                }
            }
            walk_block_extra(body, &inner, types, diags);
        }
        Stmt::ProtocolDecl { members, .. } => {
            check_duplicate_names(
                "protocol member",
                members.iter().map(|member| match member {
                    crate::ast::ProtocolMember::Method { name, .. }
                    | crate::ast::ProtocolMember::Field { name, .. } => name.as_str(),
                }),
                st.line,
                st.column,
                diags,
            );
            for member in members {
                if let crate::ast::ProtocolMember::Method { params, .. } = member {
                    check_parameter_list(params, st.line, st.column, diags);
                }
            }
        }
        Stmt::Del(target) => walk_del_target_extra(target, scope, types, diags),
        _ => {}
    }
}

fn check_duplicate_names<'a>(
    kind: &str,
    names: impl IntoIterator<Item = &'a str>,
    line: usize,
    column: usize,
    diags: &mut Vec<Diagnostic>,
) {
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            diags.push((line, column, format!("duplicate {kind} `{name}`")));
        }
    }
}

fn walk_lvalue_extra(
    target: &LValue,
    scope: &HashSet<String>,
    types: &HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    match target {
        LValue::Name(_) => {}
        LValue::Member { object, .. } => walk_expr_extra(object, scope, types, diags),
        LValue::Index { object, index } => {
            walk_expr_extra(object, scope, types, diags);
            walk_expr_extra(index, scope, types, diags);
        }
        LValue::Slice {
            object,
            start,
            end,
            step,
        } => {
            walk_expr_extra(object, scope, types, diags);
            for part in [start, end, step].into_iter().flatten() {
                walk_expr_extra(part, scope, types, diags);
            }
        }
    }
}

fn walk_del_target_extra(
    target: &crate::ast::DelTarget,
    scope: &HashSet<String>,
    types: &HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    match target {
        crate::ast::DelTarget::Name(_) => {}
        crate::ast::DelTarget::Member { object, .. } => {
            walk_expr_extra(object, scope, types, diags);
        }
        crate::ast::DelTarget::Index { object, index } => {
            walk_expr_extra(object, scope, types, diags);
            walk_expr_extra(index, scope, types, diags);
        }
    }
}

fn walk_pattern_extra(
    pattern: &crate::ast::Pattern,
    scope: &HashSet<String>,
    types: &HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    use crate::ast::{Pattern, PatternElem};
    match pattern {
        Pattern::Value(value) => walk_expr_extra(value, scope, types, diags),
        Pattern::List(items) | Pattern::Tuple(items) => {
            for item in items {
                match item {
                    PatternElem::Nested(pattern) => {
                        walk_pattern_extra(pattern, scope, types, diags);
                    }
                    PatternElem::Value(value) => walk_expr_extra(value, scope, types, diags),
                    PatternElem::Bind(_) => {}
                }
            }
        }
        Pattern::Or(patterns) => {
            for pattern in patterns {
                walk_pattern_extra(pattern, scope, types, diags);
            }
        }
        Pattern::Call { args, .. } => {
            for pattern in args {
                walk_pattern_extra(pattern, scope, types, diags);
            }
        }
        Pattern::Bind(_) | Pattern::Struct { .. } => {}
    }
}

fn pattern_binding_names(pattern: &crate::ast::Pattern) -> Vec<String> {
    use crate::ast::{Pattern, PatternElem};
    match pattern {
        Pattern::Bind(name) => vec![name.clone()],
        Pattern::List(items) | Pattern::Tuple(items) => items
            .iter()
            .flat_map(|item| match item {
                PatternElem::Bind(name) => vec![name.clone()],
                PatternElem::Nested(pattern) => pattern_binding_names(pattern),
                PatternElem::Value(_) => Vec::new(),
            })
            .collect(),
        Pattern::Struct { fields, .. } => fields.clone(),
        Pattern::Or(patterns) => patterns
            .first()
            .map(pattern_binding_names)
            .unwrap_or_default(),
        Pattern::Call { args, .. } => args.iter().flat_map(pattern_binding_names).collect(),
        Pattern::Value(_) => Vec::new(),
    }
}

fn check_type_params(
    type_params: &[(String, Option<Expr>)],
    types: &HashSet<String>,
    line: usize,
    col: usize,
    diags: &mut Vec<Diagnostic>,
) {
    check_duplicate_names(
        "type parameter",
        type_params.iter().map(|(name, _)| name.as_str()),
        line,
        col,
        diags,
    );
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

fn check_parameter_list(
    params: &[crate::ast::FuncParam],
    line: usize,
    column: usize,
    diags: &mut Vec<Diagnostic>,
) {
    let mut names = HashSet::new();
    let mut variadic = 0usize;
    let mut kw_variadic = 0usize;
    for param in params {
        if !names.insert(&param.name) {
            diags.push((
                line,
                column,
                format!("duplicate parameter `{}`", param.name),
            ));
        }
        variadic += usize::from(param.is_variadic);
        kw_variadic += usize::from(param.is_kwvariadic);
    }
    if variadic > 1 {
        diags.push((
            line,
            column,
            "a function can have at most one variadic parameter".into(),
        ));
    }
    if kw_variadic > 1 {
        diags.push((
            line,
            column,
            "a function can have at most one keyword-variadic parameter".into(),
        ));
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
        match &st.stmt {
            Stmt::If {
                then_block,
                elifs,
                else_block,
                ..
            } => {
                collect_local_lets(then_block, out);
                for (_, branch) in elifs {
                    collect_local_lets(branch, out);
                }
                if let Some(branch) = else_block {
                    collect_local_lets(branch, out);
                }
            }
            Stmt::While { body, .. }
            | Stmt::Loop { body, .. }
            | Stmt::For { body, .. }
            | Stmt::With { body, .. }
            | Stmt::Block(body)
            | Stmt::Defer(body) => collect_local_lets(body, out),
            Stmt::Try {
                body,
                catches,
                else_block,
            } => {
                collect_local_lets(body, out);
                for catch in catches {
                    collect_local_lets(&catch.body, out);
                }
                if let Some(branch) = else_block {
                    collect_local_lets(branch, out);
                }
            }
            Stmt::Match {
                cases, else_block, ..
            } => {
                for case in cases {
                    collect_local_lets(&case.body, out);
                }
                if let Some(branch) = else_block {
                    collect_local_lets(branch, out);
                }
            }
            _ => {}
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
    check_hard_returns_in_block(&ann, body, diags);
}

fn check_hard_returns_in_block(ann: &str, body: &Block, diags: &mut Vec<Diagnostic>) {
    for st in body {
        match &st.stmt {
            Stmt::Return(Some(expr)) => {
                if let Some(actual) = literal_type_name(expr) {
                    if !types_compatible(ann, actual) {
                        diags.push((
                            expr.loc.line,
                            expr.loc.column,
                            format!("cannot return {actual} from hard type {ann}"),
                        ));
                    }
                }
            }
            Stmt::Return(None) if !types_compatible(ann, "nonetype") => diags.push((
                st.line,
                st.column,
                format!("bare return is incompatible with hard type {ann}"),
            )),
            Stmt::If {
                then_block,
                elifs,
                else_block,
                ..
            } => {
                check_hard_returns_in_block(ann, then_block, diags);
                for (_, branch) in elifs {
                    check_hard_returns_in_block(ann, branch, diags);
                }
                if let Some(branch) = else_block {
                    check_hard_returns_in_block(ann, branch, diags);
                }
            }
            Stmt::While { body, .. }
            | Stmt::Loop { body, .. }
            | Stmt::For { body, .. }
            | Stmt::With { body, .. }
            | Stmt::Block(body)
            | Stmt::Defer(body) => check_hard_returns_in_block(ann, body, diags),
            Stmt::Try {
                body,
                catches,
                else_block,
            } => {
                check_hard_returns_in_block(ann, body, diags);
                for catch in catches {
                    check_hard_returns_in_block(ann, &catch.body, diags);
                }
                if let Some(branch) = else_block {
                    check_hard_returns_in_block(ann, branch, diags);
                }
            }
            Stmt::Match {
                cases, else_block, ..
            } => {
                for case in cases {
                    check_hard_returns_in_block(ann, &case.body, diags);
                }
                if let Some(branch) = else_block {
                    check_hard_returns_in_block(ann, branch, diags);
                }
            }
            Stmt::FuncDecl { .. }
            | Stmt::FriendFuncDecl { .. }
            | Stmt::MacroDecl { .. }
            | Stmt::StructDecl { .. }
            | Stmt::EnumDecl { .. } => {}
            _ => {}
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
    check_constant_failure(expr, diags);
    if let ExprKind::Go { operand } = &expr.kind {
        check_go_shared(operand, scope, diags);
        walk_expr_extra(operand, scope, types, diags);
        return;
    }
    match &expr.kind {
        ExprKind::DoFunc {
            body,
            params,
            return_type,
            return_wrapper,
            ..
        } => {
            check_parameter_list(params, expr.loc.line, expr.loc.column, diags);
            let mut inner = scope.clone();
            for p in params {
                inner.insert(p.name.clone());
                if let Some(type_expr) = &p.type_expr {
                    walk_expr_extra(type_expr, scope, types, diags);
                }
                if let Some(default) = &p.default_expr {
                    walk_expr_extra(default, scope, types, diags);
                }
            }
            if let Some(return_type) = return_type {
                walk_expr_extra(return_type, scope, types, diags);
            }
            if let Some(return_wrapper) = return_wrapper {
                walk_expr_extra(return_wrapper, scope, types, diags);
            }
            walk_block_extra(body, &inner, types, diags);
        }
        ExprKind::Unary { operand, .. }
        | ExprKind::Handle { operand }
        | ExprKind::Snap { operand }
        | ExprKind::TryPropagate { operand }
        | ExprKind::Await { operand } => walk_expr_extra(operand, scope, types, diags),
        ExprKind::Binary { left, right, .. } | ExprKind::Pipeline { left, right, .. } => {
            walk_expr_extra(left, scope, types, diags);
            walk_expr_extra(right, scope, types, diags);
        }
        ExprKind::Call { callee, args } => {
            let mut named = HashSet::new();
            let mut saw_named = false;
            for arg in args {
                if let Some(name) = &arg.name {
                    saw_named = true;
                    if !named.insert(name) {
                        diags.push((
                            arg.value.loc.line,
                            arg.value.loc.column,
                            format!("duplicate named argument `{name}`"),
                        ));
                    }
                } else if saw_named && !arg.is_splat && !arg.is_kwsplat {
                    diags.push((
                        arg.value.loc.line,
                        arg.value.loc.column,
                        "positional argument cannot follow a named argument".into(),
                    ));
                }
            }
            walk_expr_extra(callee, scope, types, diags);
            for a in args {
                walk_expr_extra(&a.value, scope, types, diags);
            }
        }
        ExprKind::Member { object, .. } => walk_expr_extra(object, scope, types, diags),
        ExprKind::Index { object, index } => {
            walk_expr_extra(object, scope, types, diags);
            walk_expr_extra(index, scope, types, diags);
        }
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => {
            walk_expr_extra(object, scope, types, diags);
            for part in [start, end, step].into_iter().flatten() {
                walk_expr_extra(part, scope, types, diags);
            }
        }
        ExprKind::TypeConvert { type_expr, value } => {
            walk_expr_extra(type_expr, scope, types, diags);
            walk_expr_extra(value, scope, types, diags);
        }
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
        ExprKind::Dict(entries) => {
            for (key, value) in entries {
                walk_expr_extra(key, scope, types, diags);
                walk_expr_extra(value, scope, types, diags);
            }
        }
        ExprKind::FString(parts) => {
            for part in parts {
                if let crate::ast::FStringPart::Expr(value) = part {
                    walk_expr_extra(value, scope, types, diags);
                }
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
            let mut inner = scope.clone();
            for item in items {
                walk_expr_extra(&item.iterable, &inner, types, diags);
                inner.insert(item.name.clone());
            }
            walk_expr_extra(elem, &inner, types, diags);
            for guard in guards {
                walk_expr_extra(guard, &inner, types, diags);
            }
        }
        ExprKind::DictComp {
            key,
            value,
            items,
            guards,
        } => {
            let mut inner = scope.clone();
            for item in items {
                walk_expr_extra(&item.iterable, &inner, types, diags);
                inner.insert(item.name.clone());
            }
            walk_expr_extra(key, &inner, types, diags);
            walk_expr_extra(value, &inner, types, diags);
            for guard in guards {
                walk_expr_extra(guard, &inner, types, diags);
            }
        }
        ExprKind::NamedAssign { value, .. } => walk_expr_extra(value, scope, types, diags),
        ExprKind::ParFor { items, body } => {
            let mut inner = scope.clone();
            for item in items {
                walk_expr_extra(&item.iterable, &inner, types, diags);
                inner.insert(item.name.clone());
            }
            walk_block_extra(body, &inner, types, diags);
        }
        ExprKind::Select { cases, else_block } => {
            for case in cases {
                walk_expr_extra(&case.event, scope, types, diags);
                let mut inner = scope.clone();
                if let Some(name) = &case.bind {
                    inner.insert(name.clone());
                }
                walk_block_extra(&case.body, &inner, types, diags);
            }
            if let Some(else_block) = else_block {
                walk_block_extra(else_block, scope, types, diags);
            }
        }
        ExprKind::Quote {
            hygienic_names,
            bindings,
            body,
        } => {
            for binding in bindings {
                walk_expr_extra(binding, scope, types, diags);
            }
            let mut inner = scope.clone();
            inner.extend(hygienic_names.iter().cloned());
            walk_block_extra(body, &inner, types, diags);
        }
        ExprKind::Match {
            subject,
            cases,
            else_block,
        } => {
            walk_expr_extra(subject, scope, types, diags);
            for case in cases {
                walk_pattern_extra(&case.pattern, scope, types, diags);
                let mut inner = scope.clone();
                inner.extend(pattern_binding_names(&case.pattern));
                walk_block_extra(&case.body, &inner, types, diags);
            }
            if let Some(else_block) = else_block {
                walk_block_extra(else_block, scope, types, diags);
            }
        }
        ExprKind::MacroCall { callee, .. } => walk_expr_extra(callee, scope, types, diags),
        _ => {}
    }
}

fn check_constant_failure(expr: &Expr, diags: &mut Vec<Diagnostic>) {
    if let ExprKind::Binary { op, right, .. } = &expr.kind {
        if matches!(op, crate::ast::BinaryOp::Div | crate::ast::BinaryOp::Mod)
            && const_number(right).is_some_and(|number| number.is_zero())
        {
            diags.push((
                right.loc.line,
                right.loc.column,
                match op {
                    crate::ast::BinaryOp::Div => "division by constant zero",
                    _ => "modulo by constant zero",
                }
                .into(),
            ));
        }
    }
    if let ExprKind::Index { object, index } = &expr.kind {
        let Some(index_value) = const_number(index).and_then(|number| number.to_i64()) else {
            return;
        };
        let length = match &object.kind {
            ExprKind::List(items) | ExprKind::Tuple(items) => Some(items.len()),
            ExprKind::String(text) => Some(text.chars().count()),
            ExprKind::Bytes(bytes) => Some(bytes.len()),
            _ => None,
        };
        if let Some(length) = length {
            let length = i64::try_from(length).unwrap_or(i64::MAX);
            if index_value < -length || index_value >= length {
                diags.push((
                    index.loc.line,
                    index.loc.column,
                    format!("constant index {index_value} is out of bounds for length {length}"),
                ));
            }
        }
    }
}

fn const_number(expr: &Expr) -> Option<crate::value::Num> {
    match &expr.kind {
        ExprKind::Number(literal) => crate::value::Num::from_literal(literal).ok(),
        ExprKind::Unary {
            op: crate::ast::UnaryOp::Neg,
            operand,
        } => match const_number(operand)? {
            crate::value::Num::Small(value) => Some(crate::value::Num::Small(-value)),
            crate::value::Num::Int(value) => Some(crate::value::Num::from_bigint(-value.as_ref())),
            crate::value::Num::Rat(value) => {
                Some(crate::value::Num::from_rational(-value.as_ref()))
            }
        },
        _ => None,
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
        let (spec, items, must_resolve_locally) = match &st.stmt {
            Stmt::Use { module, items } => {
                let (spec, local) = match module {
                    ModuleRef::FilePath { path, .. } => (path.clone(), true),
                    ModuleRef::Qualified(parts) => {
                        if parts.first().map(String::as_str) == Some("std") {
                            continue;
                        }
                        (parts.join("."), false)
                    }
                };
                (spec, Some(items.as_slice()), local)
            }
            Stmt::Import {
                path,
                path_is_string,
                ..
            } => {
                if crate::api_registry::is_std_spec(path) {
                    continue;
                }
                (path.clone(), None, *path_is_string)
            }
            _ => continue,
        };
        if crate::api_registry::is_std_spec(&spec) {
            continue;
        }
        let Some((_, idx)) = crate::lsp::workspace::load_index(&uri, &spec, docs) else {
            if must_resolve_locally {
                diags.push((
                    st.line,
                    st.column,
                    format!("cannot resolve module '{spec}'"),
                ));
            }
            continue;
        };
        let Some(items) = items else {
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
    fn hard_type_flow_checks_reassignment_and_nested_returns() {
        let reassignment = msgs("func f(x:: num) { x = \"bad\" }\n");
        assert!(
            reassignment.iter().any(|m| m.contains("hard variable x")),
            "{reassignment:?}"
        );

        let nested_return =
            msgs("func f(flag) => num { if (flag) { return \"bad\" } else { return 1 } }\n");
        assert!(
            nested_return
                .iter()
                .any(|m| m.contains("cannot return text from hard type num")),
            "{nested_return:?}"
        );

        let bare_return = msgs("func f() => num { return }\n");
        assert!(
            bare_return
                .iter()
                .any(|m| m.contains("bare return is incompatible")),
            "{bare_return:?}"
        );
    }

    #[test]
    fn immutable_bindings_reject_direct_destructured_and_captured_assignment() {
        let direct = msgs("let x = 1\nx = 2\n");
        assert!(
            direct.iter().any(|m| m.contains("immutable binding `x`")),
            "{direct:?}"
        );

        let mutable = msgs("var x = 1\nx = 2\n");
        assert!(
            !mutable.iter().any(|m| m.contains("immutable binding")),
            "{mutable:?}"
        );

        let destructured = msgs("let (a, b) = (1, 2)\n(a, b) = (3, 4)\n");
        assert_eq!(
            destructured
                .iter()
                .filter(|m| m.contains("immutable binding"))
                .count(),
            2,
            "{destructured:?}"
        );

        let captured = msgs("let x = 1\nfunc change() { x = 2 }\n");
        assert!(
            captured.iter().any(|m| m.contains("immutable binding `x`")),
            "{captured:?}"
        );

        let before_declaration = msgs("x = 1\nlet x = 2\n");
        assert!(
            !before_declaration
                .iter()
                .any(|m| m.contains("immutable binding `x`")),
            "{before_declaration:?}"
        );
    }

    #[test]
    fn constant_runtime_failures_are_reported() {
        let failures = msgs("print(10 / 0)\nprint(10 % -0)\nprint([1, 2][2])\nprint(\"x\"[-2])\n");
        assert!(
            failures
                .iter()
                .any(|m| m.contains("division by constant zero")),
            "{failures:?}"
        );
        assert!(
            failures
                .iter()
                .any(|m| m.contains("modulo by constant zero")),
            "{failures:?}"
        );
        assert_eq!(
            failures
                .iter()
                .filter(|m| m.contains("out of bounds"))
                .count(),
            2,
            "{failures:?}"
        );

        let valid = msgs("print([1, 2][-2])\nprint(\"x\"[0])\n");
        assert!(
            !valid.iter().any(|m| m.contains("out of bounds")),
            "{valid:?}"
        );
    }

    #[test]
    fn diagnostics_cover_nested_expression_entry_points() {
        for source in [
            "let (a, b) = (1 / 0, 2)\n",
            "for (x in [1][3]) { }\n",
            "func produce() { yield 1 / 0 }\n",
            "func cleanup() { defer { print(1 / 0) } }\n",
            "let xs = [1 / 0 for (x in [1])]\n",
            "struct S { let value = 1 / 0 }\n",
        ] {
            let diagnostics = msgs(source);
            assert!(
                diagnostics.iter().any(|message| {
                    message.contains("constant zero") || message.contains("out of bounds")
                }),
                "source: {source:?}; diagnostics: {diagnostics:?}"
            );
        }

        let propagated = msgs("func f() { return missing? }\n");
        assert!(
            propagated
                .iter()
                .any(|message| message.contains("undefined name `missing`")),
            "{propagated:?}"
        );
    }

    #[test]
    fn duplicate_parameters_and_named_arguments_are_reported() {
        let parameters = msgs("func f(x, x) { return x }\n");
        assert!(
            parameters.iter().any(|m| m.contains("duplicate parameter")),
            "{parameters:?}"
        );

        let arguments = msgs("func f(x) { return x }\nf(x = 1, x = 2)\n");
        assert!(
            arguments
                .iter()
                .any(|m| m.contains("duplicate named argument")),
            "{arguments:?}"
        );

        let ordering = msgs("func f(a, b) { return a + b }\nf(a = 1, 2)\n");
        assert!(
            ordering
                .iter()
                .any(|message| message.contains("positional argument cannot follow")),
            "{ordering:?}"
        );
    }

    #[test]
    fn duplicate_type_members_and_generic_names_are_reported() {
        for (source, expected) in [
            ("func f[T, T](x) { return x }\n", "duplicate type parameter"),
            ("struct S { let x\nlet x }\n", "duplicate struct field"),
            ("enum E { A A }\n", "duplicate enum member"),
            (
                "variant V { A = struct { }\nA = struct { } }\n",
                "duplicate variant case",
            ),
            (
                "variant V { A = struct { let x\nlet x } }\n",
                "duplicate variant field",
            ),
            (
                "protocol P { func run(self) { }\nfunc run(self) { } }\n",
                "duplicate protocol member",
            ),
            ("macro m(x, x) { return x }\n", "duplicate macro parameter"),
        ] {
            let diagnostics = msgs(source);
            assert!(
                diagnostics.iter().any(|message| message.contains(expected)),
                "source: {source:?}; expected: {expected}; diagnostics: {diagnostics:?}"
            );
        }

        let overloads = msgs(
            "struct S { func run(self, x: num) overload { }\nfunc run(self, x: text) overload { } }\n",
        );
        assert!(
            !overloads
                .iter()
                .any(|message| message.contains("duplicate struct method")),
            "{overloads:?}"
        );

        let metadata = diagnostic_metadata("duplicate struct field `x`");
        assert_eq!(metadata.code, "E3003");
        assert_eq!(metadata.severity, Severity::Error);
    }

    #[test]
    fn duplicate_bindings_in_one_scope_are_reported() {
        let direct = msgs("let value = 1\nlet value = 2\n");
        assert!(
            direct
                .iter()
                .any(|message| message.contains("duplicate binding `value`")),
            "{direct:?}"
        );

        let destructured = msgs("let (left, left) = (1, 2)\n");
        assert!(
            destructured
                .iter()
                .any(|message| message.contains("duplicate binding `left`")),
            "{destructured:?}"
        );

        let shadowed = msgs("let value = 1\n{ let value = 2\nprint(value) }\nprint(value)\n");
        assert!(
            !shadowed
                .iter()
                .any(|message| message.contains("duplicate binding")),
            "{shadowed:?}"
        );
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
    fn unused_nested_local_is_reported() {
        let diagnostics = msgs("func f(flag) { if (flag) { let nested = 1 }\nreturn 0 }\n");
        assert!(
            diagnostics
                .iter()
                .any(|m| m.contains("unused variable `nested`")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn unreachable_after_return() {
        let d = msgs("func f() {\n  return 1\n  2\n}\n");
        assert!(d.iter().any(|m| m.contains("unreachable")), "{d:?}");
    }

    #[test]
    fn unreachable_after_exhaustive_terminating_branches() {
        let conditional = msgs(
            "func f(flag) { if (flag) { return 1 } else { throw ValueError(\"x\") }\nprint(2) }\n",
        );
        assert!(
            conditional.iter().any(|m| m == "unreachable code"),
            "{conditional:?}"
        );

        let non_exhaustive = msgs("func f(flag) { if (flag) { return 1 }\nprint(2) }\n");
        assert!(
            !non_exhaustive.iter().any(|m| m == "unreachable code"),
            "{non_exhaustive:?}"
        );
    }

    #[test]
    fn invalid_control_flow_is_reported_without_codegen() {
        let top_level = msgs("return 1\nbreak\nyield 2\n");
        assert!(top_level.iter().any(|m| m.contains("return is only valid")));
        assert!(top_level.iter().any(|m| m.contains("loop control")));
        assert!(top_level.iter().any(|m| m.contains("yield is only valid")));

        let deferred = msgs("func f() { defer { return 1 } }\n");
        assert!(deferred
            .iter()
            .any(|m| m.contains("cannot leave a defer body")));
    }

    #[test]
    fn missing_import_and_use_modules_are_reported() {
        let import_program = Parser::parse("import \"definitely_missing.tive\" as m\nprint(m)\n")
            .expect("parse import");
        let import_diags = analyze_in(&import_program, "file:///tmp/main.tive", &HashMap::new());
        assert!(import_diags
            .iter()
            .any(|(_, _, m)| m.contains("cannot resolve module")));

        let use_program =
            Parser::parse("use \"definitely_missing.tive\".{ value }\nprint(value)\n")
                .expect("parse use");
        let use_diags = analyze_in(&use_program, "file:///tmp/main.tive", &HashMap::new());
        assert!(use_diags
            .iter()
            .any(|(_, _, m)| m.contains("cannot resolve module")));
    }

    #[test]
    fn diagnostic_metadata_distinguishes_warnings_and_errors() {
        let warning = diagnostic_metadata("unused variable `x`");
        assert_eq!(warning.severity, Severity::Warning);
        assert_eq!(warning.code, "W1002");
        assert!(warning.help.is_some());

        let error = diagnostic_metadata("cannot resolve module 'missing.tive'");
        assert_eq!(error.severity, Severity::Error);
        assert_eq!(error.code, "E2001");
        assert!(error.help.is_some());
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
