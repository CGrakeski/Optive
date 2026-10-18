//! Validation and lexical lowering for declaration-time function descriptors.

use crate::ast::{Block, LocatedStmt, Program, Stmt};
use crate::error::RuntimeError;
use crate::Result;

#[derive(Debug)]
struct Lifted {
    stmt: LocatedStmt,
    remaining: usize,
}

/// Move functions carrying `outside(n)` into their lexical owner before the
/// ordinary semantic/compiler passes see the tree.  The source AST is cloned so
/// formatting and editor ranges keep the spelling written by the user.
pub fn lower_program(program: &Program) -> Result<Program> {
    lower_program_with_imports(program, crate::const_eval::ConstImports::default())
}

pub fn lower_program_with_imports(
    program: &Program,
    imports: crate::const_eval::ConstImports,
) -> Result<Program> {
    // The compiler validates CTFE after lowering. Editor/name passes may not yet
    // have imported interfaces; unrelated imported constants must not block them.
    let context =
        crate::const_eval::ConstContext::build_with_imports(program, imports).unwrap_or_default();
    let (stmts, escaped) = lower_block(&program.stmts, true, &context)?;
    if let Some(lifted) = escaped.first() {
        return Err(RuntimeError::msg(format!(
            "function descriptor `outside` at {}:{} escapes the module scope",
            lifted.stmt.line, lifted.stmt.column
        )));
    }
    Ok(Program { stmts })
}

fn lower_function_body(
    body: &mut Block,
    context: &crate::const_eval::ConstContext,
) -> Result<Vec<Lifted>> {
    let (lowered, mut escaped) = lower_block(body, true, context)?;
    *body = lowered;
    for lifted in &mut escaped {
        lifted.remaining = lifted.remaining.saturating_sub(1);
    }
    Ok(escaped)
}

fn lower_block(
    block: &Block,
    scope_root: bool,
    context: &crate::const_eval::ConstContext,
) -> Result<(Block, Vec<Lifted>)> {
    let mut output = Vec::with_capacity(block.len());
    let mut outgoing = Vec::new();

    for located in block {
        let mut located = located.clone();
        expand_cancel_flag(&mut located)?;
        let mut child_lifts = lower_nested_bodies(&mut located.stmt, context)?;
        for lifted in child_lifts.drain(..) {
            if scope_root && lifted.remaining == 0 {
                output.push(lifted.stmt);
            } else {
                outgoing.push(lifted);
            }
        }

        let outside = take_outside_depth(&mut located.stmt, context)?;
        if outside == 0 {
            output.push(located);
        } else {
            outgoing.push(Lifted {
                stmt: located,
                remaining: outside,
            });
        }
    }

    Ok((output, outgoing))
}

/// Built-in declaration macro. The source tree keeps its original spelling;
/// lowered bodies contain an ordinary immutable local initialized per call.
fn expand_cancel_flag(located: &mut LocatedStmt) -> Result<()> {
    let Stmt::FuncDecl {
        params,
        type_params,
        descriptors,
        body,
        is_generator,
        ..
    } = &mut located.stmt
    else {
        return Ok(());
    };
    let Some(descriptor) = descriptors.iter().find(|d| d.name == "cancel_flag") else {
        return Ok(());
    };
    let Some(crate::ast::Expr {
        kind: crate::ast::ExprKind::Var(binding),
        ..
    }) = descriptor.args.first()
    else {
        return Err(RuntimeError::msg("cancel_flag requires one binding name"));
    };
    if *is_generator {
        return Err(RuntimeError::msg(
            "cancel_flag is a task descriptor and cannot annotate a generator",
        ));
    }
    if has_cancel_flag(body) {
        return Ok(());
    }
    let conflict = params
        .iter()
        .flat_map(crate::ast::FuncParam::bound_names)
        .any(|n| n == *binding)
        || type_params.iter().any(|(n, _)| n == binding)
        || body.iter().any(|s| match &s.stmt {
            Stmt::VarDecl { name, .. }
            | Stmt::FuncDecl { name, .. }
            | Stmt::StructDecl { name, .. }
            | Stmt::EnumDecl { name, .. }
            | Stmt::VariantDecl { name, .. }
            | Stmt::MacroDecl { name, .. }
            | Stmt::ProtocolDecl { name, .. } => name == binding,
            Stmt::DestructDecl { pattern, .. } => pattern.bound_names().contains(binding),
            _ => false,
        });
    if conflict {
        return Err(RuntimeError::msg(format!(
            "cancel_flag binding `{binding}` conflicts with a parameter or local declaration"
        )));
    }
    let loc = descriptor.args[0].loc;
    let init = crate::ast::Expr::new(
        loc,
        crate::ast::ExprKind::Call {
            callee: Box::new(crate::ast::Expr::new(
                loc,
                crate::ast::ExprKind::Var("__cancel_flag__".into()),
            )),
            args: vec![],
        },
    );
    body.insert(
        0,
        LocatedStmt {
            line: loc.line,
            column: loc.column,
            stmt: Stmt::VarDecl {
                visibility: crate::ast::Visibility::Default,
                is_const: false,
                is_var: false,
                name: binding.clone(),
                type_expr: None,
                type_strong: false,
                init: Some(init),
            },
        },
    );
    Ok(())
}

/// The macro's generated entry binding is retained through generic specialization.
pub fn has_cancel_flag(body: &Block) -> bool {
    body.first().is_some_and(|s| match &s.stmt {
        Stmt::VarDecl {
            init:
                Some(crate::ast::Expr {
                    kind: crate::ast::ExprKind::Call { callee, args },
                    ..
                }),
            ..
        } => {
            args.is_empty()
                && matches!(&callee.kind, crate::ast::ExprKind::Var(n) if n == "__cancel_flag__")
        }
        _ => false,
    })
}

fn lower_child(
    block: &mut Block,
    lifts: &mut Vec<Lifted>,
    context: &crate::const_eval::ConstContext,
) -> Result<()> {
    let (lowered, escaped) = lower_block(block, false, context)?;
    *block = lowered;
    lifts.extend(escaped);
    Ok(())
}

fn lower_nested_bodies(
    stmt: &mut Stmt,
    context: &crate::const_eval::ConstContext,
) -> Result<Vec<Lifted>> {
    let mut lifts = Vec::new();
    match stmt {
        Stmt::FuncDecl { body, .. } => lifts.extend(lower_function_body(body, context)?),
        Stmt::If {
            then_block,
            elifs,
            else_block,
            ..
        } => {
            lower_child(then_block, &mut lifts, context)?;
            for (_, body) in elifs {
                lower_child(body, &mut lifts, context)?;
            }
            if let Some(body) = else_block {
                lower_child(body, &mut lifts, context)?;
            }
        }
        Stmt::While { body, .. }
        | Stmt::Loop { body, .. }
        | Stmt::Defer(body)
        | Stmt::With { body, .. } => lower_child(body, &mut lifts, context)?,
        Stmt::For {
            body, else_block, ..
        } => {
            lower_child(body, &mut lifts, context)?;
            if let Some(body) = else_block {
                lower_child(body, &mut lifts, context)?;
            }
        }
        Stmt::Try {
            body,
            catches,
            else_block,
        } => {
            lower_child(body, &mut lifts, context)?;
            for clause in catches {
                lower_child(&mut clause.body, &mut lifts, context)?;
            }
            if let Some(body) = else_block {
                lower_child(body, &mut lifts, context)?;
            }
        }
        Stmt::Match {
            cases, else_block, ..
        } => {
            for case in cases {
                lower_child(&mut case.body, &mut lifts, context)?;
            }
            if let Some(body) = else_block {
                lower_child(body, &mut lifts, context)?;
            }
        }
        Stmt::Block(body) => lower_child(body, &mut lifts, context)?,
        Stmt::MacroDecl { body, .. } => lifts.extend(lower_function_body(body, context)?),
        Stmt::FriendFuncDecl {
            body: Some(body), ..
        } => lifts.extend(lower_function_body(body, context)?),
        Stmt::StructDecl { methods, .. } => {
            for method in methods {
                lifts.extend(lower_function_body(&mut method.body, context)?);
            }
        }
        Stmt::EnumDecl { methods, .. } => {
            for method in methods {
                lifts.extend(lower_function_body(&mut method.body, context)?);
            }
        }
        _ => {}
    }
    Ok(lifts)
}

fn take_outside_depth(stmt: &mut Stmt, context: &crate::const_eval::ConstContext) -> Result<usize> {
    let Stmt::FuncDecl { descriptors, .. } = stmt else {
        return Ok(0);
    };
    let Some(index) = descriptors
        .iter()
        .position(|descriptor| descriptor.name == "outside")
    else {
        return Ok(0);
    };
    let descriptor = descriptors.remove(index);
    let Some(arg) = descriptor.args.first() else {
        return Err(RuntimeError::msg(
            "function descriptor `outside` expects 1 argument",
        ));
    };
    let Some(crate::value::Value::Num(number)) = context.try_eval(arg)? else {
        return Err(RuntimeError::msg(
            "function descriptor `outside` requires a compile-time non-negative integer",
        ));
    };
    number
        .to_i64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| {
            RuntimeError::msg(
                "function descriptor `outside` requires a compile-time non-negative integer",
            )
        })
}
