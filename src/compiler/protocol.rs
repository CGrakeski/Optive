//! Protocol（协议）定义与类型约束检查。

use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::{Expr, ProtocolMember};
use crate::error::RuntimeError;
use crate::type_registry;
use crate::types::{
    self, instance_is_a, static_type_value_from_expr, type_value_base, type_value_display,
    type_values_equal,
};
use crate::value::Value;
use crate::vm::Vm;

#[derive(Debug, Clone)]
pub struct ProtocolDef {
    pub name: String,
    pub methods: Vec<String>,
    pub fields: Vec<(String, bool)>,
}

#[must_use]
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
    #[must_use]
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
            struct_defs: vm.struct_defs.snapshot_map().into_iter().collect(),
            functions: vm.functions.snapshot_map().into_iter().collect(),
            protocols: vm.protocols.snapshot_map().into_iter().collect(),
        }
    }
}

#[must_use]
pub fn type_satisfies_protocol_ctx(
    ctx: &TypeCheckContext,
    ty: &Value,
    protocol_name: &str,
) -> bool {
    let Some(def) = ctx.protocols.get(protocol_name) else {
        return false;
    };
    let type_name = type_value_base_name(ty);
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
    concrete: &Value,
    bound: &Expr,
) -> Result<(), RuntimeError> {
    let Some(bound_ty) = static_type_value_from_expr(bound) else {
        return Err(RuntimeError::msg(format!(
            "type bound `{}` is not a static type expression",
            types::type_expr_display(concrete)
        )));
    };
    check_type_bound_value_ctx(ctx, concrete, &bound_ty)
}

pub fn check_type_bound_value_ctx(
    ctx: &TypeCheckContext,
    concrete: &Value,
    bound: &Value,
) -> Result<(), RuntimeError> {
    match bound {
        Value::TypeRef(name) | Value::Text(name) if is_protocol(&ctx.protocols, name) => {
            if type_satisfies_protocol_ctx(ctx, concrete, name) {
                Ok(())
            } else {
                Err(RuntimeError::msg(format!(
                    "type `{}` does not satisfy protocol `{name}`",
                    type_value_display(concrete)
                )))
            }
        }
        Value::TypeRef(name) | Value::Text(name) => {
            if instance_is_a_type_value_ctx(ctx, concrete, name) {
                Ok(())
            } else {
                Err(RuntimeError::msg(format!(
                    "type `{}` is not compatible with bound `{name}`",
                    type_value_display(concrete)
                )))
            }
        }
        Value::TypeSpec(spec) => {
            if type_values_equal(concrete, bound)
                || instance_is_a_type_value_ctx(ctx, concrete, &spec.name)
            {
                Ok(())
            } else {
                Err(RuntimeError::msg(format!(
                    "type `{}` does not match bound `{}`",
                    type_value_display(concrete),
                    type_value_display(bound)
                )))
            }
        }
        _ => {
            // 非常规 bound：不做空 Vm 猜测，直接拒绝以免误通过。
            Err(RuntimeError::msg(format!(
                "type `{}` is not compatible with bound `{}`",
                type_value_display(concrete),
                type_value_display(bound)
            )))
        }
    }
}

fn type_value_base_name(ty: &Value) -> String {
    type_value_base(ty).map_or_else(|| type_value_display(ty), str::to_string)
}

fn type_has_method_ctx(ctx: &TypeCheckContext, type_name: &str, method: &str) -> bool {
    if type_registry::protocol_has_method(type_name, method) {
        return true;
    }
    let key = format!("{type_name}.{method}");
    ctx.functions.contains_key(&key)
}

fn type_has_field_ctx(ctx: &TypeCheckContext, type_name: &str, field: &str, mutable: bool) -> bool {
    ctx.struct_defs.get(type_name).is_some_and(|def| {
        def.fields.iter().enumerate().any(|(i, f)| {
            f == field && (!mutable || def.mutable_fields.get(i).copied().unwrap_or(false))
        })
    })
}

fn instance_is_a_type_value_ctx(ctx: &TypeCheckContext, ty: &Value, bound_name: &str) -> bool {
    match ty {
        Value::TypeRef(n) | Value::Text(n) => type_name_is_a(ctx, n, bound_name),
        Value::TypeSpec(spec) => {
            spec.name == bound_name || type_name_is_a(ctx, &spec.name, bound_name)
        }
        _ => false,
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
        return false;
    }
    // 内置类型：用样例值做原始 is-a（不依赖用户 struct_defs）。
    let sample = type_registry::sample_value_for_type_name(name);
    if matches!(sample, Value::TypeRef(_)) {
        return false;
    }
    let vm = Vm::new();
    instance_is_a(&vm, &sample, bound_name)
}
