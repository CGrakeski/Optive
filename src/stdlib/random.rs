use super::{builtin, expect_int, float_from_f64, materialize_iter, submodule};
use crate::shared::Shared;
use crate::value::{ModuleObject, Num, Value};
use crate::vm::Vm;
use crate::Result;

pub(super) fn build_random_module() -> Shared<ModuleObject> {
    submodule(
        "random",
        &[
            ("randint", builtin(random_randint)),
            ("random", builtin(random_random)),
            ("randstring", builtin(random_randstring)),
            ("choice", builtin(random_choice)),
            ("shuffle", builtin(random_shuffle)),
            ("sample", builtin(random_sample)),
            ("seed", builtin(random_seed)),
        ],
    )
}

thread_local! {
    static RNG_CELL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn rng_next() -> u64 {
    RNG_CELL.with(|cell| {
        let mut x = cell.get();
        if x == 0 {
            x = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64)
                | 1;
        }
        // xorshift64*
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        cell.set(x);
        x
    })
}

fn rng_bounded(span: u64) -> u64 {
    if span <= 1 {
        return 0;
    }
    // 拒绝采样，消除取模偏差
    let threshold = u64::MAX - (u64::MAX % span);
    loop {
        let r = rng_next();
        if r < threshold {
            return r % span;
        }
    }
}

fn random_seed(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let n = if args.is_empty() {
        1
    } else {
        expect_int("seed", args, 0)?
    };
    let state = if n == 0 { 1u64 } else { n as u64 };
    RNG_CELL.with(|c| c.set(state));
    Ok(Value::None)
}

fn random_randint(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "randint requires 2 arguments",
        ));
    }
    let lo = expect_int("randint", args, 0)?;
    let hi = expect_int("randint", args, 1)?;
    if hi < lo {
        return Err(crate::error::RuntimeError::value_err(
            "randint: hi must be >= lo",
        ));
    }
    let span = (hi as u64).wrapping_sub(lo as u64).wrapping_add(1);
    let offset = if span == 0 {
        rng_next()
    } else {
        rng_bounded(span)
    };
    let n = lo.wrapping_add(offset as i64);
    Ok(Value::Num(Num::from_bigint(n.into())))
}

fn random_random(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    // [0, 1) 用 53 位尾数，与常见 IEEE754 随机一致
    let bits = rng_next() >> 11;
    let f = (bits as f64) / ((1u64 << 53) as f64);
    Ok(Value::Num(float_from_f64(f)?))
}

fn random_randstring(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let len = if args.is_empty() {
        8
    } else {
        expect_int("randstring", args, 0)?.max(0) as usize
    };
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        let i = rng_bounded(CHARS.len() as u64) as usize;
        out.push(CHARS[i] as char);
    }
    Ok(Value::Text(out))
}

fn random_choice(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "choice requires 1 argument",
        ));
    }
    let items = materialize_iter(vm, &args[0])?;
    if items.is_empty() {
        return Err(crate::error::RuntimeError::msg("choice of empty sequence"));
    }
    let i = rng_bounded(items.len() as u64) as usize;
    Ok(items[i].clone())
}

fn random_shuffle(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "shuffle requires 1 argument",
        ));
    }
    let Value::List(list) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err(
            "shuffle requires list",
        ));
    };
    let mut items = list.borrow_mut();
    let n = items.len();
    if n > 1 {
        for i in (1..n).rev() {
            let j = rng_bounded((i + 1) as u64) as usize;
            items.swap(i, j);
        }
    }
    drop(items);
    Ok(args[0].clone())
}

fn random_sample(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "sample requires 2 arguments (pop, k)",
        ));
    }
    let mut items = materialize_iter(vm, &args[0])?;
    let k = expect_int("sample", args, 1)?.max(0) as usize;
    if k > items.len() {
        return Err(crate::error::RuntimeError::msg(
            "sample: k larger than population",
        ));
    }
    for i in 0..k {
        let j = i + (rng_next() as usize) % (items.len() - i);
        items.swap(i, j);
    }
    Ok(Value::List(Shared::new(
        items.into_iter().take(k).collect(),
    )))
}
