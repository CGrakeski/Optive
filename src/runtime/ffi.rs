//! 动态库加载与 C ABI 调用（MVP：平台默认调用约定）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::rc::Rc;

use libffi::middle::{Arg, Cif, CodePtr, Type as FfiType};
use libloading::Library;

use crate::ast::TypeExpr;
use crate::error::RuntimeError;
use crate::opcode::FunctionObject;
use crate::sized::SizedNum;
use crate::value::{BuiltinFn, ModuleObject, Value};
use crate::vm::Vm;
use crate::Result;

#[derive(Clone)]
pub struct DllHandle {
    pub path: String,
    pub lib: Rc<Library>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiType {
    Void,
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    Isize,
    Usize,
    F32,
    F64,
    Pointer,
}

impl AbiType {
    pub fn from_type_expr(vm: &Vm, ty: &TypeExpr) -> Result<Self> {
        let name = crate::types::resolve_type_expr_name(vm, ty)?;
        Self::from_type_name(&name)
    }

    pub fn from_type_name(name: &str) -> Result<Self> {
        crate::c_types::abi_from_type_name(name).ok_or_else(|| {
            RuntimeError::type_err(format!("unsupported C ABI type: {name}"))
        })
    }

    fn ffi_type(self) -> FfiType {
        match self {
            Self::Void => FfiType::void(),
            Self::Bool | Self::I8 => FfiType::i8(),
            Self::U8 => FfiType::u8(),
            Self::I16 => FfiType::i16(),
            Self::U16 => FfiType::u16(),
            Self::I32 => FfiType::i32(),
            Self::U32 => FfiType::u32(),
            Self::I64 => FfiType::i64(),
            Self::U64 => FfiType::u64(),
            Self::Isize => {
                if cfg!(target_pointer_width = "32") {
                    FfiType::i32()
                } else {
                    FfiType::i64()
                }
            }
            Self::Usize | Self::Pointer => {
                if cfg!(target_pointer_width = "32") {
                    FfiType::u32()
                } else {
                    FfiType::u64()
                }
            }
            Self::F32 => FfiType::f32(),
            Self::F64 => FfiType::f64(),
        }
    }
}

fn type_expr_name(vm: &Vm, ty: &TypeExpr) -> Result<String> {
    crate::types::resolve_type_expr_name(vm, ty)
}

pub fn load_library(path: &str) -> Result<Value> {
    let lib = unsafe { Library::new(path) }.map_err(|e| {
        RuntimeError::msg(format!("failed to load dynamic library '{path}': {e}"))
    })?;
    Ok(Value::DllHandle(Rc::new(DllHandle {
        path: path.to_string(),
        lib: Rc::new(lib),
    })))
}

/// 内置 `extern(handle[, symbol])` → 装饰器。
pub fn builtin_extern(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(RuntimeError::type_err(
            "extern requires 1 or 2 arguments (handle[, symbol])",
        ));
    }
    let handle = match &args[0] {
        Value::DllHandle(h) => h.clone(),
        _ => {
            return Err(RuntimeError::type_err(
                "extern: first argument must be a library handle from C.frompath",
            ))
        }
    };
    let symbol_override = if args.len() == 2 {
        match &args[1] {
            Value::Text(s) => Some(s.clone()),
            _ => {
                return Err(RuntimeError::type_err(
                    "extern: second argument must be text symbol name",
                ))
            }
        }
    } else {
        None
    };
    Ok(Value::Builtin(Rc::new(move |vm, deco_args| {
        if deco_args.len() != 1 {
            return Err(RuntimeError::type_err(
                "extern decorator requires 1 argument (function)",
            ));
        }
        let func = match &deco_args[0] {
            Value::Function(f) => f.clone(),
            other => {
                return Err(RuntimeError::type_err(format!(
                    "extern decorator expects function, got {}",
                    other.type_name()
                )))
            }
        };
        bind_extern_function(vm, handle.clone(), symbol_override.clone(), func)
    })))
}

fn bind_extern_function(
    vm: &mut Vm,
    handle: Rc<DllHandle>,
    symbol_override: Option<String>,
    func: Rc<FunctionObject>,
) -> Result<Value> {
    let sym_name = symbol_override.unwrap_or_else(|| func.name.clone());
    let mut name_buf = sym_name.into_bytes();
    name_buf.push(0);
    let code_ptr = {
        let sym = unsafe {
            handle.lib.get::<unsafe extern "C" fn()>(name_buf.as_slice())
        }
        .map_err(|e| {
            RuntimeError::msg(format!(
                "extern: symbol '{}' not found in '{}': {e}",
                String::from_utf8_lossy(&name_buf[..name_buf.len() - 1]),
                handle.path
            ))
        })?;
        CodePtr((*sym) as *mut c_void)
    };

    let mut arg_abis = Vec::new();
    for p in &func.params {
        if p.is_variadic || p.is_kwvariadic {
            return Err(RuntimeError::type_err(
                "extern functions cannot use *args/**kwargs",
            ));
        }
        let Some(ty) = &p.type_expr else {
            return Err(RuntimeError::type_err(format!(
                "extern parameter '{}' requires a type annotation for ABI",
                p.name
            )));
        };
        arg_abis.push(AbiType::from_type_expr(vm, ty)?);
    }
    let ret_abi = match &func.return_type {
        Some(ty) => AbiType::from_type_expr(vm, ty)?,
        None => AbiType::Void,
    };

    let arg_ffi: Vec<FfiType> = arg_abis.iter().copied().map(AbiType::ffi_type).collect();
    let cif = Cif::new(arg_ffi, ret_abi.ffi_type());

    let params = func.params.clone();
    let return_wrapper = func.return_wrapper.clone();
    let return_type = func.return_type.clone();
    let return_strong = func.return_strong;
    let func_name = func.name.clone();
    // 绑定后保留库句柄：调用性与用户侧句柄变量解耦，但库本身不得被卸载。
    let keep_lib = handle;

    let wrapper: BuiltinFn = Rc::new(move |vm, call_args| {
        let _keep_loaded = &keep_lib;
        if call_args.len() != params.len() {
            return Err(RuntimeError::type_err(format!(
                "{}() expects {} argument(s), got {}",
                func_name,
                params.len(),
                call_args.len()
            )));
        }

        let mut converted = Vec::with_capacity(call_args.len());
        for (i, param) in params.iter().enumerate() {
            let mut arg = call_args[i].clone();
            if param.implicit {
                if let Some(ty) = &param.type_expr {
                    if !crate::types::type_accepts(vm, &arg, ty) {
                        let type_name = type_expr_name(vm, ty)?;
                        arg = vm.convert_type(Value::type_ref(type_name), arg)?;
                    }
                }
            } else if let Some(ty) = &param.type_expr {
                // 非 implicit 的类型参数：须已匹配（不自动转换）
                if !crate::types::type_accepts(vm, &arg, ty) {
                    let expected = type_expr_name(vm, ty)?;
                    let msg = format!(
                        "parameter '{}': expected {}, got {} (use implicit to convert)",
                        param.name,
                        expected,
                        arg.type_name()
                    );
                    let exc = crate::exceptions::make_exception(vm, "TypeError", msg)?;
                    vm.throw_value(exc)?;
                    return Ok(Value::None);
                }
            }
            if param.type_strong {
                if let Some(ty) = &param.type_expr {
                    if let Some(detail) = crate::types::type_check_error(vm, &arg, ty) {
                        let msg = format!("parameter '{}': {detail}", param.name);
                        let exc = crate::exceptions::make_exception(vm, "TypeError", msg)?;
                        vm.throw_value(exc)?;
                        return Ok(Value::None);
                    }
                }
            }
            converted.push(arg);
        }

        let mut storage: Vec<ArgStorage> = converted
            .iter()
            .zip(arg_abis.iter())
            .map(|(v, abi)| value_to_storage(v, *abi))
            .collect::<Result<_>>()?;
        let ffi_args: Vec<Arg> = storage.iter_mut().map(|s| s.as_arg()).collect();

        let raw = unsafe { call_cif(&cif, code_ptr, &ffi_args, ret_abi)? };
        let mut out = abi_to_value(raw, ret_abi)?;
        if let Some(ref wrapper_expr) = return_wrapper {
            out = eval_wrapper_expr(vm, wrapper_expr, out)?;
        }
        if return_strong {
            if let Some(ref ty) = return_type {
                if let Some(detail) = crate::types::type_check_error(vm, &out, ty) {
                    let msg = format!("return value: {detail}");
                    let exc = crate::exceptions::make_exception(vm, "TypeError", msg)?;
                    vm.throw_value(exc)?;
                    return Ok(Value::None);
                }
            }
        }
        Ok(out)
    });

    Ok(Value::Builtin(wrapper))
}

enum ArgStorage {
    I8(i8),
    U8(u8),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F32(f32),
    F64(f64),
    Ptr(usize),
}

impl ArgStorage {
    fn as_arg(&mut self) -> Arg {
        match self {
            Self::I8(v) => Arg::new(v),
            Self::U8(v) => Arg::new(v),
            Self::I16(v) => Arg::new(v),
            Self::U16(v) => Arg::new(v),
            Self::I32(v) => Arg::new(v),
            Self::U32(v) => Arg::new(v),
            Self::I64(v) => Arg::new(v),
            Self::U64(v) => Arg::new(v),
            Self::F32(v) => Arg::new(v),
            Self::F64(v) => Arg::new(v),
            Self::Ptr(v) => Arg::new(v),
        }
    }
}

fn value_to_storage(v: &Value, abi: AbiType) -> Result<ArgStorage> {
    match abi {
        AbiType::Void => Err(RuntimeError::type_err("void cannot be a parameter")),
        AbiType::Bool => Ok(ArgStorage::I8(if v.is_truthy() { 1 } else { 0 })),
        AbiType::I8 => Ok(ArgStorage::I8(narrow_i64(v, i8::MIN as i64, i8::MAX as i64, "i8")? as i8)),
        AbiType::U8 => Ok(ArgStorage::U8(narrow_i64(v, 0, u8::MAX as i64, "u8")? as u8)),
        AbiType::I16 => {
            Ok(ArgStorage::I16(narrow_i64(v, i16::MIN as i64, i16::MAX as i64, "i16")? as i16))
        }
        AbiType::U16 => Ok(ArgStorage::U16(narrow_i64(v, 0, u16::MAX as i64, "u16")? as u16)),
        AbiType::I32 => {
            Ok(ArgStorage::I32(narrow_i64(v, i32::MIN as i64, i32::MAX as i64, "i32")? as i32))
        }
        AbiType::U32 => Ok(ArgStorage::U32(narrow_i64(v, 0, u32::MAX as i64, "u32")? as u32)),
        AbiType::I64 => Ok(ArgStorage::I64(as_i64(v)?)),
        AbiType::U64 => Ok(ArgStorage::U64(as_u64(v)?)),
        AbiType::Isize => {
            if cfg!(target_pointer_width = "32") {
                Ok(ArgStorage::I32(
                    narrow_i64(v, i32::MIN as i64, i32::MAX as i64, "isize")? as i32,
                ))
            } else {
                Ok(ArgStorage::I64(as_i64(v)?))
            }
        }
        AbiType::Usize | AbiType::Pointer => Ok(ArgStorage::Ptr(as_usize(v)?)),
        AbiType::F32 => Ok(ArgStorage::F32(as_f64(v)? as f32)),
        AbiType::F64 => Ok(ArgStorage::F64(as_f64(v)?)),
    }
}

fn narrow_i64(v: &Value, min: i64, max: i64, abi_name: &str) -> Result<i64> {
    let n = as_i64(v)?;
    if n < min || n > max {
        return Err(RuntimeError::value_err(format!(
            "FFI argument {n} does not fit in {abi_name} range [{min}, {max}]"
        )));
    }
    Ok(n)
}

enum RetStorage {
    Void,
    I8(i8),
    U8(u8),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F32(f32),
    F64(f64),
}

unsafe fn call_cif(
    cif: &Cif,
    code: CodePtr,
    args: &[Arg],
    ret: AbiType,
) -> Result<RetStorage> {
    Ok(match ret {
        AbiType::Void => {
            cif.call::<()>(code, args);
            RetStorage::Void
        }
        AbiType::Bool | AbiType::I8 => RetStorage::I8(cif.call::<i8>(code, args)),
        AbiType::U8 => RetStorage::U8(cif.call::<u8>(code, args)),
        AbiType::I16 => RetStorage::I16(cif.call::<i16>(code, args)),
        AbiType::U16 => RetStorage::U16(cif.call::<u16>(code, args)),
        AbiType::I32 => RetStorage::I32(cif.call::<i32>(code, args)),
        AbiType::U32 => RetStorage::U32(cif.call::<u32>(code, args)),
        AbiType::I64 => RetStorage::I64(cif.call::<i64>(code, args)),
        AbiType::Isize => {
            if cfg!(target_pointer_width = "32") {
                RetStorage::I64(cif.call::<i32>(code, args) as i64)
            } else {
                RetStorage::I64(cif.call::<i64>(code, args))
            }
        }
        AbiType::U64 => RetStorage::U64(cif.call::<u64>(code, args)),
        AbiType::Usize | AbiType::Pointer => {
            if cfg!(target_pointer_width = "32") {
                RetStorage::U64(cif.call::<u32>(code, args) as u64)
            } else {
                RetStorage::U64(cif.call::<u64>(code, args))
            }
        }
        AbiType::F32 => RetStorage::F32(cif.call::<f32>(code, args)),
        AbiType::F64 => RetStorage::F64(cif.call::<f64>(code, args)),
    })
}

fn abi_to_value(ret: RetStorage, abi: AbiType) -> Result<Value> {
    Ok(match (ret, abi) {
        (RetStorage::Void, _) => Value::None,
        (RetStorage::I8(v), AbiType::Bool) => Value::Bool(v != 0),
        (RetStorage::I8(v), _) => Value::Sized(SizedNum::I8(v)),
        (RetStorage::U8(v), _) => Value::Sized(SizedNum::U8(v)),
        (RetStorage::I16(v), _) => Value::Sized(SizedNum::I16(v)),
        (RetStorage::U16(v), _) => Value::Sized(SizedNum::U16(v)),
        (RetStorage::I32(v), _) => Value::Sized(SizedNum::I32(v)),
        (RetStorage::U32(v), _) => Value::Sized(SizedNum::U32(v)),
        (RetStorage::I64(v), AbiType::Isize) => Value::Sized(SizedNum::Isize(v as isize)),
        (RetStorage::I64(v), _) => Value::Sized(SizedNum::I64(v)),
        (RetStorage::U64(v), AbiType::Usize) => Value::Sized(SizedNum::Usize(v as usize)),
        (RetStorage::U64(v), AbiType::Pointer) => Value::Ptr(v as usize),
        (RetStorage::U64(v), _) => Value::Sized(SizedNum::U64(v)),
        (RetStorage::F32(v), _) => Value::Sized(SizedNum::F32(v)),
        (RetStorage::F64(v), _) => Value::Sized(SizedNum::F64(v)),
    })
}

fn as_i64(v: &Value) -> Result<i64> {
    match v {
        Value::Sized(s) => s.to_i64().ok_or_else(|| {
            RuntimeError::type_err(format!("cannot pass {} as integer ABI value", s.type_name()))
        }),
        Value::Num(n) => n
            .to_i64()
            .ok_or_else(|| RuntimeError::type_err("cannot pass non-integer num as integer ABI value")),
        Value::Bool(b) => Ok(if *b { 1 } else { 0 }),
        Value::Ptr(p) => Ok(*p as i64),
        other => Err(RuntimeError::type_err(format!(
            "cannot convert {} to integer ABI value",
            other.type_name()
        ))),
    }
}

fn as_u64(v: &Value) -> Result<u64> {
    match v {
        Value::Sized(SizedNum::U64(x)) => Ok(*x),
        Value::Sized(SizedNum::Usize(x)) => Ok(*x as u64),
        Value::Ptr(p) => Ok(*p as u64),
        _ => Ok(as_i64(v)? as u64),
    }
}

fn as_usize(v: &Value) -> Result<usize> {
    match v {
        Value::Ptr(p) => Ok(*p),
        Value::Sized(SizedNum::Usize(x)) => Ok(*x),
        Value::Sized(SizedNum::Isize(x)) => Ok(*x as usize),
        _ => Ok(as_i64(v)? as usize),
    }
}

fn as_f64(v: &Value) -> Result<f64> {
    match v {
        Value::Sized(s) => Ok(s.to_f64()),
        Value::Num(n) => n.to_f64_checked(),
        other => Err(RuntimeError::type_err(format!(
            "cannot convert {} to float ABI value",
            other.type_name()
        ))),
    }
}

fn eval_wrapper_expr(vm: &mut Vm, expr: &crate::ast::Expr, raw: Value) -> Result<Value> {
    use crate::ast::ExprKind;
    match &expr.kind {
        ExprKind::Var(name) if name == crate::ast::RET_WRAPPER_VAL => Ok(raw),
        ExprKind::TypeConvert { type_expr, value } => {
            let inner = eval_wrapper_expr(vm, value, raw)?;
            let ty_val = eval_type_operand(type_expr)?;
            vm.convert_type(ty_val, inner)
        }
        ExprKind::Call { callee, args } => {
            let c = eval_wrapper_expr(vm, callee, raw.clone())?;
            let mut call_args = Vec::new();
            for a in args {
                call_args.push(eval_wrapper_expr(vm, &a.value, raw.clone())?);
            }
            vm.call_value(c, call_args)
        }
        other => Err(RuntimeError::type_err(format!(
            "unsupported extern return wrapper expression: {other:?}"
        ))),
    }
}

fn eval_type_operand(expr: &crate::ast::Expr) -> Result<Value> {
    use crate::ast::ExprKind;
    match &expr.kind {
        ExprKind::Var(name) => Ok(Value::type_ref(name.clone())),
        ExprKind::Member { object, field } => {
            let base = eval_type_operand(object)?;
            match base {
                Value::TypeRef(n) => Ok(Value::type_ref(format!("{n}.{field}"))),
                Value::Module(m) => m
                    .borrow()
                    .get_attr(field)
                    .ok_or_else(|| RuntimeError::attr_err(format!("no export '{field}'"))),
                other => Err(RuntimeError::type_err(format!(
                    "cannot resolve type member on {}",
                    other.type_name()
                ))),
            }
        }
        _ => Err(RuntimeError::type_err(
            "return wrapper type must be a type name",
        )),
    }
}

/// 构建 `std.language.C` 模块（含 `types` 子模块）。
pub fn build_c_language_module() -> Rc<RefCell<ModuleObject>> {
    let types = build_c_types_module();
    let mut children = HashMap::new();
    children.insert("types".into(), types.clone());
    let mut exports = HashMap::new();
    exports.insert(
        "frompath".into(),
        Value::Builtin(Rc::new(|_vm, args| {
            if args.len() != 1 {
                return Err(RuntimeError::type_err("C.frompath requires 1 text path"));
            }
            let path = match &args[0] {
                Value::Text(s) => s.clone(),
                _ => return Err(RuntimeError::type_err("C.frompath requires text path")),
            };
            load_library(&path)
        })),
    );
    exports.insert("types".into(), Value::Module(types));
    Rc::new(RefCell::new(ModuleObject {
        name: "C".into(),
        full_name: "std.language.C".into(),
        exports,
        children,
        is_user: false,
    }))
}

fn build_c_types_module() -> Rc<RefCell<ModuleObject>> {
    let mut exports = HashMap::new();
    for entry in crate::c_types::C_TYPES {
        let ty = Value::type_ref(entry.full_name());
        exports.insert(entry.c_name.to_string(), ty.clone());
        for alias in entry.export_aliases {
            exports.insert((*alias).to_string(), ty.clone());
        }
    }

    Rc::new(RefCell::new(ModuleObject {
        name: "types".into(),
        full_name: "std.language.C.types".into(),
        exports,
        children: HashMap::new(),
        is_user: false,
    }))
}

pub fn build_language_module() -> Rc<RefCell<ModuleObject>> {
    let c = build_c_language_module();
    let mut children = HashMap::new();
    children.insert("C".into(), c.clone());
    let mut exports = HashMap::new();
    exports.insert("C".into(), Value::Module(c));
    Rc::new(RefCell::new(ModuleObject {
        name: "language".into(),
        full_name: "std.language".into(),
        exports,
        children,
        is_user: false,
    }))
}
