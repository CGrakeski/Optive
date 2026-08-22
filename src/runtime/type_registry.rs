//! 预装核心类型：实例检查、类型形态、方法与构造器。
//!
//! 语言内建在启动时登记于此，按普通类型元数据处理 — 别处不再散落类型名字面量。

use std::collections::HashMap;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::error::RuntimeError;
use crate::value::{BuiltinFn, DictMap, IteratorState, Num, TypeSpecData, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use crate::shared::Shared;
/// 核心原始类型名（单一来源）。
pub mod names {
    pub const NONE: &str = "nonetype";
    pub const BOOL: &str = "bool";
    pub const NUM: &str = "num";
    pub const TEXT: &str = "text";
    pub const LIST: &str = "list";
    pub const DICT: &str = "dict";
    pub const SET: &str = "set";
    pub const TUPLE: &str = "tuple";
    pub const BYTES: &str = "bytes";
    pub const ITERATOR: &str = "iterator";
    pub const TYPE: &str = "type";

    pub const ALL_PRIMITIVES: &[&str] = &[
        NUM, TEXT, BOOL, NONE, LIST, DICT, SET, TUPLE, BYTES, ITERATOR,
        // 定宽 / 指针：与 `SizedNum::ALL_NAMES` + ptr 对齐（见下方 assert）
        "i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64", "isize", "usize", "f32", "f64", "ptr",
    ];
}

/// 运行时值标签 → 原始类型名（供方法 / magic 查找）。
#[must_use]
pub const fn value_primitive_type(val: &Value) -> Option<&'static str> {
    match val {
        Value::None => Some(names::NONE),
        Value::Bool(_) => Some(names::BOOL),
        Value::Num(_) => Some(names::NUM),
        Value::Sized(s) => Some(match s {
            crate::sized::SizedNum::I8(_) => "i8",
            crate::sized::SizedNum::U8(_) => "u8",
            crate::sized::SizedNum::I16(_) => "i16",
            crate::sized::SizedNum::U16(_) => "u16",
            crate::sized::SizedNum::I32(_) => "i32",
            crate::sized::SizedNum::U32(_) => "u32",
            crate::sized::SizedNum::I64(_) => "i64",
            crate::sized::SizedNum::U64(_) => "u64",
            crate::sized::SizedNum::Isize(_) => "isize",
            crate::sized::SizedNum::Usize(_) => "usize",
            crate::sized::SizedNum::F32(_) => "f32",
            crate::sized::SizedNum::F64(_) => "f64",
        }),
        Value::Ptr(_) => Some("ptr"),
        Value::Text(_) => Some(names::TEXT),
        Value::List(_) => Some(names::LIST),
        Value::Dict(_) => Some(names::DICT),
        Value::Set(_) => Some(names::SET),
        Value::Tuple(_) => Some(names::TUPLE),
        Value::Bytes(_) => Some(names::BYTES),
        Value::Iterator(_) => Some(names::ITERATOR),
        _ => None,
    }
}

#[must_use]
pub fn is_registered_primitive(name: &str) -> bool {
    names::ALL_PRIMITIVES.contains(&name)
}

#[must_use]
pub fn is_builtin_base(name: &str) -> bool {
    is_registered_primitive(name)
}

pub fn supports_convert(vm: &Vm, type_name: &str) -> bool {
    is_registered_primitive(type_name)
        || crate::c_types::lookup_c_type(type_name).is_some()
        || vm.struct_defs.contains_key(type_name)
        || vm.variant_defs.contains_key(type_name)
}

/// `name` 是否为已登记原始类型且 `val` 是其实例。
pub fn check_primitive_instance(vm: &Vm, val: &Value, type_name: &str) -> Option<bool> {
    Some(match type_name {
        n if n == names::NUM => matches!(val, Value::Num(_)),
        n if n == names::TEXT => matches!(val, Value::Text(_)) || struct_inherits_text(vm, val),
        n if n == names::BOOL => matches!(val, Value::Bool(_)),
        n if n == names::NONE => matches!(val, Value::None),
        n if n == names::LIST => matches!(val, Value::List(_)),
        n if n == names::DICT => matches!(val, Value::Dict(_)),
        n if n == names::SET => matches!(val, Value::Set(_)),
        n if n == names::TUPLE => matches!(val, Value::Tuple(_)),
        n if n == names::BYTES => matches!(val, Value::Bytes(_)),
        n if n == names::ITERATOR => matches!(val, Value::Iterator(_)),
        n if n == names::TYPE => matches!(val, Value::TypeRef(_) | Value::TypeSpec(_)),
        "function" => matches!(
            val,
            Value::Function(_) | Value::Builtin(_) | Value::GenericFunction(_)
        ),
        "ptr" => matches!(val, Value::Ptr(_)),
        "Layout" => matches!(val, Value::Layout(_)),
        n if crate::sized::SizedNum::ALL_NAMES.contains(&n) => {
            matches!(val, Value::Sized(s) if s.type_name() == n)
        }
        n if crate::c_types::lookup_c_type(n).is_some() => crate::ffi::AbiType::from_type_name(n)
            .ok()
            .is_some_and(|abi| value_matches_abi(val, &abi)),
        _ => return None,
    })
}

fn value_matches_abi(val: &Value, abi: &crate::ffi::AbiType) -> bool {
    use crate::ffi::AbiType::{
        Bool, CStruct, CharPtr, Isize, Pointer, Usize, Void, WCharPtr, F32, F64, I16, I32, I64, I8,
        U16, U32, U64, U8,
    };
    match abi {
        Void => matches!(val, Value::None),
        Bool => matches!(val, Value::Bool(_)),
        I8 => matches!(val, Value::Sized(crate::sized::SizedNum::I8(_))),
        U8 => matches!(val, Value::Sized(crate::sized::SizedNum::U8(_))),
        I16 => matches!(val, Value::Sized(crate::sized::SizedNum::I16(_))),
        U16 => matches!(val, Value::Sized(crate::sized::SizedNum::U16(_))),
        I32 => matches!(val, Value::Sized(crate::sized::SizedNum::I32(_))),
        U32 => matches!(val, Value::Sized(crate::sized::SizedNum::U32(_))),
        I64 => matches!(val, Value::Sized(crate::sized::SizedNum::I64(_))),
        U64 => matches!(val, Value::Sized(crate::sized::SizedNum::U64(_))),
        Isize => matches!(val, Value::Sized(crate::sized::SizedNum::Isize(_))),
        Usize => matches!(val, Value::Sized(crate::sized::SizedNum::Usize(_))),
        F32 => matches!(val, Value::Sized(crate::sized::SizedNum::F32(_))),
        F64 => matches!(val, Value::Sized(crate::sized::SizedNum::F64(_))),
        Pointer | CharPtr | WCharPtr => {
            matches!(val, Value::Ptr(_) | Value::Text(_) | Value::None)
        }
        CStruct { name, .. } => matches!(val, Value::Struct(s) if s.def.name == *name),
    }
}

fn struct_inherits_text(vm: &Vm, val: &Value) -> bool {
    let Value::Struct(s) = val else {
        return false;
    };
    struct_name_is_a(vm, &s.def.name, names::TEXT)
}

pub fn struct_name_is_a(vm: &Vm, actual: &str, expected: &str) -> bool {
    if actual == expected {
        return true;
    }
    let Some(def) = vm.struct_defs.get(actual) else {
        return false;
    };
    let Some(base) = def.base.as_deref() else {
        return false;
    };
    if is_builtin_base(base) {
        base == expected
    } else {
        struct_name_is_a(vm, base, expected)
    }
}

pub fn struct_name_distance(vm: &Vm, actual: &str, expected: &str) -> Option<usize> {
    if actual == expected {
        return Some(0);
    }
    let def = vm.struct_defs.get(actual)?;
    let base = def.base.as_deref()?;
    if is_builtin_base(base) {
        if base == expected {
            Some(1)
        } else {
            None
        }
    } else {
        struct_name_distance(vm, base, expected).map(|d| d + 1)
    }
}

#[must_use]
pub fn protocol_has_method(type_name: &str, method: &str) -> bool {
    PROTOCOL_METHODS
        .iter()
        .any(|(t, m)| *t == type_name && *m == method)
}

const PROTOCOL_METHODS: &[(&str, &str)] = &[
    ("num", "__add__"),
    ("num", "__sub__"),
    ("num", "__mul__"),
    ("num", "__div__"),
    ("num", "__mod__"),
    ("num", "__pow__"),
    ("num", "__and__"),
    ("num", "__or__"),
    ("num", "__xor__"),
    ("num", "__lshift__"),
    ("num", "__rshift__"),
    ("num", "__radd__"),
    ("num", "__rsub__"),
    ("num", "__rmul__"),
    ("num", "__rdiv__"),
    ("num", "__rmod__"),
    ("num", "__rpow__"),
    ("num", "__rand__"),
    ("num", "__ror__"),
    ("num", "__rxor__"),
    ("num", "__rlshift__"),
    ("num", "__rrshift__"),
    ("num", "__neg__"),
    ("num", "__invert__"),
    ("text", "__add__"),
    ("text", "__mul__"),
    ("text", "__rmul__"),
];

#[must_use]
pub fn sample_value_for_type_name(name: &str) -> Value {
    match name {
        "num" => Value::Num(Num::Small(0)),
        "text" => Value::Text(String::new()),
        "bool" => Value::Bool(false),
        "nonetype" => Value::None,
        "list" => Value::List(Shared::new(Vec::new())),
        "dict" => Value::Dict(Shared::new(DictMap::new())),
        "set" => Value::Set(Shared::new(crate::value::SetMap::new())),
        "tuple" => Value::Tuple(Arc::from([])),
        "bytes" => Value::Bytes(Arc::new(Vec::new())),
        "iterator" => Value::Iterator(Shared::new(crate::value::IteratorState::from_list(
            Vec::new(),
        ))),
        other => Value::type_ref(other),
    }
}

/// 值的运行时类型（作类型表达式，供泛型推断）。
pub fn value_to_type_value(_vm: &Vm, val: &Value) -> Value {
    if let Some(ty) = primitive_value_to_type_value(val) {
        return ty;
    }
    match val {
        Value::Struct(s) => {
            if s.generic_args.is_empty() {
                Value::TypeRef(s.def.name.clone())
            } else {
                Value::TypeSpec(TypeSpecData::new(
                    s.def.name.clone(),
                    s.generic_args.clone(),
                ))
            }
        }
        Value::TypeSpec(_) | Value::TypeRef(_) => Value::TypeRef("type".to_string()),
        _ => Value::TypeRef(val.type_name().to_string()),
    }
}

/// 将运行时类型操作数转为 类型值（供 `std.typing` 构造）。
#[must_use]
pub fn value_to_type_value_operand(val: &Value) -> Value {
    match val {
        Value::TypeRef(s) | Value::Text(s) => Value::TypeRef(s.clone()),
        Value::TypeSpec(spec) => {
            Value::TypeSpec(TypeSpecData::new(spec.name.clone(), spec.args.clone()))
        }
        other => Value::TypeRef(other.type_name().to_string()),
    }
}

/// `Literal(...)` 实参：把 num/bool/text 编码进 type value 名字，供运行时相等比较。
pub fn literal_operand_to_type_value(val: &Value) -> crate::Result<Value> {
    match val {
        Value::Text(s) => Ok(Value::TypeRef(format!("__lit_text:{s}"))),
        Value::Bool(b) => Ok(Value::TypeRef(format!("__lit_bool:{b}"))),
        Value::Num(n) => Ok(Value::TypeRef(format!("__lit_num:{n}"))),
        Value::None => Ok(Value::TypeRef("__lit_none".to_string())),
        other => Err(RuntimeError::msg(format!(
            "Literal does not support {}",
            other.type_name()
        ))),
    }
}

fn literal_accepts(_vm: &Vm, val: &Value, params: &[Value]) -> bool {
    params.iter().any(|p| {
        let Value::TypeRef(enc) = p else {
            return false;
        };
        if enc == "__lit_none" {
            return matches!(val, Value::None);
        }
        if let Some(rest) = enc.strip_prefix("__lit_text:") {
            return matches!(val, Value::Text(t) if t == rest);
        }
        if let Some(rest) = enc.strip_prefix("__lit_bool:") {
            return matches!(
                (rest, val),
                ("true", Value::Bool(true)) | ("false", Value::Bool(false))
            );
        }
        if let Some(rest) = enc.strip_prefix("__lit_num:") {
            return matches!(val, Value::Num(n) if n.to_string() == rest);
        }
        false
    })
}

fn literal_match_distance(vm: &Vm, val: &Value, params: &[Value]) -> Option<usize> {
    if literal_accepts(vm, val, params) {
        Some(0)
    } else {
        None
    }
}

fn dict_accepts(vm: &Vm, val: &Value, params: &[Value]) -> bool {
    let [kty, vty] = params else {
        return false;
    };
    let Value::Dict(d) = val else {
        return false;
    };
    d.borrow().iter().all(|(k, v)| {
        let kv = crate::value::value_key_to_value(k);
        crate::types::value_accepts(vm, &kv, kty) && crate::types::value_accepts(vm, v, vty)
    })
}

fn dict_match_distance(vm: &Vm, val: &Value, params: &[Value]) -> Option<usize> {
    let [kty, vty] = params else {
        return None;
    };
    let Value::Dict(d) = val else {
        return None;
    };
    let mut score = 0usize;
    for (k, v) in d.borrow().iter() {
        let kv = crate::value::value_key_to_value(k);
        score += crate::types::type_value_match_distance(vm, &kv, kty)?;
        score += crate::types::type_value_match_distance(vm, v, vty)?;
    }
    Some(score)
}

fn set_accepts(vm: &Vm, val: &Value, params: &[Value]) -> bool {
    let [elem] = params else {
        return false;
    };
    let Value::Set(s) = val else {
        return false;
    };
    s.borrow()
        .iter()
        .all(|k| crate::types::value_accepts(vm, &crate::value::value_key_to_value(k), elem))
}

fn set_match_distance(vm: &Vm, val: &Value, params: &[Value]) -> Option<usize> {
    let [elem] = params else {
        return None;
    };
    let Value::Set(s) = val else {
        return None;
    };
    let mut score = 0usize;
    for k in s.borrow().iter() {
        score += crate::types::type_value_match_distance(
            vm,
            &crate::value::value_key_to_value(k),
            elem,
        )?;
    }
    Some(score)
}

#[must_use]
pub fn primitive_value_to_type_value(val: &Value) -> Option<Value> {
    match val {
        Value::None => Some(Value::TypeRef("nonetype".to_string())),
        Value::Bool(_) => Some(Value::TypeRef("bool".to_string())),
        Value::Num(_) => Some(Value::TypeRef("num".to_string())),
        Value::Text(_) => Some(Value::TypeRef("text".to_string())),
        Value::List(lst) => {
            let elems: Vec<Value> = lst
                .borrow()
                .iter()
                .filter_map(|e| {
                    primitive_value_to_type_value(e)
                        .or_else(|| Some(Value::TypeRef(e.type_name().to_string())))
                })
                .collect();
            if elems.is_empty() {
                Some(Value::TypeSpec(TypeSpecData::new(
                    "list",
                    vec![Value::TypeRef("num".to_string())],
                )))
            } else if elems
                .windows(2)
                .all(|w| crate::types::type_values_equal(&w[0], &w[1]))
            {
                Some(Value::TypeSpec(TypeSpecData::new(
                    "list",
                    vec![elems[0].clone()],
                )))
            } else {
                Some(Value::TypeSpec(TypeSpecData::new(
                    "list",
                    vec![Value::TypeSpec(TypeSpecData::new("Union", elems))],
                )))
            }
        }
        Value::Dict(_) => Some(Value::TypeRef("dict".to_string())),
        Value::Set(_) => Some(Value::TypeRef("set".to_string())),
        Value::Tuple(t) => {
            if t.is_empty() {
                Some(Value::TypeRef("tuple".to_string()))
            } else {
                let params: Vec<Value> = t
                    .iter()
                    .map(|e| {
                        primitive_value_to_type_value(e)
                            .unwrap_or_else(|| Value::TypeRef(e.type_name().to_string()))
                    })
                    .collect();
                Some(Value::TypeSpec(TypeSpecData::new("tuple", params)))
            }
        }
        Value::Bytes(_) => Some(Value::TypeRef("bytes".to_string())),
        Value::Iterator(_) => Some(Value::TypeRef("iterator".to_string())),
        _ => None,
    }
}

type TypeFormMatch = fn(&Vm, &Value, &[Value]) -> Option<usize>;
type TypeFormAccepts = fn(&Vm, &Value, &[Value]) -> bool;
type TypeFormInfer = fn(&Value, &Value, &mut HashMap<String, Value>) -> bool;
type TypeFormImplies = fn(&Vm, &Value, &Value) -> bool;

struct TypeFormEntry {
    match_distance: TypeFormMatch,
    accepts: TypeFormAccepts,
    infer: Option<TypeFormInfer>,
    implies: Option<TypeFormImplies>,
}

fn list_match_distance(vm: &Vm, val: &Value, params: &[Value]) -> Option<usize> {
    let [elem] = params else {
        return None;
    };
    let Value::List(lst) = val else {
        return None;
    };
    let mut score = 0usize;
    for item in lst.borrow().iter() {
        score += crate::types::type_value_match_distance(vm, item, elem)?;
    }
    Some(score)
}

fn list_accepts(vm: &Vm, val: &Value, params: &[Value]) -> bool {
    let [elem] = params else {
        return false;
    };
    let Value::List(lst) = val else {
        return false;
    };
    lst.borrow()
        .iter()
        .all(|item| crate::types::value_accepts(vm, item, elem))
}

fn list_infer(field_ty: &Value, val_ty: &Value, inferred: &mut HashMap<String, Value>) -> bool {
    let Value::TypeSpec(field) = field_ty else {
        return false;
    };
    if field.name != "list" || field.args.len() != 1 {
        return false;
    }
    let Value::TypeSpec(val) = val_ty else {
        return false;
    };
    if val.name != "list" || val.args.len() != 1 {
        return false;
    }
    crate::types::infer_from_field_type_inner(&field.args[0], &val.args[0], inferred)
}

fn union_match_distance(vm: &Vm, val: &Value, params: &[Value]) -> Option<usize> {
    params
        .iter()
        .filter_map(|p| crate::types::type_value_match_distance(vm, val, p))
        .min()
}

fn union_accepts(vm: &Vm, val: &Value, params: &[Value]) -> bool {
    params
        .iter()
        .any(|p| crate::types::value_accepts(vm, val, p))
}

fn maybe_match_distance(vm: &Vm, val: &Value, params: &[Value]) -> Option<usize> {
    let [inner] = params else {
        return None;
    };
    if matches!(val, Value::None) {
        Some(0)
    } else {
        crate::types::type_value_match_distance(vm, val, inner)
    }
}

fn maybe_accepts(vm: &Vm, val: &Value, params: &[Value]) -> bool {
    let [inner] = params else {
        return false;
    };
    matches!(val, Value::None) || crate::types::value_accepts(vm, val, inner)
}

/// `Tuple[T1, T2, ...]` —— 定长异构元组：长度与逐元素类型须匹配。
fn tuple_match_distance(vm: &Vm, val: &Value, params: &[Value]) -> Option<usize> {
    let Value::Tuple(t) = val else { return None };
    if t.len() != params.len() {
        return None;
    }
    let mut sum = 0usize;
    for (v, p) in t.iter().zip(params.iter()) {
        sum += crate::types::type_value_match_distance(vm, v, p)?;
    }
    Some(sum)
}

fn tuple_accepts(vm: &Vm, val: &Value, params: &[Value]) -> bool {
    let Value::Tuple(t) = val else { return false };
    if t.len() != params.len() {
        return false;
    }
    t.iter()
        .zip(params.iter())
        .all(|(v, p)| crate::types::value_accepts(vm, v, p))
}

/// `Callable` —— 任意可调用值（函数/内建/泛型函数）。带参时仅校验可调用性。
fn callable_match_distance(_vm: &Vm, val: &Value, _params: &[Value]) -> Option<usize> {
    is_callable_value(val).then_some(0)
}

const fn callable_accepts(_vm: &Vm, val: &Value, _params: &[Value]) -> bool {
    is_callable_value(val)
}

const fn is_callable_value(val: &Value) -> bool {
    matches!(
        val,
        Value::Function(_) | Value::GenericFunction(_) | Value::Builtin(_)
    )
}

fn variance_implies(vm: &Vm, actual: &Value, bound: &Value, variance: &str) -> bool {
    let Value::TypeSpec(bound_spec) = bound else {
        return false;
    };
    if bound_spec.args.len() != 1 {
        return false;
    }
    match (variance, bound_spec.name.as_str()) {
        ("Covariant", "Covariant") => {
            crate::types::type_value_implies_inner(vm, actual, &bound_spec.args[0])
        }
        ("Contravariant", "Contravariant") => {
            crate::types::type_value_implies_inner(vm, &bound_spec.args[0], actual)
        }
        ("Invariant", "Invariant") => crate::types::type_values_equal(actual, &bound_spec.args[0]),
        _ => false,
    }
}

const fn ptr_form_match_distance(_vm: &Vm, val: &Value, params: &[Value]) -> Option<usize> {
    if params.len() != 1 {
        return None;
    }
    match val {
        Value::Ptr(_) | Value::None => Some(0),
        _ => None,
    }
}

const fn ptr_form_accepts(_vm: &Vm, val: &Value, params: &[Value]) -> bool {
    if params.len() != 1 {
        return false;
    }
    matches!(val, Value::Ptr(_) | Value::None)
}

fn lookup_type_form(name: &str) -> Option<&'static TypeFormEntry> {
    let name = if crate::ptr_registry::is_ptr_type_name(name) {
        "ptr"
    } else {
        name
    };
    static FORMS: &[(&str, TypeFormEntry)] = &[
        (
            "ptr",
            TypeFormEntry {
                match_distance: ptr_form_match_distance,
                accepts: ptr_form_accepts,
                infer: None,
                implies: None,
            },
        ),
        (
            "list",
            TypeFormEntry {
                match_distance: list_match_distance,
                accepts: list_accepts,
                infer: Some(list_infer),
                implies: None,
            },
        ),
        (
            "dict",
            TypeFormEntry {
                match_distance: dict_match_distance,
                accepts: dict_accepts,
                infer: None,
                implies: None,
            },
        ),
        (
            "set",
            TypeFormEntry {
                match_distance: set_match_distance,
                accepts: set_accepts,
                infer: None,
                implies: None,
            },
        ),
        (
            "Union",
            TypeFormEntry {
                match_distance: union_match_distance,
                accepts: union_accepts,
                infer: None,
                implies: None,
            },
        ),
        (
            "Maybe",
            TypeFormEntry {
                match_distance: maybe_match_distance,
                accepts: maybe_accepts,
                infer: None,
                implies: None,
            },
        ),
        (
            "Tuple",
            TypeFormEntry {
                match_distance: tuple_match_distance,
                accepts: tuple_accepts,
                infer: None,
                implies: None,
            },
        ),
        (
            "Callable",
            TypeFormEntry {
                match_distance: callable_match_distance,
                accepts: callable_accepts,
                infer: None,
                implies: None,
            },
        ),
        (
            "Literal",
            TypeFormEntry {
                match_distance: literal_match_distance,
                accepts: literal_accepts,
                infer: None,
                implies: None,
            },
        ),
        (
            "Covariant",
            TypeFormEntry {
                match_distance: |vm, val, params| {
                    let [inner] = params else {
                        return None;
                    };
                    crate::types::type_value_match_distance(vm, val, inner)
                },
                accepts: |vm, val, params| {
                    let [inner] = params else {
                        return false;
                    };
                    crate::types::value_accepts(vm, val, inner)
                },
                infer: None,
                implies: Some(|vm, actual, bound| variance_implies(vm, actual, bound, "Covariant")),
            },
        ),
        (
            "Contravariant",
            TypeFormEntry {
                match_distance: |vm, val, params| {
                    let [inner] = params else {
                        return None;
                    };
                    crate::types::type_value_match_distance(vm, val, inner)
                },
                accepts: |vm, val, params| {
                    let [inner] = params else {
                        return false;
                    };
                    crate::types::value_accepts(vm, val, inner)
                },
                infer: None,
                implies: Some(|vm, actual, bound| {
                    variance_implies(vm, actual, bound, "Contravariant")
                }),
            },
        ),
        (
            "Invariant",
            TypeFormEntry {
                match_distance: |vm, val, params| {
                    let [inner] = params else {
                        return None;
                    };
                    crate::types::type_value_match_distance(vm, val, inner)
                },
                accepts: |vm, val, params| {
                    let [inner] = params else {
                        return false;
                    };
                    crate::types::value_accepts(vm, val, inner)
                },
                infer: None,
                implies: Some(|vm, actual, bound| variance_implies(vm, actual, bound, "Invariant")),
            },
        ),
    ];
    FORMS.iter().find(|(n, _)| *n == name).map(|(_, e)| e)
}

#[must_use]
pub fn is_type_form(name: &str) -> bool {
    lookup_type_form(name).is_some()
}

pub fn type_form_match_distance(
    vm: &Vm,
    val: &Value,
    name: &str,
    params: &[Value],
) -> Option<usize> {
    lookup_type_form(name).and_then(|f| (f.match_distance)(vm, val, params))
}

pub fn type_form_accepts(vm: &Vm, val: &Value, name: &str, params: &[Value]) -> bool {
    lookup_type_form(name).is_some_and(|f| (f.accepts)(vm, val, params))
}

pub fn type_form_infer(
    field_ty: &Value,
    val_ty: &Value,
    inferred: &mut HashMap<String, Value>,
) -> bool {
    let Value::TypeSpec(field) = field_ty else {
        return false;
    };
    if let Some(form) = lookup_type_form(&field.name) {
        if let Some(infer) = form.infer {
            return infer(field_ty, val_ty, inferred);
        }
    }
    let other = field.name.as_str();
    let Some(val_name) = crate::types::type_value_base(val_ty) else {
        return false;
    };
    if val_name != other {
        return false;
    }
    if field.args.is_empty() {
        return true;
    }
    let Value::TypeSpec(val) = val_ty else {
        return false;
    };
    val.name == other
        && field.args.len() == val.args.len()
        && field
            .args
            .iter()
            .zip(val.args.iter())
            .all(|(a, b)| crate::types::infer_from_field_type_inner(a, b, inferred))
}

pub fn type_form_implies(vm: &Vm, actual: &Value, bound: &Value) -> Option<bool> {
    let Value::TypeSpec(bound_spec) = bound else {
        return None;
    };
    let form = lookup_type_form(&bound_spec.name)?;
    form.implies.map(|f| f(vm, actual, bound))
}

#[must_use]
pub fn type_ctor_error(type_name: &str, src: &Value) -> RuntimeError {
    RuntimeError::type_err(format!(
        "TypeError: cannot construct {type_name} from {}",
        src.type_name()
    ))
}

#[must_use]
pub fn type_convert_error(type_name: &str, src: &Value) -> RuntimeError {
    RuntimeError::type_err(format!(
        "TypeError: cannot convert {} to {type_name}",
        src.type_name()
    ))
}

#[must_use]
pub fn type_ctor_arity_error(type_name: &str, expected: &str, got: usize) -> RuntimeError {
    RuntimeError::type_err(format!("TypeError: {type_name}() {expected}, got {got}"))
}

/// 构造原始类型值：`type(args)` — 非类型转换。
pub fn call_primitive_ctor(
    _vm: &mut Vm,
    type_name: &str,
    args: Vec<Value>,
) -> Option<Result<Value>> {
    match type_name {
        "text" if args.len() == 1 => Some(Ok(Value::Text(args[0].print_string()))),
        "num" if args.len() == 1 => Some(coerce_to_num(&args[0], type_ctor_error)),
        "bool" if args.len() == 1 => Some(Ok(Value::Bool(args[0].is_truthy()))),
        "list" if args.is_empty() => Some(Ok(Value::List(Shared::new(Vec::new())))),
        "list" if args.len() == 1 => Some(construct_list(&args[0])),
        "dict" if args.len().is_multiple_of(2) => Some(construct_dict_kv(args)),
        "dict" => Some(Err(type_ctor_arity_error(
            "dict",
            "requires an even number of alternating key, value arguments",
            args.len(),
        ))),
        "set" if args.is_empty() => Some(Ok(Value::Set(Shared::new(crate::value::SetMap::new())))),
        "set" => Some(construct_set(args)),
        "tuple" => Some(Ok(Value::Tuple(args.into()))),
        "bytes" if args.is_empty() => Some(Ok(Value::Bytes(Arc::new(Vec::new())))),
        "bytes" if args.len() == 1 => Some(construct_bytes(&args[0])),
        "iterator" if args.len() == 1 => Some(construct_iterator(&args[0])),
        "iterator" if args.is_empty() => Some(Err(type_ctor_arity_error(
            "iterator",
            "expects 1 argument",
            0,
        ))),
        "Channel" | "channel" => Some(crate::concurrency::construct_channel(&args)),
        "Stream" => Some(crate::concurrency::construct_stream(&args)),
        "Mutex" | "mutex" => Some(crate::concurrency::construct_mutex(&args)),
        "RWMutex" | "RwLock" => Some(crate::concurrency::construct_rwmutex(&args)),
        "WaitGroup" => Some(crate::concurrency::construct_waitgroup(&args)),
        "Semaphore" => Some(crate::concurrency::construct_semaphore(&args)),
        "Once" => Some(crate::concurrency::construct_once(&args)),
        "Barrier" => Some(crate::concurrency::construct_barrier(&args)),
        "Cond" => Some(crate::concurrency::construct_cond(&args)),
        "Atomic" => Some(crate::concurrency::construct_atomic(&args)),
        "type" if args.len() == 1 => Some(Ok(Value::type_ref(args[0].type_name()))),
        "type" => Some(Err(type_ctor_arity_error(
            "type",
            "expects 1 argument",
            args.len(),
        ))),
        _ => None,
    }
}

/// 将值转为原始类型：`type.(value)` / `convert(type, value)`。
pub fn call_primitive_convert(
    vm: &mut Vm,
    type_name: &str,
    value: &Value,
) -> Option<Result<Value>> {
    match type_name {
        "text" => Some(Ok(Value::Text(value.print_string()))),
        "num" => Some(coerce_to_num(value, type_convert_error)),
        "bool" => Some(Ok(Value::Bool(value.is_truthy()))),
        "list" => Some(convert_to_list(vm, value)),
        "set" => Some(convert_to_set(vm, value)),
        "tuple" => Some(convert_to_tuple(vm, value)),
        "bytes" => Some(convert_to_bytes(value)),
        "iterator" => Some(convert_to_iterator(value)),
        "ptr" => Some(convert_to_ptr(value)),
        n if crate::sized::SizedNum::ALL_NAMES.contains(&n) => Some(convert_to_sized(n, value)),
        n if crate::c_types::lookup_c_type(n).is_some() => Some(convert_to_c_type(n, value)),
        _ => None,
    }
}

fn convert_to_ptr(value: &Value) -> Result<Value> {
    match value {
        Value::Ptr(p) => Ok(Value::Ptr(*p)),
        Value::Sized(crate::sized::SizedNum::Usize(u)) => Ok(Value::Ptr(*u)),
        Value::Sized(crate::sized::SizedNum::Isize(i)) => Ok(Value::Ptr(*i as usize)),
        Value::Num(n) => {
            let i = n.to_i64().ok_or_else(|| type_convert_error("ptr", value))?;
            Ok(Value::Ptr(i as usize))
        }
        other => Err(type_convert_error("ptr", other)),
    }
}

fn convert_to_sized(type_name: &str, value: &Value) -> Result<Value> {
    use crate::sized::SizedNum;
    if let Value::Sized(s) = value {
        if s.type_name() == type_name {
            return Ok(Value::Sized(*s));
        }
    }
    let make_int = |bits: u32, signed: bool| -> Result<Value> {
        let n = match value {
            Value::Num(n) => n
                .to_i64()
                .ok_or_else(|| type_convert_error(type_name, value))?,
            Value::Sized(s) => s
                .to_i64()
                .ok_or_else(|| type_convert_error(type_name, value))?,
            Value::Bool(b) => i64::from(*b),
            Value::Ptr(p) => *p as i64,
            other => return Err(type_convert_error(type_name, other)),
        };
        // 禁止静默截断（implicit / Type.(v) 窄化须落在目标范围）。
        if signed {
            let (min, max) = match bits {
                8 => (i64::from(i8::MIN), i64::from(i8::MAX)),
                16 => (i64::from(i16::MIN), i64::from(i16::MAX)),
                32 => (i64::from(i32::MIN), i64::from(i32::MAX)),
                64 => (i64::MIN, i64::MAX),
                _ => return Err(type_convert_error(type_name, value)),
            };
            if n < min || n > max {
                return Err(RuntimeError::value_err(format!(
                    "cannot convert {n} to {type_name}: out of range [{min}, {max}]"
                )));
            }
        } else {
            if n < 0 {
                return Err(RuntimeError::value_err(format!(
                    "cannot convert {n} to {type_name}: negative value"
                )));
            }
            let max = match bits {
                8 => i64::from(u8::MAX),
                16 => i64::from(u16::MAX),
                32 => i64::from(u32::MAX),
                64 => i64::MAX, // 更大的无符号整数需走 BigInt 路径；此处 to_i64 已受限
                _ => return Err(type_convert_error(type_name, value)),
            };
            if bits < 64 && n > max {
                return Err(RuntimeError::value_err(format!(
                    "cannot convert {n} to {type_name}: out of range [0, {max}]"
                )));
            }
        }
        Ok(Value::Sized(match (bits, signed) {
            (8, true) => SizedNum::I8(n as i8),
            (8, false) => SizedNum::U8(n as u8),
            (16, true) => SizedNum::I16(n as i16),
            (16, false) => SizedNum::U16(n as u16),
            (32, true) => SizedNum::I32(n as i32),
            (32, false) => SizedNum::U32(n as u32),
            (64, true) => SizedNum::I64(n),
            (64, false) => SizedNum::U64(n as u64),
            _ => return Err(type_convert_error(type_name, value)),
        }))
    };
    match type_name {
        "i8" => make_int(8, true),
        "u8" => make_int(8, false),
        "i16" => make_int(16, true),
        "u16" => make_int(16, false),
        "i32" => make_int(32, true),
        "u32" => make_int(32, false),
        "i64" => make_int(64, true),
        "u64" => make_int(64, false),
        "isize" => {
            let n = match value {
                Value::Num(n) => n
                    .to_i64()
                    .ok_or_else(|| type_convert_error(type_name, value))?,
                Value::Sized(s) => s
                    .to_i64()
                    .ok_or_else(|| type_convert_error(type_name, value))?,
                Value::Ptr(p) => *p as i64,
                other => return Err(type_convert_error(type_name, other)),
            };
            Ok(Value::Sized(SizedNum::Isize(n as isize)))
        }
        "usize" => {
            let n = match value {
                Value::Num(n) => n
                    .to_i64()
                    .ok_or_else(|| type_convert_error(type_name, value))?
                    as u64,
                Value::Sized(s) => s.to_f64() as u64,
                Value::Ptr(p) => *p as u64,
                other => return Err(type_convert_error(type_name, other)),
            };
            Ok(Value::Sized(SizedNum::Usize(n as usize)))
        }
        "f32" => {
            let f = match value {
                Value::Num(n) => n.to_f64_checked()?,
                Value::Sized(s) => s.to_f64(),
                other => return Err(type_convert_error(type_name, other)),
            };
            Ok(Value::Sized(SizedNum::F32(f as f32)))
        }
        "f64" => {
            let f = match value {
                Value::Num(n) => n.to_f64_checked()?,
                Value::Sized(s) => s.to_f64(),
                other => return Err(type_convert_error(type_name, other)),
            };
            Ok(Value::Sized(SizedNum::F64(f)))
        }
        _ => Err(type_convert_error(type_name, value)),
    }
}

fn convert_to_c_type(type_name: &str, value: &Value) -> Result<Value> {
    let abi = crate::ffi::AbiType::from_type_name(type_name)?;
    // 转到对应语言定宽类型
    let lang = match abi {
        crate::ffi::AbiType::Void => return Ok(Value::None),
        crate::ffi::AbiType::Bool => "bool",
        crate::ffi::AbiType::I8 => "i8",
        crate::ffi::AbiType::U8 => "u8",
        crate::ffi::AbiType::I16 => "i16",
        crate::ffi::AbiType::U16 => "u16",
        crate::ffi::AbiType::I32 => "i32",
        crate::ffi::AbiType::U32 => "u32",
        crate::ffi::AbiType::I64 => "i64",
        crate::ffi::AbiType::U64 => "u64",
        crate::ffi::AbiType::Isize => "isize",
        crate::ffi::AbiType::Usize => "usize",
        crate::ffi::AbiType::F32 => "f32",
        crate::ffi::AbiType::F64 => "f64",
        crate::ffi::AbiType::Pointer
        | crate::ffi::AbiType::CharPtr
        | crate::ffi::AbiType::WCharPtr => "ptr",
        crate::ffi::AbiType::CStruct { .. } => {
            return Err(crate::error::RuntimeError::type_err(
                "cannot convert to C struct type by name; construct the struct",
            ));
        }
    };
    if lang == "bool" {
        return Ok(Value::Bool(value.is_truthy()));
    }
    if lang == "ptr" {
        return convert_to_ptr(value);
    }
    convert_to_sized(lang, value)
}

fn construct_list(arg: &Value) -> Result<Value> {
    match arg {
        Value::List(lst) => Ok(Value::List(lst.clone())),
        other => Err(type_ctor_error("list", other)),
    }
}

fn construct_dict_kv(args: Vec<Value>) -> Result<Value> {
    let mut map = DictMap::new();
    let mut i = 0;
    while i < args.len() {
        let key = ValueKey::from_value(&args[i])?;
        map.insert(key, args[i + 1].clone());
        i += 2;
    }
    Ok(Value::Dict(Shared::new(map)))
}

fn construct_set(args: Vec<Value>) -> Result<Value> {
    let mut set = crate::value::SetMap::new();
    for arg in args {
        set.insert(ValueKey::from_value(&arg)?);
    }
    Ok(Value::Set(Shared::new(set)))
}

fn construct_bytes(arg: &Value) -> Result<Value> {
    match arg {
        Value::Bytes(b) => Ok(Value::Bytes(b.clone())),
        Value::Text(s) => Ok(Value::Bytes(Arc::new(s.as_bytes().to_vec()))),
        Value::List(lst) => {
            let mut out = Vec::new();
            for item in lst.borrow().iter() {
                match item {
                    Value::Num(n) => {
                        let v = n.to_i64().ok_or_else(|| {
                            RuntimeError::type_err("TypeError: byte value out of range")
                        })?;
                        if !(0..=255).contains(&v) {
                            return Err(RuntimeError::type_err(
                                "TypeError: byte value out of range 0..255",
                            ));
                        }
                        out.push(v as u8);
                    }
                    other => {
                        return Err(type_ctor_error("bytes", other));
                    }
                }
            }
            Ok(Value::Bytes(Arc::new(out)))
        }
        other => Err(type_ctor_error("bytes", other)),
    }
}

fn construct_iterator(arg: &Value) -> Result<Value> {
    match arg {
        Value::List(lst) => Ok(IteratorState::from_list(lst.borrow().clone()).into_value()),
        Value::Iterator(it) => Ok(Value::Iterator(it.clone())),
        other => Err(type_ctor_error("iterator", other)),
    }
}

fn convert_to_list(vm: &mut Vm, arg: &Value) -> Result<Value> {
    match arg {
        Value::Iterator(it) => {
            let mut out = Vec::new();
            while let Some(item) = vm.advance_iterator(it)? {
                out.push(item);
            }
            Ok(Value::List(Shared::new(out)))
        }
        Value::List(lst) => Ok(Value::List(lst.clone())),
        Value::Tuple(t) => Ok(Value::List(Shared::new(t.to_vec()))),
        Value::Set(s) => {
            let items: Vec<Value> = s
                .borrow()
                .iter()
                .map(crate::value::value_key_to_value)
                .collect();
            Ok(Value::List(Shared::new(items)))
        }
        other => Err(type_convert_error("list", other)),
    }
}

fn convert_to_set(vm: &mut Vm, arg: &Value) -> Result<Value> {
    match arg {
        Value::Set(s) => Ok(Value::Set(s.clone())),
        Value::List(lst) => {
            let mut set = crate::value::SetMap::new();
            for item in lst.borrow().iter() {
                set.insert(ValueKey::from_value(item)?);
            }
            Ok(Value::Set(Shared::new(set)))
        }
        Value::Tuple(t) => {
            let mut set = crate::value::SetMap::new();
            for item in t.iter() {
                set.insert(ValueKey::from_value(item)?);
            }
            Ok(Value::Set(Shared::new(set)))
        }
        Value::Iterator(it) => {
            let mut set = crate::value::SetMap::new();
            while let Some(item) = vm.advance_iterator(it)? {
                set.insert(ValueKey::from_value(&item)?);
            }
            Ok(Value::Set(Shared::new(set)))
        }
        other => Err(type_convert_error("set", other)),
    }
}

fn convert_to_tuple(vm: &mut Vm, arg: &Value) -> Result<Value> {
    match arg {
        Value::Tuple(t) => Ok(Value::Tuple(t.clone())),
        Value::List(lst) => Ok(Value::Tuple(lst.borrow().clone().into())),
        Value::Set(s) => {
            let items: Vec<Value> = s
                .borrow()
                .iter()
                .map(crate::value::value_key_to_value)
                .collect();
            Ok(Value::Tuple(items.into()))
        }
        Value::Iterator(it) => {
            let mut out = Vec::new();
            while let Some(item) = vm.advance_iterator(it)? {
                out.push(item);
            }
            Ok(Value::Tuple(out.into()))
        }
        other => Err(type_convert_error("tuple", other)),
    }
}

fn convert_to_bytes(arg: &Value) -> Result<Value> {
    match arg {
        Value::Bytes(b) => Ok(Value::Bytes(b.clone())),
        Value::Text(s) => Ok(Value::Bytes(Arc::new(s.as_bytes().to_vec()))),
        Value::List(lst) => construct_bytes(&Value::List(lst.clone())),
        other => Err(type_convert_error("bytes", other)),
    }
}

fn convert_to_iterator(arg: &Value) -> Result<Value> {
    match arg {
        Value::List(lst) => Ok(IteratorState::from_list(lst.borrow().clone()).into_value()),
        Value::Tuple(t) => Ok(IteratorState::from_list(t.to_vec()).into_value()),
        Value::Set(s) => {
            let items: Vec<Value> = s
                .borrow()
                .iter()
                .map(crate::value::value_key_to_value)
                .collect();
            Ok(IteratorState::from_list(items).into_value())
        }
        Value::Iterator(it) => Ok(Value::Iterator(it.clone())),
        other => Err(type_convert_error("iterator", other)),
    }
}

fn coerce_to_num(arg: &Value, on_mismatch: fn(&str, &Value) -> RuntimeError) -> Result<Value> {
    match arg {
        Value::Num(n) => Ok(Value::Num(n.clone())),
        Value::Sized(s) => {
            if let Some(i) = s.to_i64() {
                Ok(Value::Num(Num::Small(i)))
            } else {
                let f = s.to_f64();
                num_rational::BigRational::from_float(f)
                    .map(|r| Value::Num(Num::from_rational(r)))
                    .ok_or_else(|| RuntimeError::value_err("non-finite floating-point result"))
            }
        }
        Value::Text(s) => {
            if s.contains('.') || s.contains('e') || s.contains('E') {
                Ok(Value::Num(Num::from_literal(s)?))
            } else {
                Ok(Value::Num(Num::from_bigint(
                    s.parse::<num_bigint::BigInt>()
                        .map_err(|_| RuntimeError::type_err("TypeError: invalid num literal"))?,
                )))
            }
        }
        Value::Bool(b) => Ok(Value::Num(Num::Small(i64::from(*b)))),
        other => Err(on_mismatch("num", other)),
    }
}

pub fn try_call_primitive_magic(
    vm: &mut Vm,
    obj: &Value,
    method: &str,
    args: Vec<Value>,
) -> Option<Result<Value>> {
    let type_name = value_primitive_type(obj)?;
    let f = vm.primitive_methods.get(type_name)?.get(method)?.clone();
    let mut full = vec![obj.clone()];
    full.extend(args);
    match f(vm, &full) {
        Ok(v) => Some(Ok(v)),
        Err(e) if e.kind() == crate::error::ExceptionKind::UnsupportedOp => None,
        Err(e) => Some(Err(e)),
    }
}

fn value_add(a: &Value, b: &Value) -> Result<Value> {
    a.add(b)
}
fn value_sub(a: &Value, b: &Value) -> Result<Value> {
    a.sub(b)
}
fn value_mul(a: &Value, b: &Value) -> Result<Value> {
    a.mul(b)
}
fn value_div(a: &Value, b: &Value) -> Result<Value> {
    a.div(b)
}
fn value_mod(a: &Value, b: &Value) -> Result<Value> {
    a.rem(b)
}
fn value_pow(a: &Value, b: &Value) -> Result<Value> {
    a.pow(b)
}
fn value_bitand(a: &Value, b: &Value) -> Result<Value> {
    a.bitand(b)
}
fn value_bitor(a: &Value, b: &Value) -> Result<Value> {
    a.bitor(b)
}
fn value_bitxor(a: &Value, b: &Value) -> Result<Value> {
    a.bitxor(b)
}
fn value_lshift(a: &Value, b: &Value) -> Result<Value> {
    a.lshift(b)
}
fn value_rshift(a: &Value, b: &Value) -> Result<Value> {
    a.rshift(b)
}

/// printf 风格字符串格式化：`fmt % args`。
///
/// - `rhs` 为 `List`/`Tuple` → 按位置消费转换说明；
/// - `rhs` 为 `Dict` → 支持 `%(key)s` 键控；
/// - 其它 → 当作单个值消费（仅一个转换说明）。
///
/// 支持的转换类型：`s r d i u f e g x X o b c %`，可选 `[flags][width][.precision]`。
fn percent_format(fmt: &str, rhs: &Value) -> Result<String> {
    let bytes: Vec<char> = fmt.chars().collect();
    let n = bytes.len();
    let mut out = String::with_capacity(fmt.len() + 8);
    let mut i = 0;
    // 位置参数迭代器
    let pos: Vec<Value> = match rhs {
        Value::List(l) => l.borrow().clone(),
        Value::Tuple(t) => t.to_vec(),
        _ => vec![rhs.clone()],
    };
    let mut pos_idx = 0usize;
    let dict = if let Value::Dict(d) = rhs {
        Some(d.clone())
    } else {
        None
    };

    while i < n {
        let c = bytes[i];
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        // 命中 %，解析转换说明
        i += 1;
        if i >= n {
            return Err(RuntimeError::value_err("incomplete format specifier"));
        }
        if bytes[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        // 可选 (key)
        let key: Option<String> = if bytes[i] == '(' {
            let start = i + 1;
            let end = (start..n)
                .find(|&j| bytes[j] == ')')
                .ok_or_else(|| RuntimeError::value_err("unterminated %(key) in format"))?;
            let k: String = bytes[start..end].iter().collect();
            i = end + 1;
            Some(k)
        } else {
            None
        };
        // flags
        let mut left_align = false;
        let mut zero_pad = false;
        let mut plus_sign = false;
        let mut space_sign = false;
        let mut alt = false;
        loop {
            match bytes.get(i) {
                Some('-') => left_align = true,
                Some('0') => zero_pad = true,
                Some('+') => plus_sign = true,
                Some(' ') => space_sign = true,
                Some('#') => alt = true,
                _ => break,
            }
            i += 1;
        }
        // width
        let mut width = 0usize;
        while let Some(d @ '0'..='9') = bytes.get(i) {
            width = width * 10 + (*d as usize - '0' as usize);
            i += 1;
        }
        // precision
        let mut precision: Option<usize> = None;
        if bytes.get(i) == Some(&'.') {
            i += 1;
            let mut p = 0usize;
            while let Some(d @ '0'..='9') = bytes.get(i) {
                p = p * 10 + (*d as usize - '0' as usize);
                i += 1;
            }
            precision = Some(p);
        }
        // type
        let conv = *bytes
            .get(i)
            .ok_or_else(|| RuntimeError::value_err("incomplete format specifier (missing type)"))?;
        i += 1;

        // 取值
        let val: Value = if let Some(k) = &key {
            let d = dict
                .as_ref()
                .ok_or_else(|| RuntimeError::value_err("%(key) requires a dict argument"))?;
            d.borrow()
                .get(&ValueKey::Text(k.clone()))
                .cloned()
                .ok_or_else(|| RuntimeError::value_err(format!("missing format key '{k}'")))?
        } else {
            if pos_idx >= pos.len() {
                return Err(RuntimeError::value_err(
                    "not enough arguments for format string",
                ));
            }
            let v = pos[pos_idx].clone();
            pos_idx += 1;
            v
        };

        let formatted = format_one(conv, &val, precision, plus_sign, space_sign, alt)?;
        let formatted = apply_width(&formatted, width, left_align, zero_pad, conv);
        out.push_str(&formatted);
    }
    if pos_idx != pos.len() && dict.is_none() {
        // 位置参数有剩余（且非 dict 模式）
        return Err(RuntimeError::value_err(
            "too many arguments for format string",
        ));
    }
    Ok(out)
}

fn format_one(
    conv: char,
    val: &Value,
    precision: Option<usize>,
    plus_sign: bool,
    space_sign: bool,
    alt: bool,
) -> Result<String> {
    match conv {
        's' => {
            let s = val.print_string();
            Ok(if let Some(p) = precision {
                s.chars().take(p).collect()
            } else {
                s
            })
        }
        'r' => Ok(val.print_string()),
        'd' | 'i' | 'u' => {
            let s = format_int_radix(val, 10, false, plus_sign, space_sign, alt)?;
            Ok(s)
        }
        'f' | 'F' => {
            let f = to_f64_for_fmt(val)?;
            Ok(format!("{:.*}", precision.unwrap_or(6), f))
        }
        'e' | 'E' => {
            let f = to_f64_for_fmt(val)?;
            let s = if conv == 'e' {
                format!("{:.*e}", precision.unwrap_or(6), f)
            } else {
                format!("{:.*E}", precision.unwrap_or(6), f)
            };
            Ok(s)
        }
        'g' | 'G' => {
            let f = to_f64_for_fmt(val)?;
            Ok(format!("{:.*}", precision.unwrap_or(6), f))
        }
        'x' => Ok(format_int_radix(
            val, 16, false, plus_sign, space_sign, alt,
        )?),
        'X' => Ok(format_int_radix(val, 16, true, plus_sign, space_sign, alt)?),
        'o' => Ok(format_int_radix(val, 8, false, plus_sign, space_sign, alt)?),
        'b' => Ok(format_int_radix(val, 2, false, plus_sign, space_sign, alt)?),
        'c' => {
            let ch = match val {
                Value::Num(n) => {
                    let cp = n
                        .to_i64()
                        .ok_or_else(|| RuntimeError::value_err("%c needs an integer codepoint"))?
                        as u32;
                    char::from_u32(cp)
                        .ok_or_else(|| RuntimeError::value_err("invalid codepoint for %c"))?
                }
                Value::Text(s) => s
                    .chars()
                    .next()
                    .ok_or_else(|| RuntimeError::value_err("%c needs a non-empty text"))?,
                _ => return Err(RuntimeError::value_err("%c needs num or text")),
            };
            Ok(ch.to_string())
        }
        other => Err(RuntimeError::value_err(format!(
            "unsupported format type '%{other}'"
        ))),
    }
}

fn to_f64_for_fmt(v: &Value) -> Result<f64> {
    match v {
        Value::Num(n) => n.to_f64_checked().ok().ok_or_else(|| {
            RuntimeError::value_err("value is not representable as float for format")
        }),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        _ => Err(RuntimeError::value_err("not a number for numeric format")),
    }
}

/// 整数按给定进制格式化（支持大整数）。`upper` 仅对 16 进制生效。`alt` 加 `0x`/`0`/`0b` 前缀。
fn format_int_radix(
    v: &Value,
    radix: u32,
    upper: bool,
    plus_sign: bool,
    space_sign: bool,
    alt: bool,
) -> Result<String> {
    let n = match v {
        Value::Num(n) => n,
        Value::Bool(b) => return Ok(i32::from(*b).to_string()),
        _ => return Err(RuntimeError::value_err("not an integer for integer format")),
    };
    let prefix = if alt {
        match radix {
            16 => {
                if upper {
                    "0X"
                } else {
                    "0x"
                }
            }
            8 => "0",
            2 => "0b",
            _ => "",
        }
    } else {
        ""
    };
    let s = match n {
        Num::Small(i) => {
            let mag = u128::from((*i).unsigned_abs());
            let body = match radix {
                10 => mag.to_string(),
                16 => format!("{mag:x}"),
                8 => format!("{mag:o}"),
                2 => format!("{mag:b}"),
                _ => return Err(RuntimeError::value_err("unsupported radix")),
            };
            let body = if upper && radix == 16 {
                body.to_uppercase()
            } else {
                body
            };
            if *i < 0 {
                format!("-{prefix}{body}")
            } else if plus_sign {
                format!("+{prefix}{body}")
            } else if space_sign {
                format!(" {prefix}{body}")
            } else {
                format!("{prefix}{body}")
            }
        }
        Num::Int(bi) => {
            use num_traits::Signed;
            let mag = bi.abs();
            let body = match radix {
                10 => mag.to_str_radix(10),
                16 => mag.to_str_radix(16),
                8 => mag.to_str_radix(8),
                2 => mag.to_str_radix(2),
                _ => return Err(RuntimeError::value_err("unsupported radix")),
            };
            let body = if upper && radix == 16 {
                body.to_uppercase()
            } else {
                body
            };
            if bi.is_negative() {
                format!("-{prefix}{body}")
            } else if plus_sign {
                format!("+{prefix}{body}")
            } else if space_sign {
                format!(" {prefix}{body}")
            } else {
                format!("{prefix}{body}")
            }
        }
        Num::Rat(_) => {
            return Err(RuntimeError::value_err(
                "rational cannot be formatted as integer; use %f or %s",
            ));
        }
    };
    Ok(s)
}

fn apply_width(s: &str, width: usize, left_align: bool, zero_pad: bool, conv: char) -> String {
    if width == 0 {
        return s.to_string();
    }
    let len = s.chars().count();
    if len >= width {
        return s.to_string();
    }
    let pad = width - len;
    // 数字零填充只对数字转换且未左对齐时生效，且填充在符号之后
    let is_numeric = matches!(
        conv,
        'd' | 'i' | 'u' | 'f' | 'e' | 'E' | 'g' | 'G' | 'x' | 'X' | 'o' | 'b'
    );
    if zero_pad && !left_align && is_numeric {
        // 找符号前缀
        let (sign, body) = if let Some(stripped) = s.strip_prefix('-') {
            ("-", stripped)
        } else if let Some(stripped) = s.strip_prefix('+') {
            ("+", stripped)
        } else if let Some(stripped) = s.strip_prefix(' ') {
            (" ", stripped)
        } else {
            ("", s)
        };
        let zeros: String = "0".repeat(pad);
        return format!("{sign}{zeros}{body}");
    }
    let padding: String = if zero_pad && !left_align {
        "0".repeat(pad)
    } else {
        " ".repeat(pad)
    };
    if left_align {
        format!("{s}{padding}")
    } else {
        format!("{padding}{s}")
    }
}

fn install_primitive_methods(vm: &mut Vm) {
    fn bin_magic(
        method: &'static str,
        reversed: bool,
        op: fn(&Value, &Value) -> Result<Value>,
    ) -> BuiltinFn {
        Arc::new(move |_vm, args| {
            if args.len() != 2 {
                return Err(RuntimeError::type_err(format!(
                    "{method} requires 2 arguments"
                )));
            }
            if reversed {
                op(&args[1], &args[0])
            } else {
                op(&args[0], &args[1])
            }
        })
    }

    let mut methods: FxHashMap<String, FxHashMap<String, BuiltinFn>> = FxHashMap::default();

    let mut num_methods = FxHashMap::default();
    num_methods.insert("__add__".into(), bin_magic("__add__", false, value_add));
    num_methods.insert("__radd__".into(), bin_magic("__radd__", true, value_add));
    num_methods.insert("__sub__".into(), bin_magic("__sub__", false, value_sub));
    num_methods.insert("__rsub__".into(), bin_magic("__rsub__", true, value_sub));
    num_methods.insert("__mul__".into(), bin_magic("__mul__", false, value_mul));
    num_methods.insert("__rmul__".into(), bin_magic("__rmul__", true, value_mul));
    num_methods.insert("__div__".into(), bin_magic("__div__", false, value_div));
    num_methods.insert("__rdiv__".into(), bin_magic("__rdiv__", true, value_div));
    num_methods.insert("__mod__".into(), bin_magic("__mod__", false, value_mod));
    num_methods.insert("__rmod__".into(), bin_magic("__rmod__", true, value_mod));
    num_methods.insert("__pow__".into(), bin_magic("__pow__", false, value_pow));
    num_methods.insert("__rpow__".into(), bin_magic("__rpow__", true, value_pow));
    num_methods.insert("__and__".into(), bin_magic("__and__", false, value_bitand));
    num_methods.insert("__rand__".into(), bin_magic("__rand__", true, value_bitand));
    num_methods.insert("__or__".into(), bin_magic("__or__", false, value_bitor));
    num_methods.insert("__ror__".into(), bin_magic("__ror__", true, value_bitor));
    num_methods.insert("__xor__".into(), bin_magic("__xor__", false, value_bitxor));
    num_methods.insert("__rxor__".into(), bin_magic("__rxor__", true, value_bitxor));
    num_methods.insert(
        "__lshift__".into(),
        bin_magic("__lshift__", false, value_lshift),
    );
    num_methods.insert(
        "__rlshift__".into(),
        bin_magic("__rlshift__", true, value_lshift),
    );
    num_methods.insert(
        "__rshift__".into(),
        bin_magic("__rshift__", false, value_rshift),
    );
    num_methods.insert(
        "__rrshift__".into(),
        bin_magic("__rrshift__", true, value_rshift),
    );
    num_methods.insert(
        "__neg__".into(),
        Arc::new(|_vm, args| {
            if args.len() != 1 {
                return Err(RuntimeError::type_err("__neg__ requires 1 argument"));
            }
            args[0].neg()
        }),
    );
    num_methods.insert(
        "__invert__".into(),
        Arc::new(|_vm, args| {
            if args.len() != 1 {
                return Err(RuntimeError::type_err("__invert__ requires 1 argument"));
            }
            args[0].invert()
        }),
    );

    let mut text_methods = FxHashMap::default();
    text_methods.insert("__add__".into(), bin_magic("__add__", false, value_add));
    text_methods.insert("__radd__".into(), bin_magic("__radd__", true, value_add));
    text_methods.insert(
        "__mod__".into(),
        Arc::new(|_vm, args| {
            if args.len() != 2 {
                return Err(RuntimeError::type_err("__mod__ requires 2 arguments"));
            }
            match (&args[0], &args[1]) {
                (Value::Text(fmt), rhs) => Ok(Value::Text(percent_format(fmt, rhs)?)),
                _ => Err(RuntimeError::type_err("text % expects (text, value)")),
            }
        }),
    );
    text_methods.insert(
        "__mul__".into(),
        Arc::new(|_vm, args| {
            if args.len() != 2 {
                return Err(RuntimeError::type_err("__mul__ requires 2 arguments"));
            }
            match (&args[0], &args[1]) {
                (Value::Text(s), Value::Num(n)) => {
                    let count = n
                        .to_i64()
                        .ok_or_else(|| RuntimeError::value_err("bad repeat count"))?;
                    if count < 0 {
                        return Err(RuntimeError::value_err("negative repeat count"));
                    }
                    Ok(Value::Text(s.repeat(count as usize)))
                }
                _ => Err(RuntimeError::type_err("text * expects (text, num)")),
            }
        }),
    );
    text_methods.insert(
        "__rmul__".into(),
        Arc::new(|_vm, args| {
            if args.len() != 2 {
                return Err(RuntimeError::type_err("__rmul__ requires 2 arguments"));
            }
            match (&args[1], &args[0]) {
                (Value::Text(s), Value::Num(n)) => {
                    let count = n
                        .to_i64()
                        .ok_or_else(|| RuntimeError::value_err("bad repeat count"))?;
                    if count < 0 {
                        return Err(RuntimeError::value_err("negative repeat count"));
                    }
                    Ok(Value::Text(s.repeat(count as usize)))
                }
                _ => Err(RuntimeError::type_err("text * expects (num, text)")),
            }
        }),
    );

    methods.insert("num".into(), num_methods);
    methods.insert("text".into(), text_methods);
    vm.primitive_methods = methods;
}

pub fn get_text_method(text: &str, field: &str) -> Result<Value> {
    let text = text.to_string();
    match field {
        "len" => Ok(Value::builtin(field, move |_vm, args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("len takes no arguments"));
            }
            Ok(Value::Num(Num::Small(text.chars().count() as i64)))
        })),
        "upper" => Ok(Value::builtin(field, move |_vm, _args| {
            Ok(Value::Text(text.to_uppercase()))
        })),
        "lower" => Ok(Value::builtin(field, move |_vm, _args| {
            Ok(Value::Text(text.to_lowercase()))
        })),
        "strip" => Ok(Value::builtin(field, move |_vm, _args| {
            Ok(Value::Text(text.trim().to_string()))
        })),
        "split" => Ok(Value::builtin(field, move |_vm, args| {
            let sep = if args.is_empty() {
                " ".to_string()
            } else {
                match &args[0] {
                    Value::Text(t) => t.clone(),
                    _ => return Err(RuntimeError::type_err("split separator must be text")),
                }
            };
            let parts: Vec<Value> = text
                .split(&sep)
                .map(|p| Value::Text(p.to_string()))
                .collect();
            Ok(Value::List(Shared::new(parts)))
        })),
        "contains" => Ok(Value::builtin(field, move |_vm, args| {
            if args.len() != 1 {
                return Err(RuntimeError::type_err("contains requires 1 argument"));
            }
            let needle = args[0].print_string();
            Ok(Value::Bool(text.contains(&needle)))
        })),
        _ => Err(RuntimeError::attr_err(format!(
            "text has no method {field}"
        ))),
    }
}

pub fn get_list_method(list: &Shared<Vec<Value>>, field: &str) -> Result<Value> {
    match field {
        "len" => {
            let owned = list.clone();
            Ok(Value::builtin(field, move |_vm, args| {
                if !args.is_empty() {
                    return Err(RuntimeError::type_err("len takes no arguments"));
                }
                Ok(Value::Num(Num::Small(owned.borrow().len() as i64)))
            }))
        }
        "append" => {
            let owned = list.clone();
            Ok(Value::builtin(field, move |vm, args| {
                if args.len() != 1 {
                    return Err(RuntimeError::type_err("append requires 1 argument"));
                }
                vm.check_list_element_write(&owned, &args[0])?;
                owned.borrow_mut().push(args[0].clone());
                Ok(Value::None)
            }))
        }
        "extend" => {
            let owned = list.clone();
            Ok(Value::builtin(field, move |vm, args| {
                if args.len() != 1 {
                    return Err(RuntimeError::type_err("extend requires 1 argument"));
                }
                let state_rc = vm.to_iterator_shared(&args[0])?;
                let mut pending = Vec::new();
                while let Some(v) = vm.advance_iterator(&state_rc)? {
                    vm.check_list_element_write(&owned, &v)?;
                    pending.push(v);
                }
                owned.borrow_mut().extend(pending);
                Ok(Value::None)
            }))
        }
        "pop" => {
            let owned = list.clone();
            Ok(Value::builtin(field, move |_vm, args| {
                let mut items = owned.borrow_mut();
                if items.is_empty() {
                    return Err(RuntimeError::index_err("pop from empty list"));
                }
                let idx = if args.is_empty() {
                    items.len() - 1
                } else {
                    match &args[0] {
                        Value::Num(Num::Small(n)) => {
                            let i = *n as isize;
                            if i < 0 {
                                (items.len() as isize + i) as usize
                            } else {
                                i as usize
                            }
                        }
                        Value::Num(Num::Int(n)) => {
                            let i: isize = n
                                .as_ref()
                                .try_into()
                                .map_err(|_| RuntimeError::index_err("pop index out of range"))?;
                            if i < 0 {
                                (items.len() as isize + i) as usize
                            } else {
                                i as usize
                            }
                        }
                        _ => return Err(RuntimeError::type_err("pop index must be integer")),
                    }
                };
                if idx >= items.len() {
                    return Err(RuntimeError::index_err("pop index out of range"));
                }
                Ok(items.remove(idx))
            }))
        }
        _ => Err(RuntimeError::attr_err(format!(
            "list has no method {field}"
        ))),
    }
}

pub fn get_dict_method(dict: &Shared<DictMap>, field: &str) -> Result<Value> {
    match field {
        "len" => {
            let d = dict.clone();
            Ok(Value::builtin(field, move |_vm, args| {
                if !args.is_empty() {
                    return Err(RuntimeError::type_err("len takes no arguments"));
                }
                Ok(Value::Num(Num::Small(d.borrow().len() as i64)))
            }))
        }
        "get" => {
            let d = dict.clone();
            Ok(Value::builtin(field, move |_vm, args| {
                if args.is_empty() || args.len() > 2 {
                    return Err(RuntimeError::type_err("get requires 1 or 2 arguments"));
                }
                let key = ValueKey::from_value(&args[0])?;
                if let Some(v) = d.borrow().get(&key) {
                    return Ok(v.clone());
                }
                if args.len() == 2 {
                    return Ok(args[1].clone());
                }
                Err(RuntimeError::key_err("key not found"))
            }))
        }
        "set" => {
            let d = dict.clone();
            Ok(Value::builtin(field, move |vm, args| {
                if args.len() != 2 {
                    return Err(RuntimeError::type_err("set requires 2 arguments"));
                }
                vm.check_dict_write(&d, &args[0], &args[1])?;
                let key = ValueKey::from_value(&args[0])?;
                d.borrow_mut().insert(key, args[1].clone());
                Ok(Value::None)
            }))
        }
        "keys" => {
            let d = dict.clone();
            Ok(Value::builtin(field, move |_vm, _args| {
                let keys: Vec<Value> = d
                    .borrow()
                    .keys()
                    .map(crate::value::value_key_to_value)
                    .collect();
                Ok(Value::List(Shared::new(keys)))
            }))
        }
        "values" => {
            let d = dict.clone();
            Ok(Value::builtin(field, move |_vm, _args| {
                Ok(Value::List(Shared::new(
                    d.borrow().values().cloned().collect(),
                )))
            }))
        }
        "items" => {
            let d = dict.clone();
            Ok(Value::builtin(field, move |_vm, _args| {
                let pairs: Vec<Value> = d
                    .borrow()
                    .iter()
                    .map(|(k, v)| {
                        Value::List(Shared::new(vec![
                            crate::value::value_key_to_value(k),
                            v.clone(),
                        ]))
                    })
                    .collect();
                Ok(Value::List(Shared::new(pairs)))
            }))
        }
        _ => Err(RuntimeError::attr_err(format!(
            "dict has no method {field}"
        ))),
    }
}

pub fn get_set_method(set: &Shared<crate::value::SetMap>, field: &str) -> Result<Value> {
    match field {
        "len" => {
            let s = set.clone();
            Ok(Value::builtin(field, move |_vm, args| {
                if !args.is_empty() {
                    return Err(RuntimeError::type_err("len takes no arguments"));
                }
                Ok(Value::Num(Num::Small(s.borrow().len() as i64)))
            }))
        }
        "add" => {
            let s = set.clone();
            Ok(Value::builtin(field, move |vm, args| {
                if args.len() != 1 {
                    return Err(RuntimeError::type_err("add requires 1 argument"));
                }
                vm.check_set_element_write(&s, &args[0])?;
                s.borrow_mut().insert(ValueKey::from_value(&args[0])?);
                Ok(Value::None)
            }))
        }
        "remove" => {
            let s = set.clone();
            Ok(Value::builtin(field, move |_vm, args| {
                if args.len() != 1 {
                    return Err(RuntimeError::type_err("remove requires 1 argument"));
                }
                let key = ValueKey::from_value(&args[0])?;
                if !s.borrow_mut().remove(&key) {
                    return Err(RuntimeError::key_err("set.remove: element not found"));
                }
                Ok(Value::None)
            }))
        }
        "contains" => {
            let s = set.clone();
            Ok(Value::builtin(field, move |_vm, args| {
                if args.len() != 1 {
                    return Err(RuntimeError::type_err("contains requires 1 argument"));
                }
                let key = ValueKey::from_value(&args[0])?;
                Ok(Value::Bool(s.borrow().contains(&key)))
            }))
        }
        _ => Err(RuntimeError::attr_err(format!("set has no method {field}"))),
    }
}

pub fn get_tuple_method(tuple: &Arc<[Value]>, field: &str) -> Result<Value> {
    match field {
        "len" => {
            let t = tuple.clone();
            Ok(Value::builtin(field, move |_vm, args| {
                if !args.is_empty() {
                    return Err(RuntimeError::type_err("len takes no arguments"));
                }
                Ok(Value::Num(Num::Small(t.len() as i64)))
            }))
        }
        _ => Err(RuntimeError::attr_err(format!(
            "tuple has no method {field}"
        ))),
    }
}

pub fn get_bytes_method(bytes: &Arc<Vec<u8>>, field: &str) -> Result<Value> {
    match field {
        "len" => {
            let b = bytes.clone();
            Ok(Value::builtin(field, move |_vm, args| {
                if !args.is_empty() {
                    return Err(RuntimeError::type_err("len takes no arguments"));
                }
                Ok(Value::Num(Num::Small(b.len() as i64)))
            }))
        }
        "decode" => {
            let b = bytes.clone();
            Ok(Value::builtin(field, move |_vm, args| {
                if !args.is_empty() {
                    return Err(RuntimeError::type_err("decode takes no arguments"));
                }
                String::from_utf8(b.as_ref().clone())
                    .map(Value::Text)
                    .map_err(|_| RuntimeError::type_err("TypeError: bytes are not valid UTF-8"))
            }))
        }
        "hex" => {
            let b = bytes.clone();
            Ok(Value::builtin(field, move |_vm, _args| {
                let mut out = String::with_capacity(b.len() * 2);
                for byte in b.iter() {
                    out.push_str(&format!("{byte:02x}"));
                }
                Ok(Value::Text(out))
            }))
        }
        _ => Err(RuntimeError::attr_err(format!(
            "bytes has no method {field}"
        ))),
    }
}

fn install_primitive_type_globals(vm: &mut Vm) {
    for ty in [
        "text",
        "num",
        "bool",
        "list",
        "dict",
        "set",
        "tuple",
        "bytes",
        "iterator",
        "nonetype",
        "type",
        "AST",
        "Frame",
        "Traceback",
        "ptr",
        "i8",
        "u8",
        "i16",
        "u16",
        "i32",
        "u32",
        "i64",
        "u64",
        "isize",
        "usize",
        "f32",
        "f64",
        "Channel",
        "Stream",
        "Mutex",
        "RWMutex",
        "RwLock",
        "WaitGroup",
        "Semaphore",
        "Once",
        "Barrier",
        "Cond",
        "Atomic",
        // 类型形态 / 特殊类型名：注解求值走 load_name，必须是全局类型句柄
        "Union",
        "Maybe",
        "Never",
        "Literal",
        "Callable",
        "Tuple",
        "Covariant",
        "Contravariant",
        "Invariant",
        "function",
    ] {
        vm.globals.or_insert_with(ty.into(), || Value::type_ref(ty));
    }
    // 优先使用元类型句柄，覆盖同名的旧式内建函数。
    vm.globals.insert("type".into(), Value::type_ref("type"));
    vm.globals
        .insert("Channel".into(), Value::type_ref("Channel"));
    vm.globals
        .insert("Stream".into(), Value::type_ref("Stream"));
    vm.globals.insert("Mutex".into(), Value::type_ref("Mutex"));
    vm.globals
        .insert("RWMutex".into(), Value::type_ref("RWMutex"));
    vm.globals
        .insert("RwLock".into(), Value::type_ref("RWMutex"));
    vm.globals
        .insert("WaitGroup".into(), Value::type_ref("WaitGroup"));
    vm.globals
        .insert("Semaphore".into(), Value::type_ref("Semaphore"));
    vm.globals.insert("Once".into(), Value::type_ref("Once"));
    vm.globals
        .insert("Barrier".into(), Value::type_ref("Barrier"));
    vm.globals.insert("Cond".into(), Value::type_ref("Cond"));
    vm.globals
        .insert("Atomic".into(), Value::type_ref("Atomic"));
}

fn primitive_convert_handler(type_name: &'static str) -> BuiltinFn {
    Arc::new(move |vm, args| {
        if args.len() != 2 {
            return Err(RuntimeError::type_err(
                "TypeError: convert handler requires 2 arguments",
            ));
        }
        match call_primitive_convert(vm, type_name, &args[1]) {
            Some(result) => result,
            None => Err(type_convert_error(type_name, &args[1])),
        }
    })
}

fn dynamic_convert_handler(type_name: String) -> BuiltinFn {
    Arc::new(move |vm, args| {
        if args.len() != 2 {
            return Err(RuntimeError::type_err(
                "TypeError: convert handler requires 2 arguments",
            ));
        }
        match call_primitive_convert(vm, &type_name, &args[1]) {
            Some(result) => result,
            None => Err(type_convert_error(&type_name, &args[1])),
        }
    })
}

fn install_primitive_convert_handlers(vm: &mut Vm) {
    let static_prims: &[&'static str] = &[
        "text", "num", "bool", "list", "iterator", "set", "tuple", "bytes", "ptr",
    ];
    let mut types: Vec<String> = static_prims
        .iter()
        .copied()
        .chain(crate::sized::SizedNum::ALL_NAMES.iter().copied())
        .map(str::to_string)
        .collect();
    types.extend(crate::c_types::all_c_type_convert_names());

    for ty in types {
        let table = vm.get_or_create_convert(&ty);
        let table_ref = table.borrow();
        let empty = table_ref.handlers.borrow().is_empty();
        drop(table_ref);
        if empty {
            let handler = if let Some(name) = static_prims
                .iter()
                .copied()
                .chain(crate::sized::SizedNum::ALL_NAMES.iter().copied())
                .find(|s| *s == ty.as_str())
            {
                primitive_convert_handler(name)
            } else {
                dynamic_convert_handler(ty.clone())
            };
            table.borrow().handlers.borrow_mut().push(Value::Builtin(
                crate::value::BuiltinObject::new("convert", handler),
            ));
        }
    }
}

/// 登记核心类型全局（`TypeRef`）与原始类型方法表。
pub fn install(vm: &mut Vm) {
    install_primitive_type_globals(vm);
    install_primitive_methods(vm);
    install_primitive_convert_handlers(vm);
}

/// 全部内建类型元数据的统一启动入口（原始类型、异常、traceback、AST）。
pub fn install_core_types(vm: &mut Vm) {
    install(vm);
    crate::exceptions::install(vm);
    crate::traceback::install(vm);
    crate::runtime_ast::register_ast_struct_types(vm);
}

#[cfg(test)]
mod primitive_name_sync {
    #[test]
    fn sized_names_match_all_primitives_tail() {
        use super::names::ALL_PRIMITIVES;
        use crate::sized::SizedNum;
        for name in SizedNum::ALL_NAMES {
            assert!(
                ALL_PRIMITIVES.contains(name),
                "{name} missing from ALL_PRIMITIVES"
            );
        }
        assert!(ALL_PRIMITIVES.contains(&"ptr"));
    }
}
