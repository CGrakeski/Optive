//! 动态库加载与 C ABI 调用。
//!
//! libffi 的 `Cif`/`CodePtr` 含裸指针，本身非 `Send`/`Sync`。M:N 下 Builtin
//! 须跨线程持有，故用 `FfiCallable` + 调用侧互斥（默认 per-callable；
//! `OPTIVE_FFI_SERIAL=1` 时回退全局可重入锁）。

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use libffi::middle::{Arg, Cif, CodePtr, Type as FfiType};
use libloading::Library;
use parking_lot::ReentrantMutex;

use crate::ast::Expr;
use crate::error::RuntimeError;
use crate::opcode::FunctionObject;
use crate::sized::SizedNum;
use crate::value::{BuiltinFn, ModuleObject, Value};
use crate::vm::Vm;
use crate::Result;

use crate::shared::Shared;

/// 跨线程持有的 FFI 调用描述；默认经 per-callable 锁调用（同符号串行，异符号可并行）。
pub(crate) struct FfiCallable {
    cif: Cif,
    code: CodePtr,
    /// 同符号互斥；同步回调重入同一符号时用可重入锁。
    lock: ReentrantMutex<()>,
}

// SAFETY: 指针指向已加载库内的稳定符号与 libffi 分配的 CIF；
// 调用侧持 per-callable（或全局串行）锁，且不同 CIF 可并行（libffi 惯例）。
unsafe impl Send for FfiCallable {}
unsafe impl Sync for FfiCallable {}

/// 可重入全局锁：`OPTIVE_FFI_SERIAL=1` 或未开启并行时的保底语义；
/// 同步回调里再次 `extern` 不会自死锁。
pub static FFI_CALL_LOCK: ReentrantMutex<()> = ReentrantMutex::new(());

static FFI_SERIAL_ENV: AtomicBool = AtomicBool::new(false);
static FFI_SERIAL_INIT: AtomicBool = AtomicBool::new(false);
static FFI_THREADS_ENV: AtomicUsize = AtomicUsize::new(0);
static FFI_THREADS_INIT: AtomicBool = AtomicBool::new(false);

fn env_truthy(name: &str) -> bool {
    match std::env::var(name) {
        Ok(s) => {
            let s = s.trim();
            !(s.is_empty() || s == "0" || s.eq_ignore_ascii_case("false") || s.eq_ignore_ascii_case("no"))
        }
        Err(_) => false,
    }
}

/// `OPTIVE_FFI_SERIAL=1` → 强制全局串行（今日语义）。
pub fn configured_ffi_serial() -> bool {
    if !FFI_SERIAL_INIT.load(Ordering::Acquire) {
        FFI_SERIAL_ENV.store(env_truthy("OPTIVE_FFI_SERIAL"), Ordering::Release);
        FFI_SERIAL_INIT.store(true, Ordering::Release);
    }
    FFI_SERIAL_ENV.load(Ordering::Acquire)
}

/// `OPTIVE_FFI_THREADS`：卸荷池线程数；`0`（默认）= 关闭卸荷，inline 调用。
pub fn configured_ffi_threads() -> usize {
    if !FFI_THREADS_INIT.load(Ordering::Acquire) {
        let n = std::env::var("OPTIVE_FFI_THREADS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        FFI_THREADS_ENV.store(n, Ordering::Release);
        FFI_THREADS_INIT.store(true, Ordering::Release);
    }
    FFI_THREADS_ENV.load(Ordering::Acquire)
}

/// 每棵 Vm 树（主实例 + helper）共享的 FFI 开关。
pub struct FfiRuntimeConfig {
    serial: AtomicBool,
    threads: AtomicUsize,
}

impl FfiRuntimeConfig {
    pub fn from_env() -> Arc<Self> {
        Arc::new(Self {
            serial: AtomicBool::new(configured_ffi_serial()),
            threads: AtomicUsize::new(configured_ffi_threads()),
        })
    }

    pub fn serial(&self) -> bool {
        self.serial.load(Ordering::Acquire)
    }

    pub fn set_serial(&self, serial: bool) {
        self.serial.store(serial, Ordering::Release);
    }

    pub fn threads(&self) -> usize {
        self.threads.load(Ordering::Acquire)
    }

    pub fn set_threads(&self, n: usize) {
        self.threads.store(n, Ordering::Release);
        if n > 0 {
            crate::ffi_pool::resize_pool(n);
        }
    }
}

#[derive(Clone)]
pub struct DllHandle {
    pub path: String,
    pub lib: Arc<Library>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallConv {
    C,
    Stdcall,
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
    /// 指针宽度；`extern` 可从 `text` 临时编成 NUL 结尾 UTF-8。
    CharPtr,
    /// 指针宽度；`extern` 可从 `text` 临时编成 UTF-16 NUL 结尾。
    WCharPtr,
}

impl AbiType {
    pub fn from_type_annotation(vm: &Vm, ty: &Expr) -> Result<Self> {
        if let Some(val) = crate::types::static_type_value_from_expr(ty) {
            return Self::from_type_value(vm, &val);
        }
        let name = crate::types::resolve_type_name_from_expr(vm, ty)?;
        if crate::ptr_registry::is_ptr_type_name(&name) {
            return Ok(Self::Pointer);
        }
        if let Some(def) = vm.struct_defs.get(&name) {
            if def.c_layout.is_some() {
                return Err(RuntimeError::type_err(format!(
                    "unsupported C ABI type: {name} (struct by-value not supported; \
                     use C.types.ptr[{name}] or C.types.void_ptr)"
                )));
            }
        }
        Self::from_type_name(&name)
    }

    fn from_type_value(vm: &Vm, val: &Value) -> Result<Self> {
        match val {
            Value::TypeSpec(spec) if crate::ptr_registry::is_ptr_type_name(&spec.name) => {
                Ok(Self::Pointer)
            }
            Value::TypeRef(name) | Value::Text(name)
                if crate::ptr_registry::is_ptr_type_name(name) =>
            {
                Ok(Self::Pointer)
            }
            _ => {
                let name = crate::types::type_value_display(val);
                if crate::ptr_registry::is_ptr_type_name(&name) {
                    return Ok(Self::Pointer);
                }
                if let Some(def) = vm.struct_defs.get(&name) {
                    if def.c_layout.is_some() {
                        return Err(RuntimeError::type_err(format!(
                            "unsupported C ABI type: {name} (struct by-value not supported; \
                             use C.types.ptr[{name}] or C.types.void_ptr)"
                        )));
                    }
                }
                Self::from_type_name(&name)
            }
        }
    }

    pub fn from_type_name(name: &str) -> Result<Self> {
        crate::c_types::abi_from_type_name(name).ok_or_else(|| {
            RuntimeError::type_err(format!("unsupported C ABI type: {name}"))
        })
    }

    pub(crate) fn ffi_type(self) -> FfiType {
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
            Self::Usize | Self::Pointer | Self::CharPtr | Self::WCharPtr => {
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

/// `(size, align)` in bytes for layout / `C.sizeof`.
pub fn abi_size_align(abi: AbiType) -> (usize, usize) {
    match abi {
        AbiType::Void => (0, 1),
        AbiType::Bool | AbiType::I8 | AbiType::U8 => (1, 1),
        AbiType::I16 | AbiType::U16 => (2, 2),
        AbiType::I32 | AbiType::U32 | AbiType::F32 => (4, 4),
        AbiType::I64 | AbiType::U64 | AbiType::F64 => (8, 8),
        AbiType::Isize
        | AbiType::Usize
        | AbiType::Pointer
        | AbiType::CharPtr
        | AbiType::WCharPtr => {
            let w = std::mem::size_of::<usize>();
            (w, w)
        }
    }
}

fn type_annotation_name(vm: &Vm, ty: &Expr) -> Result<String> {
    crate::types::resolve_type_name_from_expr(vm, ty)
}

pub fn load_library(vm: &mut Vm, path: &str) -> Result<Value> {
    vm.caps.check_ffi("C.frompath")?;
    let lib = unsafe { Library::new(path) }.map_err(|e| {
        RuntimeError::msg(format!("failed to load dynamic library '{path}': {e}"))
    })?;
    Ok(Value::DllHandle(Arc::new(DllHandle {
        path: path.to_string(),
        lib: Arc::new(lib),
    })))
}

fn parse_call_conv(s: &str) -> Result<CallConv> {
    match s.to_ascii_lowercase().as_str() {
        "c" | "cdecl" | "default" => Ok(CallConv::C),
        "stdcall" | "winapi" => Ok(CallConv::Stdcall),
        other => Err(RuntimeError::type_err(format!(
            "extern: unknown calling convention '{other}' (use \"c\" or \"stdcall\")"
        ))),
    }
}

/// 内置 `extern(handle[, symbol[, abi]])` → 装饰器。
pub fn builtin_extern(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_ffi("extern")?;
    if args.is_empty() || args.len() > 3 {
        return Err(RuntimeError::type_err(
            "extern requires 1..=3 arguments (handle[, symbol[, abi]])",
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
    let mut symbol_override = None;
    let mut conv = CallConv::C;
    if args.len() >= 2 {
        match &args[1] {
            Value::Text(s) => {
                // 二义：可能是 symbol 或 abi。若只有 2 参且像 abi 名，当作 abi。
                if args.len() == 2 && matches!(s.as_str(), "c" | "cdecl" | "stdcall" | "winapi" | "default")
                {
                    conv = parse_call_conv(s)?;
                } else {
                    symbol_override = Some(s.clone());
                }
            }
            _ => {
                return Err(RuntimeError::type_err(
                    "extern: second argument must be text (symbol or abi)",
                ))
            }
        }
    }
    if args.len() == 3 {
        match &args[2] {
            Value::Text(s) => conv = parse_call_conv(s)?,
            _ => {
                return Err(RuntimeError::type_err(
                    "extern: third argument must be text abi name",
                ))
            }
        }
    }
    Ok(Value::Builtin(Arc::new(move |vm, deco_args| {
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
        bind_extern_function(vm, handle.clone(), symbol_override.clone(), conv, func)
    })))
}

fn bind_extern_function(
    vm: &mut Vm,
    handle: Arc<DllHandle>,
    symbol_override: Option<String>,
    conv: CallConv,
    func: Arc<FunctionObject>,
) -> Result<Value> {
    vm.caps.check_ffi("extern")?;
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
        arg_abis.push(AbiType::from_type_annotation(vm, ty)?);
    }
    let ret_abi = match &func.return_type {
        Some(ty) => AbiType::from_type_annotation(vm, ty)?,
        None => AbiType::Void,
    };

    let arg_ffi: Vec<FfiType> = arg_abis.iter().copied().map(AbiType::ffi_type).collect();
    let mut cif = Cif::new(arg_ffi, ret_abi.ffi_type());
    apply_call_conv(&mut cif, conv)?;
    let ffi = Arc::new(FfiCallable {
        cif,
        code: code_ptr,
        lock: ReentrantMutex::new(()),
    });

    let params = func.params.clone();
    let return_wrapper = func.return_wrapper.clone();
    let return_type = func.return_type.clone();
    let return_strong = func.return_strong;
    let func_name = func.name.clone();
    // 绑定后保留库句柄：调用性与用户侧句柄变量解耦，但库本身不得被卸载。
    let keep_lib = handle;

    let wrapper: BuiltinFn = Arc::new(move |vm, call_args| {
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
                if let Some(ty_expr) = &param.type_expr {
                    let ty_val = crate::types::eval_type_annotation(vm, ty_expr)?;
                    if !crate::types::value_accepts(vm, &arg, &ty_val) {
                        let type_name = type_annotation_name(vm, ty_expr)?;
                        arg = vm.convert_type(Value::type_ref(type_name), arg)?;
                    }
                }
            } else if let Some(ty_expr) = &param.type_expr {
                let ty_val = crate::types::eval_type_annotation(vm, ty_expr)?;
                // 非 implicit 的类型参数：须已匹配（不自动转换）
                if !crate::types::value_accepts(vm, &arg, &ty_val) {
                    let expected = type_annotation_name(vm, ty_expr)?;
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
                if let Some(ty_expr) = &param.type_expr {
                    let ty_val = crate::types::eval_type_annotation(vm, ty_expr)?;
                    if let Some(detail) = crate::types::type_check_error(vm, &arg, &ty_val) {
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

        // 卸荷完成重入：取回结果，不再调 C。
        if let Some(pending) = vm.take_ready_ffi_wait() {
            match pending {
                Ok((raw, errno)) => {
                    super::ffi_extra::set_last_errno(errno);
                    let mut out = abi_to_value(raw, ret_abi)?;
                    if let Some(ref wrapper_expr) = return_wrapper {
                        out = eval_wrapper_expr(vm, wrapper_expr, out)?;
                    }
                    if return_strong {
                        if let Some(ref ty_expr) = return_type {
                            let ty_val = crate::types::eval_type_annotation(vm, ty_expr)?;
                            if let Some(detail) = crate::types::type_check_error(vm, &out, &ty_val)
                            {
                                let msg = format!("return value: {detail}");
                                let exc = crate::exceptions::make_exception(vm, "TypeError", msg)?;
                                vm.throw_value(exc)?;
                                return Ok(Value::None);
                            }
                        }
                    }
                    return Ok(out);
                }
                Err(msg) => return Err(RuntimeError::msg(msg)),
            }
        }
        if vm.ffi_wait_still_pending() {
            vm.block_suspend = true;
            return Ok(Value::None);
        }

        let use_serial = vm.ffi_serial();
        let offload = vm.ffi_threads() > 0 && vm.can_offload_ffi();

        let raw = if offload {
            // 卸荷路径：不设 active_vm → 同步回调会失败（首版故意禁止）。
            let pending = crate::ffi_pool::submit_call(
                ffi.clone(),
                storage,
                ret_abi,
                use_serial,
                vm.ffi_threads(),
            )?;
            vm.set_ffi_wait(pending);
            vm.block_suspend = true;
            return Ok(Value::None);
        } else {
            super::ffi_extra::with_active_vm(vm, || {
                invoke_native_call(&ffi, &mut storage, ret_abi, use_serial)
            })?
        };

        let mut out = abi_to_value(raw, ret_abi)?;
        if let Some(ref wrapper_expr) = return_wrapper {
            out = eval_wrapper_expr(vm, wrapper_expr, out)?;
        }
        if return_strong {
            if let Some(ref ty_expr) = return_type {
                let ty_val = crate::types::eval_type_annotation(vm, ty_expr)?;
                if let Some(detail) = crate::types::type_check_error(vm, &out, &ty_val) {
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

fn apply_call_conv(cif: &mut Cif, conv: CallConv) -> Result<()> {
    use libffi::middle::ffi_abi_FFI_DEFAULT_ABI;
    match conv {
        CallConv::C => {
            cif.set_abi(ffi_abi_FFI_DEFAULT_ABI);
            Ok(())
        }
        CallConv::Stdcall => {
            // win64 上 stdcall 与默认 C 约定一致；32 位 Windows 尚未接 FFI_STDCALL。
            #[cfg(all(windows, target_arch = "x86"))]
            {
                let _ = cif;
                Err(crate::error::RuntimeError::msg(
                    "stdcall/winapi calling convention is not wired on 32-bit Windows; use 64-bit or C ABI",
                ))
            }
            #[cfg(not(all(windows, target_arch = "x86")))]
            {
                cif.set_abi(ffi_abi_FFI_DEFAULT_ABI);
                Ok(())
            }
        }
    }
}

/// 持锁执行 libffi call，并采样 errno（调用线程 TLS）。
pub(crate) fn invoke_native_call(
    ffi: &FfiCallable,
    storage: &mut [ArgStorage],
    ret_abi: AbiType,
    use_serial: bool,
) -> Result<RetStorage> {
    let ffi_args: Vec<Arg> = storage.iter_mut().map(|s| s.as_arg()).collect();
    let raw = if use_serial {
        let _guard = FFI_CALL_LOCK.lock();
        unsafe { call_cif(&ffi.cif, ffi.code, &ffi_args, ret_abi) }?
    } else {
        let _guard = ffi.lock.lock();
        unsafe { call_cif(&ffi.cif, ffi.code, &ffi_args, ret_abi) }?
    };
    super::ffi_extra::sample_error_codes();
    Ok(raw)
}

/// 卸荷线程用：返回 `(结果, errno)`，不依赖调用方 TLS。
pub(crate) fn invoke_native_call_sampled(
    ffi: &FfiCallable,
    storage: &mut [ArgStorage],
    ret_abi: AbiType,
    use_serial: bool,
) -> Result<(RetStorage, i32)> {
    let ffi_args: Vec<Arg> = storage.iter_mut().map(|s| s.as_arg()).collect();
    let raw = if use_serial {
        let _guard = FFI_CALL_LOCK.lock();
        unsafe { call_cif(&ffi.cif, ffi.code, &ffi_args, ret_abi) }?
    } else {
        let _guard = ffi.lock.lock();
        unsafe { call_cif(&ffi.cif, ffi.code, &ffi_args, ret_abi) }?
    };
    let errno = super::ffi_extra::sample_error_codes_value();
    Ok((raw, errno))
}

pub(crate) enum ArgStorage {
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
    /// 缓冲 + 稳定指针槽（`as_arg` 取 `ptr`）。
    OwnedCString { buf: Vec<u8>, ptr: usize },
    OwnedWString { buf: Vec<u16>, ptr: usize },
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
            Self::OwnedCString { buf, ptr } => {
                *ptr = buf.as_ptr() as usize;
                Arg::new(ptr)
            }
            Self::OwnedWString { buf, ptr } => {
                *ptr = buf.as_ptr() as usize;
                Arg::new(ptr)
            }
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
        AbiType::CharPtr => match v {
            Value::Text(s) => {
                let mut buf = s.as_bytes().to_vec();
                buf.push(0);
                let ptr = buf.as_ptr() as usize;
                Ok(ArgStorage::OwnedCString { buf, ptr })
            }
            Value::Ptr(p) => Ok(ArgStorage::Ptr(*p)),
            Value::None => Ok(ArgStorage::Ptr(0)),
            other => Err(RuntimeError::type_err(format!(
                "char* expects text or ptr, got {}",
                other.type_name()
            ))),
        },
        AbiType::WCharPtr => match v {
            Value::Text(s) => {
                let mut buf: Vec<u16> = s.encode_utf16().collect();
                buf.push(0);
                let ptr = buf.as_ptr() as usize;
                Ok(ArgStorage::OwnedWString { buf, ptr })
            }
            Value::Ptr(p) => Ok(ArgStorage::Ptr(*p)),
            Value::None => Ok(ArgStorage::Ptr(0)),
            other => Err(RuntimeError::type_err(format!(
                "wchar_t* expects text or ptr, got {}",
                other.type_name()
            ))),
        },
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

pub(crate) enum RetStorage {
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
        AbiType::Usize | AbiType::Pointer | AbiType::CharPtr | AbiType::WCharPtr => {
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
        (
            RetStorage::U64(v),
            AbiType::Pointer | AbiType::CharPtr | AbiType::WCharPtr,
        ) => Value::Ptr(v as usize),
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

fn export_builtin(
    exports: &mut HashMap<String, Value>,
    name: &str,
    f: fn(&mut Vm, &[Value]) -> Result<Value>,
) {
    exports.insert(name.into(), Value::Builtin(Arc::new(f)));
}

/// 构建 `std.language.C` 模块（含 `types` 子模块）。
pub fn build_c_language_module() -> Shared<ModuleObject> {
    let types = build_c_types_module();
    let mut children = HashMap::new();
    children.insert("types".into(), types.clone());
    let mut exports = HashMap::new();
    exports.insert(
        "frompath".into(),
        Value::Builtin(Arc::new(|vm, args| {
            if args.len() != 1 {
                return Err(RuntimeError::type_err("C.frompath requires 1 text path"));
            }
            let path = match &args[0] {
                Value::Text(s) => s.clone(),
                _ => return Err(RuntimeError::type_err("C.frompath requires text path")),
            };
            load_library(vm, &path)
        })),
    );
    exports.insert("types".into(), Value::Module(types));
    exports.insert("layout".into(), Value::type_ref("C.layout"));
    use super::ffi_extra as x;
    export_builtin(&mut exports, "alloc", x::builtin_alloc);
    export_builtin(&mut exports, "alloc_array", x::builtin_alloc_array);
    export_builtin(&mut exports, "free", x::builtin_free);
    export_builtin(&mut exports, "sizeof", x::builtin_sizeof);
    export_builtin(&mut exports, "write_bytes", x::builtin_write_bytes);
    export_builtin(&mut exports, "read_bytes", x::builtin_read_bytes);
    export_builtin(&mut exports, "write_i32", x::builtin_write_i32);
    export_builtin(&mut exports, "read_i32", x::builtin_read_i32);
    export_builtin(&mut exports, "write_i64", x::builtin_write_i64);
    export_builtin(&mut exports, "read_i64", x::builtin_read_i64);
    export_builtin(&mut exports, "write_ptr", x::builtin_write_ptr);
    export_builtin(&mut exports, "read_ptr", x::builtin_read_ptr);
    export_builtin(&mut exports, "cstring", x::builtin_cstring);
    export_builtin(&mut exports, "cstring_to_text", x::builtin_cstring_to_text);
    export_builtin(&mut exports, "wstring", x::builtin_wstring);
    export_builtin(&mut exports, "wstring_to_text", x::builtin_wstring_to_text);
    export_builtin(&mut exports, "Struct", x::builtin_struct);
    export_builtin(&mut exports, "load", x::builtin_load);
    export_builtin(&mut exports, "store", x::builtin_store);
    export_builtin(&mut exports, "ptr_live", x::builtin_ptr_live);
    export_builtin(&mut exports, "ptr_check", x::builtin_ptr_check);
    export_builtin(&mut exports, "unsafe_ptr", x::builtin_unsafe_ptr);
    export_builtin(&mut exports, "cast_ptr", x::builtin_cast_ptr);
    export_builtin(&mut exports, "errno", x::builtin_errno);
    export_builtin(&mut exports, "last_error", x::builtin_last_error);
    export_builtin(&mut exports, "callback", x::builtin_callback);
    export_builtin(&mut exports, "callback_free", x::builtin_callback_free);
    Shared::new(ModuleObject {
        name: "C".into(),
        full_name: "std.language.C".into(),
        exports,
        children,
        is_user: false,
    })
}

fn build_c_types_module() -> Shared<ModuleObject> {
    let mut exports = HashMap::new();
    for entry in crate::c_types::C_TYPES {
        let ty = Value::type_ref(entry.full_name());
        exports.insert(entry.c_name.to_string(), ty.clone());
        for alias in entry.export_aliases {
            exports.insert((*alias).to_string(), ty.clone());
        }
    }

    Shared::new(ModuleObject {
        name: "types".into(),
        full_name: "std.language.C.types".into(),
        exports,
        children: HashMap::new(),
        is_user: false,
    })
}

pub fn build_language_module() -> Shared<ModuleObject> {
    let c = build_c_language_module();
    let mut children = HashMap::new();
    children.insert("C".into(), c.clone());
    let mut exports = HashMap::new();
    exports.insert("C".into(), Value::Module(c));
    Shared::new(ModuleObject {
        name: "language".into(),
        full_name: "std.language".into(),
        exports,
        children,
        is_user: false,
    })
}
