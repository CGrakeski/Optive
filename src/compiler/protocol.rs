//! Protocol（协议）定义与类型约束检查。

use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::{ProtocolMember, TypeExpr};
use crate::error::RuntimeError;
use crate::type_registry;
use crate::types::{instance_is_a, type_expr_display};
use crate::vm::Vm;

#[derive(Debug, Clone)]
pub struct ProtocolDef {
    pub name: String,
    pub methods: Vec<String>,
    pub fields: Vec<(String, bool)>,
}

pub fn protocol_from_members(name: String, members: Vec<ProtocolMember>) -> ProtocolDef {
    let mut methods = Vec::new();
    let mut fields = Vec::new();
    for m in members {
        match m {
            ProtocolMember::Method { name, .. } => methods.push(name),
            ProtocolMember::Field { name, mutable } => fields.push((name, mutable)),
        }
    }
    ProtocolDef {
        name,
        methods,
        fields,
    }
}

pub fn is_protocol(
    protocols: &std::collections::HashMap<String, Arc<ProtocolDef>, impl std::hash::BuildHasher>,
    name: &str,
) -> bool {
    protocols.contains_key(name)
}

pub struct TypeCheckContext {
    pub struct_defs: HashMap<String, Arc<crate::value::StructDef>>,
    pub functions: HashMap<String, Arc<crate::opcode::FunctionObject>>,
    pub protocols: HashMap<String, Arc<ProtocolDef>>,
}

impl TypeCheckContext {
    pub fn from_program(program: &crate::opcode::CompiledProgram) -> Self {
        Self {
            struct_defs: program.struct_defs.clone(),
            functions: program
                .functions
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            protocols: program
                .protocols
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    pub fn from_vm(vm: &crate::vm::Vm) -> Self {
        Self {
            struct_defs: vm
                .struct_defs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            functions: vm
                .functions
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            protocols: vm
                .protocols
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }
}

pub fn type_satisfies_protocol_ctx(
    ctx: &TypeCheckContext,
    ty: &TypeExpr,
    protocol_name: &str,
) -> bool {
    let Some(def) = ctx.protocols.get(protocol_name) else {
        return false;
    };
    let type_name = type_expr_base_name(ty);
    for method in &def.methods {
        if !type_has_method_ctx(ctx, &type_name, method) {
            return false;
        }
    }
    for (field, mutable) in &def.fields {
        if !type_has_field_ctx(ctx, &type_name, field, *mutable) {
            return false;
        }
    }
    true
}

pub fn check_type_bound_ctx(
    ctx: &TypeCheckContext,
    concrete: &TypeExpr,
    bound: &TypeExpr,
) -> Result<(), RuntimeError> {
    match bound {
        TypeExpr::Name(name) if is_protocol(&ctx.protocols, name) => {
            if type_satisfies_protocol_ctx(ctx, concrete, name) {
                Ok(())
            } else {
                Err(RuntimeError::msg(format!(
                    "type `{}` does not satisfy protocol `{name}`",
                    type_expr_display(concrete)
                )))
            }
        }
        TypeExpr::Name(name) => {
            if instance_is_a_type_expr_ctx(ctx, concrete, name) {
                Ok(())
            } else {
                Err(RuntimeError::msg(format!(
                    "type `{}` is not compatible with bound `{name}`",
                    type_expr_display(concrete)
                )))
            }
        }
        TypeExpr::Generic { name, params } => {
            let bound_ty = TypeExpr::Generic {
                name: name.clone(),
                params: params.clone(),
            };
            if type_expr_same(concrete, &bound_ty) || instance_is_a_type_expr_ctx(ctx, concrete, name) {
                Ok(())
            } else {
                Err(RuntimeError::msg(format!(
                    "type `{}` does not match bound `{}`",
                    type_expr_display(concrete),
                    type_expr_display(&bound_ty)
                )))
            }
        }
        TypeExpr::Attr { object, field } => {
            let path = format!("{}.{}", type_expr_base_name(object), field);
            if instance_is_a_type_expr_ctx(ctx, concrete, &path) {
                Ok(())
            } else {
                Err(RuntimeError::msg(format!(
                    "type `{}` is not compatible with bound `{path}`",
                    type_expr_display(concrete)
                )))
            }
        }
    }
}

fn type_expr_base_name(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Name(n) => n.clone(),
        TypeExpr::Attr { object, field } => {
            format!("{}.{}", type_expr_base_name(object), field)
        }
        TypeExpr::Generic { name, .. } => name.clone(),
    }
}

fn type_has_method_ctx(ctx: &TypeCheckContext, type_name: &str, method: &str) -> bool {
    if type_registry::protocol_has_method(type_name, method) {
        return true;
    }
    let key = format!("{type_name}.{method}");
    ctx.functions.contains_key(&key)
}

fn type_has_field_ctx(ctx: &TypeCheckContext, type_name: &str, field: &str, _mutable: bool) -> bool {
    ctx.struct_defs
        .get(type_name)
        .is_some_and(|def| def.fields.iter().any(|f| f == field))
}

fn instance_is_a_type_expr_ctx(ctx: &TypeCheckContext, ty: &TypeExpr, bound_name: &str) -> bool {
    match ty {
        TypeExpr::Name(n) => type_name_is_a(ctx, n, bound_name),
        TypeExpr::Attr { object, field } => {
            let path = format!("{}.{}", type_expr_base_name(object), field);
            type_name_is_a(ctx, &path, bound_name)
        }
        TypeExpr::Generic { name, .. } => {
            name == bound_name || type_name_is_a(ctx, name, bound_name)
        }
    }
}

fn type_name_is_a(ctx: &TypeCheckContext, name: &str, bound_name: &str) -> bool {
    if name == bound_name {
        return true;
    }
    if let Some(def) = ctx.struct_defs.get(name) {
        if let Some(base) = &def.base {
            return type_name_is_a(ctx, base, bound_name);
        }
    }
    instance_is_a(&Vm::new(), &type_registry::sample_value_for_type_name(name), bound_name)
}

fn type_expr_same(a: &TypeExpr, b: &TypeExpr) -> bool {
    match (a, b) {
        (TypeExpr::Name(x), TypeExpr::Name(y)) => x == y,
        (
            TypeExpr::Attr {
                object: o1,
                field: f1,
            },
            TypeExpr::Attr {
                object: o2,
                field: f2,
            },
        ) => f1 == f2 && type_expr_same(o1, o2),
        (
            TypeExpr::Generic {
                name: n1,
                params: p1,
            },
            TypeExpr::Generic {
                name: n2,
                params: p2,
            },
        ) => {
            n1 == n2
                && p1.len() == p2.len()
                && p1.iter().zip(p2.iter()).all(|(a, b)| type_expr_same(a, b))
        }
        _ => false,
    }
}
