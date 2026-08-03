use std::collections::HashMap;
use std::sync::Arc;

use num_bigint::BigInt;
use num_traits::Zero;

use crate::runtime_ast;
use crate::value::{IteratorKind, IteratorState, ModuleObject, Num, Value, ValueKey, DictMap};
use crate::vm::Vm;
use crate::Result;

use crate::shared::{Shared, SyncCell};
/// `format_num` / `format` 字段的默认小数精度。
const DEFAULT_NUM_PRECISION: usize = 6;
/// `take` / `skip` 等迭代器物化的预分配初始容量。
const ITER_MATERIALIZE_INIT_CAP: usize = 64;

pub fn build_std_module() -> Shared<ModuleObject> {
    let math = submodule(
        "math",
        &[
            ("sin", builtin(math_sin)),
            ("cos", builtin(math_cos)),
            ("tan", builtin(math_tan)),
            ("asin", builtin(math_asin)),
            ("acos", builtin(math_acos)),
            ("atan", builtin(math_atan)),
            ("sinh", builtin(math_sinh)),
            ("cosh", builtin(math_cosh)),
            ("tanh", builtin(math_tanh)),
            ("sqrt", builtin(math_sqrt)),
            ("cbrt", builtin(math_cbrt)),
            ("abs", builtin(math_abs)),
            ("floor", builtin(math_floor)),
            ("ceil", builtin(math_ceil)),
            ("round", builtin(math_round)),
            ("trunc", builtin(math_trunc)),
            ("pow", builtin(math_pow)),
            ("log", builtin(math_log)),
            ("log2", builtin(math_log2)),
            ("log10", builtin(math_log10)),
            ("exp", builtin(math_exp)),
            ("min", builtin(math_min)),
            ("max", builtin(math_max)),
            ("clamp", builtin(math_clamp)),
            ("gcd", builtin(math_gcd)),
            ("lcm", builtin(math_lcm)),
            ("sign", builtin(math_sign)),
            ("mod", builtin(math_mod)),
            ("degrees", builtin(math_degrees)),
            ("radians", builtin(math_radians)),
            ("is_integer", builtin(math_is_integer)),
            ("is_rational", builtin(math_is_rational)),
            ("range", builtin(math_range)),
            ("atan2", builtin(math_atan2)),
            ("hypot", builtin(math_hypot)),
            ("divmod", builtin(math_divmod)),
            ("pi", math_const_pi()),
            ("e", math_const_e()),
            ("tau", math_const_tau()),
            ("inf", math_const_inf()),
            ("-inf", math_const_neg_inf()),
            ("nan", math_const_nan()),
        ],
    );

    let io = submodule(
        "io",
        &[
            ("read_file", builtin(io_read_file)),
            ("write_file", builtin(io_write_file)),
            ("append_file", builtin(io_append_file)),
            ("read_bytes", builtin(io_read_bytes)),
            ("write_bytes", builtin(io_write_bytes)),
            ("write_line", builtin(io_write_line)),
            ("eprint", builtin(io_eprint)),
            ("read_line", builtin(io_read_line)),
            ("exists", builtin(fs_exists)),
            ("remove", builtin(fs_remove)),
        ],
    );

    let format = submodule(
        "format",
        &[
            ("format", builtin(format_format)),
            ("join", builtin(format_join)),
            ("format_num", builtin(format_format_num)),
            ("pad", builtin(format_pad)),
            ("indent", builtin(format_indent)),
        ],
    );

    let iter = submodule(
        "iter",
        &[
            ("iter", builtin(iter_iter)),
            ("next", builtin(iter_next)),
            ("to_list", builtin(iter_to_list)),
            ("to_set", builtin(iter_to_set)),
            ("enumerate", builtin(iter_enumerate)),
            ("chain", builtin(iter_chain)),
            ("map", builtin(func_map)),
            ("filter", builtin(func_filter)),
            ("zip", builtin(func_zip)),
            ("take", builtin(iter_take)),
            ("skip", builtin(iter_skip)),
            ("drop", builtin(iter_skip)),
            ("fold", builtin(iter_fold)),
            ("count", builtin(iter_count)),
            ("find", builtin(iter_find)),
            ("any", builtin(iter_any)),
            ("all", builtin(iter_all)),
            ("repeat", builtin(iter_repeat)),
            ("cycle", builtin(iter_cycle)),
        ],
    );

    let dict = submodule(
        "dict",
        &[
            ("keys", builtin(dict_keys)),
            ("values", builtin(dict_values)),
            ("items", builtin(dict_items)),
            ("get", builtin(dict_get)),
            ("from_items", builtin(dict_from_items)),
            ("from_list", builtin(dict_from_items)),
            ("update", builtin(dict_update)),
            ("merge", builtin(dict_merge)),
            ("invert", builtin(dict_invert)),
            ("setdefault", builtin(dict_setdefault)),
        ],
    );

    let ast = submodule(
        "ast",
        &[
            ("parse", builtin(ast_parse)),
            ("ast_clone", builtin(ast_clone_export)),
            ("ast_type_convert", builtin(ast_type_convert_export)),
            ("ast_call", builtin(ast_call_export)),
            ("ast_macro_call", builtin(ast_macro_call_export)),
            ("ast_vec_push", builtin(ast_vec_push_export)),
            ("ast_vec_extend", builtin(ast_vec_extend_export)),
            ("unparse", builtin(ast_unparse)),
            ("walk", builtin(ast_walk)),
        ],
    );

    let decos = submodule(
        "decos",
        &[
            ("log", builtin(decos_log)),
            ("once", builtin(decos_once)),
            ("memoize", builtin(decos_memoize)),
            ("timer", builtin(decos_timer)),
            ("debug", builtin(decos_debug)),
            ("retry", builtin(decos_retry)),
            ("validate", builtin(decos_validate)),
            ("catch", builtin(decos_catch)),
            ("deprecated", builtin(decos_deprecated)),
            ("trace", builtin(decos_trace)),
            ("singleton", builtin(decos_singleton)),
        ],
    );

    let mut std_children = HashMap::new();
    std_children.insert("math".into(), math);
    std_children.insert("io".into(), io);
    std_children.insert("format".into(), format);
    std_children.insert("iter".into(), iter);
    std_children.insert("dict".into(), dict);
    std_children.insert("ast".into(), ast);
    std_children.insert("decos".into(), decos);
    std_children.insert("typing".into(), build_typing_module());
    std_children.insert("functional".into(), build_functional_module());
    std_children.insert("collections".into(), build_collections_module());
    std_children.insert("time".into(), build_time_module());
    std_children.insert("sync".into(), build_sync_module());
    std_children.insert("text".into(), build_text_module());
    std_children.insert("path".into(), build_path_module());
    std_children.insert("fs".into(), build_fs_module());
    std_children.insert("os".into(), build_os_module());
    std_children.insert("json".into(), build_json_module());
    std_children.insert("test".into(), build_test_module());
    std_children.insert("debug".into(), build_debug_module());
    std_children.insert("random".into(), build_random_module());
    std_children.insert("re".into(), build_re_module());
    std_children.insert("hash".into(), build_hash_module());
    std_children.insert("exceptions".into(), build_exceptions_module());
    std_children.insert("language".into(), crate::ffi::build_language_module());
    std_children.insert("http".into(), build_http_module());
    std_children.insert("encoding".into(), build_encoding_module());
    std_children.insert("csv".into(), build_csv_module());
    std_children.insert("toml".into(), build_toml_module());
    std_children.insert("yaml".into(), build_yaml_module());
    std_children.insert("xml".into(), build_xml_module());

    Shared::new(ModuleObject {
        name: "std".into(),
        full_name: "std".into(),
        exports: exports(&[("concat", builtin(std_concat))]),
        children: std_children,
        is_user: false,
    })
}

fn math_const_pi() -> Value {
    Value::Num(Num::from_rational(num_rational::BigRational::new(
        BigInt::parse_bytes(b"314159265358979323846", 10).expect("pi numerator digits"),
        BigInt::parse_bytes(b"100000000000000000000", 10).expect("pi denominator digits"),
    )))
}

fn math_const_e() -> Value {
    Value::Num(Num::from_rational(num_rational::BigRational::new(
        BigInt::parse_bytes(b"271828182845904523536", 10).expect("e numerator digits"),
        BigInt::parse_bytes(b"100000000000000000000", 10).expect("e denominator digits"),
    )))
}

fn exports(entries: &[(&str, Value)]) -> HashMap<String, Value> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn builtin(f: fn(&mut Vm, &[Value]) -> Result<Value>) -> Value {
    Value::Builtin(Arc::new(f))
}

fn submodule(name: &str, entries: &[(&str, Value)]) -> Shared<ModuleObject> {
    Shared::new(ModuleObject {
        name: name.into(),
        full_name: format!("std.{name}"),
        exports: exports(entries),
        children: HashMap::new(),
        is_user: false,
    })
}

fn expect_arity(name: &str, args: &[Value], n: usize) -> Result<()> {
    if args.len() != n {
        return Err(crate::error::RuntimeError::type_err(format!(
            "{name} requires {n} argument{}",
            if n == 1 { "" } else { "s" }
        )));
    }
    Ok(())
}

fn expect_num_value(name: &str, args: &[Value], idx: usize) -> Result<Num> {
    let v = args.get(idx).ok_or_else(|| {
        crate::error::RuntimeError::type_err(format!("{name}: missing argument {idx}"))
    })?;
    match v {
        Value::Num(n) => Ok(n.clone()),
        _ => Err(crate::error::RuntimeError::type_err(format!(
            "{name}: argument must be num"
        ))),
    }
}

fn expect_num_f64(name: &str, args: &[Value], idx: usize) -> Result<f64> {
    expect_num_value(name, args, idx)?.to_f64_checked()
}

fn expect_int(name: &str, args: &[Value], idx: usize) -> Result<i64> {
    let v = args.get(idx).ok_or_else(|| {
        crate::error::RuntimeError::type_err(format!("{name}: missing argument {idx}"))
    })?;
    crate::value::expect_i64(name, v)
}

fn expect_text(name: &str, args: &[Value], idx: usize) -> Result<String> {
    match args.get(idx) {
        Some(Value::Text(s)) => Ok(s.clone()),
        _ => Err(crate::error::RuntimeError::type_err(format!(
            "{name}: argument must be text"
        ))),
    }
}

fn float_from_f64(f: f64) -> Result<Num> {
    num_rational::BigRational::from_float(f)
        .map(Num::from_rational)
        .ok_or_else(|| crate::error::RuntimeError::value_err("non-finite floating-point result"))
}

/// 超越函数经 IEEE754；精确有理运算走 Num 路径。
macro_rules! math_f1 {
    ($fn_name:ident, $api:literal, $op:expr) => {
        fn $fn_name(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
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

fn math_atan2(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("atan2", args, 2)?;
    let y = expect_num_f64("atan2", args, 0)?;
    let x = expect_num_f64("atan2", args, 1)?;
    Ok(Value::Num(float_from_f64(y.atan2(x))?))
}

fn math_hypot(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("hypot", args, 2)?;
    let a = expect_num_f64("hypot", args, 0)?;
    let b = expect_num_f64("hypot", args, 1)?;
    Ok(Value::Num(float_from_f64(a.hypot(b))?))
}

/// `divmod(a, b)` → `[a / b, a % b]`（整数商与余数，遵循有理数取模同号语义）。
fn math_divmod(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
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
fn math_const_inf() -> Value {
    Value::Num(Num::from_bigint(BigInt::from(i64::MAX)))
}

/// 同 `math_const_inf`，返回 `i64::MIN` 作为负无穷的最佳近似。
fn math_const_neg_inf() -> Value {
    Value::Num(Num::from_bigint(BigInt::from(i64::MIN)))
}

/// Optive 的 `Num` 无法表示 IEEE 754 NaN。返回 0 作为占位；
/// 用户代码不应将 `std.math.nan` 用于 NaN 检测。
fn math_const_nan() -> Value {
    Value::Num(Num::Small(0))
}

fn math_const_tau() -> Value {
    Value::Num(Num::from_rational(num_rational::BigRational::new(
        BigInt::parse_bytes(b"62831853071795864769", 10).expect("tau numerator digits"),
        BigInt::parse_bytes(b"10000000000000000000", 10).expect("tau denominator digits"),
    )))
}

fn std_concat(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let mut out = String::new();
    for arg in args {
        out.push_str(&arg.print_string());
    }
    Ok(Value::Text(out))
}

fn math_sqrt(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("sqrt", args, 1)?;
    let x = expect_num_f64("sqrt", args, 0)?;
    if x < 0.0 {
        return Err(crate::error::RuntimeError::value_err("sqrt of negative number"));
    }
    Ok(Value::Num(float_from_f64(x.sqrt())?))
}

fn math_abs(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("abs", args, 1)?;
    Ok(Value::Num(expect_num_value("abs", args, 0)?.abs_num()))
}

fn math_floor(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("floor", args, 1)?;
    Ok(Value::Num(expect_num_value("floor", args, 0)?.floor_num()))
}

fn math_ceil(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("ceil", args, 1)?;
    Ok(Value::Num(expect_num_value("ceil", args, 0)?.ceil_num()))
}

fn math_round(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("round", args, 1)?;
    Ok(Value::Num(expect_num_value("round", args, 0)?.round_num()))
}

fn math_trunc(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("trunc", args, 1)?;
    Ok(Value::Num(expect_num_value("trunc", args, 0)?.trunc_num()))
}

fn math_pow(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("pow", args, 2)?;
    args[0].pow(&args[1])
}

fn math_log(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let x = expect_num_f64("log", args, 0)?;
    if x <= 0.0 {
        return Err(crate::error::RuntimeError::value_err("log requires positive number"));
    }
    if args.len() >= 2 {
        let base = expect_num_f64("log", args, 1)?;
        Ok(Value::Num(float_from_f64(x.log(base))?))
    } else {
        Ok(Value::Num(float_from_f64(x.ln())?))
    }
}

fn math_log10(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("log10", args, 1)?;
    let x = expect_num_f64("log10", args, 0)?;
    if x <= 0.0 {
        return Err(crate::error::RuntimeError::value_err("log10 requires positive number"));
    }
    Ok(Value::Num(float_from_f64(x.log10())?))
}

fn math_min(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err("min requires at least 1 argument"));
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

fn math_max(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err("max requires at least 1 argument"));
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

fn math_clamp(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
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

fn num_as_bigint(n: &Num) -> Result<BigInt> {
    match n {
        Num::Small(i) => Ok(BigInt::from(*i)),
        Num::Int(i) => Ok(i.as_ref().clone()),
        Num::Rat(r) if r.denom() == &num_traits::One::one() => Ok(r.numer().clone()),
        _ => Err(crate::error::RuntimeError::msg("expected integer num")),
    }
}

fn bigint_gcd(mut a: BigInt, mut b: BigInt) -> BigInt {
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

fn math_gcd(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("gcd requires 2 arguments"));
    }
    let (Value::Num(a), Value::Num(b)) = (&args[0], &args[1]) else {
        return Err(crate::error::RuntimeError::type_err("gcd requires nums"));
    };
    Ok(Value::Num(Num::from_bigint(bigint_gcd(
        num_as_bigint(a)?,
        num_as_bigint(b)?,
    ))))
}

fn math_lcm(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    use num_traits::{Signed, Zero};
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("lcm requires 2 arguments"));
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

fn math_sign(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
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

fn math_mod(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("mod requires 2 arguments"));
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


fn math_is_integer(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    match args.first() {
        Some(Value::Num(n)) => {
            let ok = match n {
                Num::Small(_) | Num::Int(_) => true,
                Num::Rat(r) => r.denom() == &num_traits::One::one(),
            };
            Ok(Value::Bool(ok))
        }
        _ => Err(crate::error::RuntimeError::type_err("is_integer requires 1 num")),
    }
}

fn math_is_rational(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    match args.first() {
        Some(Value::Num(n)) => Ok(Value::Bool(matches!(n, Num::Rat(_)))),
        _ => Err(crate::error::RuntimeError::type_err("is_rational requires 1 num")),
    }
}

fn math_range(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let (start, stop, step) = match args.len() {
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
            let step = expect_int("range", args, 2)?;
            if step == 0 {
                return Err(crate::error::RuntimeError::value_err("range step must not be zero"));
            }
            (start, stop, step)
        }
        n => {
            return Err(crate::error::RuntimeError::type_err(format!(
                "range requires 1 to 3 arguments, got {n}"
            )))
        }
    };
    Ok(IteratorState::from_range(start, stop, step).as_value())
}

fn io_read_file(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "read_file requires 1 argument",
        ));
    }
    let path = expect_text("read_file", args, 0)?;
    vm.caps.check_fs("read_file", &path)?;
    let content = std::fs::read_to_string(&path).map_err(|e| {
        crate::error::RuntimeError::io_err(format!("read_file failed: {e}"))
    })?;
    vm.request_cooperative_yield();
    Ok(Value::Text(content))
}

fn io_write_file(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "write_file requires 2 arguments",
        ));
    }
    let path = expect_text("write_file", args, 0)?;
    vm.caps.check_fs("write_file", &path)?;
    let content = args[1].print_string();
    std::fs::write(&path, content).map_err(|e| {
        crate::error::RuntimeError::io_err(format!("write_file failed: {e}"))
    })?;
    vm.request_cooperative_yield();
    Ok(Value::None)
}

fn io_append_file(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "append_file requires 2 arguments",
        ));
    }
    use std::io::Write;
    let path = expect_text("append_file", args, 0)?;
    vm.caps.check_fs("append_file", &path)?;
    let content = args[1].print_string();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("append_file failed: {e}")))?;
    f.write_all(content.as_bytes())
        .map_err(|e| crate::error::RuntimeError::io_err(format!("append_file failed: {e}")))?;
    vm.request_cooperative_yield();
    Ok(Value::None)
}

fn io_read_bytes(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "read_bytes requires 1 argument",
        ));
    }
    let path = expect_text("read_bytes", args, 0)?;
    vm.caps.check_fs("read_bytes", &path)?;
    let bytes = std::fs::read(&path)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("read_bytes failed: {e}")))?;
    vm.request_cooperative_yield();
    Ok(Value::Bytes(Arc::new(bytes)))
}

fn io_write_bytes(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "write_bytes requires 2 arguments",
        ));
    }
    let path = expect_text("write_bytes", args, 0)?;
    vm.caps.check_fs("write_bytes", &path)?;
    let bytes = match &args[1] {
        Value::Bytes(b) => b.as_ref().clone(),
        Value::Text(s) => s.as_bytes().to_vec(),
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "write_bytes: content must be bytes or text",
            ))
        }
    };
    std::fs::write(&path, bytes)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("write_bytes failed: {e}")))?;
    vm.request_cooperative_yield();
    Ok(Value::None)
}

fn io_write_line(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let out = crate::value::args_join_space(args);
    println!("{out}");
    Ok(Value::None)
}

fn io_eprint(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let out = crate::value::args_join_space(args);
    eprintln!("{out}");
    Ok(Value::None)
}

fn format_format(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "format requires at least 1 argument",
        ));
    }
    let tmpl = expect_text("format", args, 0)?;
    let values = &args[1..];
    let mut result = String::new();
    let chars: Vec<char> = tmpl.chars().collect();
    let mut i = 0;
    let mut auto_idx = 0usize;
    while i < chars.len() {
        if chars[i] == '{' {
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                result.push('{');
                i += 2;
                continue;
            }
            let close = chars[i + 1..]
                .iter()
                .position(|&c| c == '}')
                .map(|p| i + 1 + p)
                .ok_or_else(|| {
                    crate::error::RuntimeError::value_err("format: unmatched '{'")
                })?;
            let inner: String = chars[i + 1..close].iter().collect();
            let idx = if inner.is_empty() {
                let n = auto_idx;
                auto_idx += 1;
                n
            } else {
                inner.parse::<usize>().map_err(|_| {
                    crate::error::RuntimeError::value_err(format!(
                        "format: invalid field {{{inner}}}"
                    ))
                })?
            };
            let v = values.get(idx).ok_or_else(|| {
                crate::error::RuntimeError::value_err(format!(
                    "format: missing argument {{{idx}}}"
                ))
            })?;
            result.push_str(&v.print_string());
            i = close + 1;
        } else if chars[i] == '}' {
            if i + 1 < chars.len() && chars[i + 1] == '}' {
                result.push('}');
                i += 2;
                continue;
            }
            return Err(crate::error::RuntimeError::value_err(
                "format: unmatched '}'",
            ));
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }
    Ok(Value::Text(result))
}

fn format_join(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("join requires 2 arguments"));
    }
    let sep = expect_text("join", args, 0)?;
    let items = value_to_list(&args[1])?;
    let parts: Vec<String> = items.iter().map(|v| v.print_string()).collect();
    Ok(Value::Text(parts.join(&sep)))
}

fn format_format_num(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(crate::error::RuntimeError::type_err(
            "format_num requires 1 or 2 arguments (n[, prec])",
        ));
    }
    let x = expect_num_f64("format_num", args, 0)?;
    let prec = if args.len() == 2 {
        expect_int("format_num", args, 1)?.max(0) as usize
    } else {
        DEFAULT_NUM_PRECISION
    };
    Ok(Value::Text(format!("{x:.prec$}")))
}

fn format_pad(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 || args.len() > 3 {
        return Err(crate::error::RuntimeError::type_err(
            "pad requires 2 or 3 arguments (s, width[, fill])",
        ));
    }
    let s = expect_text("pad", args, 0)?;
    let width = expect_int("pad", args, 1)?.max(0) as usize;
    let fill = if args.len() == 3 {
        let f = expect_text("pad", args, 2)?;
        f.chars().next().unwrap_or(' ')
    } else {
        ' '
    };
    if s.chars().count() >= width {
        return Ok(Value::Text(s));
    }
    let pad_len = width - s.chars().count();
    let mut out = String::with_capacity(width);
    for _ in 0..pad_len {
        out.push(fill);
    }
    out.push_str(&s);
    Ok(Value::Text(out))
}

fn format_indent(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "indent requires 2 arguments (text, n)",
        ));
    }
    let text = expect_text("indent", args, 0)?;
    let n = expect_int("indent", args, 1)?.max(0) as usize;
    let pad = " ".repeat(n);
    let out: Vec<String> = text
        .lines()
        .map(|line| format!("{pad}{line}"))
        .collect();
    Ok(Value::Text(out.join("\n")))
}

fn iter_iter(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("iter", args, 1)?;
    match &args[0] {
        Value::Iterator(it) => Ok(Value::Iterator(it.clone())),
        other => Ok(crate::value::value_to_iterable(other)?.as_value()),
    }
}

fn iter_to_list(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("to_list requires 1 argument"));
    }
    Ok(Value::List(Shared::new(materialize_iter(vm, &args[0])?)))
}

fn iter_to_set(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("to_set requires 1 argument"));
    }
    let mut set = crate::value::SetMap::new();
    for item in materialize_iter(vm, &args[0])? {
        set.insert(ValueKey::from_value(&item)?);
    }
    Ok(Value::Set(Shared::new(set)))
}

fn iter_enumerate(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("enumerate requires 1 argument"));
    }
    let items = value_to_list(&args[0])?;
    let pairs: Vec<Value> = items
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            Value::List(Shared::new(vec![
                Value::Num(Num::Small(i as i64)),
                item,
            ]))
        })
        .collect();
    Ok(Value::List(Shared::new(pairs)))
}

fn iter_chain(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "chain requires at least 1 argument",
        ));
    }
    let mut merged = Vec::new();
    for arg in args {
        merged.extend(value_to_list(arg)?);
    }
    Ok(Value::List(Shared::new(merged)))
}

fn iter_take(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("take requires 2 arguments"));
    }
    let n = expect_int("take", args, 1)?.max(0) as usize;
    let state = value_to_iterator_rc(&args[0])?;
    let mut out = Vec::with_capacity(n.min(ITER_MATERIALIZE_INIT_CAP));
    for _ in 0..n {
        match vm.advance_iterator(&state)? {
            Some(v) => out.push(v),
            None => break,
        }
    }
    Ok(Value::List(Shared::new(out)))
}

fn iter_skip(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("skip requires 2 arguments"));
    }
    let n = expect_int("skip", args, 1)?.max(0) as usize;
    let state = value_to_iterator_rc(&args[0])?;
    for _ in 0..n {
        if vm.advance_iterator(&state)?.is_none() {
            return Ok(Value::List(Shared::new(Vec::new())));
        }
    }
    Ok(Value::List(Shared::new(materialize_iter(
        vm,
        &Value::Iterator(state),
    )?)))
}

fn iter_next(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err("next requires an iterator"));
    }
    let state = value_to_iterator_rc(&args[0])?;
    match vm.advance_iterator(&state)? {
        Some(v) => Ok(v),
        None => {
            if args.len() >= 2 {
                Ok(args[1].clone())
            } else {
                let exc = crate::exceptions::make_exception(vm, "StopIteration", "iterator exhausted")?;
                vm.throw_value(exc)?;
                Ok(Value::None)
            }
        }
    }
}

fn iter_fold(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err(
            "fold requires 3 arguments (fn, init, iter)",
        ));
    }
    let func = expect_function("fold", args, 0)?;
    let mut acc = args[1].clone();
    for item in materialize_iter(vm, &args[2])? {
        acc = vm.call_user_function(func.clone(), vec![acc, item])?;
    }
    Ok(acc)
}

fn iter_repeat(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
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

fn iter_cycle(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("cycle requires 1 argument"));
    }
    let items = materialize_iter(vm, &args[0])?;
    Ok(Value::Iterator(Shared::new(IteratorState {
        kind: IteratorKind::Cycle { items, index: 0 },
    })))
}

fn iter_count(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("count requires 1 argument"));
    }
    Ok(Value::Num(Num::Small(
        materialize_iter(vm, &args[0])?.len() as i64,
    )))
}

fn iter_find(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "find requires 2 arguments (iterable, predicate)",
        ));
    }
    let pred = expect_function("find", args, 1)?;
    for item in materialize_iter(vm, &args[0])? {
        if vm
            .call_user_function(pred.clone(), vec![item.clone()])?
            .is_truthy()
        {
            return Ok(item);
        }
    }
    Ok(Value::None)
}

fn iter_any(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "any requires 2 arguments (iterable, predicate)",
        ));
    }
    let pred = expect_function("any", args, 1)?;
    for item in materialize_iter(vm, &args[0])? {
        if vm
            .call_user_function(pred.clone(), vec![item])?
            .is_truthy()
        {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

fn iter_all(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "all requires 2 arguments (iterable, predicate)",
        ));
    }
    let pred = expect_function("all", args, 1)?;
    for item in materialize_iter(vm, &args[0])? {
        if !vm
            .call_user_function(pred.clone(), vec![item])?
            .is_truthy()
        {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn dict_keys(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("keys requires 1 argument"));
    }
    let Value::Dict(d) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err("keys requires dict"));
    };
    let keys: Vec<Value> = d.borrow().keys().map(value_key_to_value).collect();
    Ok(Value::List(Shared::new(keys)))
}

fn dict_values(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("values requires 1 argument"));
    }
    let Value::Dict(d) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err("values requires dict"));
    };
    Ok(Value::List(Shared::new(
        d.borrow().values().cloned().collect(),
    )))
}

fn dict_items(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("items requires 1 argument"));
    }
    let Value::Dict(d) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err("items requires dict"));
    };
    let pairs: Vec<Value> = d
        .borrow()
        .iter()
        .map(|(k, v)| {
            Value::List(Shared::new(vec![
                value_key_to_value(k),
                v.clone(),
            ]))
        })
        .collect();
    Ok(Value::List(Shared::new(pairs)))
}

fn dict_get(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 || args.len() > 3 {
        return Err(crate::error::RuntimeError::type_err(
            "get requires 2 or 3 arguments",
        ));
    }
    let Value::Dict(d) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err("get requires dict"));
    };
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

fn dict_from_items(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "from_items requires 1 argument",
        ));
    }
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

fn dict_update(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("update requires 2 arguments"));
    }
    let Value::Dict(dst) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err("update requires dict"));
    };
    let Value::Dict(src) = &args[1] else {
        return Err(crate::error::RuntimeError::type_err("update requires dict"));
    };
    for (k, v) in src.borrow().iter() {
        dst.borrow_mut().insert(k.clone(), v.clone());
    }
    Ok(Value::Dict(dst.clone()))
}

fn dict_merge(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Ok(Value::Dict(Shared::new(DictMap::new())));
    }
    let mut out = DictMap::new();
    for arg in args {
        let Value::Dict(d) = arg else {
            return Err(crate::error::RuntimeError::type_err("merge requires dicts"));
        };
        for (k, v) in d.borrow().iter() {
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Dict(Shared::new(out)))
}

fn dict_invert(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("invert requires 1 argument"));
    }
    let Value::Dict(d) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err("invert requires dict"));
    };
    let mut out = DictMap::new();
    for (k, v) in d.borrow().iter() {
        let key = ValueKey::from_value(v)?;
        out.insert(key, value_key_to_value(k));
    }
    Ok(Value::Dict(Shared::new(out)))
}

fn dict_setdefault(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err(
            "setdefault requires 3 arguments (dict, key, default)",
        ));
    }
    let Value::Dict(d) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err("setdefault requires dict"));
    };
    let key = ValueKey::from_value(&args[1])?;
    let mut map = d.borrow_mut();
    if let Some(v) = map.get(&key) {
        return Ok(v.clone());
    }
    map.insert(key, args[2].clone());
    Ok(args[2].clone())
}

fn value_to_list(v: &Value) -> Result<Vec<Value>> {
    match v {
        Value::List(list) => Ok(list.borrow().clone()),
        Value::Text(s) => Ok(s.chars().map(|c| Value::Text(c.to_string())).collect()),
        _ => Err(crate::error::RuntimeError::type_err("object is not iterable")),
    }
}

fn value_key_to_value(k: &ValueKey) -> Value {
    match k {
        ValueKey::Bool(b) => Value::Bool(*b),
        ValueKey::NumInt(n) => Value::Num(Num::from_bigint(n.clone())),
        ValueKey::Text(s) => Value::Text(s.clone()),
    }
}


fn ast_parse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let source = expect_text("parse", args, 0)?;
    Ok(runtime_ast::parse_to_ast(&source)?.as_value())
}

fn ast_clone_export(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("ast_clone requires 1 argument"));
    }
    runtime_ast::clone_ast_value(&args[0])
}

fn ast_type_convert_export(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "ast_type_convert requires 2 arguments",
        ));
    }
    runtime_ast::compose_ast_type_convert(&args[0], &args[1])
}

fn ast_call_export(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("ast_call requires 2 arguments"));
    }
    runtime_ast::compose_ast_func_call(&args[0], &args[1])
}

fn ast_macro_call_export(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "ast_macro_call requires 2 arguments",
        ));
    }
    runtime_ast::compose_ast_macro_call(&args[0], &args[1])
}

fn ast_vec_push_export(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "ast_vec_push requires 2 arguments",
        ));
    }
    runtime_ast::ast_vec_push(&args[0], &args[1])
}

fn ast_vec_extend_export(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "ast_vec_extend requires 2 arguments",
        ));
    }
    runtime_ast::ast_vec_extend(&args[0], &args[1])
}

fn ast_unparse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("unparse requires 1 argument"));
    }
    let node = runtime_ast::value_as_ast(&args[0])?;
    Ok(Value::Text(runtime_ast::ast_to_source(&node)))
}

fn ast_walk(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("walk requires 2 arguments"));
    }
    let node = runtime_ast::value_as_ast(&args[0])?;
    let visitor = expect_function("walk", args, 1)?;
    runtime_ast::walk_ast_nodes(&node, &mut |n| {
        let _ = vm.call_user_function(visitor.clone(), vec![n.clone().as_value()])?;
        Ok(())
    })?;
    Ok(Value::None)
}

fn expect_function(name: &str, args: &[Value], idx: usize) -> Result<Arc<crate::opcode::FunctionObject>> {
    match args.get(idx) {
        Some(Value::Function(f)) => Ok(f.clone()),
        _ => Err(crate::error::RuntimeError::type_err(format!(
            "{name}: argument must be a function"
        ))),
    }
}

fn decos_log(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("log requires 1 argument"));
    }
    let inner = expect_function("log", args, 0)?;
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        eprintln!("log: call({})", call_args.len());
        vm.call_user_function(inner.clone(), call_args.to_vec())
    })))
}

fn decos_once(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("once requires 1 argument"));
    }
    let inner = expect_function("once", args, 0)?;
    let cached = SyncCell::new(None::<Value>);
    let called = SyncCell::new(false);
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        if *called.borrow() {
            return cached
                .borrow()
                .clone()
                .ok_or_else(|| crate::error::RuntimeError::msg("once: empty cache"));
        }
        let result = vm.call_user_function(inner.clone(), call_args.to_vec())?;
        *cached.borrow_mut() = Some(result.clone());
        *called.borrow_mut() = true;
        Ok(result)
    })))
}

fn decos_memoize(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("memoize", args, 1)?;
    let inner = expect_function("memoize", args, 0)?;
    let cache = SyncCell::new(HashMap::<Vec<ValueKey>, Value>::new());
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        let key: Vec<ValueKey> = call_args
            .iter()
            .map(ValueKey::from_value)
            .collect::<Result<Vec<_>>>()?;
        if let Some(hit) = cache.borrow().get(&key) {
            return Ok(hit.clone());
        }
        let result = vm.call_user_function(inner.clone(), call_args.to_vec())?;
        cache.borrow_mut().insert(key, result.clone());
        Ok(result)
    })))
}

// --- 其余 std 子模块 ---

fn build_typing_module() -> Shared<ModuleObject> {
    fn type_ctor(name: &str) -> Value {
        let name = name.to_string();
        let is_form = crate::type_registry::is_type_form(&name);
        Value::Builtin(Arc::new(move |_vm, args| {
            if args.is_empty() {
                if is_form {
                    return Ok(Value::TypeSpec(crate::value::TypeSpecData::new(
                        name.clone(),
                        vec![],
                    )));
                }
                return Ok(Value::type_ref(name.clone()));
            }
            let params: Vec<crate::ast::TypeExpr> = args
                .iter()
                .map(crate::type_registry::value_to_type_expr_operand)
                .collect();
            Ok(Value::TypeSpec(crate::value::TypeSpecData::new(
                name.clone(),
                params,
            )))
        }))
    }
    fn type_ctor_literal() -> Value {
        Value::Builtin(Arc::new(move |_vm, args| {
            if args.is_empty() {
                return Err(crate::error::RuntimeError::type_err(
                    "Literal requires at least 1 argument",
                ));
            }
            let params: Vec<crate::ast::TypeExpr> = args
                .iter()
                .map(crate::type_registry::literal_operand_to_type_expr)
                .collect::<crate::Result<Vec<_>>>()?;
            Ok(Value::TypeSpec(crate::value::TypeSpecData::new(
                "Literal".to_string(),
                params,
            )))
        }))
    }
    submodule(
        "typing",
        &[("Union", type_ctor("Union")),
            ("Maybe", type_ctor("Maybe")),
            ("Optional", type_ctor("Maybe")),
            ("Tuple", type_ctor("Tuple")),
            ("Callable", type_ctor("Callable")),
            ("Covariant", type_ctor("Covariant")),
            ("Contravariant", type_ctor("Contravariant")),
            ("Invariant", type_ctor("Invariant")),
            ("Never", Value::type_ref("Never")),
            ("Literal", type_ctor_literal()),
            ("fields_of", builtin(typing_fields_of)),
            ("protocol_of", builtin(typing_protocol_of)),
            ("isinstanceof", builtin(typing_isinstanceof)),],
    )
}

/// `std.typing.fields_of(value | "TypeName")` → 字段名 text 列表（含基类字段）。
fn typing_fields_of(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "fields_of requires 1 argument",
        ));
    }
    let def = match &args[0] {
        Value::Struct(s) => Some(s.def.clone()),
        Value::TypeRef(n) | Value::Text(n) => _vm.struct_defs.get(n).cloned(),
        _ => None,
    };
    let Some(def) = def else {
        return Err(crate::error::RuntimeError::type_err(
            "fields_of expects a struct value or struct type name",
        ));
    };
    Ok(Value::List(Shared::new(
        def.fields.iter().map(|f| Value::Text(f.clone())).collect(),
    )))
}

/// `std.typing.protocol_of("Name")` → `{name, methods, fields}` 或 none（非协议）。
fn typing_protocol_of(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "protocol_of requires 1 argument",
        ));
    }
    let name = match &args[0] {
        Value::Text(s) => s.clone(),
        Value::TypeRef(s) => s.clone(),
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "protocol_of expects a protocol name (text)",
            ));
        }
    };
    let Some(pd) = _vm.protocols.get(&name).cloned() else {
        return Ok(Value::None);
    };
    let mut out = DictMap::new();
    out.insert(ValueKey::Text("name".into()), Value::Text(pd.name.clone()));
    out.insert(
        ValueKey::Text("methods".into()),
        Value::List(Shared::new(
            pd.methods.iter().map(|m| Value::Text(m.clone())).collect(),
        )),
    );
    out.insert(
        ValueKey::Text("fields".into()),
        Value::List(Shared::new(
            pd.fields
                .iter()
                .map(|(f, m)| {
                    let mut d = DictMap::new();
                    d.insert(ValueKey::Text("name".into()), Value::Text(f.clone()));
                    d.insert(ValueKey::Text("mutable".into()), Value::Bool(*m));
                    Value::Dict(Shared::new(d))
                })
                .collect(),
        )),
    );
    Ok(Value::Dict(Shared::new(out)))
}

/// `std.typing.isinstanceof(value, type)` —— 运行时实例检查，替代 `is_a`。
fn typing_isinstanceof(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "isinstanceof requires 2 arguments",
        ));
    }
    let ok = match &args[1] {
        Value::TypeRef(s) | Value::Text(s) => crate::types::instance_is_a(vm, &args[0], s),
        Value::TypeSpec(spec) => {
            let ty = crate::types::type_spec_to_type_expr(spec);
            crate::types::type_accepts(vm, &args[0], &ty)
        }
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "isinstanceof expects a type handle or TypeSpec",
            ));
        }
    };
    Ok(Value::Bool(ok))
}

fn build_functional_module() -> Shared<ModuleObject> {
    submodule(
        "functional",
        &[("map", builtin(func_map)),
            ("filter", builtin(func_filter)),
            ("zip", builtin(func_zip)),
            ("reduce", builtin(func_reduce)),
            ("compose", builtin(func_compose)),
            ("partial", builtin(func_partial)),
            ("identity", builtin(func_identity)),
            ("const", builtin(func_const)),
            ("flip", builtin(func_flip)),],
    )
}

fn build_collections_module() -> Shared<ModuleObject> {
    submodule(
        "collections",
        &[("sorted", builtin(coll_sorted)),
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
            ("group_by", builtin(coll_group_by)),],
    )
}

fn build_time_module() -> Shared<ModuleObject> {
    submodule(
        "time",
        &[("now", builtin(time_now)),
            ("now_ms", builtin(time_now_ms)),
            ("monotonic", builtin(time_monotonic)),
            ("sleep", builtin(time_sleep)),
            ("sleep_ms", builtin(time_sleep_ms)),],
    )
}

fn build_sync_module() -> Shared<ModuleObject> {
    submodule(
        "sync",
        &[
            ("Channel", Value::type_ref("Channel")),
            ("Mutex", Value::type_ref("Mutex")),
            ("RWMutex", Value::type_ref("RWMutex")),
            ("WaitGroup", Value::type_ref("WaitGroup")),
            ("Semaphore", Value::type_ref("Semaphore")),
            ("Once", Value::type_ref("Once")),
            ("Barrier", Value::type_ref("Barrier")),
            ("Cond", Value::type_ref("Cond")),
            ("yield", builtin(sync_yield)),
        ],
    )
}

/// `std.sync.yield()`：主动让出当前 fiber，给其它就绪 fiber 一个运行机会。
fn sync_yield(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !args.is_empty() {
        return Err(crate::error::RuntimeError::type_err("yield requires 0 arguments"));
    }
    vm.request_cooperative_yield();
    Ok(Value::None)
}

fn build_text_module() -> Shared<ModuleObject> {
    submodule(
        "text",
        &[("upper", builtin(text_upper)),
            ("lower", builtin(text_lower)),
            ("strip", builtin(text_strip)),
            ("split", builtin(text_split)),
            ("contains", builtin(text_contains)),
            ("len", builtin(text_len)),
            ("replace", builtin(text_replace)),
            ("startswith", builtin(text_startswith)),
            ("endswith", builtin(text_endswith)),
            ("find", builtin(text_find)),
            ("join", builtin(text_join)),
            ("repeat", builtin(text_repeat)),
            ("count", builtin(text_count)),
            ("lines", builtin(text_lines)),
            ("is_digit", builtin(text_is_digit)),
            ("is_alpha", builtin(text_is_alpha)),
            ("is_space", builtin(text_is_space)),
            ("ord", builtin(text_ord)),
            ("chr", builtin(text_chr)),],
    )
}

fn build_path_module() -> Shared<ModuleObject> {
    submodule(
        "path",
        &[("join", builtin(path_join)),
            ("basename", builtin(path_basename)),
            ("dirname", builtin(path_dirname)),
            ("extension", builtin(path_extension)),
            ("stem", builtin(path_stem)),
            ("is_absolute", builtin(path_is_absolute)),
            ("abspath", builtin(path_abspath)),
            ("normalize", builtin(path_normalize)),
            ("splitext", builtin(path_splitext)),],
    )
}

fn build_fs_module() -> Shared<ModuleObject> {
    submodule(
        "fs",
        &[("exists", builtin(fs_exists)),
            ("is_file", builtin(fs_is_file)),
            ("is_dir", builtin(fs_is_dir)),
            ("list_dir", builtin(fs_list_dir)),
            ("mkdir", builtin(fs_mkdir)),
            ("mkdir_all", builtin(fs_mkdir_all)),
            ("remove", builtin(fs_remove)),
            ("remove_dir", builtin(fs_remove_dir)),
            ("rename", builtin(fs_rename)),
            ("copy", builtin(fs_copy)),
            ("read_text", builtin(io_read_file)),
            ("write_text", builtin(io_write_file)),
            ("read_bytes", builtin(io_read_bytes)),
            ("write_bytes", builtin(io_write_bytes)),],
    )
}

fn build_os_module() -> Shared<ModuleObject> {
    submodule(
        "os",
        &[("getenv", builtin(os_getenv)),
            ("setenv", builtin(os_setenv)),
            ("args", builtin(os_args)),
            ("exit", builtin(os_exit)),
            ("cwd", builtin(os_cwd)),
            ("chdir", builtin(os_chdir)),
            ("name", builtin(os_name)),],
    )
}

fn build_json_module() -> Shared<ModuleObject> {
    submodule(
        "json",
        &[("parse", builtin(json_parse)),
            ("stringify", builtin(json_stringify)),
            ("parse_file", builtin(json_parse_file)),
            ("dump", builtin(json_dump)),],
    )
}

fn build_test_module() -> Shared<ModuleObject> {
    submodule(
        "test",
        &[("assert_eq", builtin(test_assert_eq)),
            ("assert_true", builtin(test_assert_true)),
            ("assert_raises", builtin(test_assert_raises)),],
    )
}

fn build_debug_module() -> Shared<ModuleObject> {
    submodule(
        "debug",
        &[
            ("traceback", builtin(debug_traceback)),
            ("format_tb", builtin(debug_format_tb)),
            ("print_tb", builtin(debug_print_tb)),
            ("format_exception", builtin(debug_format_exception)),
            ("type_name", builtin(debug_type_name)),
            ("breakpoint", builtin(debug_breakpoint)),
        ],
    )
}

fn build_random_module() -> Shared<ModuleObject> {
    submodule(
        "random",
        &[("randint", builtin(random_randint)),
            ("random", builtin(random_random)),
            ("randstring", builtin(random_randstring)),
            ("choice", builtin(random_choice)),
            ("shuffle", builtin(random_shuffle)),
            ("sample", builtin(random_sample)),
            ("seed", builtin(random_seed)),],
    )
}

fn io_read_line(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let prompt = if args.is_empty() {
        String::new()
    } else {
        args[0].print_string()
    };
    crate::builtins::read_line_with_prompt(&prompt)
}

fn decos_timer(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let inner = expect_function("timer", args, 0)?;
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        let start = std::time::Instant::now();
        let result = vm.call_user_function(inner.clone(), call_args.to_vec())?;
        eprintln!("timer: {:?}", start.elapsed());
        Ok(result)
    })))
}

fn decos_debug(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let inner = expect_function("debug", args, 0)?;
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        let preview: Vec<String> = call_args.iter().map(|v| v.print_string()).collect();
        eprintln!("debug: call({})", preview.join(", "));
        vm.call_user_function(inner.clone(), call_args.to_vec())
    })))
}

fn decos_retry(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let inner = expect_function("retry", args, 0)?;
    let attempts = if args.len() > 1 {
        expect_int("retry", args, 1)? as usize
    } else {
        3
    };
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        let mut last_err = None;
        for _ in 0..attempts {
            match vm.call_user_function(inner.clone(), call_args.to_vec()) {
                Ok(v) => return Ok(v),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| crate::error::RuntimeError::msg("retry failed")))
    })))
}

fn decos_validate(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let pred = expect_function("validate", args, 0)?;
    let inner = if args.len() > 1 {
        Some(expect_function("validate", args, 1)?)
    } else {
        None
    };
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        let result = if let Some(f) = &inner {
            vm.call_user_function(f.clone(), call_args.to_vec())?
        } else {
            call_args.first().cloned().unwrap_or(Value::None)
        };
        let ok = vm.call_user_function(pred.clone(), vec![result.clone()])?;
        if !ok.is_truthy() {
            return Err(crate::error::RuntimeError::msg("validation failed"));
        }
        Ok(result)
    })))
}

fn decos_catch(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let inner = expect_function("catch", args, 0)?;
    let fallback = if args.len() > 1 {
        Some(expect_function("catch", args, 1)?)
    } else {
        None
    };
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        match vm.call_user_function(inner.clone(), call_args.to_vec()) {
            Ok(v) => Ok(v),
            Err(_) => {
                if let Some(f) = &fallback {
                    vm.call_user_function(f.clone(), vec![])
                } else {
                    Ok(Value::None)
                }
            }
        }
    })))
}

fn decos_deprecated(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() == 1 {
        if let Ok(inner) = expect_function("deprecated", args, 0) {
            return Ok(Value::Builtin(Arc::new(move |vm, call_args| {
                eprintln!("[deprecated]");
                vm.call_user_function(inner.clone(), call_args.to_vec())
            })));
        }
        let msg = args[0].print_string();
        return Ok(Value::Builtin(Arc::new(move |_vm, call_args| {
            let inner = expect_function("deprecated", call_args, 0)?;
            let msg = msg.clone();
            Ok(Value::Builtin(Arc::new(move |vm, args| {
                eprintln!("[deprecated] {msg}");
                vm.call_user_function(inner.clone(), args.to_vec())
            })))
        })));
    }
    Err(crate::error::RuntimeError::type_err(
        "deprecated requires 1 argument (function or message)",
    ))
}

fn decos_trace(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let inner = expect_function("trace", args, 0)?;
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        let shown: Vec<String> = call_args.iter().map(|v| v.print_string()).collect();
        eprintln!("trace: args={}", shown.join(", "));
        let result = vm.call_user_function(inner.clone(), call_args.to_vec())?;
        eprintln!("trace: => {}", result.print_string());
        Ok(result)
    })))
}

fn decos_singleton(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let factory = expect_function("singleton", args, 0)?;
    let cell = Shared::new(None::<Value>);
    Ok(Value::Builtin(Arc::new(move |vm, _call_args| {
        let mut slot = cell.borrow_mut();
        if let Some(v) = slot.as_ref() {
            return Ok(v.clone());
        }
        let v = vm.call_user_function(factory.clone(), vec![])?;
        *slot = Some(v.clone());
        Ok(v)
    })))
}

fn materialize_iter(vm: &mut Vm, v: &Value) -> Result<Vec<Value>> {
    let state = match v {
        Value::Iterator(it) => it.clone(),
        other => Shared::new(crate::value::value_to_iterable(other)?),
    };
    let mut out = Vec::new();
    while let Some(item) = vm.advance_iterator(&state)? {
        out.push(item);
    }
    Ok(out)
}

fn value_to_iterator_rc(v: &Value) -> Result<Shared<IteratorState>> {
    match v {
        Value::Iterator(it) => Ok(it.clone()),
        other => Ok(Shared::new(crate::value::value_to_iterable(other)?)),
    }
}

fn func_map(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("map requires 2 arguments"));
    }
    let func = expect_function("map", args, 0)?;
    let source = value_to_iterator_rc(&args[1])?;
    Ok(Value::Iterator(Shared::new(IteratorState {
        kind: IteratorKind::Map { func, source },
    })))
}

fn func_filter(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("filter requires 2 arguments"));
    }
    let pred = expect_function("filter", args, 0)?;
    let source = value_to_iterator_rc(&args[1])?;
    Ok(Value::Iterator(Shared::new(IteratorState {
        kind: IteratorKind::Filter { func: pred, source },
    })))
}

fn func_zip(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 {
        return Err(crate::error::RuntimeError::type_err("zip requires at least 2 arguments"));
    }
    vm.zip_iterables(args.to_vec())
}

fn func_reduce(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 {
        return Err(crate::error::RuntimeError::type_err("reduce requires at least 2 arguments"));
    }
    let func = expect_function("reduce", args, 0)?;
    let items = materialize_iter(vm, &args[1])?;
    let mut acc = args.get(2).cloned().unwrap_or(Value::None);
    for item in items {
        acc = vm.call_user_function(func.clone(), vec![acc, item])?;
    }
    Ok(acc)
}

fn func_compose(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("compose requires 2 arguments"));
    }
    let f = expect_function("compose", args, 0)?;
    let g = expect_function("compose", args, 1)?;
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        let mid = vm.call_user_function(g.clone(), call_args.to_vec())?;
        vm.call_user_function(f.clone(), vec![mid])
    })))
}

fn func_partial(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 {
        return Err(crate::error::RuntimeError::type_err("partial requires at least 2 arguments"));
    }
    let func = expect_function("partial", args, 0)?;
    let bound: Vec<Value> = args[1..].to_vec();
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        let mut full = bound.clone();
        full.extend_from_slice(call_args);
        vm.call_user_function(func.clone(), full)
    })))
}

fn func_identity(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("identity requires 1 argument"));
    }
    Ok(args[0].clone())
}

fn func_const(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("const requires 1 argument"));
    }
    let x = args[0].clone();
    Ok(Value::Builtin(Arc::new(move |_vm, _args| Ok(x.clone()))))
}

fn func_flip(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let func = expect_function("flip", args, 0)?;
    Ok(Value::Builtin(Arc::new(move |vm, call_args| {
        if call_args.len() < 2 {
            return Err(crate::error::RuntimeError::type_err(
                "flipped function requires at least 2 arguments",
            ));
        }
        let mut full = call_args.to_vec();
        full.swap(0, 1);
        vm.call_user_function(func.clone(), full)
    })))
}

fn coll_sorted(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("sorted requires 1 argument"));
    }
    let mut items = materialize_iter(vm, &args[0])?;
    items.sort_by_key(|a| a.print_string());
    Ok(Value::List(Shared::new(items)))
}

fn coll_reversed(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("reversed requires 1 argument"));
    }
    let mut items = materialize_iter(vm, &args[0])?;
    items.reverse();
    Ok(IteratorState::from_list(items).as_value())
}

fn coll_min(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let items = materialize_iter(vm, &args[0])?;
    items
        .into_iter()
        .reduce(|a, b| if a.print_string() <= b.print_string() { a } else { b })
        .ok_or_else(|| crate::error::RuntimeError::msg("min of empty"))
}

fn coll_max(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let items = materialize_iter(vm, &args[0])?;
    items
        .into_iter()
        .reduce(|a, b| if a.print_string() >= b.print_string() { a } else { b })
        .ok_or_else(|| crate::error::RuntimeError::msg("max of empty"))
}

fn coll_sum(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let items = materialize_iter(vm, &args[0])?;
    let mut total = BigInt::from(0);
    for item in items {
        if let Value::Num(n) = item {
            match n {
                Num::Small(x) => total += BigInt::from(x),
                Num::Int(x) => total += x.as_ref(),
                Num::Rat(r) if *r.denom() == BigInt::from(1) => total += r.numer(),
                Num::Rat(_) => {
                    return Err(crate::error::RuntimeError::type_err("sum requires integer values"));
                }
            }
        }
    }
    Ok(Value::Num(Num::from_bigint(total)))
}

fn coll_all(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let items = materialize_iter(vm, &args[0])?;
    Ok(Value::Bool(items.iter().all(|v| v.is_truthy())))
}

fn coll_any(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let items = materialize_iter(vm, &args[0])?;
    Ok(Value::Bool(items.iter().any(|v| v.is_truthy())))
}

fn coll_unique(vm: &mut Vm, args: &[Value]) -> Result<Value> {
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
    let items = materialize_iter(vm, &args[0])?;
    items
        .into_iter()
        .next()
        .ok_or_else(|| crate::error::RuntimeError::msg("first of empty"))
}

fn coll_last(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let items = materialize_iter(vm, &args[0])?;
    items
        .into_iter()
        .last()
        .ok_or_else(|| crate::error::RuntimeError::msg("last of empty"))
}

fn coll_nth(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("nth requires 2 arguments"));
    }
    let n = expect_int("nth", args, 1)? as usize;
    let items = materialize_iter(vm, &args[0])?;
    if let Some(v) = items.into_iter().nth(n) {
        return Ok(v);
    }
    let exc = crate::exceptions::make_exception(vm, "IndexError", format!("nth out of range: {n}"))?;
    match vm.throw_value(exc) {
        Ok(()) => Ok(Value::None),
        Err(e) => Err(e),
    }
}

fn coll_flatten(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("flatten requires 1 argument"));
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
        return Err(crate::error::RuntimeError::type_err("chunk requires 2 arguments"));
    }
    let size = expect_int("chunk", args, 1)?;
    if size <= 0 {
        return Err(crate::error::RuntimeError::type_err("chunk size must be positive"));
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

fn time_now(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("now failed: {e}")))?
        .as_secs();
    Ok(Value::Num(Num::Small(secs as i64)))
}

fn time_now_ms(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("now_ms failed: {e}")))?
        .as_millis();
    Ok(Value::Num(Num::from_bigint(BigInt::from(ms))))
}

fn time_monotonic(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    use std::time::Instant;
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = START.get_or_init(Instant::now);
    let secs = start.elapsed().as_secs_f64();
    Ok(Value::Num(float_from_f64(secs)?))
}

fn time_sleep(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let secs = if args.is_empty() {
        0.0
    } else {
        expect_num_f64("sleep", args, 0)?
    };
    std::thread::sleep(std::time::Duration::from_secs_f64(secs.max(0.0)));
    Ok(Value::None)
}

fn time_sleep_ms(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let ms = if args.is_empty() {
        0
    } else {
        expect_int("sleep_ms", args, 0)?.max(0) as u64
    };
    std::thread::sleep(std::time::Duration::from_millis(ms));
    Ok(Value::None)
}

fn text_upper(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Text(expect_text("upper", args, 0)?.to_uppercase()))
}

fn text_lower(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Text(expect_text("lower", args, 0)?.to_lowercase()))
}

fn text_strip(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Text(expect_text("strip", args, 0)?.trim().to_string()))
}

fn text_split(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("split", args, 0)?;
    let sep = if args.len() > 1 {
        expect_text("split", args, 1)?
    } else {
        " ".into()
    };
    let parts: Vec<Value> = s.split(&sep).map(|p| Value::Text(p.to_string())).collect();
    Ok(Value::List(Shared::new(parts)))
}

fn text_contains(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("contains requires 2 arguments"));
    }
    let hay = expect_text("contains", args, 0)?;
    let needle = args[1].print_string();
    Ok(Value::Bool(hay.contains(&needle)))
}

fn text_len(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Num(Num::Small(
        expect_text("len", args, 0)?.chars().count() as i64,
    )))
}

fn text_replace(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err(
            "replace requires 3 arguments",
        ));
    }
    let s = expect_text("replace", args, 0)?;
    let from = expect_text("replace", args, 1)?;
    let to = expect_text("replace", args, 2)?;
    Ok(Value::Text(s.replace(&from, &to)))
}

fn text_startswith(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    text_affix_check("startswith", args, |s, affix| s.starts_with(affix))
}

fn text_endswith(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    text_affix_check("endswith", args, |s, affix| s.ends_with(affix))
}

fn text_affix_check(
    name: &str,
    args: &[Value],
    check: impl FnOnce(&str, &str) -> bool,
) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(format!(
            "{name} requires 2 arguments"
        )));
    }
    let s = expect_text(name, args, 0)?;
    let affix = expect_text(name, args, 1)?;
    Ok(Value::Bool(check(&s, &affix)))
}

fn text_find(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("find requires 2 arguments"));
    }
    let s = expect_text("find", args, 0)?;
    let needle = expect_text("find", args, 1)?;
    Ok(match s.find(&needle) {
        Some(i) => Value::Num(Num::Small(s[..i].chars().count() as i64)),
        None => Value::Num(Num::Small(-1)),
    })
}

fn text_join(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("join requires 2 arguments"));
    }
    let sep = expect_text("join", args, 0)?;
    let parts = value_to_list(&args[1])?;
    let joined = parts
        .iter()
        .map(|v| v.print_string())
        .collect::<Vec<_>>()
        .join(&sep);
    Ok(Value::Text(joined))
}

fn text_repeat(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("repeat requires 2 arguments"));
    }
    let s = expect_text("repeat", args, 0)?;
    let n = expect_int("repeat", args, 1)?.max(0) as usize;
    Ok(Value::Text(s.repeat(n)))
}

fn text_is_digit(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    text_char_predicate("is_digit", args, |c| c.is_ascii_digit())
}

fn text_is_alpha(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    text_char_predicate("is_alpha", args, |c| c.is_ascii_alphabetic())
}

fn text_is_space(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    text_char_predicate("is_space", args, |c| c.is_whitespace())
}

fn text_char_predicate(
    name: &str,
    args: &[Value],
    pred: impl Fn(char) -> bool,
) -> Result<Value> {
    let s = expect_text(name, args, 0)?;
    Ok(Value::Bool(!s.is_empty() && s.chars().all(pred)))
}

fn text_count(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "count requires 2 arguments (s, sub)",
        ));
    }
    let s = expect_text("count", args, 0)?;
    let sub = expect_text("count", args, 1)?;
    if sub.is_empty() {
        return Ok(Value::Num(Num::Small((s.chars().count() + 1) as i64)));
    }
    Ok(Value::Num(Num::Small(s.matches(&sub).count() as i64)))
}

fn text_lines(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("lines", args, 0)?;
    let lines: Vec<Value> = s.lines().map(|l| Value::Text(l.to_string())).collect();
    Ok(Value::List(Shared::new(lines)))
}

fn text_ord(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("ord", args, 0)?;
    let ch = s.chars().next().ok_or_else(|| {
        crate::error::RuntimeError::type_err("ord requires a non-empty text")
    })?;
    Ok(Value::Num(Num::Small(ch as u32 as i64)))
}

fn text_chr(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let n = expect_int("chr", args, 0)?;
    if !(0..=0x10FFFF).contains(&n) {
        return Err(crate::error::RuntimeError::value_err("chr code point out of range"));
    }
    let Some(ch) = char::from_u32(n as u32) else {
        return Err(crate::error::RuntimeError::value_err("chr invalid code point"));
    };
    Ok(Value::Text(ch.to_string()))
}

fn path_join(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    // 语言层路径统一用 `/`（跨平台可移植），不随 OS 改 separator。
    let parts: Vec<String> = args.iter().map(|v| v.print_string()).collect();
    Ok(Value::Text(parts.join("/")))
}

fn path_map_text(
    name: &str,
    args: &[Value],
    map: impl FnOnce(&std::path::Path) -> String,
) -> Result<Value> {
    let p = expect_text(name, args, 0)?;
    Ok(Value::Text(map(std::path::Path::new(&p))))
}

fn path_os_str_component(
    name: &str,
    args: &[Value],
    pick: impl FnOnce(&std::path::Path) -> Option<&std::ffi::OsStr>,
) -> Result<Value> {
    path_map_text(name, args, |p| {
        pick(p)
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    })
}

fn path_basename(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    path_os_str_component("basename", args, |p| p.file_name())
}

fn path_dirname(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    path_map_text("dirname", args, |p| {
        p.parent()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    })
}

fn path_extension(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    path_os_str_component("extension", args, |p| p.extension())
}

fn path_stem(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    path_os_str_component("stem", args, |p| p.file_stem())
}

fn path_is_absolute(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    // 纯词法判断，不触盘，不受沙箱限制。
    path_bool_query(&crate::caps::Capabilities::full(), "is_absolute", args, |p| p.is_absolute())
}

fn path_bool_query(
    caps: &crate::caps::Capabilities,
    name: &str,
    args: &[Value],
    query: impl FnOnce(&std::path::Path) -> bool,
) -> Result<Value> {
    let p = expect_text(name, args, 0)?;
    caps.check_fs(name, &p)?;
    Ok(Value::Bool(query(std::path::Path::new(&p))))
}

fn path_abspath(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("abspath", args, 0)?;
    let abs = match std::fs::canonicalize(&p) {
        Ok(path) => path,
        Err(_) => {
            let cwd = std::env::current_dir().map_err(|e| {
                crate::error::RuntimeError::io_err(format!("abspath failed: {e}"))
            })?;
            normalize_pathbuf(cwd.join(&p))
        }
    };
    Ok(Value::Text(abs.to_string_lossy().to_string()))
}

fn normalize_pathbuf(path: std::path::PathBuf) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn path_normalize(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("normalize", args, 0)?;
    Ok(Value::Text(
        normalize_pathbuf(std::path::PathBuf::from(p))
            .to_string_lossy()
            .to_string(),
    ))
}

fn path_splitext(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("splitext", args, 0)?;
    let path = std::path::Path::new(&p);
    let (root, ext) = if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        let full = p.trim_end_matches(ext).trim_end_matches('.');
        (full.to_string(), ext.to_string())
    } else {
        (p, String::new())
    };
    Ok(Value::List(Shared::new(vec![
        Value::Text(root),
        Value::Text(ext),
    ])))
}

fn fs_exists(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    path_bool_query(&vm.caps, "exists", args, |p| p.exists())
}

fn fs_is_file(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    path_bool_query(&vm.caps, "is_file", args, |p| p.is_file())
}

fn fs_is_dir(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    path_bool_query(&vm.caps, "is_dir", args, |p| p.is_dir())
}

fn fs_list_dir(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("list_dir", args, 0)?;
    vm.caps.check_fs("list_dir", &p)?;
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&p)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("list_dir failed: {e}")))?
    {
        let entry =
            entry.map_err(|e| crate::error::RuntimeError::io_err(format!("list_dir failed: {e}")))?;
        names.push(Value::Text(
            entry.file_name().to_string_lossy().to_string(),
        ));
    }
    names.sort_by_key(|a| a.print_string());
    vm.request_cooperative_yield();
    Ok(Value::List(Shared::new(names)))
}

fn fs_mkdir(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("mkdir", args, 0)?;
    vm.caps.check_fs("mkdir", &p)?;
    std::fs::create_dir(&p)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("mkdir failed: {e}")))?;
    Ok(Value::None)
}

fn fs_mkdir_all(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("mkdir_all", args, 0)?;
    vm.caps.check_fs("mkdir_all", &p)?;
    std::fs::create_dir_all(&p)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("mkdir_all failed: {e}")))?;
    Ok(Value::None)
}

fn fs_remove(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("remove", args, 0)?;
    vm.caps.check_fs("remove", &p)?;
    std::fs::remove_file(&p)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("remove failed: {e}")))?;
    Ok(Value::None)
}

fn fs_remove_dir(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("remove_dir", args, 0)?;
    vm.caps.check_fs("remove_dir", &p)?;
    std::fs::remove_dir_all(&p)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("remove_dir failed: {e}")))?;
    Ok(Value::None)
}

fn fs_rename(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("rename requires 2 arguments"));
    }
    let from = expect_text("rename", args, 0)?;
    let to = expect_text("rename", args, 1)?;
    vm.caps.check_fs("rename", &from)?;
    vm.caps.check_fs("rename", &to)?;
    std::fs::rename(&from, &to)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("rename failed: {e}")))?;
    Ok(Value::None)
}

fn fs_copy(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("copy requires 2 arguments"));
    }
    let from = expect_text("copy", args, 0)?;
    let to = expect_text("copy", args, 1)?;
    vm.caps.check_fs("copy", &from)?;
    vm.caps.check_fs("copy", &to)?;
    std::fs::copy(&from, &to)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("copy failed: {e}")))?;
    Ok(Value::None)
}

fn os_getenv(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let key = expect_text("getenv", args, 0)?;
    Ok(std::env::var(&key)
        .map(Value::Text)
        .unwrap_or(Value::None))
}

fn os_setenv(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("setenv requires 2 arguments"));
    }
    vm.caps.check_env("setenv")?;
    let key = expect_text("setenv", args, 0)?;
    let val = args[1].print_string();
    std::env::set_var(key, val);
    Ok(Value::None)
}

fn os_args(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    let items: Vec<Value> = std::env::args().map(Value::Text).collect();
    Ok(Value::List(Shared::new(items)))
}

fn os_exit(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    // 与全局 `exit` 共用同一退出语义。
    crate::builtins::call_exit(vm, args)
}

fn os_cwd(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    let cwd = std::env::current_dir()
        .map_err(|e| crate::error::RuntimeError::io_err(format!("cwd failed: {e}")))?;
    Ok(Value::Text(cwd.to_string_lossy().to_string()))
}

fn os_chdir(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("chdir", args, 0)?;
    vm.caps.check_env("chdir")?;
    std::env::set_current_dir(&p)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("chdir failed: {e}")))?;
    Ok(Value::None)
}

fn os_name(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    Ok(Value::Text(std::env::consts::OS.to_string()))
}

fn json_parse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("parse", args, 0)?;
    let mut p = JsonParser {
        chars: s.chars().collect(),
        i: 0,
    };
    let v = p.parse_value()?;
    p.skip_ws();
    if p.i < p.chars.len() {
        return Err(crate::error::RuntimeError::msg(
            "json parse: trailing input",
        ));
    }
    Ok(v)
}

fn json_stringify(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("stringify requires 1 argument"));
    }
    Ok(Value::Text(json_stringify_value(&args[0])?))
}

fn json_parse_file(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("parse_file requires 1 argument"));
    }
    let path = expect_text("parse_file", args, 0)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("parse_file failed: {e}")))?;
    json_parse(vm, &[Value::Text(text)])
}

fn json_dump(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "dump requires 2 arguments (path, value)",
        ));
    }
    let path = expect_text("dump", args, 0)?;
    let text = json_stringify_value(&args[1])?;
    std::fs::write(&path, text)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("dump failed: {e}")))?;
    Ok(Value::None)
}

struct JsonParser {
    chars: Vec<char>,
    i: usize,
}

impl JsonParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }

    fn read_u_escape(&mut self) -> Result<u32> {
        let mut hex = String::new();
        for _ in 0..4 {
            let c = self.bump().ok_or_else(|| {
                crate::error::RuntimeError::msg("json parse: bad \\u escape")
            })?;
            hex.push(c);
        }
        u32::from_str_radix(&hex, 16)
            .map_err(|_| crate::error::RuntimeError::msg("json parse: bad \\u escape"))
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\n' | '\r' | '\t')) {
            self.i += 1;
        }
    }

    fn parse_value(&mut self) -> Result<Value> {
        self.skip_ws();
        match self.peek() {
            Some('n') => self.parse_literal("null", Value::None),
            Some('t') => self.parse_literal("true", Value::Bool(true)),
            Some('f') => self.parse_literal("false", Value::Bool(false)),
            Some('"') => Ok(Value::Text(self.parse_string()?)),
            Some('[') => self.parse_array(),
            Some('{') => self.parse_object(),
            Some('-') | Some('0'..='9') => self.parse_number(),
            Some(c) => Err(crate::error::RuntimeError::msg(format!(
                "json parse: unexpected '{c}'"
            ))),
            None => Err(crate::error::RuntimeError::msg("json parse: unexpected end")),
        }
    }

    fn parse_literal(&mut self, lit: &str, val: Value) -> Result<Value> {
        for ch in lit.chars() {
            if self.bump() != Some(ch) {
                return Err(crate::error::RuntimeError::msg(format!(
                    "json parse: expected {lit}"
                )));
            }
        }
        Ok(val)
    }

    fn parse_string(&mut self) -> Result<String> {
        if self.bump() != Some('"') {
            return Err(crate::error::RuntimeError::msg(
                "json parse: expected string",
            ));
        }
        let mut out = String::new();
        loop {
            match self.bump() {
                Some('"') => return Ok(out),
                Some('\\') => match self.bump() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('b') => out.push('\u{0008}'),
                    Some('f') => out.push('\u{000c}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let code = self.read_u_escape()?;
                        if (0xD800..=0xDBFF).contains(&code) {
                            // 高代理：必须紧跟 \uXXXX 低代理
                            if self.bump() != Some('\\') || self.bump() != Some('u') {
                                return Err(crate::error::RuntimeError::value_err(
                                    "json parse: lonely high surrogate",
                                ));
                            }
                            let low = self.read_u_escape()?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return Err(crate::error::RuntimeError::value_err(
                                    "json parse: invalid low surrogate",
                                ));
                            }
                            let cp = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            out.push(char::from_u32(cp).ok_or_else(|| {
                                crate::error::RuntimeError::value_err(
                                    "json parse: invalid unicode",
                                )
                            })?);
                        } else if (0xDC00..=0xDFFF).contains(&code) {
                            return Err(crate::error::RuntimeError::value_err(
                                "json parse: lonely low surrogate",
                            ));
                        } else {
                            out.push(char::from_u32(code).ok_or_else(|| {
                                crate::error::RuntimeError::value_err(
                                    "json parse: invalid unicode",
                                )
                            })?);
                        }
                    }
                    _ => {
                        return Err(crate::error::RuntimeError::msg(
                            "json parse: bad escape",
                        ))
                    }
                },
                Some(c) => out.push(c),
                None => {
                    return Err(crate::error::RuntimeError::msg(
                        "json parse: unterminated string",
                    ))
                }
            }
        }
    }

    fn parse_number(&mut self) -> Result<Value> {
        let start = self.i;
        if self.peek() == Some('-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some('0'..='9')) {
            self.i += 1;
        }
        let mut is_float = false;
        if self.peek() == Some('.') {
            is_float = true;
            self.i += 1;
            while matches!(self.peek(), Some('0'..='9')) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            is_float = true;
            self.i += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.i += 1;
            }
            while matches!(self.peek(), Some('0'..='9')) {
                self.i += 1;
            }
        }
        let s: String = self.chars[start..self.i].iter().collect();
        if is_float {
            let f: f64 = s
                .parse()
                .map_err(|_| crate::error::RuntimeError::msg("json parse: bad number"))?;
            Ok(Value::Num(float_from_f64(f)?))
        } else if let Ok(n) = s.parse::<i64>() {
            Ok(Value::Num(Num::Small(n)))
        } else {
            Ok(Value::Num(Num::from_bigint(
                BigInt::parse_bytes(s.as_bytes(), 10).ok_or_else(|| {
                    crate::error::RuntimeError::msg("json parse: bad number")
                })?,
            )))
        }
    }

    fn parse_array(&mut self) -> Result<Value> {
        self.bump(); // [
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(']') {
            self.bump();
            return Ok(Value::List(Shared::new(items)));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.bump() {
                Some(',') => {
                    self.skip_ws();
                    continue;
                }
                Some(']') => break,
                _ => {
                    return Err(crate::error::RuntimeError::msg(
                        "json parse: expected ',' or ']'",
                    ))
                }
            }
        }
        Ok(Value::List(Shared::new(items)))
    }

    fn parse_object(&mut self) -> Result<Value> {
        self.bump(); // {
        self.skip_ws();
        let mut map = DictMap::new();
        if self.peek() == Some('}') {
            self.bump();
            return Ok(Value::Dict(Shared::new(map)));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.bump() != Some(':') {
                return Err(crate::error::RuntimeError::msg(
                    "json parse: expected ':'",
                ));
            }
            let val = self.parse_value()?;
            map.insert(ValueKey::from_value(&Value::Text(key))?, val);
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some('}') => break,
                _ => {
                    return Err(crate::error::RuntimeError::msg(
                        "json parse: expected ',' or '}'",
                    ))
                }
            }
        }
        Ok(Value::Dict(Shared::new(map)))
    }
}

fn json_stringify_value(v: &Value) -> Result<String> {
    Ok(match v {
        Value::None => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => n.to_string(),
        Value::Text(s) => json_escape_string(s),
        Value::List(l) => {
            let parts: Result<Vec<_>> = l.borrow().iter().map(json_stringify_value).collect();
            format!("[{}]", parts?.join(","))
        }
        Value::Dict(d) => {
            let mut parts = Vec::new();
            for (k, val) in d.borrow().iter() {
                let key = match value_key_to_value(k) {
                    Value::Text(s) => s,
                    other => other.print_string(),
                };
                parts.push(format!(
                    "{}:{}",
                    json_escape_string(&key),
                    json_stringify_value(val)?
                ));
            }
            format!("{{{}}}", parts.join(","))
        }
        Value::Set(s) => {
            let parts: Result<Vec<_>> = s
                .borrow()
                .iter()
                .map(|k| json_stringify_value(&value_key_to_value(k)))
                .collect();
            format!("[{}]", parts?.join(","))
        }
        other => json_escape_string(&other.print_string()),
    })
}

fn json_escape_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn test_assert_eq(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("assert_eq requires 2 arguments"));
    }
    if args[0].print_string() != args[1].print_string() {
        let exc = crate::exceptions::make_exception(
            vm,
            "AssertionError",
            format!("{} != {}", args[0].print_string(), args[1].print_string()),
        )?;
        vm.throw_value(exc)?;
    }
    Ok(Value::None)
}

fn test_assert_true(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("assert_true requires 1 argument"));
    }
    if !args[0].is_truthy() {
        let exc = crate::exceptions::make_exception(vm, "AssertionError", "assertion failed")?;
        vm.throw_value(exc)?;
    }
    Ok(Value::None)
}

fn test_assert_raises(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "assert_raises requires 2 arguments (function, exception_type)",
        ));
    }
    let func = expect_function("assert_raises", args, 0)?;
    let exc_type = match args[1].as_type_name_operand() {
        Some(name) => name.to_string(),
        None => match &args[1] {
            Value::TypeSpec(spec) => spec.name.clone(),
            other => other.type_name_string(),
        },
    };
    match vm.call_user_function_catching(func, vec![])? {
        Ok(returned) => {
            let exc = crate::exceptions::make_exception(
                vm,
                "AssertionError",
                format!("expected {exc_type}, but no exception was raised (got {returned})"),
            )?;
            vm.throw_value(exc)?;
            Ok(Value::None)
        }
        Err(thrown) => {
            if !crate::exceptions::struct_is_a(vm, &thrown, &exc_type) {
                let exc = crate::exceptions::make_exception(
                    vm,
                    "AssertionError",
                    format!(
                        "expected {}, got {}",
                        exc_type,
                        thrown.type_name()
                    ),
                )?;
                vm.throw_value(exc)?;
            }
            Ok(Value::None)
        }
    }
}

fn debug_traceback(vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    Ok(crate::traceback::capture_traceback(vm))
}

fn debug_format_tb(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("format_tb requires 1 argument"));
    }
    Ok(Value::Text(args[0].display_string()))
}

fn debug_print_tb(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("print_tb requires 1 argument"));
    }
    println!("{}", args[0].display_string());
    Ok(Value::None)
}

fn debug_format_exception(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "format_exception requires 1 argument",
        ));
    }
    let exc = &args[0];
    let ty = exc.type_name_string();
    let msg = match exc {
        Value::Struct(s) => s
            .slots
            .borrow()
            .first()
            .map(|v| v.print_string())
            .unwrap_or_default(),
        other => other.print_string(),
    };
    let tb = crate::traceback::get_exception_traceback(exc)
        .map(|t| t.display_string())
        .unwrap_or_default();
    Ok(Value::Text(format!("{ty}: {msg}\n{tb}")))
}

fn debug_type_name(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("type_name requires 1 argument"));
    }
    Ok(Value::Text(args[0].type_name_string()))
}

fn debug_breakpoint(vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    if let Some(dbg) = &vm.debug {
        dbg.borrow_mut()
            .request_break(crate::debug::StopReason::Explicit);
    }
    Ok(Value::None)
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
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E3779B97F4A7C15)
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
        return Err(crate::error::RuntimeError::type_err("randint requires 2 arguments"));
    }
    let lo = expect_int("randint", args, 0)?;
    let hi = expect_int("randint", args, 1)?;
    if hi < lo {
        return Err(crate::error::RuntimeError::value_err(
            "randint: hi must be >= lo",
        ));
    }
    let span = (hi - lo + 1) as u64;
    let n = lo + rng_bounded(span) as i64;
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
        return Err(crate::error::RuntimeError::type_err("choice requires 1 argument"));
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
        return Err(crate::error::RuntimeError::type_err("shuffle requires 1 argument"));
    }
    let Value::List(list) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err("shuffle requires list"));
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

fn build_re_module() -> Shared<ModuleObject> {
    submodule(
        "re",
        &[("compile", builtin(re_compile)),
            ("match", builtin(re_match)),
            ("findall", builtin(re_findall)),
            ("sub", builtin(re_sub)),
            ("split", builtin(re_split)),],
    )
}

fn build_hash_module() -> Shared<ModuleObject> {
    submodule(
        "hash",
        &[("md5", builtin(hash_md5)),
            ("sha256", builtin(hash_sha256)),
            ("hmac", builtin(hash_hmac)),],
    )
}

fn build_exceptions_module() -> Shared<ModuleObject> {
    submodule(
        "exceptions",
        &[("bases", builtin(exc_bases)),
            ("chain", builtin(exc_chain)),
            ("tree", builtin(exc_tree)),],
    )
}

fn re_compile(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let pat = expect_text("compile", args, 0)?;
    let re = regex::Regex::new(&pat)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("invalid regex: {e}")))?;
    let re = Arc::new(re);
    let re_m = re.clone();
    let re_f = re.clone();
    let re_s = re.clone();
    let re_p = re.clone();
    Ok(Value::Module(Shared::new(ModuleObject {
        name: "Pattern".into(),
        full_name: format!("re.Pattern({pat})"),
        exports: exports(&[
            (
                "match",
                Value::Builtin(Arc::new(move |vm, a| re_match_impl(vm, &re_m, a))),
            ),
            (
                "findall",
                Value::Builtin(Arc::new(move |_vm, a| {
                    let text = expect_text("findall", a, 0)?;
                    let out: Vec<Value> = re_f
                        .find_iter(&text)
                        .map(|m| Value::Text(m.as_str().to_string()))
                        .collect();
                    Ok(Value::List(Shared::new(out)))
                })),
            ),
            (
                "sub",
                Value::Builtin(Arc::new(move |_vm, a| {
                    if a.len() != 2 {
                        return Err(crate::error::RuntimeError::type_err(
                            "Pattern.sub requires (repl, text)",
                        ));
                    }
                    let repl = expect_text("sub", a, 0)?;
                    let text = expect_text("sub", a, 1)?;
                    Ok(Value::Text(
                        re_s.replace_all(&text, repl.as_str()).into_owned(),
                    ))
                })),
            ),
            (
                "split",
                Value::Builtin(Arc::new(move |_vm, a| {
                    let text = expect_text("split", a, 0)?;
                    let out: Vec<Value> = re_p
                        .split(&text)
                        .map(|s| Value::Text(s.to_string()))
                        .collect();
                    Ok(Value::List(Shared::new(out)))
                })),
            ),
            ("pattern", Value::Text(pat)),
        ]),
        children: HashMap::new(),
        is_user: false,
    })))
}

fn re_match(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("match requires 2 arguments"));
    }
    let pat = expect_text("match", args, 0)?;
    let text = expect_text("match", args, 1)?;
    let re = regex::Regex::new(&pat)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("invalid regex: {e}")))?;
    re_match_impl(vm, &re, &[Value::Text(text)])
}

fn re_match_impl(_vm: &mut Vm, re: &regex::Regex, args: &[Value]) -> Result<Value> {
    let text = expect_text("match", args, 0)?;
    let m = re.find(&text);
    Ok(match m {
        Some(mat) => {
            let mut d = DictMap::new();
            d.insert(
                ValueKey::Text("0".into()),
                Value::Text(mat.as_str().to_string()),
            );
            d.insert(
                ValueKey::Text("start".into()),
                Value::Num(Num::Small(mat.start() as i64)),
            );
            d.insert(
                ValueKey::Text("end".into()),
                Value::Num(Num::Small(mat.end() as i64)),
            );
            Value::Dict(Shared::new(d))
        }
        None => Value::None,
    })
}

fn re_findall(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("findall requires 2 arguments"));
    }
    let pat = expect_text("findall", args, 0)?;
    let text = expect_text("findall", args, 1)?;
    let re = regex::Regex::new(&pat)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("invalid regex: {e}")))?;
    let out: Vec<Value> = re
        .find_iter(&text)
        .map(|m| Value::Text(m.as_str().to_string()))
        .collect();
    Ok(Value::List(Shared::new(out)))
}

fn re_sub(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err("sub requires 3 arguments"));
    }
    let pat = expect_text("sub", args, 0)?;
    let repl = expect_text("sub", args, 1)?;
    let text = expect_text("sub", args, 2)?;
    let re = regex::Regex::new(&pat)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("invalid regex: {e}")))?;
    Ok(Value::Text(re.replace_all(&text, repl.as_str()).into_owned()))
}

fn re_split(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("split requires 2 arguments"));
    }
    let pat = expect_text("split", args, 0)?;
    let text = expect_text("split", args, 1)?;
    let re = regex::Regex::new(&pat)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("invalid regex: {e}")))?;
    let out: Vec<Value> = re
        .split(&text)
        .map(|s| Value::Text(s.to_string()))
        .collect();
    Ok(Value::List(Shared::new(out)))
}

fn hash_md5(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    use md5::Md5;
    use digest::Digest;
    let text = expect_text("md5", args, 0)?;
    let digest = Md5::digest(text.as_bytes());
    Ok(Value::Text(hex::encode(digest)))
}

fn hash_sha256(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    use sha2::{Digest, Sha256};
    let text = expect_text("sha256", args, 0)?;
    let digest = Sha256::digest(text.as_bytes());
    Ok(Value::Text(hex::encode(digest)))
}

fn hash_hmac(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    use hmac::{Hmac, Mac};
    use sha2::{Sha256, Sha512};
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err(
            "hmac requires (key, msg, algo)",
        ));
    }
    let key = expect_text("hmac", args, 0)?;
    let msg = expect_text("hmac", args, 1)?;
    let algo = expect_text("hmac", args, 2)?;
    match algo.as_str() {
        "sha256" => {
            type H = Hmac<Sha256>;
            let mut mac = H::new_from_slice(key.as_bytes())
                .map_err(|e| crate::error::RuntimeError::msg(format!("hmac key error: {e}")))?;
            mac.update(msg.as_bytes());
            Ok(Value::Text(hex::encode(mac.finalize().into_bytes())))
        }
        "sha512" => {
            type H = Hmac<Sha512>;
            let mut mac = H::new_from_slice(key.as_bytes())
                .map_err(|e| crate::error::RuntimeError::msg(format!("hmac key error: {e}")))?;
            mac.update(msg.as_bytes());
            Ok(Value::Text(hex::encode(mac.finalize().into_bytes())))
        }
        other => Err(crate::error::RuntimeError::value_err(format!(
            "hmac unsupported algo '{other}' (use sha256 or sha512)"
        ))),
    }
}

fn exc_bases(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let name = match args.first() {
        Some(Value::Text(s)) => s.as_str(),
        _ => return Err(crate::error::RuntimeError::type_err("bases requires exception type name")),
    };
    Ok(match crate::exceptions::direct_base(vm, name) {
        Some(base) => Value::Text(base),
        None => Value::None,
    })
}

fn exc_chain(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let name = match args.first() {
        Some(Value::Text(s)) => s.as_str(),
        _ => return Err(crate::error::RuntimeError::type_err("chain requires exception type name")),
    };
    let chain: Vec<Value> = crate::exceptions::inheritance_chain(vm, name)
        .into_iter()
        .map(Value::Text)
        .collect();
    Ok(Value::List(Shared::new(chain)))
}

fn exc_tree(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    let mut out = DictMap::new();
    for (kind, base) in crate::exceptions::exception_hierarchy() {
        out.insert(
            ValueKey::Text(kind.type_name().to_string()),
            base.map(|b| Value::Text(b.type_name().to_string()))
                .unwrap_or(Value::None),
        );
    }
    Ok(Value::Dict(Shared::new(out)))
}

// ── std.http ──────────────────────────────────────────────────────────

fn build_http_module() -> Shared<ModuleObject> {
    submodule(
        "http",
        &[
            ("get", builtin(http_get)),
            ("post", builtin(http_post)),
            ("put", builtin(http_put)),
            ("delete", builtin(http_delete)),
            ("patch", builtin(http_patch)),
            ("head", builtin(http_head)),
            ("request", builtin(http_request)),
        ],
    )
}

fn extract_headers(name: &str, opts: &Value) -> Result<reqwest::header::HeaderMap> {
    let mut headers = reqwest::header::HeaderMap::new();
    // 只读 opts.headers 子表；避免 timeout/proxy/body 等控制字段被误当作请求头。
    let hdrs = match opts {
        Value::Dict(d) => d.borrow().get(&ValueKey::Text("headers".into())).cloned(),
        _ => None,
    };
    if let Some(Value::Dict(d)) = hdrs {
        for (k, v) in d.borrow().iter() {
            let key_str = match k {
                ValueKey::Text(s) => s.as_str(),
                _ => continue,
            };
            let val_str = match v {
                Value::Text(s) => s.as_str(),
                Value::Num(n) => &n.to_string(),
                Value::Bool(b) => &b.to_string(),
                _ => continue,
            };
            let hn = reqwest::header::HeaderName::try_from(key_str)
                .map_err(|e| crate::error::RuntimeError::value_err(format!("{name}: invalid header name '{key_str}': {e}")))?;
            let hv = reqwest::header::HeaderValue::try_from(val_str)
                .map_err(|e| crate::error::RuntimeError::value_err(format!("{name}: invalid header value: {e}")))?;
            headers.insert(hn, hv);
        }
    }
    Ok(headers)
}

fn opt_str(opts: &Value, key: &str) -> Option<String> {
    if let Value::Dict(d) = opts {
        if let Some(Value::Text(s)) = d.borrow().get(&ValueKey::Text(key.into())) {
            return Some(s.clone());
        }
    }
    None
}

fn opt_bool(opts: &Value, key: &str) -> Option<bool> {
    if let Value::Dict(d) = opts {
        if let Some(Value::Bool(b)) = d.borrow().get(&ValueKey::Text(key.into())) {
            return Some(*b);
        }
    }
    None
}

fn opt_num(opts: &Value, key: &str) -> Option<i64> {
    if let Value::Dict(d) = opts {
        if let Some(Value::Num(n)) = d.borrow().get(&ValueKey::Text(key.into())) {
            return n.to_i64();
        }
    }
    None
}

fn extract_timeout(opts: &Value) -> Option<std::time::Duration> {
    if let Value::Dict(d) = opts {
        if let Some(Value::Num(n)) = d.borrow().get(&ValueKey::Text("timeout".into())) {
            if let Some(secs) = n.to_i64() {
                return Some(std::time::Duration::from_secs(secs.max(0) as u64));
            }
        }
    }
    None
}

fn response_to_dict(resp: reqwest::blocking::Response) -> Result<Value> {
    let status = resp.status().as_u16();
    let url = resp.url().to_string();
    let mut header_map = DictMap::new();
    for (k, v) in resp.headers().iter() {
        let val = v.to_str().unwrap_or("");
        header_map.insert(
            ValueKey::Text(k.as_str().to_string()),
            Value::Text(val.to_string()),
        );
    }
    let body = resp
        .text()
        .map_err(|e| crate::error::RuntimeError::io_err(format!("http: failed to read body: {e}")))?;
    let mut out = DictMap::new();
    out.insert(ValueKey::Text("status".into()), Value::Num(Num::Small(status as i64)));
    out.insert(ValueKey::Text("body".into()), Value::Text(body));
    out.insert(ValueKey::Text("headers".into()), Value::Dict(Shared::new(header_map)));
    out.insert(
        ValueKey::Text("ok".into()),
        Value::Bool((200..300).contains(&status)),
    );
    out.insert(ValueKey::Text("url".into()), Value::Text(url));
    Ok(Value::Dict(Shared::new(out)))
}

fn build_client(opts: &Value) -> Result<reqwest::blocking::Client> {
    let mut builder = reqwest::blocking::Client::builder();
    if let Some(dur) = extract_timeout(opts) {
        builder = builder.timeout(dur);
    }
    // 代理：opts.proxy = "http://host:port" 或 socks5
    if let Some(p) = opt_str(opts, "proxy") {
        let proxy = reqwest::Proxy::all(&p).map_err(|e| {
            crate::error::RuntimeError::value_err(format!("http: invalid proxy '{p}': {e}"))
        })?;
        builder = builder.proxy(proxy);
    }
    // User-Agent
    if let Some(ua) = opt_str(opts, "user_agent") {
        builder = builder.user_agent(ua);
    }
    // 跟随重定向：bool（开/关）或 num（最大次数）
    if let Some(follow) = opt_bool(opts, "follow_redirects") {
        builder = builder.redirect(if follow {
            reqwest::redirect::Policy::default()
        } else {
            reqwest::redirect::Policy::none()
        });
    } else if let Some(n) = opt_num(opts, "follow_redirects") {
        builder = builder.redirect(reqwest::redirect::Policy::limited(n.max(0) as usize));
    }
    builder
        .build()
        .map_err(|e| crate::error::RuntimeError::io_err(format!("http: failed to build client: {e}")))
}

/// 从 opts.auth（"user:pass" 或 {user,pass}）解析基本认证，按请求应用。
fn apply_auth(
    mut req: reqwest::blocking::RequestBuilder,
    opts: &Value,
) -> reqwest::blocking::RequestBuilder {
    if let Some(auth) = opt_str(opts, "auth") {
        if let Some(idx) = auth.find(':') {
            let (u, p) = auth.split_at(idx);
            req = req.basic_auth(u, Some(&p[1..]));
        }
    } else if let Value::Dict(d) = opts {
        let auth_val = d.borrow().get(&ValueKey::Text("auth".into())).cloned();
        if let Some(Value::Dict(ad)) = auth_val {
            let user = match ad.borrow().get(&ValueKey::Text("user".into())) {
                Some(Value::Text(s)) => s.clone(),
                _ => String::new(),
            };
            let pass = match ad.borrow().get(&ValueKey::Text("pass".into())) {
                Some(Value::Text(s)) => Some(s.clone()),
                _ => None,
            };
            req = req.basic_auth(user, pass);
        }
    }
    req
}

fn http_get(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("http.get")?;
    let url = expect_text("http.get", args, 0)?;
    let opts = args.get(1).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    let mut req = client.get(url.as_str());
    if let Value::Dict(_) = &opts {
        req = req.headers(extract_headers("http.get", &opts)?);
    }
    req = apply_auth(req, &opts);
    let resp = req
        .send()
        .map_err(|e| crate::error::RuntimeError::io_err(format!("http.get '{url}' failed: {e}")))?;
    let r = response_to_dict(resp);
    if r.is_ok() {
        vm.request_cooperative_yield();
    }
    r
}

fn http_post(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("http.post")?;
    let url = expect_text("http.post", args, 0)?;
    let body = expect_text("http.post", args, 1)?;
    let opts = args.get(2).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    let mut req = client.post(url.as_str()).body(body);
    if let Value::Dict(_) = &opts {
        req = req.headers(extract_headers("http.post", &opts)?);
    }
    req = apply_auth(req, &opts);
    let resp = req
        .send()
        .map_err(|e| crate::error::RuntimeError::io_err(format!("http.post '{url}' failed: {e}")))?;
    let r = response_to_dict(resp);
    if r.is_ok() {
        vm.request_cooperative_yield();
    }
    r
}

fn http_put(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("http.put")?;
    let url = expect_text("http.put", args, 0)?;
    let body = expect_text("http.put", args, 1)?;
    let opts = args.get(2).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    let mut req = client.put(url.as_str()).body(body);
    if let Value::Dict(_) = &opts {
        req = req.headers(extract_headers("http.put", &opts)?);
    }
    req = apply_auth(req, &opts);
    let resp = req
        .send()
        .map_err(|e| crate::error::RuntimeError::io_err(format!("http.put '{url}' failed: {e}")))?;
    let r = response_to_dict(resp);
    if r.is_ok() {
        vm.request_cooperative_yield();
    }
    r
}

fn http_delete(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("http.delete")?;
    let url = expect_text("http.delete", args, 0)?;
    let opts = args.get(1).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    let mut req = client.delete(url.as_str());
    if let Value::Dict(_) = &opts {
        req = req.headers(extract_headers("http.delete", &opts)?);
    }
    req = apply_auth(req, &opts);
    let resp = req
        .send()
        .map_err(|e| crate::error::RuntimeError::io_err(format!("http.delete '{url}' failed: {e}")))?;
    let r = response_to_dict(resp);
    if r.is_ok() {
        vm.request_cooperative_yield();
    }
    r
}

fn http_patch(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("http.patch")?;
    let url = expect_text("http.patch", args, 0)?;
    let body = expect_text("http.patch", args, 1)?;
    let opts = args.get(2).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    let mut req = client.patch(url.as_str()).body(body);
    if let Value::Dict(_) = &opts {
        req = req.headers(extract_headers("http.patch", &opts)?);
    }
    req = apply_auth(req, &opts);
    let resp = req
        .send()
        .map_err(|e| crate::error::RuntimeError::io_err(format!("http.patch '{url}' failed: {e}")))?;
    let r = response_to_dict(resp);
    if r.is_ok() {
        vm.request_cooperative_yield();
    }
    r
}

fn http_head(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("http.head")?;
    let url = expect_text("http.head", args, 0)?;
    let opts = args.get(1).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    let mut req = client.head(url.as_str());
    if let Value::Dict(_) = &opts {
        req = req.headers(extract_headers("http.head", &opts)?);
    }
    req = apply_auth(req, &opts);
    let resp = req
        .send()
        .map_err(|e| crate::error::RuntimeError::io_err(format!("http.head '{url}' failed: {e}")))?;
    let r = response_to_dict(resp);
    if r.is_ok() {
        vm.request_cooperative_yield();
    }
    r
}

fn http_request(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("http.request")?;
    let method = expect_text("http.request", args, 0)?;
    let url = expect_text("http.request", args, 1)?;
    let opts = args.get(2).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    let m = method.to_uppercase();
    let mut req_builder = match m.as_str() {
        "GET" => client.get(url.as_str()),
        "POST" => client.post(url.as_str()),
        "PUT" => client.put(url.as_str()),
        "DELETE" => client.delete(url.as_str()),
        "PATCH" => client.patch(url.as_str()),
        "HEAD" => client.head(url.as_str()),
        other => {
            return Err(crate::error::RuntimeError::type_err(format!(
                "http.request: unsupported method '{other}'"
            )));
        }
    };
    if let Value::Dict(_) = &opts {
        req_builder = req_builder.headers(extract_headers("http.request", &opts)?);
        if let Value::Dict(d) = &opts {
            if let Some(Value::Text(s)) = d.borrow().get(&ValueKey::Text("body".into())) {
                req_builder = req_builder.body(s.clone());
            }
        }
    }
    let req_builder = apply_auth(req_builder, &opts);
    let resp = req_builder
        .send()
        .map_err(|e| crate::error::RuntimeError::io_err(format!("http.request '{m} {url}' failed: {e}")))?;
    let r = response_to_dict(resp);
    if r.is_ok() {
        vm.request_cooperative_yield();
    }
    r
}

// ---------------------------------------------------------------------------
// std.encoding —— base64 / hex / url / gzip 编解码
// ---------------------------------------------------------------------------

fn enc_input_bytes(v: &Value) -> Result<Vec<u8>> {
    match v {
        Value::Text(s) => Ok(s.as_bytes().to_vec()),
        Value::Bytes(b) => Ok(b.as_ref().clone()),
        _ => Err(crate::error::RuntimeError::type_err(
            "expected text or bytes",
        )),
    }
}

fn enc_base64_encode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("base64_encode", args, 1)?;
    let data = enc_input_bytes(&args[0])?;
    use base64::Engine;
    Ok(Value::Text(
        base64::engine::general_purpose::STANDARD.encode(&data),
    ))
}

fn enc_base64_decode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("base64_decode", args, 1)?;
    let s = expect_text("base64_decode", args, 0)?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .map_err(|e| crate::error::RuntimeError::value_err(format!("base64_decode: {e}")))?;
    Ok(Value::Bytes(Arc::new(bytes)))
}

fn enc_hex_encode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("hex_encode", args, 1)?;
    let data = enc_input_bytes(&args[0])?;
    Ok(Value::Text(hex::encode(&data)))
}

fn enc_hex_decode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("hex_decode", args, 1)?;
    let s = expect_text("hex_decode", args, 0)?;
    let bytes = hex::decode(s)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("hex_decode: {e}")))?;
    Ok(Value::Bytes(Arc::new(bytes)))
}

/// URL 百分号编码：保留 unreserved 字符（A-Za-z0-9-._~），其余 %XX。
fn enc_url_encode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("url_encode", args, 1)?;
    let s = expect_text("url_encode", args, 0)?;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(Value::Text(out))
}

fn enc_url_decode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("url_decode", args, 1)?;
    let s = expect_text("url_decode", args, 0)?;
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            if i + 2 >= bytes.len() {
                return Err(crate::error::RuntimeError::value_err(
                    "url_decode: incomplete %XX escape",
                ));
            }
            let hi = hex_val(bytes[i + 1])?;
            let lo = hex_val(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else if b == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(b);
            i += 1;
        }
    }
    Ok(Value::Text(
        String::from_utf8(out)
            .map_err(|e| crate::error::RuntimeError::value_err(format!("url_decode: {e}")))?,
    ))
}

fn hex_val(b: u8) -> Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(crate::error::RuntimeError::value_err(format!(
            "url_decode: invalid hex digit '{}'",
            b as char
        ))),
    }
}

fn enc_gzip_encode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("gzip_encode", args, 1)?;
    let data = enc_input_bytes(&args[0])?;
    use flate2::write::GzEncoder;
    use std::io::Write;
    let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(&data)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("gzip_encode: {e}")))?;
    let out = encoder
        .finish()
        .map_err(|e| crate::error::RuntimeError::io_err(format!("gzip_encode: {e}")))?;
    Ok(Value::Bytes(Arc::new(out)))
}

fn enc_gzip_decode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("gzip_decode", args, 1)?;
    let data = enc_input_bytes(&args[0])?;
    use flate2::read::GzDecoder;
    use std::io::Read;
    let mut dec = GzDecoder::new(&data[..]);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("gzip_decode: {e}")))?;
    Ok(Value::Bytes(Arc::new(out)))
}

fn build_encoding_module() -> Shared<ModuleObject> {
    submodule(
        "encoding",
        &[
            ("base64_encode", builtin(enc_base64_encode)),
            ("base64_decode", builtin(enc_base64_decode)),
            ("hex_encode", builtin(enc_hex_encode)),
            ("hex_decode", builtin(enc_hex_decode)),
            ("url_encode", builtin(enc_url_encode)),
            ("url_decode", builtin(enc_url_decode)),
            ("gzip_encode", builtin(enc_gzip_encode)),
            ("gzip_decode", builtin(enc_gzip_decode)),
        ],
    )
}

// ---------------------------------------------------------------------------
// 数据格式解析标准库 —— csv / toml / yaml / xml
// ---------------------------------------------------------------------------

/// `serde_json::Value` → Optive `Value`（供 toml/yaml 经 serde 中转后复用）。
fn serde_json_to_optive(v: &serde_json::Value) -> Result<Value> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Value::None,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Num(Num::Small(i))
            } else if let Some(u) = n.as_u64() {
                if let Ok(i) = i64::try_from(u) {
                    Value::Num(Num::Small(i))
                } else {
                    Value::Num(Num::from_bigint(num_bigint::BigInt::from(u)))
                }
            } else if let Some(f) = n.as_f64() {
                Value::Num(float_from_f64(f)?)
            } else {
                Value::None
            }
        }
        J::String(s) => Value::Text(s.clone()),
        J::Array(a) => {
            let items: Result<Vec<_>> = a.iter().map(serde_json_to_optive).collect();
            Value::List(Shared::new(items?))
        }
        J::Object(o) => {
            let mut d = DictMap::new();
            for (k, val) in o.iter() {
                d.insert(ValueKey::Text(k.clone()), serde_json_to_optive(val)?);
            }
            Value::Dict(Shared::new(d))
        }
    })
}

// --- std.toml ---

fn toml_parse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("parse", args, 1)?;
    let s = expect_text("parse", args, 0)?;
    let toml_val: toml::Value = toml::from_str(&s)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("toml parse: {e}")))?;
    let jv = serde_json::to_value(&toml_val)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("toml convert: {e}")))?;
    serde_json_to_optive(&jv)
}

fn build_toml_module() -> Shared<ModuleObject> {
    submodule("toml", &[("parse", builtin(toml_parse))])
}

// --- std.yaml ---

fn yaml_parse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("parse", args, 1)?;
    let s = expect_text("parse", args, 0)?;
    let yaml_val: serde_yaml::Value = serde_yaml::from_str(&s)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("yaml parse: {e}")))?;
    let jv = serde_json::to_value(&yaml_val)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("yaml convert: {e}")))?;
    serde_json_to_optive(&jv)
}

fn build_yaml_module() -> Shared<ModuleObject> {
    submodule("yaml", &[("parse", builtin(yaml_parse))])
}

// --- std.csv ---

/// `parse(text, opts?)`：`opts.header`（默认 true）控制首行是否为字段名。
/// 有表头 → 返回 dict 列表；无表头 → 返回 list 列表。
fn csv_parse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "csv.parse requires at least 1 argument",
        ));
    }
    let text = expect_text("csv.parse", args, 0)?;
    // 默认 header=true；仅当 opts.header 显式为 false 时关闭。
    let header = match args.get(1) {
        Some(Value::Dict(d)) => !matches!(
            d.borrow().get(&ValueKey::Text("header".into())),
            Some(Value::Bool(false))
        ),
        _ => true,
    };

    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(header)
        .flexible(true)
        .from_reader(text.as_bytes());
    let rows: Vec<Vec<String>> = rdr
        .records()
        .map(|r| {
            r.map_err(|e| crate::error::RuntimeError::value_err(format!("csv parse: {e}")))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(|r| r.iter().map(|s| s.to_string()).collect())
        .collect();

    if header {
        let headers = rdr
            .headers()
            .map_err(|e| crate::error::RuntimeError::value_err(format!("csv headers: {e}")))?
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let out: Vec<Value> = rows
            .iter()
            .map(|row| {
                let mut d = DictMap::new();
                for (i, h) in headers.iter().enumerate() {
                    let v = row.get(i).map(|s| s.as_str()).unwrap_or("");
                    d.insert(ValueKey::Text(h.clone()), Value::Text(v.to_string()));
                }
                Value::Dict(Shared::new(d))
            })
            .collect();
        Ok(Value::List(Shared::new(out)))
    } else {
        let out: Vec<Value> = rows
            .iter()
            .map(|row| {
                Value::List(Shared::new(
                    row.iter()
                        .map(|s| Value::Text(s.clone()))
                        .collect(),
                ))
            })
            .collect();
        Ok(Value::List(Shared::new(out)))
    }
}

fn build_csv_module() -> Shared<ModuleObject> {
    submodule("csv", &[("parse", builtin(csv_parse))])
}

// --- std.xml ---

/// XML 元素 → dict：`{tag, attrs, text, children}`。
/// `text` 为直接文本内容（trimmed）；`children` 为子元素 dict 列表。
fn xml_element_to_value(node: roxmltree::Node) -> Value {
    let mut d = DictMap::new();
    d.insert(
        ValueKey::Text("tag".into()),
        Value::Text(node.tag_name().name().to_string()),
    );
    // 属性
    let mut attrs = DictMap::new();
    for a in node.attributes() {
        attrs.insert(
            ValueKey::Text(a.name().to_string()),
            Value::Text(a.value().to_string()),
        );
    }
    d.insert(
        ValueKey::Text("attrs".into()),
        Value::Dict(Shared::new(attrs)),
    );
    // 直接文本 + 子元素
    let mut text = String::new();
    let mut children = Vec::new();
    for child in node.children() {
        if child.is_element() {
            children.push(xml_element_to_value(child));
        } else if child.is_text() {
            text.push_str(child.text().unwrap_or(""));
        }
    }
    let trimmed = text.trim().to_string();
    d.insert(
        ValueKey::Text("text".into()),
        if trimmed.is_empty() {
            Value::None
        } else {
            Value::Text(trimmed)
        },
    );
    d.insert(
        ValueKey::Text("children".into()),
        Value::List(Shared::new(children)),
    );
    Value::Dict(Shared::new(d))
}

fn xml_parse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("parse", args, 1)?;
    let s = expect_text("parse", args, 0)?;
    let doc = roxmltree::Document::parse(&s)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("xml parse: {e}")))?;
    Ok(xml_element_to_value(doc.root_element()))
}

fn build_xml_module() -> Shared<ModuleObject> {
    submodule("xml", &[("parse", builtin(xml_parse))])
}
