//! 硬注解运行时检查：注解是 `Expr`，求值得到类型值后再 `is_a`。

use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::{Expr, ExprKind};
use crate::error::RuntimeError;
use crate::opcode::FunctionObject;
use crate::protocol;
use crate::type_registry;
use crate::value::{TypeSpecData, Value};
use crate::vm::Vm;

/// 值是否为元类型 `type` 的实例（类型句柄 / 类型形态）。
#[must_use]
pub const fn value_is_type(t: &Value) -> bool {
    matches!(t, Value::TypeRef(_) | Value::TypeSpec(_))
}

/// 将类型索引操作数规范为类型实参列表（每个元素已是类型值）。
pub fn type_index_operand_to_args(val: &Value) -> crate::Result<Vec<Value>> {
    match val {
        Value::TypeRef(_) | Value::TypeSpec(_) => Ok(vec![val.clone()]),
        Value::Text(name) => Ok(vec![Value::type_ref(name.clone())]),
        Value::List(lst) => lst
            .borrow()
            .iter()
            .map(|v| {
                let mut args = type_index_operand_to_args(v)?;
                if args.len() != 1 {
                    return Err(RuntimeError::msg(
                        "expected single type argument in this position",
                    ));
                }
                Ok(args.remove(0))
            })
            .collect(),
        other => Err(RuntimeError::msg(format!(
            "expected type argument, got {}",
            other.type_name()
        ))),
    }
}

/// `val` 是否为 `type_name` 的实例或子类型（is-a，非类型句柄相等）。
pub fn instance_is_a(vm: &Vm, val: &Value, type_name: &str) -> bool {
    if type_name == "Never" {
        return false;
    }
    if vm.protocols.contains_key(type_name) {
        return value_satisfies_protocol(vm, val, type_name);
    }
    if let Some(is_prim) = type_registry::check_primitive_instance(vm, val, type_name) {
        return is_prim;
    }
    if let Some(edef) = vm.enum_defs.get(type_name) {
        return matches!(
            val,
            Value::EnumMember(m) if Arc::ptr_eq(&m.def, &edef)
        );
    }
    if let Value::Struct(s) = val {
        type_registry::struct_name_is_a(vm, &s.def.name, type_name)
    } else if let Value::Variant(v) = val {
        v.inst_name == type_name || v.def.name == type_name
    } else {
        false
    }
}

pub fn instance_match_distance(vm: &Vm, val: &Value, type_name: &str) -> Option<usize> {
    if !instance_is_a(vm, val, type_name) {
        return None;
    }
    match val {
        Value::Struct(s) => type_registry::struct_name_distance(vm, &s.def.name, type_name),
        _ => Some(0),
    }
}

/// `val` 是否满足类型值 `ty`（`TypeRef` / `TypeSpec`）。
pub fn value_accepts(vm: &Vm, val: &Value, ty: &Value) -> bool {
    match ty {
        Value::TypeRef(name) if name == "Never" => false,
        Value::TypeRef(name) if vm.protocols.contains_key(name) => {
            value_satisfies_protocol(vm, val, name)
        }
        Value::TypeRef(name) => instance_is_a(vm, val, name),
        Value::TypeSpec(spec) => {
            if type_registry::is_type_form(&spec.name) {
                type_registry::type_form_accepts(vm, val, &spec.name, &spec.args)
            } else if let Value::Struct(s) = val {
                type_registry::struct_name_is_a(vm, &s.def.name, &spec.name)
                    && generic_args_match(&s.generic_args, &spec.args)
            } else {
                // 无类型实参的规格才允许退化为名字匹配。
                spec.args.is_empty() && instance_is_a(vm, val, &spec.name)
            }
        }
        Value::Text(name) => instance_is_a(vm, val, name),
        _ => false,
    }
}

pub fn type_value_match_distance(vm: &Vm, val: &Value, ty: &Value) -> Option<usize> {
    match ty {
        Value::TypeRef(name) | Value::Text(name) => instance_match_distance(vm, val, name),
        Value::TypeSpec(spec) => {
            if let Some(score) =
                type_registry::type_form_match_distance(vm, val, &spec.name, &spec.args)
            {
                Some(score)
            } else if type_registry::is_type_form(&spec.name) {
                None
            } else if let Value::Struct(s) = val {
                if type_registry::struct_name_is_a(vm, &s.def.name, &spec.name)
                    && generic_args_match(&s.generic_args, &spec.args)
                {
                    type_registry::struct_name_distance(vm, &s.def.name, &spec.name)
                } else {
                    None
                }
            } else if spec.args.is_empty() {
                instance_match_distance(vm, val, &spec.name)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 定义处求值并绑定函数上的全部类型注解；结果必须是类型值。
/// 之后调用只比对 `param_types` / `return_type_value`，不再求值注解表达式。
pub fn bind_function_annotations(vm: &mut Vm, func: &mut FunctionObject) -> crate::Result<()> {
    if func.types_resolved() {
        return Ok(());
    }
    let prev = vm.annotation_bind_env.take();
    vm.annotation_bind_env.clone_from(&func.module_env);
    let result = (|| {
        let mut param_types = Vec::with_capacity(func.params.len());
        for param in &func.params {
            match &param.type_expr {
                None => param_types.push(None),
                Some(ann) => {
                    let ty = eval_type_annotation(vm, ann).map_err(|e| {
                        RuntimeError::type_err(format!(
                            "parameter '{}': {}",
                            param.name,
                            e.message()
                        ))
                    })?;
                    param_types.push(Some(ty));
                }
            }
        }
        let return_type_value = match &func.return_type {
            None => None,
            Some(ann) => Some(eval_type_annotation(vm, ann).map_err(|e| {
                RuntimeError::type_err(format!("return: {}", e.message()))
            })?),
        };
        func.param_types = param_types;
        func.return_type_value = return_type_value;
        func.set_types_resolved(true);
        Ok(())
    })();
    vm.annotation_bind_env = prev;
    result
}

pub fn dispatch_match_score(vm: &mut Vm, func: &FunctionObject, args: &[Value]) -> Option<usize> {
    let required = func
        .params
        .iter()
        .filter(|p| !p.is_variadic && !p.is_kwvariadic && p.default_expr.is_none())
        .count();
    if args.len() < required {
        return None;
    }
    if func.variadic_param_index.is_none()
        && func.kwvariadic_param_index.is_none()
        && args.len() > func.params.iter().filter(|p| !p.is_kwvariadic).count()
    {
        return None;
    }
    let mut score = 0usize;
    for (i, param) in func.params.iter().enumerate() {
        if param.is_variadic || param.is_kwvariadic {
            continue;
        }
        let Some(ty) = func.param_types.get(i).and_then(|t| t.as_ref()) else {
            continue;
        };
        let val = args.get(i)?;
        score += type_value_match_distance(vm, val, ty)?;
    }
    Some(score)
}

pub fn type_value_display(ty: &Value) -> String {
    match ty {
        Value::TypeRef(n) | Value::Text(n) => n.clone(),
        Value::TypeSpec(spec) => {
            if spec.args.is_empty() {
                format!("{}[]", spec.name)
            } else {
                let inner: Vec<String> = spec.args.iter().map(type_value_display).collect();
                format!("{}[{}]", spec.name, inner.join(", "))
            }
        }
        other => other.print_string(),
    }
}

/// 失败时返回 `expected X, got Y`；成功返回 `None`。
pub fn type_check_error(vm: &Vm, val: &Value, ty: &Value) -> Option<String> {
    if !value_is_type(ty) {
        return Some(format!(
            "type annotation evaluated to {}, expected a type",
            ty.type_name()
        ));
    }
    if value_accepts(vm, val, ty) {
        return None;
    }
    Some(explain_mismatch(vm, val, ty, ""))
}

fn mismatch_message(expected: &str, got: &str, path: &str) -> String {
    if path.is_empty() {
        format!("expected {expected}, got {got}")
    } else {
        format!("expected {expected}, got {got} at {path}")
    }
}

fn explain_mismatch(vm: &Vm, val: &Value, ty: &Value, path: &str) -> String {
    match ty {
        Value::TypeSpec(spec) if spec.name == "list" && spec.args.len() == 1 => {
            if let Value::List(lst) = val {
                for (i, item) in lst.borrow().iter().enumerate() {
                    if !value_accepts(vm, item, &spec.args[0]) {
                        let child = format!("{path}[{i}]");
                        return explain_mismatch(vm, item, &spec.args[0], &child);
                    }
                }
            }
            mismatch_message(&type_value_display(ty), val.type_name(), path)
        }
        Value::TypeSpec(spec) if spec.name == "dict" && spec.args.len() == 2 => {
            if let Value::Dict(d) = val {
                for (k, v) in d.borrow().iter() {
                    let kv = crate::value::value_key_to_value(k);
                    let key_disp = kv.print_string();
                    if !value_accepts(vm, &kv, &spec.args[0]) {
                        let child = format!("{path}[{key_disp}]");
                        return explain_mismatch(vm, &kv, &spec.args[0], &child);
                    }
                    if !value_accepts(vm, v, &spec.args[1]) {
                        let child = format!("{path}[{key_disp}]");
                        return explain_mismatch(vm, v, &spec.args[1], &child);
                    }
                }
            }
            mismatch_message(&type_value_display(ty), val.type_name(), path)
        }
        Value::TypeSpec(spec) if spec.name == "set" && spec.args.len() == 1 => {
            if let Value::Set(s) = val {
                for k in s.borrow().iter() {
                    let elem = crate::value::value_key_to_value(k);
                    if !value_accepts(vm, &elem, &spec.args[0]) {
                        let child = if path.is_empty() {
                            format!("{{{}}}", elem.print_string())
                        } else {
                            format!("{path}{{{}}}", elem.print_string())
                        };
                        return explain_mismatch(vm, &elem, &spec.args[0], &child);
                    }
                }
            }
            mismatch_message(&type_value_display(ty), val.type_name(), path)
        }
        Value::TypeSpec(spec) if spec.name == "Maybe" && spec.args.len() == 1 => {
            if matches!(val, Value::None) {
                return mismatch_message(&type_value_display(ty), val.type_name(), path);
            }
            if !value_accepts(vm, val, &spec.args[0]) {
                return explain_mismatch(vm, val, &spec.args[0], path);
            }
            mismatch_message(&type_value_display(ty), val.type_name(), path)
        }
        _ => mismatch_message(&type_value_display(ty), val.type_name(), path),
    }
}

pub fn seal_container_contract(vm: &mut Vm, val: &Value, ty: &Value) {
    match ty {
        Value::TypeSpec(spec) if spec.name == "list" && spec.args.len() == 1 => {
            if let Value::List(rc) = val {
                vm.list_element_contracts
                    .insert(rc.as_ptr() as usize, spec.args[0].clone());
            }
        }
        Value::TypeSpec(spec) if spec.name == "dict" && spec.args.len() == 2 => {
            if let Value::Dict(rc) = val {
                vm.dict_contracts.insert(
                    rc.as_ptr() as usize,
                    (spec.args[0].clone(), spec.args[1].clone()),
                );
            }
        }
        Value::TypeSpec(spec) if spec.name == "set" && spec.args.len() == 1 => {
            if let Value::Set(rc) = val {
                vm.set_element_contracts
                    .insert(rc.as_ptr() as usize, spec.args[0].clone());
            }
        }
        _ => {}
    }
}

fn value_satisfies_protocol(vm: &Vm, val: &Value, protocol_name: &str) -> bool {
    let type_name = match val {
        Value::Struct(s) => s.def.name.clone(),
        other => other.type_name().to_string(),
    };
    let ctx = protocol_ctx_from_vm(vm);
    protocol::type_satisfies_protocol_ctx(&ctx, &Value::type_ref(type_name), protocol_name)
}

/// 值的运行时类型（作类型值，供泛型推断）。
pub fn value_to_type_value(vm: &Vm, val: &Value) -> Value {
    type_registry::value_to_type_value(vm, val)
}

#[must_use]
pub fn substitute_type_value(ty: &Value, subs: &HashMap<String, Value>) -> Value {
    match ty {
        Value::TypeRef(n) | Value::Text(n) => subs
            .get(n)
            .cloned()
            .unwrap_or_else(|| Value::type_ref(n.clone())),
        Value::TypeSpec(spec) => Value::TypeSpec(TypeSpecData::new(
            spec.name.clone(),
            spec.args
                .iter()
                .map(|a| substitute_type_value(a, subs))
                .collect(),
        )),
        other => other.clone(),
    }
}

fn unify_type_param(inferred: &mut HashMap<String, Value>, param: &str, ty: Value) -> bool {
    if let Some(existing) = inferred.get(param) {
        type_values_equal(existing, &ty)
    } else {
        inferred.insert(param.to_string(), ty);
        true
    }
}

fn generic_args_match(actual: &[Value], expected: &[Value]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected.iter())
            .all(|(a, e)| type_values_equal(a, e))
}

#[must_use]
pub fn type_values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::TypeRef(x), Value::TypeRef(y)) | (Value::Text(x), Value::Text(y)) => x == y,
        (Value::TypeRef(x), Value::Text(y)) | (Value::Text(x), Value::TypeRef(y)) => x == y,
        (Value::TypeSpec(x), Value::TypeSpec(y)) => {
            x.name == y.name
                && x.args.len() == y.args.len()
                && x.args
                    .iter()
                    .zip(y.args.iter())
                    .all(|(u, v)| type_values_equal(u, v))
        }
        _ => false,
    }
}

#[must_use]
pub fn type_value_base(ty: &Value) -> Option<&str> {
    match ty {
        Value::TypeRef(n) | Value::Text(n) => Some(n.as_str()),
        Value::TypeSpec(spec) => Some(spec.name.as_str()),
        _ => None,
    }
}

/// 类型名是否可接受 `[T]` 索引（泛型 struct 或 type form）。
pub fn is_generic_type_formable(vm: &Vm, type_name: &str) -> bool {
    vm.struct_defs
        .get(type_name)
        .is_some_and(|def| !def.type_params.is_empty())
        || type_registry::is_type_form(type_name)
}

pub fn infer_from_field_type_inner(
    field_ty: &Value,
    val_ty: &Value,
    inferred: &mut HashMap<String, Value>,
) -> bool {
    match field_ty {
        Value::TypeRef(p) | Value::Text(p) => unify_type_param(inferred, p, val_ty.clone()),
        Value::TypeSpec(_) => type_registry::type_form_infer(field_ty, val_ty, inferred),
        _ => true,
    }
}

pub fn infer_generic_args(
    vm: &Vm,
    def: &crate::value::StructDef,
    args: &[Value],
) -> HashMap<String, Value> {
    let mut inferred = HashMap::new();
    for (i, val) in args.iter().enumerate() {
        if let Some(info) = def.field_types.get(i) {
            if let Some(ref field_expr) = info.type_expr {
                // 字段注解是 Expr；推断时只折叠静态类型字面（Var / Index / Member）。
                if let Some(field_ty) = static_type_value_from_expr(field_expr) {
                    let val_ty = value_to_type_value(vm, val);
                    let _ = infer_from_field_type_inner(&field_ty, &val_ty, &mut inferred);
                }
            }
        }
    }
    inferred
}

/// 将静态类型字面 Expr 折成类型值（`num`、`list[num]`、`C.types.int` 经属性链 → TypeRef(`int`)）。
/// 未知 Member 链不拼点分 TypeRef（禁止发明 `a.b.c` 身份）。
#[must_use]
pub fn static_type_value_from_expr(expr: &Expr) -> Option<Value> {
    match &expr.kind {
        ExprKind::Var(name) => Some(Value::type_ref(name.clone())),
        ExprKind::Member { .. } => {
            let parts = static_member_parts(expr)?;
            let c_name = resolve_c_types_attr(&parts)?;
            Some(Value::type_ref(c_name.to_string()))
        }
        ExprKind::Index { object, index } => {
            let name = match &object.kind {
                ExprKind::Var(n) => n.clone(),
                ExprKind::Member { .. } => {
                    let parts = static_member_parts(object)?;
                    resolve_c_types_attr(&parts)?.to_string()
                }
                _ => return None,
            };
            let args = static_type_args_from_index(index)?;
            Some(Value::TypeSpec(TypeSpecData::new(name, args)))
        }
        _ => None,
    }
}

fn static_member_parts(expr: &Expr) -> Option<Vec<String>> {
    match &expr.kind {
        ExprKind::Var(name) => Some(vec![name.clone()]),
        ExprKind::Member { object, field } => {
            let mut parts = static_member_parts(object)?;
            parts.push(field.clone());
            Some(parts)
        }
        _ => None,
    }
}

/// `….C.types.<attr>` → 规范 c_name（getattr，不拼点分 TypeRef）。
#[must_use]
pub fn resolve_c_types_attr(parts: &[String]) -> Option<&'static str> {
    match parts {
        [.., mod_c, types, attr] if mod_c == "C" && types == "types" => {
            crate::c_types::lookup_c_type(attr).map(|e| e.c_name)
        }
        _ => None,
    }
}

fn static_type_args_from_index(index: &Expr) -> Option<Vec<Value>> {
    match &index.kind {
        ExprKind::List(items) => items.iter().map(static_type_value_from_expr).collect(),
        other => Some(vec![static_type_value_from_expr(&Expr {
            loc: index.loc,
            kind: other.clone(),
        })?]),
    }
}

pub fn check_type_param_bounds(
    vm: &Vm,
    type_params: &[(String, Option<Expr>)],
    generic_args: &[Value],
) -> bool {
    if generic_args.len() != type_params.len() {
        return false;
    }
    for (i, (_, bound)) in type_params.iter().enumerate() {
        let actual = &generic_args[i];
        if let Some(bound_expr) = bound {
            let Some(bound_ty) = static_type_value_from_expr(bound_expr) else {
                return false;
            };
            if !type_value_implies(vm, actual, &bound_ty) {
                return false;
            }
        }
    }
    true
}

pub fn type_value_implies(vm: &Vm, actual: &Value, bound: &Value) -> bool {
    if let Some(result) = type_registry::type_form_implies(vm, actual, bound) {
        return result;
    }
    type_value_implies_inner(vm, actual, bound)
}

pub fn type_value_implies_inner(vm: &Vm, actual: &Value, bound: &Value) -> bool {
    match (actual, bound) {
        (Value::TypeRef(a), Value::TypeRef(b)) | (Value::Text(a), Value::Text(b)) => {
            a == b
                || type_satisfies_bound_name(vm, actual, b)
                || type_registry::struct_name_is_a(vm, a, b)
        }
        (Value::TypeRef(a), Value::Text(b)) | (Value::Text(a), Value::TypeRef(b)) => {
            a == b
                || type_satisfies_bound_name(vm, actual, b)
                || type_registry::struct_name_is_a(vm, a, b)
        }
        (Value::TypeSpec(a), Value::TypeSpec(b)) => {
            (a.name == b.name || type_registry::struct_name_is_a(vm, &a.name, &b.name))
                && a.args.len() == b.args.len()
                && a.args
                    .iter()
                    .zip(b.args.iter())
                    .all(|(x, y)| type_value_implies_inner(vm, x, y))
        }
        (Value::TypeRef(a) | Value::Text(a), Value::TypeSpec(b)) => {
            a == &b.name || type_registry::struct_name_is_a(vm, a, &b.name)
        }
        (Value::TypeSpec(a), Value::TypeRef(b) | Value::Text(b)) => {
            a.name == *b
                || type_satisfies_bound_name(vm, actual, b)
                || type_registry::struct_name_is_a(vm, &a.name, b)
        }
        _ => false,
    }
}

fn type_satisfies_bound_name(vm: &Vm, actual: &Value, bound_name: &str) -> bool {
    if !vm.protocols.contains_key(bound_name) {
        return false;
    }
    let ctx = protocol_ctx_from_vm(vm);
    protocol::type_satisfies_protocol_ctx(&ctx, actual, bound_name)
}

fn protocol_ctx_from_vm(vm: &Vm) -> protocol::TypeCheckContext {
    protocol::TypeCheckContext {
        struct_defs: vm.struct_defs.snapshot_map().into_iter().collect(),
        functions: vm.functions.snapshot_map().into_iter().collect(),
        protocols: vm.protocols.snapshot_map().into_iter().collect(),
    }
}

/// 兼容旧名：显示类型值。
#[must_use]
pub fn type_expr_display(ty: &Value) -> String {
    type_value_display(ty)
}

/// 兼容旧名：`value_accepts`。
pub fn type_accepts(vm: &Vm, val: &Value, ty: &Value) -> bool {
    value_accepts(vm, val, ty)
}

/// 将静态类型注解 `Expr` 解析为点分类型名（编译期 / ABI）。
pub fn resolve_type_name_from_expr(_vm: &Vm, expr: &Expr) -> crate::Result<String> {
    if let Some(v) = static_type_value_from_expr(expr) {
        return Ok(type_value_display(&v));
    }
    match &expr.kind {
        ExprKind::Var(name) => Ok(name.clone()),
        ExprKind::Member { object, field } => Ok(format!(
            "{}.{}",
            resolve_type_name_from_expr(_vm, object)?,
            field
        )),
        _ => Err(RuntimeError::type_err("expected type name")),
    }
}

/// 运行时求值类型注解：走 VM 真求值（`load_name` / 属性 / 索引 / 调用），结果须为类型值。
pub fn eval_type_annotation(vm: &mut Vm, expr: &Expr) -> crate::Result<Value> {
    let v = vm.eval_expr(expr)?;
    if !value_is_type(&v) {
        return Err(RuntimeError::type_err(format!(
            "type annotation evaluated to {}, expected a type",
            v.type_name()
        )));
    }
    Ok(v)
}

fn type_value_to_expr(loc: crate::ast::SourceLoc, ty: &Value) -> Expr {
    match ty {
        Value::TypeRef(n) | Value::Text(n) => Expr::new(loc, ExprKind::Var(n.clone())),
        Value::TypeSpec(spec) => {
            let object = Expr::new(loc, ExprKind::Var(spec.name.clone()));
            let index = if spec.args.len() == 1 {
                type_value_to_expr(loc, &spec.args[0])
            } else {
                Expr::new(
                    loc,
                    ExprKind::List(
                        spec.args
                            .iter()
                            .map(|a| type_value_to_expr(loc, a))
                            .collect(),
                    ),
                )
            };
            Expr::new(
                loc,
                ExprKind::Index {
                    object: Box::new(object),
                    index: Box::new(index),
                },
            )
        }
        other => Expr::new(loc, ExprKind::Var(other.type_name().to_string())),
    }
}

/// 在类型注解 `Expr` 树中替换泛型形参（单态化 AST 用）。
#[must_use]
pub fn substitute_type_annotation(expr: &Expr, subs: &HashMap<String, Value>) -> Expr {
    match &expr.kind {
        ExprKind::Var(name) if subs.contains_key(name) => {
            type_value_to_expr(expr.loc, &subs[name])
        }
        ExprKind::Member { object, field } => Expr::new(
            expr.loc,
            ExprKind::Member {
                object: Box::new(substitute_type_annotation(object, subs)),
                field: field.clone(),
            },
        ),
        ExprKind::Index { object, index } => Expr::new(
            expr.loc,
            ExprKind::Index {
                object: Box::new(substitute_type_annotation(object, subs)),
                index: Box::new(substitute_type_annotation(index, subs)),
            },
        ),
        ExprKind::List(items) => Expr::new(
            expr.loc,
            ExprKind::List(
                items
                    .iter()
                    .map(|e| substitute_type_annotation(e, subs))
                    .collect(),
            ),
        ),
        _ => expr.clone(),
    }
}

/// 替换泛型形参并得到类型值（运行时字段检查用）。
#[must_use]
pub fn substitute_type_expr(expr: &Expr, subs: &HashMap<String, Value>) -> Value {
    let subbed = substitute_type_annotation(expr, subs);
    static_type_value_from_expr(&subbed).unwrap_or_else(|| {
        if let ExprKind::Var(n) = &subbed.kind {
            return subs
                .get(n)
                .cloned()
                .unwrap_or_else(|| Value::type_ref(n.clone()));
        }
        Value::type_ref("object")
    })
}
