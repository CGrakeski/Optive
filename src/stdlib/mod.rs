use std::collections::HashMap;
use std::sync::Arc;

use crate::runtime_ast;
use crate::value::{DictMap, IteratorKind, IteratorState, ModuleObject, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use crate::shared::{Shared, SyncCell};

mod collections;
mod dict;
mod encoding;
mod format;
mod http_client;
mod http_server;
mod io;
mod iter;
mod json;
mod log;
mod math;
mod net;
mod os;
mod random;
mod serde_val;
mod sqlite;
mod test;
mod text;
mod time;

use dict::*;
use encoding::*;
use format::*;
use io::*;
use iter::*;
use json::*;
use math::*;
use os::*;
use serde_val::*;
use time::*;

/// `format_num` / `format` 字段的默认小数精度。
pub(crate) const DEFAULT_NUM_PRECISION: usize = 6;
/// `to_list` / `to_set` / `cycle` 等物化路径的预分配提示（保留作文档锚点）。
#[allow(dead_code)]
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
    std_children.insert(
        "collections".into(),
        collections::build_collections_module(),
    );
    std_children.insert("time".into(), build_time_module());
    std_children.insert("sync".into(), build_sync_module());
    std_children.insert("async".into(), build_async_module());
    std_children.insert("text".into(), text::build_text_module());
    std_children.insert("path".into(), build_path_module());
    std_children.insert("fs".into(), build_fs_module());
    std_children.insert("os".into(), build_os_module());
    std_children.insert("json".into(), build_json_module());
    std_children.insert("test".into(), test::build_test_module());
    std_children.insert("debug".into(), build_debug_module());
    std_children.insert("random".into(), random::build_random_module());
    std_children.insert("re".into(), build_re_module());
    std_children.insert("hash".into(), build_hash_module());
    std_children.insert("exceptions".into(), build_exceptions_module());
    std_children.insert("language".into(), crate::ffi::build_language_module());
    std_children.insert("http".into(), http_client::build_http_module());
    std_children.insert("log".into(), log::build_log_module());
    std_children.insert("net".into(), net::build_net_module());
    std_children.insert("sqlite".into(), sqlite::build_sqlite_module());
    std_children.insert("encoding".into(), build_encoding_module());
    std_children.insert("csv".into(), build_csv_module());
    std_children.insert("toml".into(), build_toml_module());
    std_children.insert("yaml".into(), build_yaml_module());
    std_children.insert("xml".into(), build_xml_module());

    Shared::new(ModuleObject {
        name: "std".into(),
        exports: exports(&[("concat", builtin(std_concat))]),
        children: std_children,
        is_user: false,
    })
}

pub(crate) fn exports(entries: &[(&str, Value)]) -> HashMap<String, Value> {
    entries
        .iter()
        .map(|(k, v)| {
            let named = match v {
                Value::Builtin(b) if b.name.as_ref() == "<anon>" => {
                    Value::Builtin(crate::value::BuiltinObject::new(*k, b.func.clone()))
                }
                other => other.clone(),
            };
            ((*k).to_string(), named)
        })
        .collect()
}

/// 未具名的 fn 指针 builtin；经 [`exports`] / [`submodule`] 时用导出名覆盖。
pub(crate) fn builtin(f: fn(&mut Vm, &[Value]) -> Result<Value>) -> Value {
    Value::builtin_fn("<anon>", f)
}

pub(crate) fn named_builtin(name: &str, f: fn(&mut Vm, &[Value]) -> Result<Value>) -> Value {
    Value::builtin_fn(name, f)
}

pub(crate) fn submodule(name: &str, entries: &[(&str, Value)]) -> Shared<ModuleObject> {
    Shared::new(ModuleObject {
        name: name.into(),
        exports: exports(entries),
        children: HashMap::new(),
        is_user: false,
    })
}

pub(crate) fn expect_arity(name: &str, args: &[Value], n: usize) -> Result<()> {
    if args.len() != n {
        return Err(crate::error::RuntimeError::type_err(format!(
            "{name} requires {n} argument{}",
            if n == 1 { "" } else { "s" }
        )));
    }
    Ok(())
}

pub(crate) fn expect_num_value(name: &str, args: &[Value], idx: usize) -> Result<Num> {
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

pub(crate) fn expect_num_f64(name: &str, args: &[Value], idx: usize) -> Result<f64> {
    expect_num_value(name, args, idx)?.to_f64_checked()
}

pub(crate) fn expect_int(name: &str, args: &[Value], idx: usize) -> Result<i64> {
    let v = args.get(idx).ok_or_else(|| {
        crate::error::RuntimeError::type_err(format!("{name}: missing argument {idx}"))
    })?;
    crate::value::expect_i64(name, v)
}

pub(crate) fn expect_text(name: &str, args: &[Value], idx: usize) -> Result<String> {
    match args.get(idx) {
        Some(Value::Text(s)) => Ok(s.clone()),
        _ => Err(crate::error::RuntimeError::type_err(format!(
            "{name}: argument must be text"
        ))),
    }
}

pub(crate) fn expect_bytes(name: &str, args: &[Value], idx: usize) -> Result<Arc<Vec<u8>>> {
    match args.get(idx) {
        Some(Value::Bytes(b)) => Ok(b.clone()),
        _ => Err(crate::error::RuntimeError::type_err(format!(
            "{name}: argument must be bytes"
        ))),
    }
}

pub(crate) fn expect_text_or_bytes(name: &str, args: &[Value], idx: usize) -> Result<Vec<u8>> {
    match args.get(idx) {
        Some(Value::Text(s)) => Ok(s.as_bytes().to_vec()),
        Some(Value::Bytes(_)) => Ok(expect_bytes(name, args, idx)?.as_ref().clone()),
        _ => Err(crate::error::RuntimeError::type_err(format!(
            "{name}: argument must be text or bytes"
        ))),
    }
}

pub(crate) fn expect_dict(name: &str, args: &[Value], idx: usize) -> Result<Shared<DictMap>> {
    match args.get(idx) {
        Some(Value::Dict(d)) => Ok(d.clone()),
        _ => Err(crate::error::RuntimeError::type_err(format!(
            "{name}: argument must be dict"
        ))),
    }
}

pub(crate) fn io_map(op: &str, err: impl std::fmt::Display) -> crate::error::RuntimeError {
    crate::error::RuntimeError::io_err(format!("{op}: {err}"))
}

pub(crate) fn float_from_f64(f: f64) -> Result<Num> {
    num_rational::BigRational::from_float(f)
        .map(Num::from_rational)
        .ok_or_else(|| crate::error::RuntimeError::value_err("non-finite floating-point result"))
}

fn std_concat(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let mut out = String::new();
    for arg in args {
        out.push_str(&arg.print_string());
    }
    Ok(Value::Text(out))
}

pub(crate) fn value_to_list(v: &Value) -> Result<Vec<Value>> {
    match v {
        Value::List(list) => Ok(list.borrow().clone()),
        Value::Text(s) => Ok(s.chars().map(|c| Value::Text(c.to_string())).collect()),
        _ => Err(crate::error::RuntimeError::type_err(
            "object is not iterable",
        )),
    }
}

pub(crate) fn value_key_to_value(k: &ValueKey) -> Value {
    match k {
        ValueKey::Bool(b) => Value::Bool(*b),
        ValueKey::NumInt(n) => Value::Num(Num::from_bigint(n.clone())),
        ValueKey::Text(s) => Value::Text(s.clone()),
    }
}

fn ast_parse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let source = expect_text("parse", args, 0)?;
    Ok(runtime_ast::parse_to_ast(&source)?.into_value())
}

fn ast_clone_export(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    runtime_ast::with_arity1("ast_clone", args, runtime_ast::clone_ast_value)
}

macro_rules! ast_export_natural2 {
    ($(($name:ident, $api:literal, $compose:path)),+ $(,)?) => {
        $(
            fn $name(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
                runtime_ast::with_arity2($api, args, $compose)
            }
        )+
    };
}

ast_export_natural2! {
    (ast_type_convert_export, "ast_type_convert", runtime_ast::compose_ast_type_convert),
    (ast_call_export, "ast_call", runtime_ast::compose_ast_func_call),
    (ast_macro_call_export, "ast_macro_call", runtime_ast::compose_ast_macro_call),
    (ast_vec_push_export, "ast_vec_push", runtime_ast::ast_vec_push),
    (ast_vec_extend_export, "ast_vec_extend", runtime_ast::ast_vec_extend),
}

fn ast_unparse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "unparse requires 1 argument",
        ));
    }
    let node = runtime_ast::value_as_ast(&args[0])?;
    Ok(Value::Text(runtime_ast::ast_to_source(&node)))
}

fn ast_walk(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "walk requires 2 arguments",
        ));
    }
    let node = runtime_ast::value_as_ast(&args[0])?;
    let visitor = expect_function("walk", args, 1)?;
    runtime_ast::walk_ast_nodes(&node, &mut |n| {
        let _ = vm.call_user_function(visitor.clone(), vec![n.clone().into_value()])?;
        Ok(())
    })?;
    Ok(Value::None)
}

pub(crate) fn expect_function(
    name: &str,
    args: &[Value],
    idx: usize,
) -> Result<Arc<crate::opcode::FunctionObject>> {
    match args.get(idx) {
        Some(Value::Function(f)) => Ok(f.clone()),
        _ => Err(crate::error::RuntimeError::type_err(format!(
            "{name}: argument must be a function"
        ))),
    }
}

fn decos_log(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "log requires 1 argument",
        ));
    }
    let inner = expect_function("log", args, 0)?;
    Ok(Value::builtin("log", move |vm, call_args| {
        eprintln!("log: call({})", call_args.len());
        vm.call_user_function(inner.clone(), call_args.to_vec())
    }))
}

fn decos_once(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "once requires 1 argument",
        ));
    }
    let inner = expect_function("once", args, 0)?;
    let cached = SyncCell::new(None::<Value>);
    let called = SyncCell::new(false);
    Ok(Value::builtin("once", move |vm, call_args| {
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
    }))
}

fn decos_memoize(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("memoize", args, 1)?;
    let inner = expect_function("memoize", args, 0)?;
    let cache = SyncCell::new(HashMap::<Vec<ValueKey>, Value>::new());
    Ok(Value::builtin("memoize", move |vm, call_args| {
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
    }))
}

// --- 其余 std 子模块 ---

fn build_typing_module() -> Shared<ModuleObject> {
    fn type_ctor(name: &str) -> Value {
        let name = name.to_string();
        let is_form = crate::type_registry::is_type_form(&name);
        Value::builtin(name.clone(), move |_vm, args| {
            if args.is_empty() {
                if is_form {
                    return Ok(Value::TypeSpec(crate::value::TypeSpecData::new(
                        name.clone(),
                        vec![],
                    )));
                }
                return Ok(Value::type_ref(name.clone()));
            }
            let params: Vec<Value> = args
                .iter()
                .map(crate::type_registry::value_to_type_value_operand)
                .collect();
            Ok(Value::TypeSpec(crate::value::TypeSpecData::new(
                name.clone(),
                params,
            )))
        })
    }
    fn type_ctor_literal() -> Value {
        Value::builtin("Literal", move |_vm, args| {
            if args.is_empty() {
                return Err(crate::error::RuntimeError::type_err(
                    "Literal requires at least 1 argument",
                ));
            }
            let params: Vec<Value> = args
                .iter()
                .map(crate::type_registry::literal_operand_to_type_value)
                .collect::<crate::Result<Vec<_>>>()?;
            Ok(Value::TypeSpec(crate::value::TypeSpecData::new(
                "Literal".to_string(),
                params,
            )))
        })
    }
    submodule(
        "typing",
        &[
            ("Union", type_ctor("Union")),
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
            ("isinstanceof", builtin(typing_isinstanceof)),
        ],
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
        Value::TypeRef(n) | Value::Text(n) => _vm.struct_defs.get(n),
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
    let Some(pd) = _vm.protocols.get(&name) else {
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
            let ty = Value::TypeSpec(spec.clone());
            crate::types::value_accepts(vm, &args[0], &ty)
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
        &[
            ("map", builtin(func_map)),
            ("filter", builtin(func_filter)),
            ("zip", builtin(func_zip)),
            ("reduce", builtin(func_reduce)),
            ("compose", builtin(func_compose)),
            ("partial", builtin(func_partial)),
            ("identity", builtin(func_identity)),
            ("const", builtin(func_const)),
            ("flip", builtin(func_flip)),
        ],
    )
}

fn build_sync_module() -> Shared<ModuleObject> {
    let atomic = submodule(
        "Atomic",
        &[
            ("num", builtin(sync_atomic_num)),
            ("bool", builtin(sync_atomic_bool)),
        ],
    );
    Shared::new(ModuleObject {
        name: "sync".into(),
        exports: exports(&[
            ("Channel", Value::type_ref("Channel")),
            ("Mutex", Value::type_ref("Mutex")),
            ("RWMutex", Value::type_ref("RWMutex")),
            ("RwLock", Value::type_ref("RWMutex")),
            ("WaitGroup", Value::type_ref("WaitGroup")),
            ("Semaphore", Value::type_ref("Semaphore")),
            ("Once", Value::type_ref("Once")),
            ("Barrier", Value::type_ref("Barrier")),
            ("Cond", Value::type_ref("Cond")),
            ("Atomic", Value::Module(atomic)),
            ("yield", builtin(sync_yield)),
        ]),
        children: HashMap::new(),
        is_user: false,
    })
}

fn build_async_module() -> Shared<ModuleObject> {
    submodule(
        "async",
        &[
            ("taskgroup", builtin(async_taskgroup)),
            ("gather", builtin(async_gather)),
            ("race", builtin(async_race)),
            ("with_timeout", builtin(async_with_timeout)),
            ("par_map", builtin(async_par_map)),
            ("par_each", builtin(async_par_each)),
            ("workers", builtin(async_workers)),
            ("Stream", Value::type_ref("Stream")),
            ("stream", builtin(async_stream_new)),
            ("stream_of", builtin(async_stream_of)),
            ("stream_from_gen", builtin(async_stream_from_gen)),
            ("stream_map", builtin(async_stream_map)),
            ("stream_filter", builtin(async_stream_filter)),
            ("stream_take", builtin(async_stream_take)),
        ],
    )
}

fn async_taskgroup(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    crate::concurrency::construct_taskgroup(args)
}

fn async_with_timeout(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    crate::concurrency::construct_timeout_ctx(args)
}

fn async_gather(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "gather requires 1 list argument",
        ));
    }
    let items = match &args[0] {
        Value::List(list) => list.borrow().clone(),
        Value::Tuple(t) => t.iter().cloned().collect(),
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "gather expects a list of tasks",
            ))
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let v = vm.await_value(item)?;
        if vm.block_suspend {
            return Ok(Value::None);
        }
        out.push(v);
    }
    Ok(Value::List(Shared::new(out)))
}

fn async_race(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "race requires 1 list argument",
        ));
    }
    let items = match &args[0] {
        Value::List(list) => list.borrow().clone(),
        Value::Tuple(t) => t.iter().cloned().collect(),
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "race expects a list of tasks",
            ))
        }
    };
    if items.is_empty() {
        return Err(crate::error::RuntimeError::value_err(
            "race requires a non-empty list",
        ));
    }
    use crate::value::TaskState;
    let cancel_losers = |vm: &mut Vm, winner: &Value| {
        for item in &items {
            if let Value::Task(task) = item {
                if let Value::Task(w) = winner {
                    if Shared::ptr_eq(task, w) {
                        continue;
                    }
                }
                vm.cancel_task(task);
            }
        }
    };
    loop {
        let mut any_open = false;
        for item in &items {
            match item {
                Value::Task(task) => match task.borrow().state.clone() {
                    TaskState::Done(v) => {
                        cancel_losers(vm, item);
                        return Ok(v);
                    }
                    TaskState::Failed(e) => {
                        cancel_losers(vm, item);
                        vm.throw_value(e)?;
                        return Ok(Value::None);
                    }
                    TaskState::Pending { .. } | TaskState::Suspended => {
                        any_open = true;
                        vm.ensure_task_runnable(task);
                    }
                    TaskState::Running => {
                        any_open = true;
                    }
                },
                other => {
                    cancel_losers(vm, item);
                    return Ok(other.clone());
                }
            }
        }
        if !any_open {
            return Err(crate::error::RuntimeError::msg(
                "race: internal error, no open tasks",
            ));
        }
        vm.wait_or_deadlock("race blocked")?;
        if vm.block_suspend {
            return Ok(Value::None);
        }
    }
}

fn async_par_map(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "par_map requires 2 arguments (iterable, callable)",
        ));
    }
    let items = materialize_iter(vm, &args[0])?;
    let f = args[1].clone();
    let mut tasks = Vec::with_capacity(items.len());
    for item in items {
        tasks.push(vm.spawn_task(f.clone(), vec![item]));
    }
    async_gather(vm, &[Value::List(Shared::new(tasks))])
}

fn async_par_each(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "par_each requires 2 arguments (iterable, callable)",
        ));
    }
    let _ = async_par_map(vm, args)?;
    if vm.block_suspend {
        return Ok(Value::None);
    }
    Ok(Value::None)
}

fn async_workers(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "workers takes no arguments",
        ));
    }
    Ok(Value::Num(Num::Small(vm.mn.worker_count() as i64)))
}

fn async_stream_new(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    crate::concurrency::construct_stream(args)
}

/// `stream_of(xs)`：物化为拉取 Stream（按 `next` / `for-in` 消费，无后台推送竞态）。
fn async_stream_of(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "stream_of requires 1 iterable argument",
        ));
    }
    let items = materialize_iter(vm, &args[0])?;
    Ok(crate::concurrency::stream_from_iterator(Shared::new(
        IteratorState::from_list(items),
    )))
}

/// `stream_from_gen(g)`：按需从生成器/`Iterator` 拉取（不先物化全集）。
///
/// `g` 可为：
/// - 无参 `gen` 函数（自动调用得到 iterator）
/// - 已启动的 generator iterator（如 `count()`）
/// - 其它可迭代对象
fn async_stream_from_gen(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "stream_from_gen requires 1 iterable/generator argument",
        ));
    }
    let source = match &args[0] {
        Value::Function(f) if f.is_generator() => {
            // 无参 gen：`stream_from_gen(count)` ≡ `stream_from_gen(count())`
            vm.call_user_function(f.clone(), vec![])?
        }
        other => other.clone(),
    };
    let iter = vm.to_iterator_shared(&source)?;
    Ok(crate::concurrency::stream_from_iterator(iter))
}

fn async_stream_map(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "stream_map(stream, f) requires 2 arguments",
        ));
    }
    let func = expect_function("stream_map", args, 1)?;
    let source = vm.to_iterator_shared(&args[0])?;
    Ok(crate::concurrency::stream_from_iterator(Shared::new(
        IteratorState {
            kind: IteratorKind::Map { func, source },
        },
    )))
}

fn async_stream_filter(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "stream_filter(stream, pred) requires 2 arguments",
        ));
    }
    let pred = expect_function("stream_filter", args, 1)?;
    let source = vm.to_iterator_shared(&args[0])?;
    Ok(crate::concurrency::stream_from_iterator(Shared::new(
        IteratorState {
            kind: IteratorKind::Filter { func: pred, source },
        },
    )))
}

fn async_stream_take(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "stream_take(stream, n) requires 2 arguments",
        ));
    }
    let n = crate::value::expect_i64("stream_take n", &args[1])?;
    if n < 0 {
        return Err(crate::error::RuntimeError::type_err(
            "stream_take n must be non-negative",
        ));
    }
    let source = vm.to_iterator_shared(&args[0])?;
    Ok(crate::concurrency::stream_from_iterator(Shared::new(
        IteratorState {
            kind: IteratorKind::Take {
                remaining: n as usize,
                source,
            },
        },
    )))
}

fn sync_atomic_num(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    crate::concurrency::construct_atomic_num(args)
}

fn sync_atomic_bool(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    crate::concurrency::construct_atomic_bool(args)
}

/// `std.sync.yield()`：主动让出当前 fiber，给其它就绪 fiber 一个运行机会。
fn sync_yield(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "yield requires 0 arguments",
        ));
    }
    vm.request_cooperative_yield();
    Ok(Value::None)
}

fn build_path_module() -> Shared<ModuleObject> {
    submodule(
        "path",
        &[
            ("join", builtin(path_join)),
            ("basename", builtin(path_basename)),
            ("dirname", builtin(path_dirname)),
            ("extension", builtin(path_extension)),
            ("stem", builtin(path_stem)),
            ("is_absolute", builtin(path_is_absolute)),
            ("abspath", builtin(path_abspath)),
            ("normalize", builtin(path_normalize)),
            ("splitext", builtin(path_splitext)),
        ],
    )
}

fn build_fs_module() -> Shared<ModuleObject> {
    submodule(
        "fs",
        &[
            ("exists", builtin(fs_exists)),
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
            ("write_bytes", builtin(io_write_bytes)),
        ],
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

fn decos_timer(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let inner = expect_function("timer", args, 0)?;
    Ok(Value::builtin("timer", move |vm, call_args| {
        let start = std::time::Instant::now();
        let result = vm.call_user_function(inner.clone(), call_args.to_vec())?;
        eprintln!("timer: {:?}", start.elapsed());
        Ok(result)
    }))
}

fn decos_debug(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let inner = expect_function("debug", args, 0)?;
    Ok(Value::builtin("debug", move |vm, call_args| {
        let preview: Vec<String> = call_args
            .iter()
            .map(super::runtime::value::Value::print_string)
            .collect();
        eprintln!("debug: call({})", preview.join(", "));
        vm.call_user_function(inner.clone(), call_args.to_vec())
    }))
}

fn decos_retry(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let inner = expect_function("retry", args, 0)?;
    let attempts = if args.len() > 1 {
        let n = expect_int("retry", args, 1)?;
        if n <= 0 {
            return Err(crate::error::RuntimeError::value_err(
                "retry() attempts must be a positive integer",
            ));
        }
        n as usize
    } else {
        3
    };
    Ok(Value::builtin("retry", move |vm, call_args| {
        let mut last_err = None;
        for _ in 0..attempts {
            match vm.call_user_function(inner.clone(), call_args.to_vec()) {
                Ok(v) => return Ok(v),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| crate::error::RuntimeError::msg("retry failed")))
    }))
}

fn decos_validate(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let pred = expect_function("validate", args, 0)?;
    let inner = if args.len() > 1 {
        Some(expect_function("validate", args, 1)?)
    } else {
        None
    };
    Ok(Value::builtin("validate", move |vm, call_args| {
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
    }))
}

fn decos_catch(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let inner = expect_function("catch", args, 0)?;
    let fallback = if args.len() > 1 {
        Some(expect_function("catch", args, 1)?)
    } else {
        None
    };
    Ok(Value::builtin("catch", move |vm, call_args| {
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
    }))
}

fn decos_deprecated(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() == 1 {
        if let Ok(inner) = expect_function("deprecated", args, 0) {
            return Ok(Value::builtin("deprecated", move |vm, call_args| {
                eprintln!("[deprecated]");
                vm.call_user_function(inner.clone(), call_args.to_vec())
            }));
        }
        let msg = args[0].print_string();
        return Ok(Value::builtin("deprecated", move |_vm, call_args| {
            let inner = expect_function("deprecated", call_args, 0)?;
            let msg = msg.clone();
            Ok(Value::builtin("deprecated", move |vm, args| {
                eprintln!("[deprecated] {msg}");
                vm.call_user_function(inner.clone(), args.to_vec())
            }))
        }));
    }
    Err(crate::error::RuntimeError::type_err(
        "deprecated requires 1 argument (function or message)",
    ))
}

fn decos_trace(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let inner = expect_function("trace", args, 0)?;
    Ok(Value::builtin("trace", move |vm, call_args| {
        let shown: Vec<String> = call_args
            .iter()
            .map(super::runtime::value::Value::print_string)
            .collect();
        eprintln!("trace: args={}", shown.join(", "));
        let result = vm.call_user_function(inner.clone(), call_args.to_vec())?;
        eprintln!("trace: => {}", result.print_string());
        Ok(result)
    }))
}

fn decos_singleton(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let factory = expect_function("singleton", args, 0)?;
    let cell = Shared::new(None::<Value>);
    Ok(Value::builtin("singleton", move |vm, _call_args| {
        let mut slot = cell.borrow_mut();
        if let Some(v) = slot.as_ref() {
            return Ok(v.clone());
        }
        let v = vm.call_user_function(factory.clone(), vec![])?;
        *slot = Some(v.clone());
        Ok(v)
    }))
}

fn func_reduce(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 {
        return Err(crate::error::RuntimeError::type_err(
            "reduce requires at least 2 arguments",
        ));
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
        return Err(crate::error::RuntimeError::type_err(
            "compose requires 2 arguments",
        ));
    }
    let f = expect_function("compose", args, 0)?;
    let g = expect_function("compose", args, 1)?;
    Ok(Value::builtin("compose", move |vm, call_args| {
        let mid = vm.call_user_function(g.clone(), call_args.to_vec())?;
        vm.call_user_function(f.clone(), vec![mid])
    }))
}

fn func_partial(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 {
        return Err(crate::error::RuntimeError::type_err(
            "partial requires at least 2 arguments",
        ));
    }
    let func = expect_function("partial", args, 0)?;
    let bound: Vec<Value> = args[1..].to_vec();
    Ok(Value::builtin("partial", move |vm, call_args| {
        let mut full = bound.clone();
        full.extend_from_slice(call_args);
        vm.call_user_function(func.clone(), full)
    }))
}

fn func_identity(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "identity requires 1 argument",
        ));
    }
    Ok(args[0].clone())
}

fn func_const(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "const requires 1 argument",
        ));
    }
    let x = args[0].clone();
    Ok(Value::builtin("const", move |_vm, _args| Ok(x.clone())))
}

fn func_flip(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let func = expect_function("flip", args, 0)?;
    Ok(Value::builtin("flip", move |vm, call_args| {
        if call_args.len() < 2 {
            return Err(crate::error::RuntimeError::type_err(
                "flipped function requires at least 2 arguments",
            ));
        }
        let mut full = call_args.to_vec();
        full.swap(0, 1);
        vm.call_user_function(func.clone(), full)
    }))
}

fn path_join(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let parts: Vec<String> = args
        .iter()
        .map(super::runtime::value::Value::print_string)
        .collect();
    if parts.is_empty() {
        return Ok(Value::Text(String::new()));
    }
    // Windows：首段为盘符/UNC/`\\?\` 绝对路径时用原生 PathBuf，避免与 `/` join 混斜杠
    // （`abspath` 曾返回 `\\?\…` 时 `is_dir`/`exists` 会全假）。
    #[cfg(windows)]
    {
        let first = std::path::Path::new(&parts[0]);
        let looks_win_abs = first.is_absolute()
            || parts[0].starts_with(r"\\?\")
            || (parts[0].len() >= 2
                && parts[0].as_bytes().get(1) == Some(&b':')
                && parts[0]
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphabetic));
        if looks_win_abs {
            let mut buf = std::path::PathBuf::from(&parts[0]);
            for p in parts.iter().skip(1) {
                if p.is_empty() {
                    continue;
                }
                buf.push(p.trim_start_matches(['/', '\\']));
            }
            return Ok(Value::Text(buf.to_string_lossy().into_owned()));
        }
    }
    // 相对路径段仍用 `/`，跨平台可移植。
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
        pick(p).and_then(|s| s.to_str()).unwrap_or("").to_string()
    })
}

macro_rules! define_path_os_components {
    ($(($fn_name:ident, $api:literal, $pick:expr)),+ $(,)?) => {
        $(
            fn $fn_name(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
                path_os_str_component($api, args, $pick)
            }
        )+
    };
}

define_path_os_components! {
    (path_basename, "basename", |p: &std::path::Path| p.file_name()),
    (path_extension, "extension", |p: &std::path::Path| p.extension()),
    (path_stem, "stem", |p: &std::path::Path| p.file_stem()),
}

fn path_dirname(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    path_map_text("dirname", args, |p| {
        p.parent()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    })
}

fn path_is_absolute(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    // 纯词法判断，不触盘，不受沙箱限制。
    let p = expect_text("is_absolute", args, 0)?;
    Ok(Value::Bool(std::path::Path::new(&p).is_absolute()))
}

/// Windows `canonicalize` 常带 `\\?\` 前缀；对外 API 剥掉以便与 `join` / 用户路径一致。
#[cfg(windows)]
fn strip_windows_extended_prefix(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

fn path_abspath(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("abspath", args, 0)?;
    let abs = vm.caps.canonical_host_path("abspath", &p)?;
    #[cfg(windows)]
    let out = strip_windows_extended_prefix(&abs.to_string_lossy());
    #[cfg(not(windows))]
    let out = abs.to_string_lossy().into_owned();
    Ok(Value::Text(out))
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
    let p = expect_text("exists", args, 0)?;
    Ok(Value::Bool(vm.caps.exists("exists", &p)?))
}

fn fs_is_file(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("is_file", args, 0)?;
    Ok(Value::Bool(vm.caps.is_file("is_file", &p)?))
}

fn fs_is_dir(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("is_dir", args, 0)?;
    Ok(Value::Bool(vm.caps.is_dir("is_dir", &p)?))
}

fn fs_list_dir(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("list_dir", args, 0)?;
    let mut names: Vec<Value> = vm
        .caps
        .read_dir_names("list_dir", &p)?
        .into_iter()
        .map(|name| Value::Text(name.to_string_lossy().into_owned()))
        .collect();
    names.sort_by_key(super::runtime::value::Value::print_string);
    vm.request_cooperative_yield();
    Ok(Value::List(Shared::new(names)))
}

fn fs_mkdir(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("mkdir", args, 0)?;
    vm.caps.create_dir("mkdir", &p, false)?;
    Ok(Value::None)
}

fn fs_mkdir_all(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("mkdir_all", args, 0)?;
    vm.caps.create_dir("mkdir_all", &p, true)?;
    Ok(Value::None)
}

fn fs_remove(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("remove", args, 0)?;
    vm.caps.remove_file("remove", &p)?;
    Ok(Value::None)
}

fn fs_remove_dir(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("remove_dir", args, 0)?;
    vm.caps.remove_dir_all("remove_dir", &p)?;
    Ok(Value::None)
}

fn fs_rename(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "rename requires 2 arguments",
        ));
    }
    let from = expect_text("rename", args, 0)?;
    let to = expect_text("rename", args, 1)?;
    vm.caps.rename("rename", &from, &to)?;
    Ok(Value::None)
}

fn fs_copy(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "copy requires 2 arguments",
        ));
    }
    let from = expect_text("copy", args, 0)?;
    let to = expect_text("copy", args, 1)?;
    vm.caps.copy("copy", &from, &to)?;
    Ok(Value::None)
}

fn debug_traceback(vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    Ok(crate::traceback::capture_traceback(vm))
}

fn debug_format_tb(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "format_tb requires 1 argument",
        ));
    }
    Ok(Value::Text(args[0].display_string()))
}

fn debug_print_tb(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "print_tb requires 1 argument",
        ));
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
            .map(super::runtime::value::Value::print_string)
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
        return Err(crate::error::RuntimeError::type_err(
            "type_name requires 1 argument",
        ));
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

fn build_re_module() -> Shared<ModuleObject> {
    submodule(
        "re",
        &[
            ("compile", builtin(re_compile)),
            ("match", builtin(re_match)),
            ("findall", builtin(re_findall)),
            ("sub", builtin(re_sub)),
            ("split", builtin(re_split)),
        ],
    )
}

fn build_hash_module() -> Shared<ModuleObject> {
    submodule(
        "hash",
        &[
            ("md5", builtin(hash_md5)),
            ("sha256", builtin(hash_sha256)),
            ("sha512", builtin(hash_sha512)),
            ("hmac", builtin(hash_hmac)),
        ],
    )
}

fn build_exceptions_module() -> Shared<ModuleObject> {
    submodule(
        "exceptions",
        &[
            ("bases", builtin(exc_bases)),
            ("chain", builtin(exc_chain)),
            ("tree", builtin(exc_tree)),
        ],
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
    let re_p = re;
    Ok(Value::Module(Shared::new(ModuleObject {
        name: "Pattern".into(),
        exports: exports(&[
            (
                "match",
                Value::builtin("match", move |vm, a| re_match_impl(vm, &re_m, a)),
            ),
            (
                "findall",
                Value::builtin("findall", move |_vm, a| {
                    let text = expect_text("findall", a, 0)?;
                    let out: Vec<Value> = re_f
                        .find_iter(&text)
                        .map(|m| Value::Text(m.as_str().to_string()))
                        .collect();
                    Ok(Value::List(Shared::new(out)))
                }),
            ),
            (
                "sub",
                Value::builtin("sub", move |_vm, a| {
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
                }),
            ),
            (
                "split",
                Value::builtin("split", move |_vm, a| {
                    let text = expect_text("split", a, 0)?;
                    let out: Vec<Value> = re_p
                        .split(&text)
                        .map(|s| Value::Text(s.to_string()))
                        .collect();
                    Ok(Value::List(Shared::new(out)))
                }),
            ),
            ("pattern", Value::Text(pat)),
        ]),
        children: HashMap::new(),
        is_user: false,
    })))
}

fn re_match(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "match requires 2 arguments",
        ));
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
        return Err(crate::error::RuntimeError::type_err(
            "findall requires 2 arguments",
        ));
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
        return Err(crate::error::RuntimeError::type_err(
            "sub requires 3 arguments",
        ));
    }
    let pat = expect_text("sub", args, 0)?;
    let repl = expect_text("sub", args, 1)?;
    let text = expect_text("sub", args, 2)?;
    let re = regex::Regex::new(&pat)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("invalid regex: {e}")))?;
    Ok(Value::Text(
        re.replace_all(&text, repl.as_str()).into_owned(),
    ))
}

fn re_split(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "split requires 2 arguments",
        ));
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
    use digest::Digest;
    use md5::Md5;
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

fn hash_sha512(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    use sha2::{Digest, Sha512};
    let text = expect_text("sha512", args, 0)?;
    let digest = Sha512::digest(text.as_bytes());
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
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "bases requires exception type name",
            ))
        }
    };
    Ok(match crate::exceptions::direct_base(vm, name) {
        Some(base) => Value::Text(base),
        None => Value::None,
    })
}

fn exc_chain(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let name = match args.first() {
        Some(Value::Text(s)) => s.as_str(),
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "chain requires exception type name",
            ))
        }
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
            base.map_or(Value::None, |b| Value::Text(b.type_name().to_string())),
        );
    }
    Ok(Value::Dict(Shared::new(out)))
}

// ---------------------------------------------------------------------------
// 数据格式解析标准库 —— csv / toml / yaml / xml
// ---------------------------------------------------------------------------

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
        .map(|r| r.map_err(|e| crate::error::RuntimeError::value_err(format!("csv parse: {e}"))))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(|r| r.iter().map(std::string::ToString::to_string).collect())
        .collect();

    if header {
        let headers = rdr
            .headers()
            .map_err(|e| crate::error::RuntimeError::value_err(format!("csv headers: {e}")))?
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();
        let out: Vec<Value> = rows
            .iter()
            .map(|row| {
                let mut d = DictMap::new();
                for (i, h) in headers.iter().enumerate() {
                    let v = row.get(i).map_or("", std::string::String::as_str);
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
                    row.iter().map(|s| Value::Text(s.clone())).collect(),
                ))
            })
            .collect();
        Ok(Value::List(Shared::new(out)))
    }
}

fn csv_stringify(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("stringify", args, 1)?;
    let rows = match &args[0] {
        Value::List(list) => list.borrow().clone(),
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "csv.stringify expects a list of rows (each row a list)",
            ))
        }
    };
    let mut wtr = csv::Writer::from_writer(vec![]);
    for row in rows {
        let cells: Vec<String> = match row {
            Value::List(list) => list
                .borrow()
                .iter()
                .map(super::runtime::value::Value::print_string)
                .collect(),
            Value::Tuple(t) => t
                .iter()
                .map(super::runtime::value::Value::print_string)
                .collect(),
            other => {
                return Err(crate::error::RuntimeError::type_err(format!(
                    "csv.stringify row must be list/tuple, got {}",
                    other.type_name()
                )))
            }
        };
        wtr.write_record(&cells)
            .map_err(|e| crate::error::RuntimeError::value_err(format!("csv.stringify: {e}")))?;
    }
    let data = wtr
        .into_inner()
        .map_err(|e| crate::error::RuntimeError::value_err(format!("csv.stringify: {e}")))?;
    Ok(Value::Text(String::from_utf8(data).map_err(|e| {
        crate::error::RuntimeError::value_err(format!("csv.stringify: {e}"))
    })?))
}

fn build_csv_module() -> Shared<ModuleObject> {
    submodule(
        "csv",
        &[
            ("parse", builtin(csv_parse)),
            ("stringify", builtin(csv_stringify)),
        ],
    )
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

const XML_MAX_DEPTH: usize = 64;

fn xml_stringify(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("stringify", args, 1)?;
    Ok(Value::Text(xml_value_to_string(&args[0], 0)?))
}

fn xml_value_to_string(v: &Value, depth: usize) -> Result<String> {
    if depth >= XML_MAX_DEPTH {
        return Err(crate::error::RuntimeError::value_err(
            "xml.stringify: document exceeds maximum nesting depth",
        ));
    }
    let Value::Dict(d) = v else {
        return Err(crate::error::RuntimeError::type_err(
            "xml.stringify expects a dict {tag, attrs, text, children}",
        ));
    };
    let d = d.borrow();
    let tag = match d.get(&ValueKey::Text("tag".into())) {
        Some(Value::Text(s)) if !s.is_empty() => s.clone(),
        _ => {
            return Err(crate::error::RuntimeError::value_err(
                "xml.stringify: missing text field `tag`",
            ))
        }
    };
    if !xml_tag_is_valid(&tag) {
        return Err(crate::error::RuntimeError::value_err(format!(
            "xml.stringify: invalid tag `{tag}`"
        )));
    }
    let mut out = String::new();
    out.push('<');
    out.push_str(&tag);
    if let Some(Value::Dict(attrs)) = d.get(&ValueKey::Text("attrs".into())) {
        for (k, val) in attrs.borrow().iter() {
            let key = match k {
                ValueKey::Text(s) => s.as_str(),
                _ => continue,
            };
            let vs = match val {
                Value::Text(s) => s.clone(),
                Value::None => continue,
                other => other.print_string(),
            };
            out.push(' ');
            out.push_str(key);
            out.push_str("=\"");
            out.push_str(&xml_escape_attr(&vs));
            out.push('"');
        }
    }
    let text = match d.get(&ValueKey::Text("text".into())) {
        Some(Value::Text(s)) => s.as_str(),
        _ => "",
    };
    let children = match d.get(&ValueKey::Text("children".into())) {
        Some(Value::List(l)) => l.borrow().clone(),
        Some(Value::Tuple(t)) => t.iter().cloned().collect(),
        _ => Vec::new(),
    };
    if text.is_empty() && children.is_empty() {
        out.push_str("/>");
        return Ok(out);
    }
    out.push('>');
    if !text.is_empty() {
        out.push_str(&xml_escape_text(text));
    }
    for child in children {
        out.push_str(&xml_value_to_string(&child, depth + 1)?);
    }
    out.push_str("</");
    out.push_str(&tag);
    out.push('>');
    Ok(out)
}

/// XML Name：ASCII 字母/`_` 开头；可含数字/`-`；至多一个 `:`（前缀/本地名各自合法）。
fn xml_tag_is_valid(tag: &str) -> bool {
    if tag.is_empty() {
        return false;
    }
    let mut saw_colon = false;
    let mut at_name_start = true;
    for c in tag.chars() {
        if c == ':' {
            if saw_colon || at_name_start {
                return false;
            }
            saw_colon = true;
            at_name_start = true;
            continue;
        }
        if at_name_start {
            if !(c.is_ascii_alphabetic() || c == '_') {
                return false;
            }
            at_name_start = false;
            continue;
        }
        if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return false;
        }
    }
    !at_name_start
}

fn xml_escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_escape_attr(s: &str) -> String {
    xml_escape_text(s)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn build_xml_module() -> Shared<ModuleObject> {
    submodule(
        "xml",
        &[
            ("parse", builtin(xml_parse)),
            ("stringify", builtin(xml_stringify)),
        ],
    )
}
