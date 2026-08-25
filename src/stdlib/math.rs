//! `std.math`：常数与超越 / 有理函数。

use num_bigint::BigInt;
use num_traits::Zero;

use crate::shared::Shared;
use crate::value::{IteratorState, Num, Value};
use crate::vm::Vm;
use crate::Result;

use super::{expect_arity, expect_int, expect_num_f64, expect_num_value, float_from_f64};

pub(super) fn math_const_pi() -> Value {
    Value::Num(Num::from_rational(num_rational::BigRational::new(
        BigInt::parse_bytes(b"314159265358979323846", 10).expect("pi numerator digits"),
        BigInt::parse_bytes(b"100000000000000000000", 10).expect("pi denominator digits"),
    )))
}

pub(super) fn math_const_e() -> Value {
    Value::Num(Num::from_rational(num_rational::BigRational::new(
        BigInt::parse_bytes(b"271828182845904523536", 10).expect("e numerator digits"),
        BigInt::parse_bytes(b"100000000000000000000", 10).expect("e denominator digits"),
    )))
}

/// 超越函数经 IEEE754；精确有理运算走 Num 路径。
macro_rules! math_f1 {
    ($fn_name:ident, $api:literal, $op:expr) => {
        pub(super) fn $fn_name(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
            expect_arity($api, args, 1)?;
            let x = expect_num_f64($api, args, 0)?;
            Ok(Value::Num(float_from_f64(($op)(x))?))
        }
    };
}

math_f1!(math_sin, "sin", f64::sin);
math_f1!(math_cos, "cos", f64::cos);
math_f1!(math_tan, "tan", f64::tan);
math_f1!(math_exp, "exp", f64::exp);
math_f1!(math_degrees, "degrees", f64::to_degrees);
math_f1!(math_radians, "radians", f64::to_radians);
math_f1!(math_asin, "asin", f64::asin);
math_f1!(math_acos, "acos", f64::acos);
math_f1!(math_atan, "atan", f64::atan);
math_f1!(math_sinh, "sinh", f64::sinh);
math_f1!(math_cosh, "cosh", f64::cosh);
math_f1!(math_tanh, "tanh", f64::tanh);
math_f1!(math_log2, "log2", f64::log2);
math_f1!(math_cbrt, "cbrt", f64::cbrt);

pub(super) fn math_atan2(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("atan2", args, 2)?;
    let y = expect_num_f64("atan2", args, 0)?;
    let x = expect_num_f64("atan2", args, 1)?;
    Ok(Value::Num(float_from_f64(y.atan2(x))?))
}

pub(super) fn math_hypot(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("hypot", args, 2)?;
    let a = expect_num_f64("hypot", args, 0)?;
    let b = expect_num_f64("hypot", args, 1)?;
    Ok(Value::Num(float_from_f64(a.hypot(b))?))
}

/// `divmod(a, b)` → `[a / b, a % b]`（整数商与余数，遵循有理数取模同号语义）。
pub(super) fn math_divmod(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("divmod", args, 2)?;
    let (Value::Num(a), Value::Num(b)) = (&args[0], &args[1]) else {
        return Err(crate::error::RuntimeError::type_err("divmod requires nums"));
    };
    if b.is_zero() {
        return Err(crate::error::RuntimeError::value_err("divmod by zero"));
    }
    let q = Value::Num(a.clone()).div(&args[1])?;
    let r = Value::Num(a.clone()).rem(&args[1])?;
    Ok(Value::List(Shared::new(vec![q, r])))
}

/// Optive 的 `Num` 基于有理数，无法精确表达 IEEE 754 infinity。
/// 此处返回 `i64::MAX` 作为最佳近似；与 -inf/inf 比较时等于自身。
pub(super) fn math_const_inf() -> Value {
    Value::Num(Num::from_bigint(BigInt::from(i64::MAX)))
}

/// 同 `math_const_inf`，返回 `i64::MIN` 作为负无穷的最佳近似。
pub(super) fn math_const_neg_inf() -> Value {
    Value::Num(Num::from_bigint(BigInt::from(i64::MIN)))
}

/// Optive 的 `Num` 无法表示 IEEE 754 NaN。返回 0 作为占位；
/// 用户代码不应将 `std.math.nan` 用于 NaN 检测。
pub(super) const fn math_const_nan() -> Value {
    Value::Num(Num::Small(0))
}

pub(super) fn math_const_tau() -> Value {
    Value::Num(Num::from_rational(num_rational::BigRational::new(
        BigInt::parse_bytes(b"62831853071795864769", 10).expect("tau numerator digits"),
        BigInt::parse_bytes(b"10000000000000000000", 10).expect("tau denominator digits"),
    )))
}

pub(super) fn math_sqrt(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("sqrt", args, 1)?;
    let x = expect_num_f64("sqrt", args, 0)?;
    if x < 0.0 {
        return Err(crate::error::RuntimeError::value_err(
            "sqrt of negative number",
        ));
    }
    Ok(Value::Num(float_from_f64(x.sqrt())?))
}

macro_rules! define_math_num_unary {
    ($(($fn_name:ident, $api:literal, $method:ident)),+ $(,)?) => {
        $(
            pub(super) fn $fn_name(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
                expect_arity($api, args, 1)?;
                Ok(Value::Num(expect_num_value($api, args, 0)?.$method()))
            }
        )+
    };
}

define_math_num_unary! {
    (math_abs, "abs", abs_num),
    (math_floor, "floor", floor_num),
    (math_ceil, "ceil", ceil_num),
    (math_round, "round", round_num),
    (math_trunc, "trunc", trunc_num),
}

pub(super) fn math_pow(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("pow", args, 2)?;
    args[0].pow(&args[1])
}

pub(super) fn math_log(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let x = expect_num_f64("log", args, 0)?;
    if x <= 0.0 {
        return Err(crate::error::RuntimeError::value_err(
            "log requires positive number",
        ));
    }
    if args.len() >= 2 {
        let base = expect_num_f64("log", args, 1)?;
        Ok(Value::Num(float_from_f64(x.log(base))?))
    } else {
        Ok(Value::Num(float_from_f64(x.ln())?))
    }
}

pub(super) fn math_log10(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("log10", args, 1)?;
    let x = expect_num_f64("log10", args, 0)?;
    if x <= 0.0 {
        return Err(crate::error::RuntimeError::value_err(
            "log10 requires positive number",
        ));
    }
    Ok(Value::Num(float_from_f64(x.log10())?))
}

pub(super) fn math_min(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "min requires at least 1 argument",
        ));
    }
    let mut best = match &args[0] {
        Value::Num(n) => n.clone(),
        _ => return Err(crate::error::RuntimeError::type_err("min requires nums")),
    };
    for arg in &args[1..] {
        match arg {
            Value::Num(n) if n.cmp_num(&best) == std::cmp::Ordering::Less => best = n.clone(),
            Value::Num(_) => {}
            _ => return Err(crate::error::RuntimeError::type_err("min requires nums")),
        }
    }
    Ok(Value::Num(best))
}

pub(super) fn math_max(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "max requires at least 1 argument",
        ));
    }
    let mut best = match &args[0] {
        Value::Num(n) => n.clone(),
        _ => return Err(crate::error::RuntimeError::type_err("max requires nums")),
    };
    for arg in &args[1..] {
        match arg {
            Value::Num(n) if n.cmp_num(&best) == std::cmp::Ordering::Greater => best = n.clone(),
            Value::Num(_) => {}
            _ => return Err(crate::error::RuntimeError::type_err("max requires nums")),
        }
    }
    Ok(Value::Num(best))
}

pub(super) fn math_clamp(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err(
            "clamp requires 3 arguments (x, lo, hi)",
        ));
    }
    match (&args[0], &args[1], &args[2]) {
        (Value::Num(x), Value::Num(lo), Value::Num(hi)) => {
            let v = if x.cmp_num(lo) == std::cmp::Ordering::Less {
                lo.clone()
            } else if x.cmp_num(hi) == std::cmp::Ordering::Greater {
                hi.clone()
            } else {
                x.clone()
            };
            Ok(Value::Num(v))
        }
        _ => Err(crate::error::RuntimeError::type_err("clamp requires nums")),
    }
}

pub(super) fn num_as_bigint(n: &Num) -> Result<BigInt> {
    match n {
        Num::Small(i) => Ok(BigInt::from(*i)),
        Num::Int(i) => Ok(i.as_ref().clone()),
        Num::Rat(r) if r.denom() == &num_traits::One::one() => Ok(r.numer().clone()),
        _ => Err(crate::error::RuntimeError::msg("expected integer num")),
    }
}

pub(super) fn bigint_gcd(mut a: BigInt, mut b: BigInt) -> BigInt {
    use num_traits::{Signed, Zero};
    a = a.abs();
    b = b.abs();
    while !b.is_zero() {
        let t = b.clone();
        b = &a % &b;
        a = t;
    }
    a
}

pub(super) fn math_gcd(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "gcd requires 2 arguments",
        ));
    }
    let (Value::Num(a), Value::Num(b)) = (&args[0], &args[1]) else {
        return Err(crate::error::RuntimeError::type_err("gcd requires nums"));
    };
    Ok(Value::Num(Num::from_bigint(bigint_gcd(
        num_as_bigint(a)?,
        num_as_bigint(b)?,
    ))))
}

pub(super) fn math_lcm(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    use num_traits::{Signed, Zero};
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "lcm requires 2 arguments",
        ));
    }
    let (Value::Num(a), Value::Num(b)) = (&args[0], &args[1]) else {
        return Err(crate::error::RuntimeError::type_err("lcm requires nums"));
    };
    let aa = num_as_bigint(a)?;
    let bb = num_as_bigint(b)?;
    if aa.is_zero() || bb.is_zero() {
        return Ok(Value::Num(Num::Small(0)));
    }
    let g = bigint_gcd(aa.clone(), bb.clone());
    let l = (aa.abs() / g) * bb.abs();
    Ok(Value::Num(Num::from_bigint(l)))
}

pub(super) fn math_sign(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    use num_traits::{Signed, Zero};
    match args.first() {
        Some(Value::Num(n)) => {
            let z = n.to_rational();
            let s = if z.is_zero() {
                0
            } else if z.is_positive() {
                1
            } else {
                -1
            };
            Ok(Value::Num(Num::Small(s)))
        }
        _ => Err(crate::error::RuntimeError::type_err("sign requires 1 num")),
    }
}

pub(super) fn math_mod(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "mod requires 2 arguments",
        ));
    }
    match (&args[0], &args[1]) {
        (Value::Num(a), Value::Num(b)) => {
            let aa = a.to_rational();
            let bb = b.to_rational();
            if bb.is_zero() {
                return Err(crate::error::RuntimeError::zero_div("mod by zero"));
            }
            Ok(Value::Num(Num::from_rational(&aa % &bb)))
        }
        _ => Err(crate::error::RuntimeError::type_err("mod requires nums")),
    }
}

pub(super) fn math_is_integer(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    match args.first() {
        Some(Value::Num(n)) => {
            let ok = match n {
                Num::Small(_) | Num::Int(_) => true,
                Num::Rat(r) => r.denom() == &num_traits::One::one(),
            };
            Ok(Value::Bool(ok))
        }
        _ => Err(crate::error::RuntimeError::type_err(
            "is_integer requires 1 num",
        )),
    }
}

pub(super) fn math_is_rational(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    match args.first() {
        Some(Value::Num(n)) => Ok(Value::Bool(matches!(n, Num::Rat(_)))),
        _ => Err(crate::error::RuntimeError::type_err(
            "is_rational requires 1 num",
        )),
    }
}

pub(super) fn math_range(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let (start, stop, stride) = match args.len() {
        1 => {
            let stop = expect_int("range", args, 0)?;
            (0, stop, 1)
        }
        2 => {
            let start = expect_int("range", args, 0)?;
            let stop = expect_int("range", args, 1)?;
            (start, stop, 1)
        }
        3 => {
            let start = expect_int("range", args, 0)?;
            let stop = expect_int("range", args, 1)?;
            let stride = expect_int("range", args, 2)?;
            if stride == 0 {
                return Err(crate::error::RuntimeError::value_err(
                    "range step must not be zero",
                ));
            }
            (start, stop, stride)
        }
        n => {
            return Err(crate::error::RuntimeError::type_err(format!(
                "range requires 1 to 3 arguments, got {n}"
            )))
        }
    };
    Ok(IteratorState::from_range(start, stop, stride).into_value())
}
