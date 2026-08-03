//! 宏 / quote / eval 用的运行时 AST 值。

use std::collections::HashMap;

use crate::ast::{
    Block, BinaryOp, CallArg, CatchClause, CatchPattern, Expr, ExprKind, ForItem, LValue,
    LocatedStmt, MacroCallArg, MatchCase, Pattern, PatternElem, Stmt, TypeExpr, UnaryOp, Visibility,
};
use crate::codegen::Generator;
use crate::error::RuntimeError;
use crate::parser::Parser;
use crate::shared::Shared;
use crate::value::{FieldTypeInfo, Num, StructDef, StructInstance, Value};
use crate::vm::Vm;
use crate::Result;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstNodeKind {
    Unknown,
    Number,
    String,
    Bool,
    NoneLit,
    VarRef,
    Placeholder,
    Unary,
    Binary,
    FuncCall,
    MacroCall,
    MemberAccess,
    TypeConvert,
    IndexAccess,
    Slice,
    Vector,
    Dictionary,
    DictEntry,
    QuoteExpr,
    BlockStmt,
    VarDecl,
    Assign,
    ReturnStmt,
    IfStmt,
    WhileStmt,
    ForStmt,
    LoopStmt,
    BreakStmt,
    ContinueStmt,
    ThrowStmt,
    TryStmt,
    MatchStmt,
    MatchCase,
    MatchPattern,
    ExprStmt,
    /// 包装：捕获的 `Value::RuntimeAst` 绑定（宏参数），与计算得到的字面量区分。
    FrozenAst,
}

impl AstNodeKind {
    pub fn struct_name(self) -> Option<&'static str> {
        match self {
            AstNodeKind::Number => Some("AstNumber"),
            AstNodeKind::String => Some("AstString"),
            AstNodeKind::Bool => Some("AstBool"),
            AstNodeKind::VarRef => Some("AstVarRef"),
            AstNodeKind::Unary => Some("AstUnary"),
            AstNodeKind::Binary => Some("AstBinary"),
            AstNodeKind::FuncCall => Some("AstFuncCall"),
            AstNodeKind::MacroCall => Some("AstMacroCall"),
            AstNodeKind::MemberAccess => Some("AstMemberAccess"),
            AstNodeKind::TypeConvert => Some("AstTypeConvert"),
            AstNodeKind::IndexAccess => Some("AstIndexAccess"),
            AstNodeKind::Vector => Some("AstVector"),
            AstNodeKind::QuoteExpr => Some("AstQuote"),
            _ => None,
        }
    }
}

/// 将宏参数 `::` 注解名（如 `VarRefNode`）映射为节点种类。
pub fn annotation_to_kind(name: &str) -> Option<AstNodeKind> {
    match name {
        "VarRefNode" | "AstVarRef" => Some(AstNodeKind::VarRef),
        "NumberNode" | "AstNumber" => Some(AstNodeKind::Number),
        "StringNode" | "AstString" => Some(AstNodeKind::String),
        "BoolNode" | "AstBool" => Some(AstNodeKind::Bool),
        "UnaryNode" | "AstUnary" => Some(AstNodeKind::Unary),
        "BinaryNode" | "AstBinary" => Some(AstNodeKind::Binary),
        "FuncCallExprNode" | "AstFuncCall" => Some(AstNodeKind::FuncCall),
        "MacroCallExprNode" | "AstMacroCall" => Some(AstNodeKind::MacroCall),
        "MemberAccessNode" | "AstMemberAccess" => Some(AstNodeKind::MemberAccess),
        "TypeConvertExprNode" | "AstTypeConvert" => Some(AstNodeKind::TypeConvert),
        "IndexAccessNode" | "AstIndexAccess" => Some(AstNodeKind::IndexAccess),
        "VectorNode" | "AstVector" => Some(AstNodeKind::Vector),
        "QuoteExprNode" | "AstQuote" => Some(AstNodeKind::QuoteExpr),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct AstCallArg {
    pub kw_name: String,
    pub is_splat: bool,
    pub value: RuntimeAstNode,
}

#[derive(Debug, Clone)]
pub struct RuntimeAstNode {
    pub kind: AstNodeKind,
    pub line: usize,
    pub text: String,
    pub bool_val: bool,
    pub stmts: Vec<RuntimeAstNode>,
    pub children: Vec<RuntimeAstNode>,
    pub slot_a: Option<Box<RuntimeAstNode>>,
    pub slot_b: Option<Box<RuntimeAstNode>>,
    pub slot_c: Option<Box<RuntimeAstNode>>,
    pub hygienic_names: Vec<String>,
    pub binding_names: Vec<String>,
    pub bindings: Vec<RuntimeAstNode>,
    pub call_args: Vec<AstCallArg>,
}

impl RuntimeAstNode {
    pub fn as_value(self) -> Value {
        Value::RuntimeAst(Arc::new(self))
    }
}

pub fn value_is_ast(v: &Value) -> bool {
    matches!(v, Value::RuntimeAst(_))
}

pub fn value_as_ast(v: &Value) -> Result<RuntimeAstNode> {
    match v {
        Value::RuntimeAst(n) => Ok((**n).clone()),
        other => Err(RuntimeError::msg(format!(
            "expected AST, got {}",
            other.type_name()
        ))),
    }
}

pub fn ast_from_expr(expr: &Expr) -> RuntimeAstNode {
    match &expr.kind {
        ExprKind::Number(n) => RuntimeAstNode {
            kind: AstNodeKind::Number,
            text: n.clone(),
            ..default_node()
        },
        ExprKind::String(s) => RuntimeAstNode {
            kind: AstNodeKind::String,
            text: s.clone(),
            ..default_node()
        },
        ExprKind::FString(parts) => {
            let mut text = String::new();
            for part in parts {
                match part {
                    crate::ast::FStringPart::Text(s) => text.push_str(s),
                    crate::ast::FStringPart::Expr(_) => text.push_str("{...}"),
                }
            }
            RuntimeAstNode {
                kind: AstNodeKind::String,
                text,
                ..default_node()
            }
        }
        ExprKind::Bool(b) => RuntimeAstNode {
            kind: AstNodeKind::Bool,
            bool_val: *b,
            ..default_node()
        },
        ExprKind::None => RuntimeAstNode {
            kind: AstNodeKind::NoneLit,
            ..default_node()
        },
        ExprKind::Var(name) => RuntimeAstNode {
            kind: AstNodeKind::VarRef,
            text: name.clone(),
            ..default_node()
        },
        ExprKind::Placeholder => RuntimeAstNode {
            kind: AstNodeKind::Placeholder,
            ..default_node()
        },
        ExprKind::Unary { op, operand } => RuntimeAstNode {
            kind: AstNodeKind::Unary,
            text: unary_op_text(*op).into(),
            slot_a: Some(Box::new(ast_from_expr(operand))),
            ..default_node()
        },
        ExprKind::Binary { op, left, right } => RuntimeAstNode {
            kind: AstNodeKind::Binary,
            text: binary_op_text(*op).into(),
            slot_a: Some(Box::new(ast_from_expr(left))),
            slot_b: Some(Box::new(ast_from_expr(right))),
            ..default_node()
        },
        ExprKind::Call { callee, args } => {
            let mut node = RuntimeAstNode {
                kind: AstNodeKind::FuncCall,
                slot_a: Some(Box::new(ast_from_expr(callee))),
                ..default_node()
            };
            node.call_args = args.iter().map(ast_from_call_arg).collect();
            node
        }
        ExprKind::MacroCall { callee, args } => {
            let mut node = RuntimeAstNode {
                kind: AstNodeKind::MacroCall,
                slot_a: Some(Box::new(ast_from_expr(callee))),
                ..default_node()
            };
            node.call_args = args.iter().map(ast_from_macro_call_arg).collect();
            node
        }
        ExprKind::Member { object, field } => RuntimeAstNode {
            kind: AstNodeKind::MemberAccess,
            text: field.clone(),
            slot_a: Some(Box::new(ast_from_expr(object))),
            ..default_node()
        },
        ExprKind::Index { object, index } => RuntimeAstNode {
            kind: AstNodeKind::IndexAccess,
            slot_a: Some(Box::new(ast_from_expr(object))),
            slot_b: Some(Box::new(ast_from_expr(index))),
            ..default_node()
        },
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => RuntimeAstNode {
            kind: AstNodeKind::Slice,
            slot_a: Some(Box::new(ast_from_expr(object))),
            slot_b: start.as_ref().map(|e| Box::new(ast_from_expr(e))),
            slot_c: end.as_ref().map(|e| Box::new(ast_from_expr(e))),
            children: step
                .as_ref()
                .map(|e| vec![ast_from_expr(e)])
                .unwrap_or_default(),
            ..default_node()
        },
        ExprKind::TypeConvert { type_expr, value } => RuntimeAstNode {
            kind: AstNodeKind::TypeConvert,
            slot_a: Some(Box::new(ast_from_expr(type_expr))),
            slot_b: Some(Box::new(ast_from_expr(value))),
            ..default_node()
        },
        ExprKind::List(elems) => RuntimeAstNode {
            kind: AstNodeKind::Vector,
            children: elems.iter().map(ast_from_expr).collect(),
            ..default_node()
        },
        ExprKind::Dict(entries) => {
            let mut node = RuntimeAstNode {
                kind: AstNodeKind::Dictionary,
                ..default_node()
            };
            for (k, v) in entries {
                node.children.push(RuntimeAstNode {
                    kind: AstNodeKind::DictEntry,
                    slot_a: Some(Box::new(ast_from_expr(k))),
                    slot_b: Some(Box::new(ast_from_expr(v))),
                    ..default_node()
                });
            }
            node
        }
        ExprKind::Quote {
            hygienic_names,
            bindings,
            body,
        } => RuntimeAstNode {
            kind: AstNodeKind::QuoteExpr,
            hygienic_names: hygienic_names.clone(),
            bindings: bindings.iter().map(ast_from_expr).collect(),
            binding_names: bindings
                .iter()
                .filter_map(|e| {
                    if let ExprKind::Var(n) = &e.kind {
                        Some(n.clone())
                    } else {
                        None
                    }
                })
                .collect(),
            slot_a: Some(Box::new(ast_from_block(body))),
            ..default_node()
        },
        ExprKind::DoFunc { .. }
        | ExprKind::Pipeline { .. }
        | ExprKind::ListComp { .. }
        | ExprKind::SetComp { .. }
        | ExprKind::DictComp { .. }
        | ExprKind::GeneratorExp { .. }
        | ExprKind::Match { .. }
        | ExprKind::IfThenElse { .. }
        | ExprKind::Handle { .. }
        | ExprKind::Go { .. }
        | ExprKind::Await { .. }
        | ExprKind::Suspend
        | ExprKind::Select { .. }
        | ExprKind::NamedAssign { .. }
        | ExprKind::Set(_)
        | ExprKind::Tuple(_)
        | ExprKind::Bytes(_) => RuntimeAstNode {
            kind: AstNodeKind::Unknown,
            ..default_node()
        },
    }
}

fn ast_from_call_arg(arg: &CallArg) -> AstCallArg {
    AstCallArg {
        kw_name: arg.name.clone().unwrap_or_default(),
        is_splat: arg.is_splat || arg.is_kwsplat,
        value: ast_from_expr(&arg.value),
    }
}

fn ast_from_macro_call_arg(arg: &MacroCallArg) -> AstCallArg {
    AstCallArg {
        kw_name: String::new(),
        is_splat: arg.is_splat,
        value: (*arg.node).clone(),
    }
}

pub fn ast_from_block(block: &Block) -> RuntimeAstNode {
    RuntimeAstNode {
        kind: AstNodeKind::BlockStmt,
        stmts: block
            .iter()
            .filter(|ls| !matches!(ls.stmt, Stmt::Comment { .. }))
            .map(|ls| ast_from_stmt(&ls.stmt))
            .collect(),
        ..default_node()
    }
}

pub fn ast_from_stmt(stmt: &Stmt) -> RuntimeAstNode {
    match stmt {
        Stmt::VarDecl {
            name,
            is_const,
            init,
            ..
        } => RuntimeAstNode {
            kind: AstNodeKind::VarDecl,
            text: name.clone(),
            bool_val: *is_const,
            slot_a: init.as_ref().map(|e| Box::new(ast_from_expr(e))),
            ..default_node()
        },
        Stmt::Assign { target, value } => {
            let mut node = RuntimeAstNode {
                kind: AstNodeKind::Assign,
                slot_a: Some(Box::new(ast_from_expr(value))),
                ..default_node()
            };
            if let LValue::Name(n) = target {
                node.text = n.clone();
            } else {
                node.slot_b = Some(Box::new(lvalue_to_ast(target)));
            }
            node
        }
        Stmt::Return(expr) => RuntimeAstNode {
            kind: AstNodeKind::ReturnStmt,
            slot_a: expr.as_ref().map(|e| Box::new(ast_from_expr(e))),
            ..default_node()
        },
        Stmt::If {
            cond,
            then_block,
            elifs,
            else_block,
        } => {
            let mut elif_stmts: Vec<RuntimeAstNode> = elifs
                .iter()
                .map(|(c, b)| {
                    RuntimeAstNode {
                        kind: AstNodeKind::IfStmt,
                        slot_a: Some(Box::new(ast_from_expr(c))),
                        slot_b: Some(Box::new(ast_from_block(b))),
                        ..default_node()
                    }
                })
                .collect();
            let mut node = RuntimeAstNode {
                kind: AstNodeKind::IfStmt,
                slot_a: Some(Box::new(ast_from_expr(cond))),
                slot_b: Some(Box::new(ast_from_block(then_block))),
                slot_c: else_block.as_ref().map(|b| Box::new(ast_from_block(b))),
                ..default_node()
            };
            node.stmts = std::mem::take(&mut elif_stmts);
            node
        }
        Stmt::While { cond, body } => RuntimeAstNode {
            kind: AstNodeKind::WhileStmt,
            slot_a: Some(Box::new(ast_from_expr(cond))),
            slot_b: Some(Box::new(ast_from_block(body))),
            ..default_node()
        },
        Stmt::For { items, body } => RuntimeAstNode {
            kind: AstNodeKind::ForStmt,
            slot_a: items.first().map(|i| Box::new(ast_from_expr(&i.iterable))),
            text: items.first().map(|i| i.name.clone()).unwrap_or_default(),
            slot_b: Some(Box::new(ast_from_block(body))),
            ..default_node()
        },
        Stmt::Loop { count, body } => RuntimeAstNode {
            kind: AstNodeKind::LoopStmt,
            slot_a: count.as_ref().map(|e| Box::new(ast_from_expr(e))),
            slot_b: Some(Box::new(ast_from_block(body))),
            ..default_node()
        },
        Stmt::Break => RuntimeAstNode {
            kind: AstNodeKind::BreakStmt,
            ..default_node()
        },
        Stmt::Continue => RuntimeAstNode {
            kind: AstNodeKind::ContinueStmt,
            ..default_node()
        },
        Stmt::Throw(e) => RuntimeAstNode {
            kind: AstNodeKind::ThrowStmt,
            slot_a: Some(Box::new(ast_from_expr(e))),
            ..default_node()
        },
        Stmt::Match {
            subject,
            cases,
            else_block,
        } => {
            let mut node = RuntimeAstNode {
                kind: AstNodeKind::MatchStmt,
                slot_a: Some(Box::new(ast_from_expr(subject))),
                slot_c: else_block.as_ref().map(|b| Box::new(ast_from_block(b))),
                ..default_node()
            };
            for case in cases {
                node.stmts.push(RuntimeAstNode {
                    kind: AstNodeKind::MatchCase,
                    slot_a: Some(Box::new(pattern_to_ast(&case.pattern))),
                    slot_b: Some(Box::new(ast_from_block(&case.body))),
                    ..default_node()
                });
            }
            node
        }
        Stmt::Try {
            body,
            catches,
            else_block,
        } => {
            let mut node = RuntimeAstNode {
                kind: AstNodeKind::TryStmt,
                slot_a: Some(Box::new(ast_from_block(body))),
                slot_c: else_block.as_ref().map(|b| Box::new(ast_from_block(b))),
                ..default_node()
            };
            for catch in catches {
                let (name, type_name) = match &catch.pattern {
                    CatchPattern::Wildcard => (String::new(), None),
                    CatchPattern::Bind { name, type_name } => (name.clone(), type_name.clone()),
                };
                node.stmts.push(RuntimeAstNode {
                    kind: AstNodeKind::Unknown,
                    text: "Catch".into(),
                    hygienic_names: type_name.into_iter().collect(),
                    slot_a: if name.is_empty() {
                        None
                    } else {
                        Some(Box::new(RuntimeAstNode {
                            kind: AstNodeKind::VarRef,
                            text: name,
                            ..default_node()
                        }))
                    },
                    slot_b: Some(Box::new(ast_from_block(&catch.body))),
                    ..default_node()
                });
            }
            node
        }
        Stmt::Expr(e) => RuntimeAstNode {
            kind: AstNodeKind::ExprStmt,
            slot_a: Some(Box::new(ast_from_expr(e))),
            ..default_node()
        },
        Stmt::Block(b) => ast_from_block(b),
        _ => RuntimeAstNode {
            kind: AstNodeKind::Unknown,
            ..default_node()
        },
    }
}

fn lvalue_to_ast(lv: &LValue) -> RuntimeAstNode {
    match lv {
        LValue::Name(n) => RuntimeAstNode {
            kind: AstNodeKind::VarRef,
            text: n.clone(),
            ..default_node()
        },
        LValue::Member { object, field } => RuntimeAstNode {
            kind: AstNodeKind::MemberAccess,
            text: field.clone(),
            slot_a: Some(Box::new(ast_from_expr(object))),
            ..default_node()
        },
        LValue::Index { object, index } => RuntimeAstNode {
            kind: AstNodeKind::IndexAccess,
            slot_a: Some(Box::new(ast_from_expr(object))),
            slot_b: Some(Box::new(ast_from_expr(index))),
            ..default_node()
        },
        LValue::Slice {
            object,
            start,
            end,
            step,
        } => RuntimeAstNode {
            kind: AstNodeKind::Slice,
            slot_a: Some(Box::new(ast_from_expr(object))),
            slot_b: start.as_ref().map(|e| Box::new(ast_from_expr(e))),
            slot_c: end.as_ref().map(|e| Box::new(ast_from_expr(e))),
            children: step
                .as_ref()
                .map(|e| vec![ast_from_expr(e)])
                .unwrap_or_default(),
            ..default_node()
        },
    }
}

fn pattern_to_ast(pat: &Pattern) -> RuntimeAstNode {
    match pat {
        Pattern::Bind(n) => RuntimeAstNode {
            kind: AstNodeKind::MatchPattern,
            text: "Bind".into(),
            binding_names: vec![n.clone()],
            ..default_node()
        },
        Pattern::Value(e) => RuntimeAstNode {
            kind: AstNodeKind::MatchPattern,
            text: "Expr".into(),
            slot_a: Some(Box::new(ast_from_expr(e))),
            ..default_node()
        },
        Pattern::List(elems) => {
            let mut node = RuntimeAstNode {
                kind: AstNodeKind::MatchPattern,
                text: "Vector".into(),
                ..default_node()
            };
            for el in elems {
                node.children.push(match el {
                    PatternElem::Bind(n) => RuntimeAstNode {
                        kind: AstNodeKind::MatchPattern,
                        text: "Bind".into(),
                        binding_names: vec![n.clone()],
                        ..default_node()
                    },
                    PatternElem::Nested(p) => pattern_to_ast(p),
                    PatternElem::Value(e) => RuntimeAstNode {
                        kind: AstNodeKind::MatchPattern,
                        text: "Expr".into(),
                        slot_a: Some(Box::new(ast_from_expr(e))),
                        ..default_node()
                    },
                });
            }
            node
        }
        Pattern::Struct { type_name, fields } => RuntimeAstNode {
            kind: AstNodeKind::MatchPattern,
            text: "Struct".into(),
            hygienic_names: vec![type_name.clone()],
            binding_names: fields.clone(),
            ..default_node()
        },
        Pattern::Or(alts) => RuntimeAstNode {
            kind: AstNodeKind::MatchPattern,
            text: "Or".into(),
            children: alts.iter().map(pattern_to_ast).collect(),
            ..default_node()
        },
        Pattern::Call { type_name, args } => RuntimeAstNode {
            kind: AstNodeKind::MatchPattern,
            text: "Call".into(),
            hygienic_names: vec![type_name.clone()],
            children: args.iter().map(pattern_to_ast).collect(),
            ..default_node()
        },
    }
}

fn default_node() -> RuntimeAstNode {
    RuntimeAstNode {
        kind: AstNodeKind::Unknown,
        line: 0,
        text: String::new(),
        bool_val: false,
        stmts: Vec::new(),
        children: Vec::new(),
        slot_a: None,
        slot_b: None,
        slot_c: None,
        hygienic_names: Vec::new(),
        binding_names: Vec::new(),
        bindings: Vec::new(),
        call_args: Vec::new(),
    }
}

fn unary_op_text(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "not",
        UnaryOp::TruthyNot => "!",
        UnaryOp::Invert => "~",
    }
}

fn binary_op_text(op: BinaryOp) -> &'static str {
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

pub fn quote_ast(
    hygienic_names: Vec<String>,
    captured_bindings: Vec<(String, RuntimeAstNode)>,
    mut body: RuntimeAstNode,
) -> RuntimeAstNode {
    let mut rename_map = HashMap::new();
    for (i, name) in hygienic_names.iter().enumerate() {
        rename_map.insert(name.clone(), format!("__q_{name}_{i}"));
    }
    rename_in_node(&mut body, &rename_map);

    RuntimeAstNode {
        kind: AstNodeKind::QuoteExpr,
        hygienic_names,
        binding_names: captured_bindings.iter().map(|(n, _)| n.clone()).collect(),
        bindings: captured_bindings.into_iter().map(|(_, v)| v).collect(),
        slot_a: Some(Box::new(body)),
        ..default_node()
    }
}

fn rename_in_node(node: &mut RuntimeAstNode, rename_map: &HashMap<String, String>) {
    if node.kind == AstNodeKind::VarRef || node.kind == AstNodeKind::VarDecl {
        if let Some(new_name) = rename_map.get(&node.text) {
            node.text = new_name.clone();
        }
    }
    for stmt in &mut node.stmts {
        rename_in_node(stmt, rename_map);
    }
    for child in &mut node.children {
        rename_in_node(child, rename_map);
    }
    for arg in &mut node.call_args {
        rename_in_node(&mut arg.value, rename_map);
    }
    if let Some(slot) = &mut node.slot_a {
        rename_in_node(slot, rename_map);
    }
    if let Some(slot) = &mut node.slot_b {
        rename_in_node(slot, rename_map);
    }
    if let Some(slot) = &mut node.slot_c {
        rename_in_node(slot, rename_map);
    }
}

pub fn binding_var_name_for_quote(expr: &RuntimeAstNode) -> Result<String> {
    if expr.kind == AstNodeKind::VarRef {
        Ok(expr.text.clone())
    } else {
        Err(RuntimeError::type_err("quote binding must be a simple identifier"))
    }
}

pub fn capture_quote_binding_value(vm: &Vm, expr: &RuntimeAstNode) -> Result<Value> {
    let name = binding_var_name_for_quote(expr)?;
    vm.load_name(&name)
}

pub fn value_to_quote_binding_ast(value: &Value) -> Result<RuntimeAstNode> {
    match value {
        Value::RuntimeAst(n) => Ok(RuntimeAstNode {
            kind: AstNodeKind::FrozenAst,
            slot_a: Some(Box::new((**n).clone())),
            ..default_node()
        }),
        Value::Num(n) => Ok(RuntimeAstNode {
            kind: AstNodeKind::Number,
            text: n.to_string(),
            ..default_node()
        }),
        Value::Text(s) => Ok(RuntimeAstNode {
            kind: AstNodeKind::String,
            text: s.clone(),
            ..default_node()
        }),
        Value::Bool(b) => Ok(RuntimeAstNode {
            kind: AstNodeKind::Bool,
            bool_val: *b,
            ..default_node()
        }),
        Value::List(items) => {
            let children: Vec<RuntimeAstNode> = items
                .borrow()
                .iter()
                .map(value_to_quote_binding_ast)
                .collect::<Result<_>>()?;
            Ok(RuntimeAstNode {
                kind: AstNodeKind::Vector,
                children,
                ..default_node()
            })
        }
        other => Err(RuntimeError::type_err(format!(
            "quote binding value must be AST or literal, got {}",
            other.type_name()
        ))),
    }
}

/// 展开时将 quote AST 中烘焙的 `with` 绑定还原为运行时值。
pub fn quote_binding_to_value(node: &RuntimeAstNode) -> Result<Value> {
    match node.kind {
        AstNodeKind::Number => Ok(Value::Num(Num::from_literal(&node.text)?)),
        AstNodeKind::String => Ok(Value::Text(node.text.clone())),
        AstNodeKind::Bool => Ok(Value::Bool(node.bool_val)),
        AstNodeKind::NoneLit => Ok(Value::None),
        AstNodeKind::FrozenAst => {
            let inner = node.slot_a.as_ref().ok_or_else(|| {
                RuntimeError::msg("FrozenAst binding missing payload")
            })?;
            Ok(Value::RuntimeAst(Arc::new((**inner).clone())))
        }
        AstNodeKind::Vector => {
            let items: Vec<Value> = node
                .children
                .iter()
                .map(quote_binding_to_value)
                .collect::<Result<_>>()?;
            Ok(Value::List(Shared::new(items)))
        }
        _ => Ok(node.clone().as_value()),
    }
}

pub fn ast_to_source(node: &RuntimeAstNode) -> String {
    match node.kind {
        AstNodeKind::Number | AstNodeKind::String => node.text.clone(),
        AstNodeKind::Bool => node.bool_val.to_string(),
        AstNodeKind::NoneLit => "none".into(),
        AstNodeKind::VarRef => node.text.clone(),
        AstNodeKind::Unary => format!(
            "{}{}",
            node.text,
            slot_str(node.slot_a.as_deref())
        ),
        AstNodeKind::Binary => format!(
            "({} {} {})",
            slot_str(node.slot_a.as_deref()),
            node.text,
            slot_str(node.slot_b.as_deref())
        ),
        AstNodeKind::FuncCall | AstNodeKind::MacroCall => {
            let callee = slot_str(node.slot_a.as_deref());
            let args = node
                .children
                .iter()
                .map(ast_to_source)
                .collect::<Vec<_>>()
                .join(", ");
            if node.kind == AstNodeKind::MacroCall {
                format!("{callee}{{{args}}}")
            } else {
                format!("{callee}({args})")
            }
        }
        AstNodeKind::MemberAccess => {
            format!(
                "{}.{}",
                slot_str(node.slot_a.as_deref()),
                node.text
            )
        }
        AstNodeKind::IndexAccess => {
            format!(
                "{}[{}]",
                slot_str(node.slot_a.as_deref()),
                slot_str(node.slot_b.as_deref())
            )
        }
        AstNodeKind::Vector => {
            let items = node
                .children
                .iter()
                .map(ast_to_source)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{items}]")
        }
        AstNodeKind::QuoteExpr => {
            format!("quote({})", slot_str(node.slot_a.as_deref()))
        }
        AstNodeKind::BlockStmt => node
            .stmts
            .iter()
            .map(ast_to_source)
            .collect::<Vec<_>>()
            .join("\n"),
        AstNodeKind::ExprStmt => slot_str(node.slot_a.as_deref()),
        _ => format!("<ast:{:?}>", node.kind),
    }
}

/// 深度优先遍历 AST 节点树；对每个节点调用 `visit`。
pub fn walk_ast_nodes(node: &RuntimeAstNode, visit: &mut dyn FnMut(&RuntimeAstNode) -> Result<()>) -> Result<()> {
    visit(node)?;
    for child in &node.children {
        walk_ast_nodes(child, visit)?;
    }
    for stmt in &node.stmts {
        walk_ast_nodes(stmt, visit)?;
    }
    for binding in &node.bindings {
        walk_ast_nodes(binding, visit)?;
    }
    if let Some(a) = node.slot_a.as_deref() {
        walk_ast_nodes(a, visit)?;
    }
    if let Some(b) = node.slot_b.as_deref() {
        walk_ast_nodes(b, visit)?;
    }
    if let Some(c) = node.slot_c.as_deref() {
        walk_ast_nodes(c, visit)?;
    }
    Ok(())
}

fn slot_str(slot: Option<&RuntimeAstNode>) -> String {
    slot.map(ast_to_source).unwrap_or_default()
}

pub fn eval_ast_value(vm: &mut Vm, node: &RuntimeAstNode) -> Result<Value> {
    let body = if node.kind == AstNodeKind::QuoteExpr {
        node.slot_a.as_deref().ok_or_else(|| RuntimeError::msg("empty quote AST"))?
    } else {
        node
    };

    if is_type_convert_to_text(body) {
        return Ok(Value::Text(ast_to_source(
            body.slot_b.as_deref().unwrap_or(body),
        )));
    }

    let block = ast_to_block(body)?;
    let program = crate::ast::Program { stmts: block };

    let compiled = if node.kind == AstNodeKind::QuoteExpr && !node.binding_names.is_empty() {
        Generator::compile_snippet(&program, &node.binding_names)?
    } else {
        Generator::new().compile(&program)?
    };

    let saved = vm.snapshot_for_eval();
    if let Some(caller) = vm.macro_eval_scope().cloned() {
        vm.globals = caller.globals;
        vm.locals_stack = caller.locals_stack;
        vm.name_to_slot = caller.name_to_slot;
    }
    if node.kind == AstNodeKind::QuoteExpr {
        vm.push_quote_binding_scope(node)?;
    }

    vm.run_snippet(compiled)?;

    let result = vm.stack_top();
    vm.restore_eval_snapshot(saved);
    Ok(result)
}

fn is_type_convert_to_text(node: &RuntimeAstNode) -> bool {
    node.kind == AstNodeKind::TypeConvert
        && node
            .slot_a
            .as_ref()
            .is_some_and(|t| t.kind == AstNodeKind::VarRef && t.text == "text")
}

fn ast_to_block(node: &RuntimeAstNode) -> Result<Block> {
    match node.kind {
        AstNodeKind::BlockStmt => node
            .stmts
            .iter()
            .map(ast_to_stmt)
            .collect::<Result<Vec<_>>>()
            .map(|stmts| {
                stmts
                    .into_iter()
                    .map(|stmt| LocatedStmt { line: 0, column: 1, stmt })
                    .collect()
            }),
        _ => Ok(vec![LocatedStmt { line: 0, column: 1, stmt: ast_to_stmt(node)?,
        }]),
    }
}

fn ast_to_stmt(node: &RuntimeAstNode) -> Result<Stmt> {
    match node.kind {
        AstNodeKind::BlockStmt => Ok(Stmt::Block(
            node.stmts
                .iter()
                .map(ast_to_stmt)
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .map(|stmt| LocatedStmt { line: 0, column: 1, stmt })
                .collect(),
        )),
        AstNodeKind::VarDecl => Ok(Stmt::VarDecl {
            visibility: Visibility::Internal,
            is_const: node.bool_val,
            is_var: true,
            name: node.text.clone(),
            type_expr: None,
            type_strong: false,
            init: node
                .slot_a
                .as_ref()
                .map(|s| ast_to_expr(s))
                .transpose()?,
        }),
        AstNodeKind::Assign => {
            let target = if !node.text.is_empty() {
                LValue::Name(node.text.clone())
            } else {
                ast_to_lvalue(node.slot_b.as_deref().ok_or_else(|| {
                    RuntimeError::value_err("invalid assign AST")
                })?)?
            };
            Ok(Stmt::Assign {
                target,
                value: ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                    RuntimeError::msg("assign missing value")
                })?)?,
            })
        }
        AstNodeKind::ReturnStmt => Ok(Stmt::Return(
            node.slot_a.as_ref().map(|s| ast_to_expr(s)).transpose()?,
        )),
        AstNodeKind::IfStmt => {
            let mut elifs = Vec::new();
            for elif in &node.stmts {
                elifs.push((
                    ast_to_expr(elif.slot_a.as_deref().ok_or_else(|| {
                        RuntimeError::msg("elif missing cond")
                    })?)?,
                    ast_to_block(elif.slot_b.as_deref().ok_or_else(|| {
                        RuntimeError::msg("elif missing body")
                    })?)?,
                ));
            }
            Ok(Stmt::If {
                cond: ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                    RuntimeError::msg("if missing cond")
                })?)?,
                then_block: ast_to_block(node.slot_b.as_deref().ok_or_else(|| {
                    RuntimeError::msg("if missing body")
                })?)?,
                elifs,
                else_block: node
                    .slot_c
                    .as_ref()
                    .map(|b| ast_to_block(b))
                    .transpose()?,
            })
        }
        AstNodeKind::WhileStmt => Ok(Stmt::While {
            cond: ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                RuntimeError::msg("while missing cond")
            })?)?,
            body: ast_to_block(node.slot_b.as_deref().ok_or_else(|| {
                RuntimeError::msg("while missing body")
            })?)?,
        }),
        AstNodeKind::ForStmt => Ok(Stmt::For {
            items: vec![ForItem {
                name: node.text.clone(),
                iterable: ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                    RuntimeError::msg("for missing iterable")
                })?)?,
            }],
            body: ast_to_block(node.slot_b.as_deref().ok_or_else(|| {
                RuntimeError::msg("for missing body")
            })?)?,
        }),
        AstNodeKind::LoopStmt => Ok(Stmt::Loop {
            count: node.slot_a.as_ref().map(|s| ast_to_expr(s)).transpose()?,
            body: ast_to_block(node.slot_b.as_deref().ok_or_else(|| {
                RuntimeError::msg("loop missing body")
            })?)?,
        }),
        AstNodeKind::BreakStmt => Ok(Stmt::Break),
        AstNodeKind::ContinueStmt => Ok(Stmt::Continue),
        AstNodeKind::ThrowStmt => Ok(Stmt::Throw(ast_to_expr(
            node.slot_a.as_deref().ok_or_else(|| RuntimeError::msg("throw missing expr"))?,
        )?)),
        AstNodeKind::MatchStmt => {
            let mut cases = Vec::new();
            for case in &node.stmts {
                cases.push(MatchCase {
                    pattern: ast_to_pattern(case.slot_a.as_deref().ok_or_else(|| {
                        RuntimeError::msg("match case missing pattern")
                    })?)?,
                    body: ast_to_block(case.slot_b.as_deref().ok_or_else(|| {
                        RuntimeError::msg("match case missing body")
                    })?)?,
                });
            }
            Ok(Stmt::Match {
                subject: ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                    RuntimeError::msg("match missing subject")
                })?)?,
                cases,
                else_block: node
                    .slot_c
                    .as_ref()
                    .map(|b| ast_to_block(b))
                    .transpose()?,
            })
        }
        AstNodeKind::TryStmt => {
            let mut catches = Vec::new();
            for catch in &node.stmts {
                if catch.text != "Catch" {
                    continue;
                }
                let pattern = if let Some(name_node) = catch.slot_a.as_deref() {
                    CatchPattern::Bind {
                        name: name_node.text.clone(),
                        type_name: catch.hygienic_names.first().cloned(),
                    }
                } else {
                    CatchPattern::Wildcard
                };
                catches.push(CatchClause {
                    pattern,
                    body: ast_to_block(catch.slot_b.as_deref().ok_or_else(|| {
                        RuntimeError::msg("catch missing body")
                    })?)?,
                });
            }
            Ok(Stmt::Try {
                body: ast_to_block(node.slot_a.as_deref().ok_or_else(|| {
                    RuntimeError::msg("try missing body")
                })?)?,
                catches,
                else_block: node
                    .slot_c
                    .as_ref()
                    .map(|b| ast_to_block(b))
                    .transpose()?,
            })
        }
        AstNodeKind::ExprStmt => Ok(Stmt::Expr(ast_to_expr(
            node.slot_a.as_deref().ok_or_else(|| RuntimeError::msg("expr stmt missing expr"))?,
        )?)),
        _ => ast_to_expr(node).map(Stmt::Expr),
    }
}

fn ast_to_pattern(node: &RuntimeAstNode) -> Result<Pattern> {
    match node.text.as_str() {
        "Expr" => Ok(Pattern::Value(Box::new(ast_to_expr(
            node.slot_a.as_deref().ok_or_else(|| RuntimeError::msg("pattern expr"))?,
        )?))),
        "Bind" => Ok(Pattern::Bind(
            node.binding_names.first().cloned().unwrap_or_default(),
        )),
        "Vector" => Ok(Pattern::List(
            node.children
                .iter()
                .map(|c| match c.text.as_str() {
                    "Bind" => Ok(PatternElem::Bind(
                        c.binding_names.first().cloned().unwrap_or_default(),
                    )),
                    "Expr" => Ok(PatternElem::Value(Box::new(ast_to_expr(
                        c.slot_a.as_deref().ok_or_else(|| RuntimeError::msg("pattern"))?,
                    )?))),
                    _ => Ok(PatternElem::Nested(ast_to_pattern(c)?)),
                })
                .collect::<Result<_>>()?,
        )),
        "Struct" => Ok(Pattern::Struct {
            type_name: node.hygienic_names.first().cloned().unwrap_or_default(),
            fields: node.binding_names.clone(),
        }),
        "Or" => Ok(Pattern::Or(
            node.children.iter().map(ast_to_pattern).collect::<Result<_>>()?,
        )),
        "Call" => Ok(Pattern::Call {
            type_name: node.hygienic_names.first().cloned().unwrap_or_default(),
            args: node.children.iter().map(ast_to_pattern).collect::<Result<_>>()?,
        }),
        _ => Err(RuntimeError::msg(format!(
            "unknown match pattern tag: {}",
            node.text
        )),
        ),
    }
}

fn ast_to_lvalue(node: &RuntimeAstNode) -> Result<LValue> {
    match node.kind {
        AstNodeKind::VarRef => Ok(LValue::Name(node.text.clone())),
        AstNodeKind::MemberAccess => Ok(LValue::Member {
            object: Box::new(ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                RuntimeError::msg("member lvalue")
            })?)?),
            field: node.text.clone(),
        }),
        AstNodeKind::IndexAccess => Ok(LValue::Index {
            object: Box::new(ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                RuntimeError::msg("index lvalue")
            })?)?),
            index: Box::new(ast_to_expr(node.slot_b.as_deref().ok_or_else(|| {
                RuntimeError::msg("index lvalue")
            })?)?),
        }),
        _ => Err(RuntimeError::value_err("invalid lvalue AST")),
    }
}

fn ast_to_expr(node: &RuntimeAstNode) -> Result<Expr> {
    match node.kind {
        AstNodeKind::Number => Ok(Expr::at(0, 1, ExprKind::Number(node.text.clone()))),
        AstNodeKind::String => Ok(Expr::at(0, 1, ExprKind::String(node.text.clone()))),
        AstNodeKind::Bool => Ok(Expr::at(0, 1, ExprKind::Bool(node.bool_val))),
        AstNodeKind::NoneLit => Ok(Expr::at(0, 1, ExprKind::None)),
        AstNodeKind::VarRef => Ok(Expr::at(0, 1, ExprKind::Var(node.text.clone()))),
        AstNodeKind::Placeholder => Ok(Expr::at(0, 1, ExprKind::Placeholder)),
        AstNodeKind::Unary => Ok(Expr::at(0, 1, ExprKind::Unary {
            op: parse_unary_op(&node.text)?,
            operand: Box::new(ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                RuntimeError::msg("unary missing operand")
            })?)?),
        })),
        AstNodeKind::Binary => Ok(Expr::at(0, 1, ExprKind::Binary {
            op: parse_binary_op(&node.text)?,
            left: Box::new(ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                RuntimeError::msg("binary missing left")
            })?)?),
            right: Box::new(ast_to_expr(node.slot_b.as_deref().ok_or_else(|| {
                RuntimeError::msg("binary missing right")
            })?)?),
        })),
        AstNodeKind::FuncCall => Ok(Expr::at(0, 1, ExprKind::Call {
            callee: Box::new(ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                RuntimeError::msg("call missing callee")
            })?)?),
            args: node.call_args.iter().map(ast_to_call_arg).collect::<Result<_>>()?,
        })),
        AstNodeKind::MacroCall => Ok(Expr::at(0, 1, ExprKind::MacroCall {
            callee: Box::new(ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                RuntimeError::msg("macro call missing callee")
            })?)?),
            args: node
                .call_args
                .iter()
                .map(ast_to_macro_call_arg)
                .collect::<Result<_>>()?,
        })),
        AstNodeKind::MemberAccess => Ok(Expr::at(0, 1, ExprKind::Member {
            object: Box::new(ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                RuntimeError::msg("member missing object")
            })?)?),
            field: node.text.clone(),
        })),
        AstNodeKind::IndexAccess => Ok(Expr::at(0, 1, ExprKind::Index {
            object: Box::new(ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                RuntimeError::msg("index missing object")
            })?)?),
            index: Box::new(ast_to_expr(node.slot_b.as_deref().ok_or_else(|| {
                RuntimeError::msg("index missing index")
            })?)?),
        })),
        AstNodeKind::TypeConvert => Ok(Expr::at(0, 1, ExprKind::TypeConvert {
            type_expr: Box::new(ast_to_expr(node.slot_a.as_deref().ok_or_else(|| {
                RuntimeError::msg("type convert missing type")
            })?)?),
            value: Box::new(ast_to_expr(node.slot_b.as_deref().ok_or_else(|| {
                RuntimeError::msg("type convert missing value")
            })?)?),
        })),
        AstNodeKind::Vector => Ok(Expr::at(0, 1, ExprKind::List(
            node.children.iter().map(ast_to_expr).collect::<Result<_>>()?,
        ))),
        AstNodeKind::BlockStmt => {
            let stmts = ast_to_block(node)?;
            if stmts.len() == 1 {
                if let Stmt::Expr(e) = &stmts[0].stmt {
                    return Ok(e.clone());
                }
            }
            Err(RuntimeError::msg("block used as expression in eval"))
        }
        AstNodeKind::QuoteExpr => Ok(Expr::at(0, 1, ExprKind::Quote {
            hygienic_names: node.hygienic_names.clone(),
            bindings: node
                .binding_names
                .iter()
                .zip(node.bindings.iter())
                .map(|(name, _)| Ok(Expr::at(0, 1, ExprKind::Var(name.clone()))))
                .collect::<Result<_>>()?,
            body: ast_to_block(node.slot_a.as_deref().ok_or_else(|| {
                RuntimeError::msg("quote missing body")
            })?)?,
        })),
        _ => Err(RuntimeError::msg(format!(
            "cannot convert AST kind {:?} to expression",
            node.kind
        ))),
    }
}

fn ast_to_call_arg(arg: &AstCallArg) -> Result<CallArg> {
    Ok(CallArg {
        name: if arg.kw_name.is_empty() {
            None
        } else {
            Some(arg.kw_name.clone())
        },
        is_splat: arg.is_splat,
        is_kwsplat: false,
        value: ast_to_expr(&arg.value)?,
    })
}

fn ast_to_macro_call_arg(arg: &AstCallArg) -> Result<MacroCallArg> {
    Ok(MacroCallArg {
        is_splat: arg.is_splat,
        node: Arc::new(arg.value.clone()),
    })
}

fn parse_unary_op(text: &str) -> Result<UnaryOp> {
    match text {
        "-" => Ok(UnaryOp::Neg),
        "not" => Ok(UnaryOp::Not),
        "!" => Ok(UnaryOp::TruthyNot),
        "~" => Ok(UnaryOp::Invert),
        _ => Err(RuntimeError::msg(format!("unknown unary op: {text}"))),
    }
}

fn parse_binary_op(text: &str) -> Result<BinaryOp> {
    match text {
        "+" => Ok(BinaryOp::Add),
        "-" => Ok(BinaryOp::Sub),
        "*" => Ok(BinaryOp::Mul),
        "/" => Ok(BinaryOp::Div),
        "%" => Ok(BinaryOp::Mod),
        "**" => Ok(BinaryOp::Pow),
        "&" => Ok(BinaryOp::BitAnd),
        "|" => Ok(BinaryOp::BitOr),
        "^" => Ok(BinaryOp::BitXor),
        "<<" => Ok(BinaryOp::LShift),
        ">>" => Ok(BinaryOp::RShift),
        "==" => Ok(BinaryOp::Eq),
        "!=" => Ok(BinaryOp::Ne),
        "<" => Ok(BinaryOp::Lt),
        "<=" => Ok(BinaryOp::Le),
        ">" => Ok(BinaryOp::Gt),
        ">=" => Ok(BinaryOp::Ge),
        "in" => Ok(BinaryOp::In),
        "is" => Ok(BinaryOp::Is),
        "is not" => Ok(BinaryOp::IsNot),
        "and" => Ok(BinaryOp::And),
        "or" => Ok(BinaryOp::Or),
        _ => Err(RuntimeError::msg(format!("unknown binary op: {text}"))),
    }
}

pub fn parse_to_ast(source: &str) -> Result<RuntimeAstNode> {
    let program = Parser::parse(source).map_err(|e| RuntimeError::msg(e.to_string()))?;
    Ok(RuntimeAstNode {
        kind: AstNodeKind::BlockStmt,
        stmts: program.stmts.iter().map(|ls| ast_from_stmt(&ls.stmt)).collect(),
        ..default_node()
    })
}

pub fn clone_ast_value(v: &Value) -> Result<Value> {
    Ok(Value::RuntimeAst(Arc::new(value_as_ast(v)?)))
}

pub fn compose_ast_type_convert(type_ast: &Value, value_ast: &Value) -> Result<Value> {
    let mut node = RuntimeAstNode {
        kind: AstNodeKind::TypeConvert,
        slot_a: Some(Box::new(value_as_ast(type_ast)?)),
        slot_b: Some(Box::new(value_as_ast(value_ast)?)),
        ..default_node()
    };
    std::mem::swap(&mut node.slot_a, &mut node.slot_b);
    Ok(node.as_value())
}

pub fn compose_ast_func_call(callee: &Value, args_vec: &Value) -> Result<Value> {
    compose_ast_call(AstNodeKind::FuncCall, callee, args_vec, "func_call")
}

pub fn compose_ast_macro_call(callee: &Value, args_vec: &Value) -> Result<Value> {
    compose_ast_call(AstNodeKind::MacroCall, callee, args_vec, "macro_call")
}

fn compose_ast_call(
    kind: AstNodeKind,
    callee: &Value,
    args_vec: &Value,
    ctx: &str,
) -> Result<Value> {
    let args = expect_ast_list(args_vec, ctx)?;
    let node = RuntimeAstNode {
        kind,
        slot_a: Some(Box::new(value_as_ast(callee)?)),
        call_args: args
            .into_iter()
            .map(|a| AstCallArg {
                kw_name: String::new(),
                is_splat: false,
                value: a,
            })
            .collect(),
        ..default_node()
    };
    Ok(node.as_value())
}

fn expect_ast_list(v: &Value, ctx: &str) -> Result<Vec<RuntimeAstNode>> {
    let Value::List(lst) = v else {
        return Err(RuntimeError::type_err(format!("{ctx} expects a list")));
    };
    lst.borrow()
        .iter()
        .map(value_as_ast)
        .collect()
}

pub fn ast_vec_push(vec_value: &Value, ast_value: &Value) -> Result<Value> {
    let Value::List(lst) = vec_value else {
        return Err(RuntimeError::type_err("ast_vec_push expects a list"));
    };
    lst.borrow_mut().push(clone_ast_value(ast_value)?);
    Ok(vec_value.clone())
}

pub fn ast_vec_extend(vec_value: &Value, more: &Value) -> Result<Value> {
    let Value::List(lst) = vec_value else {
        return Err(RuntimeError::type_err("ast_vec_extend expects a list"));
    };
    let more_nodes = expect_ast_list(more, "ast_vec_extend")?;
    let mut borrow = lst.borrow_mut();
    for n in more_nodes {
        borrow.push(Value::RuntimeAst(Arc::new(n)));
    }
    Ok(vec_value.clone())
}

pub fn ast_struct_value(vm: &Vm, v: &Value) -> Result<Value> {
    let node = value_as_ast(v)?;
    runtime_ast_to_struct(vm, &node)
}

fn runtime_ast_to_struct(vm: &Vm, node: &RuntimeAstNode) -> Result<Value> {
    let ast_field = |slot: Option<&RuntimeAstNode>| -> Value {
        slot.map(|n| n.clone().as_value())
            .unwrap_or(Value::None)
    };
    let call_args_list = |args: &[AstCallArg]| -> Value {
        Value::List(Shared::new(
            args.iter()
                .map(|a| a.value.clone().as_value())
                .collect(),
        ))
    };

    let (name, slots): (&str, Vec<Value>) = match node.kind {
        AstNodeKind::Number => ("AstNumber", vec![Value::Text(node.text.clone())]),
        AstNodeKind::String => ("AstString", vec![Value::Text(node.text.clone())]),
        AstNodeKind::Bool => ("AstBool", vec![Value::Bool(node.bool_val)]),
        AstNodeKind::VarRef => ("AstVarRef", vec![Value::Text(node.text.clone())]),
        AstNodeKind::Unary => (
            "AstUnary",
            vec![
                Value::Text(node.text.clone()),
                ast_field(node.slot_a.as_deref()),
            ],
        ),
        AstNodeKind::Binary => (
            "AstBinary",
            vec![
                Value::Text(node.text.clone()),
                ast_field(node.slot_a.as_deref()),
                ast_field(node.slot_b.as_deref()),
            ],
        ),
        AstNodeKind::FuncCall => (
            "AstFuncCall",
            vec![
                ast_field(node.slot_a.as_deref()),
                call_args_list(&node.call_args),
            ],
        ),
        AstNodeKind::MacroCall => (
            "AstMacroCall",
            vec![
                ast_field(node.slot_a.as_deref()),
                call_args_list(&node.call_args),
            ],
        ),
        AstNodeKind::MemberAccess => (
            "AstMemberAccess",
            vec![
                ast_field(node.slot_a.as_deref()),
                Value::Text(node.text.clone()),
            ],
        ),
        AstNodeKind::TypeConvert => (
            "AstTypeConvert",
            vec![
                ast_field(node.slot_a.as_deref()),
                ast_field(node.slot_b.as_deref()),
            ],
        ),
        AstNodeKind::IndexAccess => (
            "AstIndexAccess",
            vec![
                ast_field(node.slot_a.as_deref()),
                ast_field(node.slot_b.as_deref()),
            ],
        ),
        AstNodeKind::Vector => (
            "AstVector",
            vec![Value::List(Shared::new(
                node.children
                    .iter()
                    .map(|c| c.clone().as_value())
                    .collect(),
            ))],
        ),
        AstNodeKind::QuoteExpr => (
            "AstQuote",
            vec![
                ast_field(node.slot_a.as_deref()),
                Value::List(Shared::new(
                    node.bindings.iter().map(|b| b.clone().as_value()).collect(),
                )),
            ],
        ),
        other => {
            return Err(RuntimeError::msg(format!(
                "cannot convert AST kind {other:?} to struct"
            )))
        }
    };

    make_struct_instance(vm, name, slots)
}

fn make_struct_instance(vm: &Vm, name: &str, slots: Vec<Value>) -> Result<Value> {
    let def = vm
        .struct_defs
        .get(name)
        .cloned()
        .ok_or_else(|| RuntimeError::msg(format!("unknown struct type: {name}")))?;
    Ok(Value::Struct(Arc::new(StructInstance {
        def,
        slots: crate::shared::SyncCell::new(slots),
        generic_args: Vec::new(),
    })))
}

pub fn register_ast_struct_types(vm: &mut Vm) {
    let defs = [
        ("AstNode", None, vec![] as Vec<(&str, &str)>),
        ("AstNumber", Some("AstNode"), vec![("value", "text")]),
        ("AstString", Some("AstNode"), vec![("value", "text")]),
        ("AstBool", Some("AstNode"), vec![("value", "bool")]),
        ("AstVarRef", Some("AstNode"), vec![("name", "text")]),
        (
            "AstUnary",
            Some("AstNode"),
            vec![("op", "text"), ("operand", "AST")],
        ),
        (
            "AstBinary",
            Some("AstNode"),
            vec![("op", "text"), ("left", "AST"), ("right", "AST")],
        ),
        (
            "AstFuncCall",
            Some("AstNode"),
            vec![("callee", "AST"), ("args", "list")],
        ),
        (
            "AstMacroCall",
            Some("AstNode"),
            vec![("callee", "AST"), ("args", "list")],
        ),
        (
            "AstMemberAccess",
            Some("AstNode"),
            vec![("object", "AST"), ("member", "text")],
        ),
        (
            "AstTypeConvert",
            Some("AstNode"),
            vec![("type_expr", "AST"), ("value", "AST")],
        ),
        (
            "AstIndexAccess",
            Some("AstNode"),
            vec![("object", "AST"), ("index", "AST")],
        ),
        ("AstVector", Some("AstNode"), vec![("elements", "list")]),
        (
            "AstQuote",
            Some("AstNode"),
            vec![("body", "AST"), ("bindings", "list")],
        ),
    ];

    for (name, base, fields) in defs {
        vm.struct_defs.entry(name.to_string()).or_insert_with(|| {
            Arc::new(StructDef {
                name: name.to_string(),
                base: base.map(str::to_string),
                fields: fields.iter().map(|(f, _)| f.to_string()).collect(),
                mutable_fields: fields.iter().map(|_| false).collect(),
                typed: false,
                field_types: fields
                    .iter()
                    .map(|_| FieldTypeInfo::default())
                    .collect(),
                type_params: Vec::new(),
                c_layout: None,
            })
        });
        vm.globals
            .or_insert_with(name.to_string(), || Value::type_ref(name));
    }
    vm.globals
        .or_insert_with("AST".to_string(), || Value::type_ref("AST"));
}

pub fn check_macro_param_ast_kind(param_type: &TypeExpr, ast: &RuntimeAstNode) -> Result<()> {
    let TypeExpr::Name(name) = param_type else {
        return Ok(());
    };
    let Some(expected) = annotation_to_kind(name) else {
        return Ok(());
    };
    if ast.kind != expected {
        return Err(RuntimeError::msg(format!(
            "macro parameter expected AST node kind {name}, got {:?}",
            ast.kind
        )));
    }
    Ok(())
}
