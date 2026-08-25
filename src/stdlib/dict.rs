//! `std.dict`：字典操作。

use crate::value::{DictMap, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use crate::shared::Shared;

use super::{expect_arity, expect_dict, value_key_to_value, value_to_list};

pub(super) fn dict_keys(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("keys", args, 1)?;
    let d = expect_dict("keys", args, 0)?;
    let keys: Vec<Value> = d.borrow().keys().map(value_key_to_value).collect();
    Ok(Value::List(Shared::new(keys)))
}

pub(super) fn dict_values(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("values", args, 1)?;
    let d = expect_dict("values", args, 0)?;
    let values: Vec<Value> = d.borrow().values().cloned().collect();
    Ok(Value::List(Shared::new(values)))
}

pub(super) fn dict_items(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("items", args, 1)?;
    let d = expect_dict("items", args, 0)?;
    let pairs: Vec<Value> = d
        .borrow()
        .iter()
        .map(|(k, v)| Value::List(Shared::new(vec![value_key_to_value(k), v.clone()])))
        .collect();
    Ok(Value::List(Shared::new(pairs)))
}

pub(super) fn dict_get(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 || args.len() > 3 {
        return Err(crate::error::RuntimeError::type_err(
            "get requires 2 or 3 arguments",
        ));
    }
    let d = expect_dict("get", args, 0)?;
    let key = ValueKey::from_value(&args[1])?;
    if let Some(v) = d.borrow().get(&key) {
        return Ok(v.clone());
    }
    if args.len() == 3 {
        return Ok(args[2].clone());
    }
    let msg = format!("Key not found: {}", args[1].print_string());
    let exc = crate::exceptions::make_exception(vm, "KeyError", msg)?;
    match vm.throw_value(exc) {
        Ok(()) => Ok(Value::None),
        Err(e) => Err(e),
    }
}

pub(super) fn dict_from_items(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("from_items", args, 1)?;
    let mut map = DictMap::new();
    for item in value_to_list(&args[0])? {
        let pair = value_to_list(&item)?;
        if pair.len() != 2 {
            return Err(crate::error::RuntimeError::type_err(
                "from_items: each item must be [key, value]",
            ));
        }
        map.insert(ValueKey::from_value(&pair[0])?, pair[1].clone());
    }
    Ok(Value::Dict(Shared::new(map)))
}

pub(super) fn dict_update(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("update", args, 2)?;
    let dst = expect_dict("update", args, 0)?;
    let src = expect_dict("update", args, 1)?;
    let entries: Vec<_> = src
        .borrow()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    {
        let mut dst = dst.borrow_mut();
        for (k, v) in entries {
            dst.insert(k, v);
        }
    }
    Ok(Value::Dict(dst.clone()))
}

pub(super) fn dict_merge(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Ok(Value::Dict(Shared::new(DictMap::new())));
    }
    let mut out = DictMap::new();
    for (i, _) in args.iter().enumerate() {
        let d = expect_dict("merge", args, i)?;
        for (k, v) in d.borrow().iter() {
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Dict(Shared::new(out)))
}

pub(super) fn dict_invert(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("invert", args, 1)?;
    let d = expect_dict("invert", args, 0)?;
    let mut out = DictMap::new();
    for (k, v) in d.borrow().iter() {
        let key = ValueKey::from_value(v)?;
        out.insert(key, value_key_to_value(k));
    }
    Ok(Value::Dict(Shared::new(out)))
}

pub(super) fn dict_setdefault(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err(
            "setdefault requires 3 arguments (dict, key, default)",
        ));
    }
    let d = expect_dict("setdefault", args, 0)?;
    let key = ValueKey::from_value(&args[1])?;
    let mut map = d.borrow_mut();
    if let Some(v) = map.get(&key) {
        return Ok(v.clone());
    }
    map.insert(key, args[2].clone());
    Ok(args[2].clone())
}
