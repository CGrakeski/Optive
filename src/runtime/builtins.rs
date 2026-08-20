use std::collections::HashMap;
use std::sync::Arc;

use num_bigint::BigInt;
use num_traits::Zero;

use crate::runtime_ast;
use crate::traceback;
use crate::types;
use crate::value::{DictMap, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use crate::shared::{Shared, SyncCell};
pub fn install_globals(vm: &mut Vm) {
    vm.globals.insert("true".into(), Value::Bool(true));
    vm.globals.insert("false".into(), Value::Bool(false));
    vm.globals.insert("none".into(), Value::None);
    type GlobalBuiltin = fn(&mut Vm, &[Value]) -> Result<Value>;
    let builtins: Vec<(&str, GlobalBuiltin)> = vec![
        ("print", builtin_print),
        ("len", builtin_len),
        ("str", builtin_str),
        ("repr", builtin_repr),
        ("eval", builtin_eval),
        ("quote", builtin_quote),
        ("ast_struct", builtin_ast_struct),
        ("__ast_clone__", builtin_ast_clone),
        ("__ast_type_convert__", builtin_ast_type_convert),
        ("__ast_func_call__", builtin_ast_func_call),
        ("__ast_macro_call__", builtin_ast_macro_call),
        ("__ast_vec_push__", builtin_ast_vec_push),
        ("__ast_vec_extend__", builtin_ast_vec_extend),
        ("__register_dispatch_handler__", builtin_register_dispatch_handler),
        ("__ensure_dispatch__", builtin_ensure_dispatch),
        ("convert", builtin_convert),
        ("__make_closure__", builtin_make_closure),
        ("__with_exit__", builtin_with_exit),
        ("is_a", builtin_is_a),
        ("isinstanceof", builtin_is_a),
        ("hash", builtin_hash),
        ("copy", builtin_copy),
        ("deepcopy", builtin_deepcopy),
        ("id", builtin_id),
        ("iter", builtin_iter),
        ("next", builtin_next),
        ("input", builtin_input),
        ("int", builtin_int),
        ("dict", builtin_dict_ctor),
        ("rational", builtin_rational),
        ("floatstring", builtin_floatstring),
        ("now", builtin_now),
        ("gc", builtin_gc),
        ("exit", builtin_exit),
        ("help", builtin_help),
        ("extern", crate::ffi::builtin_extern),
        ("__zip_iter__", builtin_zip_iter),
        ("__make_genexpr__", builtin_make_genexpr),
        ("__attach_defaults__", builtin_attach_defaults),
        ("__merge_kwargs__", builtin_merge_kwargs),
        ("__variant_is__", builtin_variant_is),
        ("__variant_payload__", builtin_variant_payload),
        ("__finalize_enum__", builtin_finalize_enum),
    ];
    for (name, f) in builtins {
        vm.globals.insert(name.into(), Value::builtin_fn(name, f));
    }
}
fn builtin_print(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let out = crate::value::args_join_space(args);
    println!("{out}");
    Ok(Value::None)
}

fn builtin_len(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("len requires 1 argument"));
    }
    if let Some(r) = vm.try_call_magic(&args[0], "__len__", vec![]) {
        return r;
    }
    let n = match &args[0] {
        Value::List(v) => v.borrow().len(),
        Value::Text(s) => s.chars().count(),
        Value::Dict(d) => d.borrow().len(),
        Value::Set(s) => s.borrow().len(),
        Value::Tuple(t) => t.len(),
        Value::Bytes(b) => b.len(),
        Value::Iterator(_) => {
            return Err(crate::error::RuntimeError::type_err(
                "len not supported for iterator",
            ))
        }
        other => {
            return Err(crate::error::RuntimeError::type_err(format!(
                "len not supported for {}",
                other.type_name()
            )))
        }
    };
    Ok(Value::Num(Num::Small(n as i64)))
}

fn builtin_str(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("str requires 1 argument"));
    }
    let v = &args[0];
    if let Some(r) = vm.try_call_magic(v, "__str__", vec![]) {
        let s = r?;
        return Ok(match s {
            Value::Text(t) => Value::Text(t),
            other => Value::Text(other.print_string()),
        });
    }
    if let Some(r) = vm.try_call_magic(v, "__repr__", vec![]) {
        let s = r?;
        return Ok(match s {
            Value::Text(t) => Value::Text(t),
            other => Value::Text(other.print_string()),
        });
    }
    Ok(Value::Text(v.print_string()))
}

fn builtin_eval(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("eval requires 1 argument"));
    }
    let ast = runtime_ast::value_as_ast(&args[0])?;
    runtime_ast::eval_ast_value(vm, &ast)
}

fn builtin_quote(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err("quote requires 3 arguments"));
    }
    let Value::List(hyg) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err(
            "quote expects a list of hygienic names",
        ));
    };
    let Value::List(bind_exprs) = &args[1] else {
        return Err(crate::error::RuntimeError::type_err(
            "quote expects a list of binding expressions",
        ));
    };
    let body = runtime_ast::value_as_ast(&args[2])?;

    let hygienic: Vec<String> = hyg
        .borrow()
        .iter()
        .map(|v| match v {
            Value::Text(s) => Ok(s.clone()),
            _ => Err(crate::error::RuntimeError::type_err(
                "hygienic names must be text",
            )),
        })
        .collect::<Result<_>>()?;

    let mut captured = Vec::new();
    for elem in bind_exprs.borrow().iter() {
        let bind_expr = runtime_ast::value_as_ast(elem)?;
        let name = runtime_ast::binding_var_name_for_quote(&bind_expr)?;
        let bound = runtime_ast::capture_quote_binding_value(vm, &bind_expr)?;
        captured.push((
            name,
            runtime_ast::value_to_quote_binding_ast(&bound)?,
        ));
    }

    Ok(runtime_ast::quote_ast(hygienic, captured, body).into_value())
}

fn builtin_ast_struct(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    runtime_ast::with_arity1("ast_struct", args, |v| runtime_ast::ast_struct_value(vm, v))
}

fn builtin_ast_clone(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    runtime_ast::with_arity1("__ast_clone__", args, runtime_ast::clone_ast_value)
}

macro_rules! builtin_ast_flip2 {
    ($(($name:ident, $api:literal, $compose:path)),+ $(,)?) => {
        $(
            fn $name(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
                runtime_ast::with_arity2_flip($api, args, $compose)
            }
        )+
    };
}

builtin_ast_flip2! {
    (builtin_ast_type_convert, "__ast_type_convert__", runtime_ast::compose_ast_type_convert),
    (builtin_ast_func_call, "__ast_func_call__", runtime_ast::compose_ast_func_call),
    (builtin_ast_macro_call, "__ast_macro_call__", runtime_ast::compose_ast_macro_call),
    (builtin_ast_vec_push, "__ast_vec_push__", runtime_ast::ast_vec_push),
    (builtin_ast_vec_extend, "__ast_vec_extend__", runtime_ast::ast_vec_extend),
}

fn builtin_register_dispatch_handler(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "__register_dispatch_handler__ requires 2 arguments",
        ));
    }
    let (Value::Text(name) | Value::TypeRef(name)) = &args[0] else {
        return Err(crate::error::RuntimeError::msg("expected dispatch name"));
    };
    let Value::Function(func) = &args[1] else {
        return Err(crate::error::RuntimeError::msg("expected function handler"));
    };
    let table = vm.get_or_create_dispatch(name);
    table
        .borrow()
        .handlers
        .borrow_mut()
        .push(Value::Function(func.clone()));
    vm.store_global_by_name(name, Value::Dispatch(table));
    Ok(Value::None)
}

fn builtin_ensure_dispatch(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "__ensure_dispatch__ requires 1 argument",
        ));
    }
    let (Value::Text(name) | Value::TypeRef(name)) = &args[0] else {
        return Err(crate::error::RuntimeError::msg("expected dispatch name"));
    };
    let table = vm.get_or_create_dispatch(name);
    Ok(Value::Dispatch(table))
}

fn builtin_convert(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("convert requires 2 arguments"));
    }
    vm.convert_type(args[0].clone(), args[1].clone())
}

fn builtin_make_closure(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "__make_closure__ requires 2 arguments",
        ));
    }
    let Value::Function(func) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err(
            "__make_closure__ expects a function",
        ));
    };
    let Value::Dict(map) = &args[1] else {
        return Err(crate::error::RuntimeError::type_err(
            "__make_closure__ expects a dict of captures",
        ));
    };
    let mut f = (**func).clone();
    for (k, _) in map.borrow().iter() {
        if let ValueKey::Text(name) = k {
            let cell = vm.upgrade_binding_to_cell(name)?;
            f.captured
                .get_or_insert_with(HashMap::new)
                .insert(name.clone(), Value::Cell(cell));
        }
    }
    f.refresh_hot_call_argc();
    Ok(Value::Function(Arc::new(f)))
}

fn builtin_with_exit(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(crate::error::RuntimeError::type_err(
            "__with_exit__ requires 1 or 2 arguments",
        ));
    }
    let ctx = &args[0];
    let (exc_type, exc_val, exc_tb) = if args.len() == 2 && !matches!(args[1], Value::None) {
        let exc = &args[1];
        let tb = traceback::get_exception_traceback(exc).unwrap_or(Value::None);
        (
            Value::Text(exc.type_name_string()),
            exc.clone(),
            tb,
        )
    } else {
        (Value::None, Value::None, Value::None)
    };
    let exit_args = vec![exc_type, exc_val, exc_tb];
    // 直接调 Builtin，保留 `block_suspend`，让外层 `__with_exit__` Call 重试。
    // 若走 `call_method`→`call_value`，内层会清掉 suspend 并错误地 arm 内层方法。
    let method = vm.get_attr_value(ctx, "__exit__")?;
    match method {
        Value::Builtin(b) => b.call(vm, &exit_args),
        other => vm.call_value(other, exit_args),
    }
}

fn builtin_is_a(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("is_a requires 2 arguments"));
    }
    let ok = match &args[1] {
        Value::TypeRef(s) | Value::Text(s) => types::instance_is_a(vm, &args[0], s),
        Value::TypeSpec(spec) => {
            let ty = Value::TypeSpec(spec.clone());
            types::value_accepts(vm, &args[0], &ty)
        }
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "is_a expects type handle or TypeSpec",
            ))
        }
    };
    Ok(Value::Bool(ok))
}

fn builtin_hash(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("hash requires 1 argument"));
    }
    let h = crate::value::hash_value(&args[0])?;
    Ok(Value::Num(Num::Small(h)))
}

fn builtin_copy(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("copy requires 1 argument"));
    }
    Ok(match &args[0] {
        Value::List(l) => Value::List(Shared::new(l.borrow().clone())),
        Value::Dict(d) => Value::Dict(Shared::new(d.borrow().clone())),
        Value::Set(s) => Value::Set(Shared::new(s.borrow().clone())),
        other => other.clone(),
    })
}

fn builtin_deepcopy(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("deepcopy requires 1 argument"));
    }
    let mut memo = HashMap::new();
    deep_copy_value(&args[0], &mut memo)
}

fn deep_copy_value(v: &Value, memo: &mut HashMap<usize, Value>) -> Result<Value> {
    if let Some(key) = cycle_key(v) {
        if let Some(cached) = memo.get(&key) {
            return Ok(cached.clone());
        }
    }
    Ok(match v {
        Value::List(l) => {
            let key = l.as_ptr() as usize;
            let out = Shared::new(Vec::new());
            memo.insert(key, Value::List(out.clone()));
            let items = l
                .borrow()
                .iter()
                .map(|item| deep_copy_value(item, memo))
                .collect::<Result<Vec<_>>>()?;
            *out.borrow_mut() = items;
            Value::List(out)
        }
        Value::Dict(d) => {
            let key = d.as_ptr() as usize;
            let out = Shared::new(DictMap::new());
            memo.insert(key, Value::Dict(out.clone()));
            let mut copied = DictMap::new();
            for (k, val) in d.borrow().iter() {
                copied.insert(k.clone(), deep_copy_value(val, memo)?);
            }
            *out.borrow_mut() = copied;
            Value::Dict(out)
        }
        Value::Set(s) => {
            let key = s.as_ptr() as usize;
            let out = Shared::new(s.borrow().clone());
            memo.insert(key, Value::Set(out.clone()));
            Value::Set(out)
        }
        Value::Tuple(t) => {
            let items = t
                .iter()
                .map(|item| deep_copy_value(item, memo))
                .collect::<Result<Vec<_>>>()?;
            Value::Tuple(Arc::from(items.into_boxed_slice()))
        }
        Value::Bytes(b) => Value::Bytes(Arc::new(b.as_ref().clone())),
        Value::Struct(s) => {
            let key = Arc::as_ptr(s) as usize;
            let out = Arc::new(crate::value::StructInstance {
                def: s.def.clone(),
                slots: SyncCell::new(Vec::new()),
                generic_args: s.generic_args.clone(),
            });
            memo.insert(key, Value::Struct(out.clone()));
            let slots = s
                .slots
                .borrow()
                .iter()
                .map(|item| deep_copy_value(item, memo))
                .collect::<Result<Vec<_>>>()?;
            *out.slots.borrow_mut() = slots;
            Value::Struct(out)
        }
        Value::Variant(v) => {
            let key = Arc::as_ptr(v) as usize;
            if let Some(cached) = memo.get(&key) {
                return Ok(cached.clone());
            }
            let filled = Arc::new(crate::value::VariantInstance {
                inst_name: v.inst_name.clone(),
                def: v.def.clone(),
                generic_args: v.generic_args.clone(),
                case_idx: v.case_idx,
                payload: deep_copy_value(&v.payload, memo)?,
            });
            memo.insert(key, Value::Variant(filled.clone()));
            Value::Variant(filled)
        }
        Value::Cell(c) => {
            let key = c.as_ptr() as usize;
            let out = Shared::new(Value::None);
            memo.insert(key, Value::Cell(out.clone()));
            *out.borrow_mut() = deep_copy_value(&c.borrow(), memo)?;
            Value::Cell(out)
        }
        Value::RuntimeAst(a) => Value::RuntimeAst(Arc::new((**a).clone())),
        other => other.clone(),
    })
}

fn cycle_key(v: &Value) -> Option<usize> {
    match v {
        Value::List(r) => Some(r.as_ptr() as usize),
        Value::Dict(r) => Some(r.as_ptr() as usize),
        Value::Set(r) => Some(r.as_ptr() as usize),
        Value::Struct(r) => Some(Arc::as_ptr(r) as usize),
        Value::Variant(r) => Some(Arc::as_ptr(r) as usize),
        Value::Cell(r) => Some(r.as_ptr() as usize),
        _ => None,
    }
}

fn builtin_id(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("id requires 1 argument"));
    }
    let ptr = match &args[0] {
        Value::List(r) => r.as_ptr() as usize,
        Value::Dict(r) => r.as_ptr() as usize,
        Value::Set(r) => r.as_ptr() as usize,
        Value::Function(r) => Arc::as_ptr(r) as usize,
        Value::Struct(r) => Arc::as_ptr(r) as usize,
        Value::Iterator(r) => r.as_ptr() as usize,
        Value::Text(s) => s.as_ptr() as usize,
        other => std::ptr::from_ref::<Value>(other) as usize,
    };
    Ok(Value::Num(Num::from_bigint((ptr as u64).into())))
}

fn builtin_iter(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("iter requires 1 argument"));
    }
    Ok(Value::Iterator(vm.to_iterator_shared(&args[0])?))
}

fn builtin_next(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("next requires 1 argument"));
    }
    let state = vm.to_iterator_shared(&args[0])?;
    if let Some(v) = vm.advance_iterator(&state)? { Ok(v) } else {
        let exc = crate::exceptions::make_exception(vm, "StopIteration", "iterator exhausted")?;
        vm.throw_value(exc)?;
        Ok(Value::None)
    }
}

fn builtin_input(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let prompt = if args.is_empty() {
        String::new()
    } else {
        args[0].print_string()
    };
    read_line_with_prompt(&prompt)
}

/// 带可选提示的行输入（`input` 与 `std.io.read_line` 共用）。
pub(crate) fn read_line_with_prompt(prompt: &str) -> Result<Value> {
    use std::io::{self, Write};
    if !prompt.is_empty() {
        print!("{prompt}");
        io::stdout().flush().ok();
    }
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("read_line failed: {e}")))?;
    Ok(Value::Text(line.trim_end().to_string()))
}

fn builtin_int(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("int requires 1 argument"));
    }
    match &args[0] {
        Value::Num(n) => {
            if let Some(i) = n.to_i64() {
                return Ok(Value::Num(Num::small(i)));
            }
            if let Num::Int(n) = n {
                return Ok(Value::Num(Num::Int(n.clone())));
            }
            if let Num::Rat(r) = n {
                if r.denom() == &num_traits::One::one() {
                    return Ok(Value::Num(Num::from_bigint(r.numer().clone())));
                }
            }
            Err(crate::error::RuntimeError::msg("int: non-integer num"))
        }
        Value::Text(s) => {
            let n: num_bigint::BigInt = s
                .trim()
                .parse()
                .map_err(|_| crate::error::RuntimeError::value_err("int: invalid text"))?;
            Ok(Value::Num(Num::from_bigint(n)))
        }
        Value::Bool(b) => Ok(Value::Num(Num::Small(i64::from(*b)))),
        other => Err(crate::error::RuntimeError::type_err(format!(
            "int not supported for {}",
            other.type_name()
        ))),
    }
}

fn builtin_dict_ctor(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !args.len().is_multiple_of(2) {
        return Err(crate::error::RuntimeError::type_err(
            "dict() requires an even number of alternating key, value arguments",
        ));
    }
    let mut map = DictMap::new();
    let mut i = 0;
    while i < args.len() {
        let key = ValueKey::from_value(&args[i])?;
        map.insert(key, args[i + 1].clone());
        i += 2;
    }
    Ok(Value::Dict(Shared::new(map)))
}

fn builtin_rational(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err("rational requires 2 arguments"));
    }
    let numer = match &args[0] {
        Value::Num(Num::Small(n)) => BigInt::from(*n),
        Value::Num(Num::Int(n)) => n.as_ref().clone(),
        _ => return Err(crate::error::RuntimeError::type_err("rational numerator must be int")),
    };
    let denom = match &args[1] {
        Value::Num(Num::Small(n)) => BigInt::from(*n),
        Value::Num(Num::Int(n)) => n.as_ref().clone(),
        _ => return Err(crate::error::RuntimeError::type_err("rational denominator must be int")),
    };
    if denom.is_zero() {
        return Err(crate::error::RuntimeError::msg("rational denominator is zero"));
    }
    Ok(Value::Num(Num::from_rational(num_rational::BigRational::new(
        numer, denom,
    ))))
}

fn builtin_floatstring(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("floatstring requires 1 argument"));
    }
    match &args[0] {
        Value::Num(n) => {
            let f = n.to_f64_checked()?;
            Ok(Value::Text(format!("{f}")))
        }
        other => Err(crate::error::RuntimeError::type_err(format!(
            "floatstring requires num, got {}",
            other.type_name()
        ))),
    }
}

fn builtin_now(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| crate::error::RuntimeError::io_err(format!("now failed: {e}")))?
        .as_secs();
    Ok(Value::Num(Num::Small(secs as i64)))
}

fn builtin_gc(vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    let cleared = vm.gc_collect();
    Ok(Value::Num(Num::Small(cleared as i64)))
}

fn builtin_exit(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    call_exit(_vm, args)
}

/// 进程退出（全局 `exit` 与 `std.os.exit` 共用）。
pub(crate) fn call_exit(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let code = if args.is_empty() {
        0
    } else {
        match &args[0] {
            Value::Num(Num::Small(n)) => {
                i32::try_from(*n).map_err(|_| {
                    crate::error::RuntimeError::value_err("exit code out of range for i32")
                })?
            }
            Value::Num(Num::Int(n)) => n.as_ref().try_into().map_err(|_| {
                crate::error::RuntimeError::value_err("exit code out of range for i32")
            })?,
            Value::Num(Num::Rat(_)) => {
                return Err(crate::error::RuntimeError::type_err(
                    "exit requires an integer code",
                ));
            }
            other => {
                return Err(crate::error::RuntimeError::type_err(format!(
                    "exit requires integer, got {}",
                    other.type_name()
                )));
            }
        }
    };
    std::process::exit(code);
}

fn builtin_help(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        println!(
            "Optive help\n\
             Builtins: print, len, type, range, iter, next, input, exit, help, eval, ...\n\
             Import std: use std.math / std.io / std.json / ...\n\
             Typing:\n\
               : T      soft annotation\n\
               :: T     strong runtime contract\n\
               -> T     soft return type\n\
               => T     strong return type\n\
             help(x) prints a value."
        );
    } else {
        println!("{}", args[0].print_string());
    }
    Ok(Value::None)
}

fn builtin_repr(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err("repr requires 1 argument"));
    }
    Ok(Value::Text(args[0].display_string()))
}

fn builtin_zip_iter(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "__zip_iter__ requires at least 1 argument",
        ));
    }
    vm.zip_iterables(args.to_vec())
}

fn builtin_make_genexpr(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err(
            "__make_genexpr__ requires (source, elem_func, guards_list)",
        ));
    }
    let source = vm.to_iterator_shared(&args[0])?;
    let elem = match &args[1] {
        Value::Function(f) => f.clone(),
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "__make_genexpr__ elem must be a function",
            ))
        }
    };
    let arity = elem.params.len().max(1);
    let guards = match &args[2] {
        Value::List(list) => {
            let mut out = Vec::new();
            for g in list.borrow().iter() {
                match g {
                    Value::Function(f) => out.push(f.clone()),
                    _ => {
                        return Err(crate::error::RuntimeError::type_err(
                            "__make_genexpr__ guards must be functions",
                        ))
                    }
                }
            }
            out
        }
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "__make_genexpr__ guards must be a list",
            ))
        }
    };
    Ok(Value::Iterator(Shared::new(
        crate::value::IteratorState {
            kind: crate::value::IteratorKind::GenExpr {
                source,
                arity,
                elem,
                guards,
            },
        },
    )))
}

fn builtin_attach_defaults(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "__attach_defaults__ requires (defaults_list, function)",
        ));
    }
    let defaults_list = match &args[0] {
        Value::List(l) => l.borrow().clone(),
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "__attach_defaults__ defaults must be a list",
            ))
        }
    };
    let mut func = match &args[1] {
        Value::Function(f) => (**f).clone(),
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "__attach_defaults__ expects a function",
            ))
        }
    };
    let mut di = 0usize;
    func.defaults = func
        .params
        .iter()
        .map(|p| {
            if p.default_expr.is_some() {
                let v = defaults_list.get(di).cloned().unwrap_or(Value::None);
                di += 1;
                Some(v)
            } else {
                None
            }
        })
        .collect();
    func.refresh_hot_call_argc();
    Ok(Value::Function(Arc::new(func)))
}

fn builtin_merge_kwargs(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "__merge_kwargs__ requires (dst_dict, src_dict)",
        ));
    }
    let Value::Dict(dst) = &args[0] else {
        return Err(crate::error::RuntimeError::type_err(
            "__merge_kwargs__ dst must be dict",
        ));
    };
    let Value::Dict(src) = &args[1] else {
        return Err(crate::error::RuntimeError::type_err(
            "__merge_kwargs__ src must be dict",
        ));
    };
    for (k, v) in src.borrow().iter() {
        dst.borrow_mut().insert(k.clone(), v.clone());
    }
    Ok(Value::Dict(dst.clone()))
}

fn builtin_variant_is(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "__variant_is__ expects (value, variant_name)",
        ));
    }
    let expected = match &args[1] {
        Value::TypeRef(s) | Value::Text(s) => s.as_str(),
        other => other.type_name(),
    };
    let ok = matches!(
        &args[0],
        Value::Variant(v) if v.inst_name == expected || v.def.name == expected
    );
    Ok(Value::Bool(ok))
}

fn builtin_variant_payload(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "__variant_payload__ expects (variant)",
        ));
    }
    match &args[0] {
        Value::Variant(v) => Ok(v.payload.clone()),
        other => Err(crate::error::RuntimeError::type_err(format!(
            "__variant_payload__ expects variant, got {}",
            other.type_name()
        ))),
    }
}

fn builtin_finalize_enum(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err(
            "__finalize_enum__ expects (enum_name, member_names, values_dict)",
        ));
    }
    let enum_name = match &args[0] {
        Value::TypeRef(s) | Value::Text(s) => s.as_str(),
        other => {
            return Err(crate::error::RuntimeError::type_err(format!(
                "__finalize_enum__ enum_name must be type name, got {}",
                other.type_name()
            )));
        }
    };
    let member_names: Vec<String> = match &args[1] {
        Value::List(lst) => lst
            .borrow()
            .iter()
            .map(|v| match v {
                Value::Text(s) => Ok(s.clone()),
                other => Err(crate::error::RuntimeError::type_err(format!(
                    "__finalize_enum__ member name must be text, got {}",
                    other.type_name()
                ))),
            })
            .collect::<Result<_>>()?,
        other => {
            return Err(crate::error::RuntimeError::type_err(format!(
                "__finalize_enum__ member_names must be list, got {}",
                other.type_name()
            )));
        }
    };
    let values = match &args[2] {
        Value::Dict(d) => d.borrow().clone(),
        other => {
            return Err(crate::error::RuntimeError::type_err(format!(
                "__finalize_enum__ values must be dict, got {}",
                other.type_name()
            )));
        }
    };
    crate::enum_variant::finalize_enum_from_dict(vm, enum_name, &member_names, &values)?;
    Ok(Value::None)
}
