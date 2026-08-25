use num_bigint::BigInt;

use super::{builtin, expect_arity, expect_function, expect_int, materialize_iter, submodule};
use crate::shared::Shared;
use crate::value::{DictMap, IteratorState, ModuleObject, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

pub(super) fn build_collections_module() -> Shared<ModuleObject> {
    submodule(
        "collections",
        &[
            ("sorted", builtin(coll_sorted)),
            ("reversed", builtin(coll_reversed)),
            ("min", builtin(coll_min)),
            ("max", builtin(coll_max)),
            ("sum", builtin(coll_sum)),
            ("all", builtin(coll_all)),
            ("any", builtin(coll_any)),
            ("unique", builtin(coll_unique)),
            ("first", builtin(coll_first)),
            ("last", builtin(coll_last)),
            ("nth", builtin(coll_nth)),
            ("flatten", builtin(coll_flatten)),
            ("chunk", builtin(coll_chunk)),
            ("count", builtin(coll_count)),
            ("group_by", builtin(coll_group_by)),
        ],
    )
}

fn coll_sorted(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "sorted requires 1 argument",
        ));
    }
    let mut items = materialize_iter(vm, &args[0])?;
    items.sort_by_key(Value::print_string);
    Ok(Value::List(Shared::new(items)))
}

fn coll_reversed(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "reversed requires 1 argument",
        ));
    }
    let mut items = materialize_iter(vm, &args[0])?;
    items.reverse();
    Ok(IteratorState::from_list(items).into_value())
}

fn coll_min(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("min", args, 1)?;
    let items = materialize_iter(vm, &args[0])?;
    items
        .into_iter()
        .reduce(|a, b| {
            if a.print_string() <= b.print_string() {
                a
            } else {
                b
            }
        })
        .ok_or_else(|| crate::error::RuntimeError::msg("min of empty"))
}

fn coll_max(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("max", args, 1)?;
    let items = materialize_iter(vm, &args[0])?;
    items
        .into_iter()
        .reduce(|a, b| {
            if a.print_string() >= b.print_string() {
                a
            } else {
                b
            }
        })
        .ok_or_else(|| crate::error::RuntimeError::msg("max of empty"))
}

fn coll_sum(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("sum", args, 1)?;
    let items = materialize_iter(vm, &args[0])?;
    let mut total = BigInt::from(0);
    for item in items {
        if let Value::Num(n) = item {
            match n {
                Num::Small(x) => total += BigInt::from(x),
                Num::Int(x) => total += x.as_ref(),
                Num::Rat(r) if *r.denom() == BigInt::from(1) => total += r.numer(),
                Num::Rat(_) => {
                    return Err(crate::error::RuntimeError::type_err(
                        "sum requires integer values",
                    ));
                }
            }
        }
    }
    Ok(Value::Num(Num::from_bigint(total)))
}

fn coll_all(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("all", args, 1)?;
    let items = materialize_iter(vm, &args[0])?;
    Ok(Value::Bool(items.iter().all(Value::is_truthy)))
}

fn coll_any(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("any", args, 1)?;
    let items = materialize_iter(vm, &args[0])?;
    Ok(Value::Bool(items.iter().any(Value::is_truthy)))
}

fn coll_unique(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("unique", args, 1)?;
    let items = materialize_iter(vm, &args[0])?;
    let mut seen = crate::value::SetMap::new();
    let mut out = Vec::new();
    for item in items {
        let key = ValueKey::from_value(&item)?;
        if seen.insert(key) {
            out.push(item);
        }
    }
    Ok(Value::List(Shared::new(out)))
}

fn coll_first(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("first", args, 1)?;
    let items = materialize_iter(vm, &args[0])?;
    items
        .into_iter()
        .next()
        .ok_or_else(|| crate::error::RuntimeError::msg("first of empty"))
}

fn coll_last(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("last", args, 1)?;
    let items = materialize_iter(vm, &args[0])?;
    items
        .into_iter()
        .last()
        .ok_or_else(|| crate::error::RuntimeError::msg("last of empty"))
}

fn coll_nth(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "nth requires 2 arguments",
        ));
    }
    let n = expect_int("nth", args, 1)? as usize;
    let items = materialize_iter(vm, &args[0])?;
    if let Some(v) = items.into_iter().nth(n) {
        return Ok(v);
    }
    let exc =
        crate::exceptions::make_exception(vm, "IndexError", format!("nth out of range: {n}"))?;
    match vm.throw_value(exc) {
        Ok(()) => Ok(Value::None),
        Err(e) => Err(e),
    }
}

fn coll_flatten(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "flatten requires 1 argument",
        ));
    }
    let mut out = Vec::new();
    for item in materialize_iter(vm, &args[0])? {
        match &item {
            Value::List(_) | Value::Tuple(_) | Value::Set(_) | Value::Iterator(_) => {
                out.extend(materialize_iter(vm, &item)?);
            }
            other => out.push(other.clone()),
        }
    }
    Ok(Value::List(Shared::new(out)))
}

fn coll_chunk(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "chunk requires 2 arguments",
        ));
    }
    let size = expect_int("chunk", args, 1)?;
    if size <= 0 {
        return Err(crate::error::RuntimeError::type_err(
            "chunk size must be positive",
        ));
    }
    let size = size as usize;
    let items = materialize_iter(vm, &args[0])?;
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for item in items {
        cur.push(item);
        if cur.len() == size {
            out.push(Value::List(Shared::new(std::mem::take(&mut cur))));
        }
    }
    if !cur.is_empty() {
        out.push(Value::List(Shared::new(cur)));
    }
    Ok(Value::List(Shared::new(out)))
}

fn coll_count(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() == 1 {
        return Ok(Value::Num(Num::Small(
            materialize_iter(vm, &args[0])?.len() as i64,
        )));
    }
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "count requires 1 or 2 arguments",
        ));
    }
    let needle = &args[1];
    let mut n = 0i64;
    for item in materialize_iter(vm, &args[0])? {
        if item.print_string() == needle.print_string() {
            n += 1;
        }
    }
    Ok(Value::Num(Num::Small(n)))
}

fn coll_group_by(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "group_by requires 2 arguments (xs, key_fn)",
        ));
    }
    let key_fn = expect_function("group_by", args, 1)?;
    let mut out = DictMap::new();
    for item in materialize_iter(vm, &args[0])? {
        let key_v = vm.call_user_function(key_fn.clone(), vec![item.clone()])?;
        let key = ValueKey::from_value(&key_v)?;
        if let Some(Value::List(list)) = out.get(&key).cloned() {
            list.borrow_mut().push(item);
        } else {
            out.insert(key, Value::List(Shared::new(vec![item])));
        }
    }
    Ok(Value::Dict(Shared::new(out)))
}
