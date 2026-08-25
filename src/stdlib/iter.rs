//! `std.iter`：迭代器与 map/filter/zip。

use crate::value::{IteratorKind, IteratorState, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use crate::shared::Shared;

use super::{expect_arity, expect_function, expect_int};

pub(super) fn iter_iter(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("iter", args, 1)?;
    Ok(Value::Iterator(vm.to_iterator_shared(&args[0])?))
}

pub(super) fn iter_to_list(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "to_list requires 1 argument",
        ));
    }
    Ok(Value::List(Shared::new(materialize_iter(vm, &args[0])?)))
}

pub(super) fn iter_to_set(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "to_set requires 1 argument",
        ));
    }
    let mut set = crate::value::SetMap::new();
    for item in materialize_iter(vm, &args[0])? {
        set.insert(ValueKey::from_value(&item)?);
    }
    Ok(Value::Set(Shared::new(set)))
}

pub(super) fn iter_enumerate(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "enumerate requires 1 argument",
        ));
    }
    let source = value_to_iterator_rc(vm, &args[0])?;
    // 跟踪源游标；enumerate 包装自身也需可被 GC 看见。
    vm.gc.track_iter(&source);
    let it = Shared::new(IteratorState {
        kind: IteratorKind::Enumerate { index: 0, source },
    });
    vm.gc.track_iter(&it);
    Ok(Value::Iterator(it))
}

pub(super) fn iter_chain(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "chain requires at least 1 argument",
        ));
    }
    let mut sources = Vec::with_capacity(args.len());
    for arg in args {
        let src = value_to_iterator_rc(vm, arg)?;
        vm.gc.track_iter(&src);
        sources.push(src);
    }
    let it = Shared::new(IteratorState {
        kind: IteratorKind::Chain {
            sources,
            current: 0,
        },
    });
    vm.gc.track_iter(&it);
    Ok(Value::Iterator(it))
}

pub(super) fn iter_take(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "take requires 2 arguments",
        ));
    }
    let n = expect_int("take", args, 1)?.max(0) as usize;
    let source = value_to_iterator_rc(vm, &args[0])?;
    vm.gc.track_iter(&source);
    let it = Shared::new(IteratorState {
        kind: IteratorKind::Take {
            remaining: n,
            source,
        },
    });
    vm.gc.track_iter(&it);
    Ok(Value::Iterator(it))
}

pub(super) fn iter_skip(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "skip requires 2 arguments",
        ));
    }
    let n = expect_int("skip", args, 1)?.max(0) as usize;
    let source = value_to_iterator_rc(vm, &args[0])?;
    vm.gc.track_iter(&source);
    let it = Shared::new(IteratorState {
        kind: IteratorKind::Skip {
            remaining: n,
            source,
        },
    });
    vm.gc.track_iter(&it);
    Ok(Value::Iterator(it))
}

pub(super) fn iter_next(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "next requires an iterator",
        ));
    }
    let state = value_to_iterator_rc(vm, &args[0])?;
    match vm.advance_iterator(&state)? {
        Some(v) => Ok(v),
        None => {
            if args.len() >= 2 {
                Ok(args[1].clone())
            } else {
                let exc =
                    crate::exceptions::make_exception(vm, "StopIteration", "iterator exhausted")?;
                vm.throw_value(exc)?;
                Ok(Value::None)
            }
        }
    }
}

pub(super) fn iter_fold(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err(
            "fold requires 3 arguments (fn, init, iter)",
        ));
    }
    let func = expect_function("fold", args, 0)?;
    let mut acc = args[1].clone();
    let state = value_to_iterator_rc(vm, &args[2])?;
    while let Some(item) = vm.advance_iterator(&state)? {
        acc = vm.call_user_function(func.clone(), vec![acc, item])?;
    }
    Ok(acc)
}

pub(super) fn iter_repeat(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(crate::error::RuntimeError::type_err(
            "repeat requires 1 or 2 arguments (value[, n])",
        ));
    }
    let value = args[0].clone();
    let remaining = if args.len() == 2 {
        Some(expect_int("repeat", args, 1)?.max(0) as usize)
    } else {
        None
    };
    Ok(Value::Iterator(Shared::new(IteratorState {
        kind: IteratorKind::Repeat { value, remaining },
    })))
}

pub(super) fn iter_cycle(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "cycle requires 1 argument",
        ));
    }
    // cycle 需要可回放的有限序列，因此物化源一次。
    let items = materialize_iter(vm, &args[0])?;
    Ok(Value::Iterator(Shared::new(IteratorState {
        kind: IteratorKind::Cycle { items, index: 0 },
    })))
}

pub(super) fn iter_count(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "count requires 1 argument",
        ));
    }
    let state = value_to_iterator_rc(vm, &args[0])?;
    let mut n = 0i64;
    while vm.advance_iterator(&state)?.is_some() {
        n = n.saturating_add(1);
    }
    Ok(Value::Num(Num::Small(n)))
}

pub(super) fn iter_find(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "find requires 2 arguments (iterable, predicate)",
        ));
    }
    let pred = expect_function("find", args, 1)?;
    let state = value_to_iterator_rc(vm, &args[0])?;
    while let Some(item) = vm.advance_iterator(&state)? {
        if vm
            .call_user_function(pred.clone(), vec![item.clone()])?
            .is_truthy()
        {
            return Ok(item);
        }
    }
    Ok(Value::None)
}

pub(super) fn iter_any(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "any requires 2 arguments (iterable, predicate)",
        ));
    }
    let pred = expect_function("any", args, 1)?;
    let state = value_to_iterator_rc(vm, &args[0])?;
    while let Some(item) = vm.advance_iterator(&state)? {
        if vm.call_user_function(pred.clone(), vec![item])?.is_truthy() {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

pub(super) fn iter_all(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "all requires 2 arguments (iterable, predicate)",
        ));
    }
    let pred = expect_function("all", args, 1)?;
    let state = value_to_iterator_rc(vm, &args[0])?;
    while let Some(item) = vm.advance_iterator(&state)? {
        if !vm.call_user_function(pred.clone(), vec![item])?.is_truthy() {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

pub(super) fn materialize_iter(vm: &mut Vm, v: &Value) -> Result<Vec<Value>> {
    let state = vm.to_iterator_shared(v)?;
    let mut out = Vec::new();
    while let Some(item) = vm.advance_iterator(&state)? {
        out.push(item);
    }
    Ok(out)
}

pub(super) fn value_to_iterator_rc(vm: &mut Vm, v: &Value) -> Result<Shared<IteratorState>> {
    vm.to_iterator_shared(v)
}

pub(super) fn func_map(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "map requires 2 arguments",
        ));
    }
    let func = expect_function("map", args, 0)?;
    let source = value_to_iterator_rc(vm, &args[1])?;
    Ok(Value::Iterator(Shared::new(IteratorState {
        kind: IteratorKind::Map { func, source },
    })))
}

pub(super) fn func_filter(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "filter requires 2 arguments",
        ));
    }
    let pred = expect_function("filter", args, 0)?;
    let source = value_to_iterator_rc(vm, &args[1])?;
    Ok(Value::Iterator(Shared::new(IteratorState {
        kind: IteratorKind::Filter { func: pred, source },
    })))
}

pub(super) fn func_zip(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 {
        return Err(crate::error::RuntimeError::type_err(
            "zip requires at least 2 arguments",
        ));
    }
    vm.zip_iterables(args.to_vec())
}
