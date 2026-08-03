//! `::` / `=>` 注解的运行时类型接受检查。

use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::TypeExpr;
use crate::error::RuntimeError;
use crate::opcode::FunctionObject;
use crate::protocol;
use crate::type_registry;
use crate::value::Value;
use crate::vm::Vm;

/// `name` 是否支持经运行时索引的 `Name[type_arg]`（泛型结构体构造）。
pub fn is_generic_type_formable(vm: &Vm, name: &str) -> bool {
    vm.struct_defs
        .get(name)
        .is_some_and(|def| !def.type_params.is_empty())
}

/// 将运行时类型索引操作数（`num`、`list[num]`、`[num, text]`）转为类型实参。
pub fn type_index_operand_to_args(val: &Value) -> crate::Result<Vec<TypeExpr>> {
    match val {
        Value::TypeRef(name) | Value::Text(name) => Ok(vec![TypeExpr::Name(name.clone())]),
        Value::TypeSpec(spec) => Ok(vec![TypeExpr::Generic {
            name: spec.name.clone(),
            params: spec.args.clone(),
        }]),
        Value::List(lst) => lst
            .borrow()
            .iter()
            .map(single_type_index_operand)
            .collect(),
        other => Err(crate::error::RuntimeError::msg(format!(
            "expected type argument, got {}",
            other.type_name()
        ))),
    }
}

fn single_type_index_operand(val: &Value) -> crate::Result<TypeExpr> {
    let mut args = type_index_operand_to_args(val)?;
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::msg(
            "expected single type argument in this position",
        ));
    }
    Ok(args.remove(0))
}

/// 将运行时 `TypeSpec` 转为 `TypeExpr`。
/// 无参的类型形态（如 `Callable()`、`Maybe()`）仍走 `Generic`，避免被当成普通类型名。
pub fn type_spec_to_type_expr(spec: &crate::value::TypeSpecData) -> TypeExpr {
    if spec.args.is_empty() && !type_registry::is_type_form(&spec.name) {
        TypeExpr::Name(spec.name.clone())
    } else {
        TypeExpr::Generic {
            name: spec.name.clone(),
            params: spec.args.clone(),
        }
    }
}

/// `val` 是否为 `type_name` 的实例或子类型（is-a，非类型句柄相等）。
pub fn instance_is_a(vm: &Vm, val: &Value, type_name: &str) -> bool {
    if type_name == "Never" {
        return false;
    }
    if protocol::is_protocol(&vm.protocols, type_name) {
        return value_satisfies_protocol(vm, val, type_name);
    }
    if let Some(is_prim) = type_registry::check_primitive_instance(vm, val, type_name) {
        return is_prim;
    }
    if let Some(edef) = vm.enum_defs.get(type_name) {
        return matches!(
            val,
            Value::EnumMember(m) if Arc::ptr_eq(&m.def, edef)
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

/// 从 `val` 运行时类型到 `type_name` 的子类型距离；非实例则 `None`。
pub fn instance_match_distance(vm: &Vm, val: &Value, type_name: &str) -> Option<usize> {
    if !instance_is_a(vm, val, type_name) {
        return None;
    }
    match val {
        Value::Struct(s) => type_registry::struct_name_distance(vm, &s.def.name, type_name),
        _ => Some(0),
    }
}

pub fn type_expr_match_distance(vm: &Vm, val: &Value, ty: &TypeExpr) -> Option<usize> {
    match ty {
        TypeExpr::Name(name) => instance_match_distance(vm, val, name),
        TypeExpr::Attr { .. } => {
            let name = resolve_type_expr_name(vm, ty).ok()?;
            instance_match_distance(vm, val, &name)
        }
        TypeExpr::Generic { name, params } => {
            if let Some(score) = type_registry::type_form_match_distance(vm, val, name, params) {
                Some(score)
            } else if type_registry::is_type_form(name) {
                None
            } else {
                instance_match_distance(vm, val, name)
            }
        }
    }
}

/// 分发处理器匹配的总子类型距离分；越小越优先。
pub fn dispatch_match_score(vm: &Vm, func: &FunctionObject, args: &[Value]) -> Option<usize> {
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
        if let Some(ty) = &param.type_expr {
            let val = args.get(i)?;
            score += type_expr_match_distance(vm, val, ty)?;
        }
    }
    Some(score)
}

/// `val` 是否满足类型表达式 `ty`。
pub fn type_accepts(vm: &Vm, val: &Value, ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Name(name) if name == "Never" => false,
        TypeExpr::Name(name) if protocol::is_protocol(&vm.protocols, name) => {
            value_satisfies_protocol(vm, val, name)
        }
        TypeExpr::Name(name) => instance_is_a(vm, val, name),
        TypeExpr::Attr { .. } => match resolve_type_expr_name(vm, ty) {
            Ok(name) => instance_is_a(vm, val, &name),
            Err(_) => false,
        },
        TypeExpr::Generic { name, params } => {
            if type_registry::is_type_form(name) {
                type_registry::type_form_accepts(vm, val, name, params)
            } else {
                type_registry::type_form_accepts(vm, val, name, params)
                    || instance_is_a(vm, val, name)
            }
        }
    }
}

/// 将类型表达式解析为规范类型名（`TypeRef` 字符串或原始名）。
/// `C.types.int` 通过 getattr 得到模块导出的类型句柄。
pub fn resolve_type_expr_name(vm: &Vm, ty: &TypeExpr) -> Result<String, RuntimeError> {
    match resolve_type_expr_value(vm, ty)? {
        Value::TypeRef(n) | Value::Text(n) => Ok(n),
        Value::TypeSpec(spec) => Ok(spec.name.clone()),
        other => Err(RuntimeError::type_err(format!(
            "type expression resolved to {}, expected a type",
            other.type_name()
        ))),
    }
}

pub fn resolve_type_expr_value(vm: &Vm, ty: &TypeExpr) -> Result<Value, RuntimeError> {
    match ty {
        TypeExpr::Name(name) => match vm.load_name(name) {
            Ok(v) => Ok(v),
            // 未绑定名字当作裸类型句柄（如 `num`、`i32`）
            Err(_) => Ok(Value::type_ref(name.clone())),
        },
        TypeExpr::Attr { object, field } => {
            let base = resolve_type_expr_value(vm, object)?;
            match &base {
                Value::Module(m) => m
                    .borrow()
                    .get_attr(field)
                    .ok_or_else(|| {
                        RuntimeError::attr_err(format!(
                            "module '{}' has no export '{field}'",
                            m.borrow().full_name
                        ))
                    }),
                Value::TypeRef(n) => Ok(Value::type_ref(format!("{n}.{field}"))),
                other => Err(RuntimeError::type_err(format!(
                    "cannot get attribute '{field}' on {}",
                    other.type_name()
                ))),
            }
        }
        TypeExpr::Generic { name, params } => Ok(Value::TypeSpec(crate::value::TypeSpecData::new(
            name.clone(),
            params.clone(),
        ))),
    }
}

/// 失败时返回带路径的 `expected X, got Y`；成功返回 `None`。
pub fn type_check_error(vm: &Vm, val: &Value, ty: &TypeExpr) -> Option<String> {
    if type_accepts(vm, val, ty) {
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

fn explain_mismatch(vm: &Vm, val: &Value, ty: &TypeExpr, path: &str) -> String {
    match ty {
        TypeExpr::Generic { name, params } if name == "list" && params.len() == 1 => {
            if let Value::List(lst) = val {
                for (i, item) in lst.borrow().iter().enumerate() {
                    if !type_accepts(vm, item, &params[0]) {
                        let child = format!("{path}[{i}]");
                        return explain_mismatch(vm, item, &params[0], &child);
                    }
                }
            }
            mismatch_message(&type_expr_display(ty), val.type_name(), path)
        }
        TypeExpr::Generic { name, params } if name == "dict" && params.len() == 2 => {
            if let Value::Dict(d) = val {
                for (k, v) in d.borrow().iter() {
                    let kv = crate::value::value_key_to_value(k);
                    let key_disp = kv.print_string();
                    if !type_accepts(vm, &kv, &params[0]) {
                        let child = format!("{path}[{key_disp}]");
                        return explain_mismatch(vm, &kv, &params[0], &child);
                    }
                    if !type_accepts(vm, v, &params[1]) {
                        let child = format!("{path}[{key_disp}]");
                        return explain_mismatch(vm, v, &params[1], &child);
                    }
                }
            }
            mismatch_message(&type_expr_display(ty), val.type_name(), path)
        }
        TypeExpr::Generic { name, params } if name == "set" && params.len() == 1 => {
            if let Value::Set(s) = val {
                for k in s.borrow().iter() {
                    let elem = crate::value::value_key_to_value(k);
                    if !type_accepts(vm, &elem, &params[0]) {
                        let child = if path.is_empty() {
                            format!("{{{}}}", elem.print_string())
                        } else {
                            format!("{path}{{{}}}", elem.print_string())
                        };
                        return explain_mismatch(vm, &elem, &params[0], &child);
                    }
                }
            }
            mismatch_message(&type_expr_display(ty), val.type_name(), path)
        }
        TypeExpr::Generic { name, params } if name == "Maybe" && params.len() == 1 => {
            if matches!(val, Value::None) {
                return mismatch_message(&type_expr_display(ty), val.type_name(), path);
            }
            if !type_accepts(vm, val, &params[0]) {
                return explain_mismatch(vm, val, &params[0], path);
            }
            mismatch_message(&type_expr_display(ty), val.type_name(), path)
        }
        _ => mismatch_message(&type_expr_display(ty), val.type_name(), path),
    }
}

/// 强注解成功后，把容器契约挂到对象上（别名共享）。
pub fn seal_container_contract(vm: &mut Vm, val: &Value, ty: &TypeExpr) {
    match ty {
        TypeExpr::Generic { name, params } if name == "list" && params.len() == 1 => {
            if let Value::List(rc) = val {
                vm.list_element_contracts
                    .insert(rc.as_ptr() as usize, params[0].clone());
            }
        }
        TypeExpr::Generic { name, params } if name == "dict" && params.len() == 2 => {
            if let Value::Dict(rc) = val {
                vm.dict_contracts.insert(
                    rc.as_ptr() as usize,
                    (params[0].clone(), params[1].clone()),
                );
            }
        }
        TypeExpr::Generic { name, params } if name == "set" && params.len() == 1 => {
            if let Value::Set(rc) = val {
                vm.set_element_contracts
                    .insert(rc.as_ptr() as usize, params[0].clone());
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
    protocol::type_satisfies_protocol_ctx(&ctx, &TypeExpr::Name(type_name), protocol_name)
}

pub fn type_expr_display(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Name(n) => n.clone(),
        TypeExpr::Attr { object, field } => format!("{}.{}", type_expr_display(object), field),
        TypeExpr::Generic { name, params } => {
            let inner: Vec<String> = params.iter().map(type_expr_display).collect();
            format!("{name}[{}]", inner.join(", "))
        }
    }
}

/// 值的运行时类型（作类型表达式，供泛型推断）。
pub fn value_to_type_expr(vm: &Vm, val: &Value) -> TypeExpr {
    type_registry::value_to_type_expr(vm, val)
}

pub fn substitute_type_expr(ty: &TypeExpr, subs: &HashMap<String, TypeExpr>) -> TypeExpr {
    match ty {
        TypeExpr::Name(n) => subs.get(n).cloned().unwrap_or_else(|| TypeExpr::Name(n.clone())),
        TypeExpr::Attr { object, field } => TypeExpr::Attr {
            object: Box::new(substitute_type_expr(object, subs)),
            field: field.clone(),
        },
        TypeExpr::Generic { name, params } => TypeExpr::Generic {
            name: name.clone(),
            params: params
                .iter()
                .map(|p| substitute_type_expr(p, subs))
                .collect(),
        },
    }
}

fn unify_type_param(
    inferred: &mut HashMap<String, TypeExpr>,
    param: &str,
    ty: TypeExpr,
) -> bool {
    if let Some(existing) = inferred.get(param) {
        existing == &ty
    } else {
        inferred.insert(param.to_string(), ty);
        true
    }
}

pub fn type_expr_base(ty: &TypeExpr) -> Option<&str> {
    match ty {
        TypeExpr::Name(n) => Some(n),
        TypeExpr::Attr { .. } => None,
        TypeExpr::Generic { name, .. } => Some(name),
    }
}

pub fn infer_from_field_type_inner(
    field_ty: &TypeExpr,
    val_ty: &TypeExpr,
    inferred: &mut HashMap<String, TypeExpr>,
) -> bool {
    match field_ty {
        TypeExpr::Name(p) => unify_type_param(inferred, p, val_ty.clone()),
        TypeExpr::Attr { .. } => true,
        TypeExpr::Generic { .. } => {
            type_registry::type_form_infer(field_ty, val_ty, inferred)
        }
    }
}

fn infer_from_field_type(
    field_ty: &TypeExpr,
    val_ty: &TypeExpr,
    inferred: &mut HashMap<String, TypeExpr>,
) -> bool {
    infer_from_field_type_inner(field_ty, val_ty, inferred)
}

/// 由构造实参与字段类型推断泛型类型实参。
pub fn infer_generic_args(
    vm: &Vm,
    def: &crate::value::StructDef,
    args: &[Value],
) -> HashMap<String, TypeExpr> {
    let mut inferred = HashMap::new();
    for (i, val) in args.iter().enumerate() {
        if let Some(info) = def.field_types.get(i) {
            if let Some(ref field_ty) = info.type_expr {
                let val_ty = value_to_type_expr(vm, val);
                let _ = infer_from_field_type(field_ty, &val_ty, &mut inferred);
            }
        }
    }
    inferred
}

pub fn check_type_param_bounds(
    vm: &Vm,
    type_params: &[(String, Option<TypeExpr>)],
    generic_args: &[TypeExpr],
) -> bool {
    if generic_args.len() != type_params.len() {
        return false;
    }
    for (i, (_, bound)) in type_params.iter().enumerate() {
        let actual = &generic_args[i];
        if let Some(bound_ty) = bound {
            if !type_expr_implies(vm, actual, bound_ty) {
                return false;
            }
        }
    }
    true
}

pub fn type_expr_implies(vm: &Vm, actual: &TypeExpr, bound: &TypeExpr) -> bool {
    if let Some(result) = type_registry::type_form_implies(vm, actual, bound) {
        return result;
    }
    type_expr_implies_inner(vm, actual, bound)
}

pub fn type_expr_implies_inner(vm: &Vm, actual: &TypeExpr, bound: &TypeExpr) -> bool {
    match (actual, bound) {
        (TypeExpr::Name(a), TypeExpr::Name(b)) => {
            a == b
                || type_satisfies_bound_name(vm, actual, b)
                || type_registry::struct_name_is_a(vm, a, b)
        }
        (
            TypeExpr::Generic {
                name: a,
                params: ap,
            },
            TypeExpr::Generic {
                name: b,
                params: bp,
            },
        ) => {
            (a == b || type_registry::struct_name_is_a(vm, a, b))
                && ap.len() == bp.len()
                && ap
                    .iter()
                    .zip(bp.iter())
                    .all(|(x, y)| type_expr_implies_inner(vm, x, y))
        }
        (TypeExpr::Name(a), TypeExpr::Generic { name: b, .. }) => {
            a == b || type_registry::struct_name_is_a(vm, a, b)
        }
        (TypeExpr::Generic { name: a, .. }, TypeExpr::Name(b)) => {
            a == b
                || type_satisfies_bound_name(vm, actual, b)
                || type_registry::struct_name_is_a(vm, a, b)
        }
        (TypeExpr::Attr { .. }, _) | (_, TypeExpr::Attr { .. }) => {
            let Ok(a) = resolve_type_expr_name(vm, actual) else {
                return false;
            };
            let Ok(b) = resolve_type_expr_name(vm, bound) else {
                return false;
            };
            a == b || type_registry::struct_name_is_a(vm, &a, &b)
        }
    }
}

fn type_satisfies_bound_name(vm: &Vm, actual: &TypeExpr, bound_name: &str) -> bool {
    if !protocol::is_protocol(&vm.protocols, bound_name) {
        return false;
    }
    let ctx = protocol_ctx_from_vm(vm);
    protocol::type_satisfies_protocol_ctx(&ctx, actual, bound_name)
}

fn protocol_ctx_from_vm(vm: &Vm) -> protocol::TypeCheckContext {
    protocol::TypeCheckContext {
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
