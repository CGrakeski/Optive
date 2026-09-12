//! Registry of compile-time-pure builtins.
//!
//! Effect decisions live here so `const_eval` does not hard-code function names.

use std::sync::Arc;

use crate::error::RuntimeError;
use crate::value::{FrozenValue, Num, Value, ValueKey};
use crate::Result;

pub fn is_pure_builtin(name: &str) -> bool {
    lookup(name).is_some()
}

pub fn eval_pure_builtin(name: &str, args: &[Value]) -> Result<Value> {
    let builtin = lookup(name)
        .ok_or_else(|| RuntimeError::msg(format!("`{name}` is not a compile-time-pure builtin")))?;
    (builtin.eval)(args)
}

struct PureBuiltin {
    name: &'static str,
    eval: fn(&[Value]) -> Result<Value>,
}

fn lookup(name: &str) -> Option<&'static PureBuiltin> {
    PURE_BUILTINS.iter().find(|builtin| builtin.name == name)
}

static PURE_BUILTINS: &[PureBuiltin] = &[
    PureBuiltin {
        name: "len",
        eval: eval_len,
    },
    PureBuiltin {
        name: "min",
        eval: eval_min,
    },
    PureBuiltin {
        name: "max",
        eval: eval_max,
    },
    PureBuiltin {
        name: "abs",
        eval: eval_abs,
    },
    PureBuiltin {
        name: "range",
        eval: eval_range,
    },
    PureBuiltin {
        name: "str",
        eval: eval_str,
    },
    PureBuiltin {
        name: "int",
        eval: eval_int,
    },
    PureBuiltin {
        name: "tuple",
        eval: eval_tuple,
    },
];

fn one_arg<'a>(name: &str, args: &'a [Value]) -> Result<&'a Value> {
    if args.len() != 1 {
        return Err(RuntimeError::msg(format!(
            "compile-time {name} expects one argument"
        )));
    }
    Ok(&args[0])
}

fn eval_len(args: &[Value]) -> Result<Value> {
    let value = one_arg("len", args)?;
    let len = match value {
        Value::Tuple(values) => values.len(),
        Value::Text(text) => text.chars().count(),
        Value::Bytes(bytes) => bytes.len(),
        Value::Frozen(frozen) => match frozen.as_ref() {
            FrozenValue::List(values) => values.len(),
            FrozenValue::Dict(values) => values.len(),
            FrozenValue::Set(values) => values.len(),
        },
        _ => {
            return Err(RuntimeError::msg(
                "compile-time len requires a sequence or collection",
            ))
        }
    };
    Ok(Value::Num(Num::from_i64(len as i64)))
}

fn eval_min(args: &[Value]) -> Result<Value> {
    numeric_extremum("min", args, true)
}

fn eval_max(args: &[Value]) -> Result<Value> {
    numeric_extremum("max", args, false)
}

fn numeric_extremum(name: &str, args: &[Value], want_min: bool) -> Result<Value> {
    if args.is_empty() {
        return Err(RuntimeError::msg(format!(
            "compile-time {name} requires at least one argument"
        )));
    }
    let mut best = match &args[0] {
        Value::Num(num) => num.clone(),
        _ => {
            return Err(RuntimeError::msg(format!(
                "compile-time {name} requires numbers"
            )))
        }
    };
    for arg in args.iter().skip(1) {
        let Value::Num(num) = arg else {
            return Err(RuntimeError::msg(format!(
                "compile-time {name} requires numbers"
            )));
        };
        let ord = num.to_rational().cmp(&best.to_rational());
        if (want_min && ord.is_lt()) || (!want_min && ord.is_gt()) {
            best = num.clone();
        }
    }
    Ok(Value::Num(best))
}

fn eval_abs(args: &[Value]) -> Result<Value> {
    match one_arg("abs", args)? {
        Value::Num(num) => Ok(Value::Num(num.abs_num())),
        _ => Err(RuntimeError::msg("compile-time abs requires a number")),
    }
}

fn eval_range(args: &[Value]) -> Result<Value> {
    let (start, end, step) = match args {
        [end] => (0, int_arg("range", end)?, 1),
        [start, end] => (int_arg("range", start)?, int_arg("range", end)?, 1),
        [start, end, step] => (
            int_arg("range", start)?,
            int_arg("range", end)?,
            int_arg("range", step)?,
        ),
        _ => {
            return Err(RuntimeError::msg(
                "compile-time range expects 1, 2, or 3 arguments",
            ))
        }
    };
    if step == 0 {
        return Err(RuntimeError::msg("compile-time range step cannot be 0"));
    }
    let mut values = Vec::new();
    let mut current = start;
    while if step > 0 {
        current < end
    } else {
        current > end
    } {
        values.push(Value::Num(Num::from_i64(current)));
        current = current
            .checked_add(step)
            .ok_or_else(|| RuntimeError::msg("compile-time range overflow"))?;
    }
    Ok(Value::Frozen(Arc::new(FrozenValue::List(values.into()))))
}

fn eval_str(args: &[Value]) -> Result<Value> {
    Ok(Value::Text(one_arg("str", args)?.print_string()))
}

fn eval_int(args: &[Value]) -> Result<Value> {
    match one_arg("int", args)? {
        Value::Num(num) => num
            .to_i64()
            .map(|value| Value::Num(Num::from_i64(value)))
            .ok_or_else(|| RuntimeError::msg("compile-time int requires a finite integer")),
        Value::Bool(value) => Ok(Value::Num(Num::from_i64(i64::from(*value)))),
        Value::Text(text) => Ok(Value::Num(Num::from_literal(text.trim())?)),
        _ => Err(RuntimeError::msg(
            "compile-time int requires a number, bool, or text",
        )),
    }
}

fn eval_tuple(args: &[Value]) -> Result<Value> {
    Ok(Value::Tuple(Arc::from(args.to_vec().into_boxed_slice())))
}

fn int_arg(name: &str, value: &Value) -> Result<i64> {
    match value {
        Value::Num(num) => num
            .to_i64()
            .ok_or_else(|| RuntimeError::msg(format!("compile-time {name} requires integers"))),
        _ => Err(RuntimeError::msg(format!(
            "compile-time {name} requires integers"
        ))),
    }
}

pub fn value_in(needle: &Value, haystack: &Value) -> Result<bool> {
    Ok(match haystack {
        Value::Text(text) => match needle {
            Value::Text(part) => text.contains(part),
            _ => false,
        },
        Value::Tuple(values) => values
            .iter()
            .try_fold(false, |found, item| Ok(found || item.eq(needle)?))?,
        Value::Bytes(bytes) => match needle {
            Value::Num(num) => num
                .to_i64()
                .and_then(|value| u8::try_from(value).ok())
                .is_some_and(|byte| bytes.contains(&byte)),
            _ => false,
        },
        Value::Frozen(frozen) => match frozen.as_ref() {
            FrozenValue::List(values) => values
                .iter()
                .try_fold(false, |found, item| Ok(found || item.eq(needle)?))?,
            FrozenValue::Set(values) => values.contains(&ValueKey::from_value(needle)?),
            FrozenValue::Dict(values) => {
                let key = ValueKey::from_value(needle)?;
                values.iter().any(|(candidate, _)| candidate == &key)
            }
        },
        _ => {
            return Err(RuntimeError::msg(
                "compile-time `in` requires text, bytes, tuple, or a frozen collection",
            ))
        }
    })
}
