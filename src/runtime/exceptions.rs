//! 内建异常类型。


use crate::error::ExceptionKind;
use crate::value::{FieldTypeInfo, StructDef, Value};
use crate::vm::Vm;
use std::sync::Arc;

fn exc_def(name: &str, base: Option<&str>) -> Arc<StructDef> {
    Arc::new(StructDef {
        name: name.to_string(),
        base: base.map(str::to_string),
        fields: vec!["message".to_string(), "traceback".to_string()],
        mutable_fields: vec![true, true],
        typed: false,
        field_types: vec![FieldTypeInfo::default(), FieldTypeInfo::default()],
        type_params: Vec::new(),
        c_layout: None,
    })
}

/// 登记内建异常结构体类型与全局构造器。
pub fn install(vm: &mut Vm) {
    for kind in ExceptionKind::ALL {
        let name = kind.type_name();
        let base = kind.parent().map(|p| p.type_name().to_string());
        vm.struct_defs
            .entry(name.to_string())
            .or_insert_with(|| exc_def(name, base.as_deref()));
        vm.globals
            .or_insert_with(name.to_string(), || Value::type_ref(name));
    }
}

pub fn is_exception(vm: &Vm, val: &Value) -> bool {
    struct_is_a(vm, val, "BaseException")
}

pub fn struct_is_a(vm: &Vm, val: &Value, type_name: &str) -> bool {
    let Value::Struct(s) = val else {
        return false;
    };
    let mut current = Some(s.def.name.as_str());
    while let Some(name) = current {
        if name == type_name {
            return true;
        }
        current = vm
            .struct_defs
            .get(name)
            .and_then(|d| d.base.as_deref());
    }
    false
}

/// 返回直接基类类型名（若有）。
pub fn direct_base(vm: &Vm, type_name: &str) -> Option<String> {
    vm.struct_defs
        .get(type_name)
        .and_then(|d| d.base.clone())
}

/// 从 `type_name` 到根的继承链（含自身）。
pub fn inheritance_chain(vm: &Vm, type_name: &str) -> Vec<String> {
    let mut chain = vec![type_name.to_string()];
    let mut seen = std::collections::HashSet::new();
    seen.insert(type_name.to_string());
    let mut current = type_name;
    while let Some(base) = direct_base(vm, current) {
        if !seen.insert(base.clone()) {
            break;
        }
        chain.push(base.clone());
        current = chain
            .last()
            .expect("exception chain non-empty (theoretically unreachable)");
    }
    chain
}

pub fn kind_of_value(val: &Value) -> Option<ExceptionKind> {
    let Value::Struct(s) = val else {
        return None;
    };
    ExceptionKind::from_type_name(&s.def.name)
}

/// 构造异常实例（message 字段；traceback 在 throw 时填入）。
pub fn make_exception(vm: &Vm, type_name: &str, message: impl Into<String>) -> crate::Result<Value> {
    let def = vm
        .struct_defs
        .get(type_name)
        .cloned()
        .ok_or_else(|| crate::error::RuntimeError::msg(format!("unknown exception: {type_name}")))?;
    Ok(Value::Struct(Arc::new(crate::value::StructInstance {
        def,
        slots: crate::shared::SyncCell::new(vec![
            Value::Text(message.into()),
            Value::None,
        ]),
        generic_args: Vec::new(),
    })))
}

pub fn make_exception_kind(vm: &Vm, kind: ExceptionKind, message: impl Into<String>) -> crate::Result<Value> {
    make_exception(vm, kind.type_name(), message)
}

pub fn exception_message(exc: &Value) -> Option<String> {
    let Value::Struct(s) = exc else {
        return None;
    };
    match s.slots.borrow().first() {
        Some(Value::Text(msg)) => Some(msg.clone()),
        _ => None,
    }
}

/// 未捕获异常的展示：`TypeName: message`（与 Python 一致）。
pub fn format_uncaught(exc: &Value) -> String {
    let Value::Struct(s) = exc else {
        return exc.display_string();
    };
    let name = s.def.name.as_str();
    match exception_message(exc) {
        Some(msg) if !msg.is_empty() => format!("{name}: {msg}"),
        _ => name.to_string(),
    }
}

/// 供 `std.exceptions.tree` 等使用的继承表。
pub fn exception_hierarchy() -> Vec<(ExceptionKind, Option<ExceptionKind>)> {
    ExceptionKind::ALL
        .iter()
        .copied()
        .map(|k| (k, k.parent()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RuntimeError;

    #[test]
    fn typed_errors_carry_kind() {
        assert_eq!(
            RuntimeError::zero_div("division by zero").kind(),
            ExceptionKind::ZeroDivision
        );
        assert_eq!(
            RuntimeError::name_err("undefined name: foo").kind(),
            ExceptionKind::NameError
        );
        assert_eq!(
            RuntimeError::index_err("index out of range").kind(),
            ExceptionKind::IndexError
        );
        assert_eq!(
            RuntimeError::unsupported("unsupported + between num and text").kind(),
            ExceptionKind::UnsupportedOp
        );
        assert_eq!(
            RuntimeError::unsupported("x").kind().type_name(),
            "TypeError"
        );
    }

    #[test]
    fn hierarchy_covers_all_kinds() {
        let h = exception_hierarchy();
        assert!(h.iter().any(|(k, _)| *k == ExceptionKind::ZeroDivision));
        assert_eq!(
            ExceptionKind::ZeroDivision.parent(),
            Some(ExceptionKind::ArithmeticError)
        );
        assert_eq!(
            ExceptionKind::DeadlockError.parent(),
            Some(ExceptionKind::Runtime)
        );
        assert_eq!(ExceptionKind::DeadlockError.type_name(), "DeadlockError");
        assert_eq!(
            RuntimeError::deadlock("channel recv blocked").kind(),
            ExceptionKind::DeadlockError
        );
    }
}
