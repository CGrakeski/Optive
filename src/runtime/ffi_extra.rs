//! FFI `扩展：内存读写、CString、结构体布局、errno/last_error、同步回调`。

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use libffi::low::{self as ffi_low, CallbackMut};
use libffi::middle::{Cif, Closure, Type as FfiType};
use parking_lot::Mutex;

use crate::ast::{Expr, ExprKind};
use crate::error::RuntimeError;
use crate::ffi::{abi_size_align, AbiType};
use crate::ptr_registry::{self, PtrEntry, PtrKind};
use crate::shared::Shared;
use crate::sized::SizedNum;
use crate::value::{builtin_repr, ModuleObject, Num, StructDef, StructInstance, Value};
use crate::vm::Vm;
use crate::Result;

thread_local! {
    static LAST_ERRNO: Cell<i32> = const { Cell::new(0) };
    static FFI_ACTIVE_VM: Cell<*mut Vm> = const { Cell::new(std::ptr::null_mut()) };
}

pub fn sample_error_codes() {
    let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    LAST_ERRNO.with(|c| c.set(code));
}

/// 采样并返回 errno（卸荷线程把值带回调用方）。
#[must_use]
pub fn sample_error_codes_value() -> i32 {
    sample_error_codes();
    LAST_ERRNO.with(std::cell::Cell::get)
}

pub fn set_last_errno(code: i32) {
    LAST_ERRNO.with(|c| c.set(code));
}

pub fn with_active_vm<R>(vm: &mut Vm, f: impl FnOnce() -> R) -> R {
    let ptr = std::ptr::from_mut::<Vm>(vm);
    let prev = FFI_ACTIVE_VM.with(|c| c.replace(ptr));
    let out = f();
    FFI_ACTIVE_VM.with(|c| c.set(prev));
    out
}

fn active_vm<'a>() -> Result<&'a mut Vm> {
    let ptr = FFI_ACTIVE_VM.with(std::cell::Cell::get);
    if ptr.is_null() {
        return Err(RuntimeError::msg(
            "native callback invoked outside an active FFI call (sync callbacks only)",
        ));
    }
    Ok(unsafe { &mut *ptr })
}

pub fn builtin_errno(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !args.is_empty() {
        return Err(RuntimeError::type_err(format!(
            "{} takes no arguments",
            builtin_repr("errno")
        )));
    }
    Ok(Value::Num(Num::from_i64(i64::from(
        LAST_ERRNO.with(std::cell::Cell::get),
    ))))
}

pub fn builtin_last_error(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    // 与 errno 相同采样点（Windows 上 raw_os_error 即 GetLastError）。
    builtin_errno(_vm, args)
}

pub fn builtin_alloc(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(format!(
            "{}(nbytes|TypeRef) requires 1 argument",
            builtin_repr("alloc")
        )));
    }
    match &args[0] {
        Value::TypeRef(name) => {
            if let Some(def) = vm.struct_defs.get(name) {
                let layout = def.native_layout.as_ref().ok_or_else(|| {
                    RuntimeError::type_err(format!(
                        "{}: struct '{name}' has no layout (annotate with `typed struct ... : <layout>`)",
                        builtin_repr("alloc")
                    ))
                })?;
                let addr =
                    ptr_registry::alloc_owned(layout.size, layout.align, Some(name.clone()))?;
                return Ok(Value::Ptr(addr));
            }
            let (sz, align) = abi_size_align(&AbiType::from_type_name(name)?);
            let addr = ptr_registry::alloc_owned(sz, align, Some(name.clone()))?;
            Ok(Value::Ptr(addr))
        }
        other => {
            let n = expect_usize("alloc", other)?;
            let addr = ptr_registry::alloc_owned(n, 8, None)?;
            Ok(Value::Ptr(addr))
        }
    }
}

pub fn builtin_free(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(RuntimeError::type_err(format!(
            "{}(ptr[, size]) requires pointer (size optional if registered)",
            builtin_repr("free")
        )));
    }
    let p = expect_ptr("free", &args[0])?;
    if p == 0 {
        return Ok(Value::None);
    }
    // 若已登记为 Owned，用登记表释放（忽略多余 size，但若传入则校验一致）。
    if let Some(e) = ptr_registry::lookup(p) {
        if e.kind == PtrKind::Owned {
            if args.len() == 2 {
                let size = expect_usize("free", &args[1])?;
                if size != 0 && size != e.nbytes {
                    return Err(RuntimeError::value_err(format!(
                        "{}: size {size} != registered {}",
                        builtin_repr("free"),
                        e.nbytes
                    )));
                }
            }
            ptr_registry::free_owned(p)?;
            return Ok(Value::None);
        }
    }
    // 兼容路径已移除：未登记指针的 size 释放无法得知对齐，易 UB。
    Err(RuntimeError::value_err(format!(
        "{}: pointer not registered; only free Owned pointers from {} / Struct.alloc",
        builtin_repr("free"),
        builtin_repr("alloc")
    )))
}

pub fn builtin_sizeof(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(format!(
            "{} requires 1 type (TypeRef or {} layout)",
            builtin_repr("sizeof"),
            builtin_repr("Struct")
        )));
    }
    let size = match &args[0] {
        Value::TypeRef(n) => {
            if let Some(def) = vm.struct_defs.get(n) {
                if let Some(layout) = &def.native_layout {
                    layout.size
                } else {
                    abi_size_align(&AbiType::from_type_name(n)?).0
                }
            } else {
                abi_size_align(&AbiType::from_type_name(n)?).0
            }
        }
        Value::Module(m) => match m.borrow().exports.get("__c_struct_size__") {
            Some(Value::Num(n)) => n
                .to_i64()
                .ok_or_else(|| RuntimeError::type_err("invalid struct size"))?
                as usize,
            _ => {
                return Err(RuntimeError::type_err(format!(
                    "{}: expected TypeRef or {} module",
                    builtin_repr("sizeof"),
                    builtin_repr("Struct")
                )))
            }
        },
        _ => {
            return Err(RuntimeError::type_err(format!(
                "{}: expected TypeRef or {} module",
                builtin_repr("sizeof"),
                builtin_repr("Struct")
            )))
        }
    };
    Ok(Value::Num(Num::from_i64(size as i64)))
}

pub fn builtin_alloc_array(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(RuntimeError::type_err(format!(
            "{}(T, n) requires type and count",
            builtin_repr("alloc_array")
        )));
    }
    let (elem_name, stride, align) = resolve_elem_type(vm, &args[0])?;
    let n = expect_usize("alloc_array", &args[1])?;
    let nbytes = stride.checked_mul(n).ok_or_else(|| {
        RuntimeError::value_err(format!("{}: size overflow", builtin_repr("alloc_array")))
    })?;
    let addr = ptr_registry::alloc_owned(nbytes, align, Some(elem_name))?;
    Ok(Value::Ptr(addr))
}

pub fn builtin_ptr_live(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(format!(
            "{}(p) requires 1 argument",
            builtin_repr("ptr_live")
        )));
    }
    let p = expect_ptr("ptr_live", &args[0])?;
    // Owned 且未 free；外来 unsafe_ptr 为 false（见 docs/ffi.md）。
    Ok(Value::Bool(ptr_registry::is_live(p)))
}

pub fn builtin_ptr_check(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(RuntimeError::type_err(format!(
            "{}(p, T) requires pointer and type",
            builtin_repr("ptr_check")
        )));
    }
    let p = expect_ptr("ptr_check", &args[0])?;
    let (want, _, _) = resolve_elem_type(vm, &args[1])?;
    let Some(e) = ptr_registry::lookup(p) else {
        return Ok(Value::Bool(false));
    };
    Ok(Value::Bool(e.elem.as_deref() == Some(want.as_str())))
}

pub fn builtin_unsafe_ptr(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(format!(
            "{}(p) requires 1 pointer argument",
            builtin_repr("unsafe_ptr")
        )));
    }
    let p = expect_ptr("unsafe_ptr", &args[0])?;
    if p == 0 {
        return Err(RuntimeError::value_err(format!(
            "{}: null pointer",
            builtin_repr("unsafe_ptr")
        )));
    }
    // 已登记（含 Owned）则保持原条目，避免把可 free 指针降级为 foreign。
    if ptr_registry::lookup(p).is_none() {
        ptr_registry::register(PtrEntry {
            addr: p,
            nbytes: usize::MAX / 4,
            align: 1,
            elem: None,
            kind: PtrKind::ForeignUnsafe,
        });
    }
    Ok(Value::Ptr(p))
}

pub fn builtin_cast_ptr(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(RuntimeError::type_err(format!(
            "{}(p, T) requires pointer and element type",
            builtin_repr("cast_ptr")
        )));
    }
    let p = expect_ptr("cast_ptr", &args[0])?;
    let (elem, _, _) = resolve_elem_type(vm, &args[1])?;
    ptr_registry::set_elem(p, Some(elem))?;
    Ok(Value::Ptr(p))
}

fn resolve_elem_type(vm: &Vm, v: &Value) -> Result<(String, usize, usize)> {
    match v {
        Value::TypeRef(name) | Value::Text(name) => {
            if let Some(def) = vm.struct_defs.get(name) {
                if let Some(layout) = &def.native_layout {
                    return Ok((name.clone(), layout.size, layout.align.max(1)));
                }
            }
            let abi = AbiType::from_type_name(name)?;
            let (sz, al) = abi_size_align(&abi);
            Ok((name.clone(), sz, al.max(1)))
        }
        Value::TypeSpec(spec) if ptr_registry::is_ptr_type_name(&spec.name) => {
            Err(RuntimeError::type_err(format!(
                "{} element must be pointee type, not ptr[T]",
                builtin_repr("alloc_array")
            )))
        }
        other => Err(RuntimeError::type_err(format!(
            "expected TypeRef, got {}",
            other.type_name()
        ))),
    }
}

pub fn builtin_write_bytes(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(RuntimeError::type_err(format!(
            "{}(ptr, offset, bytes) requires 3 arguments",
            builtin_repr("write_bytes")
        )));
    }
    let base = expect_ptr("write_bytes", &args[0])?;
    let off = expect_usize("write_bytes", &args[1])?;
    let bytes = match &args[2] {
        Value::Text(s) => s.as_bytes().to_vec(),
        Value::List(l) => {
            let mut out = Vec::new();
            for v in l.borrow().iter() {
                let b = expect_usize("write_bytes", v)?;
                if b > 255 {
                    return Err(RuntimeError::value_err("byte must be 0..=255"));
                }
                out.push(b as u8);
            }
            out
        }
        other => {
            return Err(RuntimeError::type_err(format!(
                "{}: expected text or list of bytes, got {}",
                builtin_repr("write_bytes"),
                other.type_name()
            )))
        }
    };
    if base == 0 {
        return Err(RuntimeError::value_err(format!(
            "{}: null pointer",
            builtin_repr("write_bytes")
        )));
    }
    ptr_registry::check_access(base, off, bytes.len())?;
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (base + off) as *mut u8, bytes.len());
    }
    Ok(Value::Num(Num::from_i64(bytes.len() as i64)))
}

pub fn builtin_read_bytes(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(RuntimeError::type_err(format!(
            "{}(ptr, offset, len) requires 3 arguments",
            builtin_repr("read_bytes")
        )));
    }
    let base = expect_ptr("read_bytes", &args[0])?;
    let off = expect_usize("read_bytes", &args[1])?;
    let len = expect_usize("read_bytes", &args[2])?;
    if base == 0 {
        return Err(RuntimeError::value_err(format!(
            "{}: null pointer",
            builtin_repr("read_bytes")
        )));
    }
    ptr_registry::check_access(base, off, len)?;
    let mut buf = vec![0u8; len];
    unsafe {
        std::ptr::copy_nonoverlapping((base + off) as *const u8, buf.as_mut_ptr(), len);
    }
    Ok(Value::List(Shared::new(
        buf.into_iter()
            .map(|b| Value::Num(Num::from_i64(i64::from(b))))
            .collect(),
    )))
}

pub fn builtin_write_i32(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    write_int::<i32>(
        args,
        "write_i32",
        |v| Ok(expect_i64_label("i32", v)? as i32),
    )
}
pub fn builtin_read_i32(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    read_int::<i32>(args, "read_i32", |v| Value::Sized(SizedNum::I32(v)))
}
pub fn builtin_write_i64(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    write_int::<i64>(args, "write_i64", expect_i64_ctx)
}
pub fn builtin_read_i64(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    read_int::<i64>(args, "read_i64", |v| Value::Sized(SizedNum::I64(v)))
}
pub fn builtin_write_ptr(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    write_int::<usize>(args, "write_ptr", |v| expect_ptr_or_usize_label("ptr", v))
}
pub fn builtin_read_ptr(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    read_int::<usize>(args, "read_ptr", Value::Ptr)
}

fn expect_i64_ctx(v: &Value) -> Result<i64> {
    expect_i64_label("i64", v)
}

fn write_int<T: Copy>(
    args: &[Value],
    name: &str,
    conv: impl FnOnce(&Value) -> Result<T>,
) -> Result<Value> {
    let ctx = builtin_repr(name);
    if args.len() != 3 {
        return Err(RuntimeError::type_err(format!(
            "{ctx}(ptr, offset, value) requires 3 arguments"
        )));
    }
    let base = expect_ptr(name, &args[0])?;
    let off = expect_usize(name, &args[1])?;
    let v = conv(&args[2])?;
    if base == 0 {
        return Err(RuntimeError::value_err(format!("{ctx}: null pointer")));
    }
    ptr_registry::check_access(base, off, std::mem::size_of::<T>())?;
    unsafe {
        std::ptr::write_unaligned((base + off) as *mut T, v);
    }
    Ok(Value::None)
}

fn read_int<T: Copy>(args: &[Value], name: &str, to_val: impl FnOnce(T) -> Value) -> Result<Value> {
    let ctx = builtin_repr(name);
    if args.len() != 2 {
        return Err(RuntimeError::type_err(format!(
            "{ctx}(ptr, offset) requires 2 arguments"
        )));
    }
    let base = expect_ptr(name, &args[0])?;
    let off = expect_usize(name, &args[1])?;
    if base == 0 {
        return Err(RuntimeError::value_err(format!("{ctx}: null pointer")));
    }
    ptr_registry::check_access(base, off, std::mem::size_of::<T>())?;
    let v = unsafe { std::ptr::read_unaligned((base + off) as *const T) };
    Ok(to_val(v))
}

pub fn builtin_cstring(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(format!(
            "{}(text) -> [ptr, size]; free with {}(ptr, size)",
            builtin_repr("cstring"),
            builtin_repr("free")
        )));
    }
    let Value::Text(s) = &args[0] else {
        return Err(RuntimeError::type_err(format!(
            "{} requires text",
            builtin_repr("cstring")
        )));
    };
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    let n = bytes.len();
    let addr = ptr_registry::alloc_owned(n, 1, Some("u8".into()))?;
    if addr != 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, n);
        }
    }
    Ok(Value::List(Shared::new(vec![
        Value::Ptr(addr),
        Value::Num(Num::from_i64(n as i64)),
    ])))
}

pub fn builtin_cstring_to_text(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(format!(
            "{}(ptr) requires 1 pointer",
            builtin_repr("cstring_to_text")
        )));
    }
    let p = expect_ptr("cstring_to_text", &args[0])?;
    if p == 0 {
        return Ok(Value::Text(String::new()));
    }
    if !ptr_registry::is_registered(p) {
        return Err(RuntimeError::value_err(format!(
            "{}: pointer not registered ({} to allow)",
            builtin_repr("cstring_to_text"),
            builtin_repr("unsafe_ptr")
        )));
    }
    // 拷贝到 Optive 托管 `text`；调用方仍须按所有权释放 C 侧缓冲。
    let s = unsafe { std::ffi::CStr::from_ptr(p as *const std::ffi::c_char) };
    let text = s
        .to_str()
        .map_err(|e| {
            RuntimeError::value_err(format!(
                "{}: invalid UTF-8: {e}",
                builtin_repr("cstring_to_text")
            ))
        })?
        .to_string();
    Ok(Value::Text(text))
}

pub fn builtin_wstring(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(format!(
            "{}(text) -> [ptr, nbytes]; free with {}",
            builtin_repr("wstring"),
            builtin_repr("free")
        )));
    }
    let Value::Text(s) = &args[0] else {
        return Err(RuntimeError::type_err(format!(
            "{} requires text",
            builtin_repr("wstring")
        )));
    };
    let mut units: Vec<u16> = s.encode_utf16().collect();
    units.push(0);
    let nbytes = units.len() * 2;
    let addr = ptr_registry::alloc_owned(nbytes, 2, Some("u16".into()))?;
    if addr != 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(units.as_ptr(), addr as *mut u16, units.len());
        }
    }
    Ok(Value::List(Shared::new(vec![
        Value::Ptr(addr),
        Value::Num(Num::from_i64(nbytes as i64)),
    ])))
}

pub fn builtin_wstring_to_text(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(format!(
            "{}(ptr) requires 1 pointer",
            builtin_repr("wstring_to_text")
        )));
    }
    let p = expect_ptr("wstring_to_text", &args[0])?;
    if p == 0 {
        return Ok(Value::Text(String::new()));
    }
    if !ptr_registry::is_registered(p) {
        return Err(RuntimeError::value_err(format!(
            "{}: pointer not registered ({} to allow)",
            builtin_repr("wstring_to_text"),
            builtin_repr("unsafe_ptr")
        )));
    }
    // 拷贝到 Optive 托管 `text`。
    let mut units = Vec::new();
    let mut i = 0usize;
    loop {
        let u = unsafe { std::ptr::read_unaligned((p as *const u16).add(i)) };
        if u == 0 {
            break;
        }
        units.push(u);
        i += 1;
        if i > 10_000_000 {
            return Err(RuntimeError::value_err(format!(
                "{}: string too long",
                builtin_repr("wstring_to_text")
            )));
        }
    }
    Ok(Value::Text(String::from_utf16_lossy(&units)))
}

pub fn builtin_struct(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(format!(
            "{}(fields) requires 1 list of [name, type] pairs",
            builtin_repr("Struct")
        )));
    }
    let Value::List(fields_v) = &args[0] else {
        return Err(RuntimeError::type_err(format!(
            "{}: fields must be a list",
            builtin_repr("Struct")
        )));
    };
    let mut fields = Vec::new();
    let mut offset = 0usize;
    let mut max_align = 1usize;
    for item in fields_v.borrow().iter() {
        let Value::List(pair) = item else {
            return Err(RuntimeError::type_err(format!(
                "{} field must be [name, type]",
                builtin_repr("Struct")
            )));
        };
        let pair = pair.borrow();
        if pair.len() != 2 {
            return Err(RuntimeError::type_err(format!(
                "{} field must be [name, type]",
                builtin_repr("Struct")
            )));
        }
        let name = match &pair[0] {
            Value::Text(s) => s.clone(),
            _ => {
                return Err(RuntimeError::type_err(format!(
                    "{} field name must be text",
                    builtin_repr("Struct")
                )))
            }
        };
        let abi = match &pair[1] {
            Value::TypeRef(n) => AbiType::from_type_name(n)?,
            other => {
                return Err(RuntimeError::type_err(format!(
                    "{} field type must be TypeRef, got {}",
                    builtin_repr("Struct"),
                    other.type_name()
                )))
            }
        };
        let (size, align) = abi_size_align(&abi);
        offset = align_up(offset, align);
        max_align = max_align.max(align);
        fields.push(FieldLayout {
            name,
            abi,
            offset,
            size,
        });
        offset += size;
    }
    let total = align_up(offset, max_align.max(1));
    let layout = Arc::new(StructLayout {
        fields,
        size: total,
        align: max_align,
    });

    let mut exports = HashMap::new();
    exports.insert(
        "__c_struct_size__".into(),
        Value::Num(Num::from_i64(total as i64)),
    );
    exports.insert("size".into(), Value::Num(Num::from_i64(total as i64)));

    let layout_a = layout.clone();
    exports.insert(
        "alloc".into(),
        Value::builtin("alloc", move |_vm, a| {
            if !a.is_empty() {
                return Err(RuntimeError::type_err("Struct.alloc takes no arguments"));
            }
            let n = layout_a.size;
            let addr = ptr_registry::alloc_owned(n, layout_a.align.max(1), None)?;
            Ok(Value::List(Shared::new(vec![
                Value::Ptr(addr),
                Value::Num(Num::from_i64(n as i64)),
            ])))
        }),
    );
    let layout_r = layout.clone();
    exports.insert(
        "read".into(),
        Value::builtin("read", move |_vm, a| {
            if a.len() != 2 {
                return Err(RuntimeError::type_err(
                    "Struct.read(ptr, field_name) requires 2 arguments",
                ));
            }
            let base = expect_ptr("Struct.read", &a[0])?;
            let Value::Text(name) = &a[1] else {
                return Err(RuntimeError::type_err("field name must be text"));
            };
            let f = layout_r
                .fields
                .iter()
                .find(|f| f.name == *name)
                .ok_or_else(|| RuntimeError::attr_err(format!("no field '{name}'")))?;
            ptr_registry::check_access(base, f.offset, f.size)?;
            read_field(base, f)
        }),
    );
    let layout_w = layout.clone();
    exports.insert(
        "write".into(),
        Value::builtin("write", move |_vm, a| {
            if a.len() != 3 {
                return Err(RuntimeError::type_err(
                    "Struct.write(ptr, field_name, value) requires 3 arguments",
                ));
            }
            let base = expect_ptr("Struct.write", &a[0])?;
            let Value::Text(name) = &a[1] else {
                return Err(RuntimeError::type_err("field name must be text"));
            };
            let f = layout_w
                .fields
                .iter()
                .find(|f| f.name == *name)
                .ok_or_else(|| RuntimeError::attr_err(format!("no field '{name}'")))?;
            ptr_registry::check_access(base, f.offset, f.size)?;
            write_field(base, f, &a[2])?;
            Ok(Value::None)
        }),
    );
    let layout_o = layout;
    exports.insert(
        "offsetof".into(),
        Value::builtin("offsetof", move |_vm, a| {
            if a.len() != 1 {
                return Err(RuntimeError::type_err(
                    "Struct.offsetof(field_name) requires 1 argument",
                ));
            }
            let Value::Text(name) = &a[0] else {
                return Err(RuntimeError::type_err("field name must be text"));
            };
            let f = layout_o
                .fields
                .iter()
                .find(|f| f.name == *name)
                .ok_or_else(|| RuntimeError::attr_err(format!("no field '{name}'")))?;
            Ok(Value::Num(Num::from_i64(f.offset as i64)))
        }),
    );
    Ok(Value::Module(Shared::new(ModuleObject {
        name: "CStruct".into(),
        exports,
        children: HashMap::new(),
        is_user: false,
    })))
}

#[derive(Debug, Clone)]
pub struct NativeFieldLayout {
    pub name: String,
    pub abi: AbiType,
    pub offset: usize,
    pub size: usize,
}

/// 结构体在本地/FFI 侧的具体字节布局（由一等 `Layout` 策略算出）。
#[derive(Debug, Clone)]
pub struct NativeStructLayout {
    pub fields: Vec<NativeFieldLayout>,
    pub size: usize,
    pub align: usize,
}

/// 布局打包策略（可扩展；本期内建 C ABI）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutStrategy {
    /// 按字段声明顺序，C 对齐规则（`align_up` + 尾部对齐）。
    CAbi,
}

/// 一等布局对象：`typed struct S {...} : <layout>` 中的 `<layout>`。
/// 身份是对象本身（Arc），不是点分路径字符串。
#[derive(Debug, Clone)]
pub struct LayoutObject {
    pub strategy: LayoutStrategy,
}

fn c_abi_layout() -> Arc<LayoutObject> {
    static LAYOUT: std::sync::OnceLock<Arc<LayoutObject>> = std::sync::OnceLock::new();
    LAYOUT
        .get_or_init(|| {
            Arc::new(LayoutObject {
                strategy: LayoutStrategy::CAbi,
            })
        })
        .clone()
}

/// 内建 C 模块导出的 `layout` 一等值（经 getattr 取得，不经路径字面量）。
#[must_use]
pub fn c_layout_value() -> Value {
    Value::Layout(c_abi_layout())
}

fn member_attr_parts(ty: &Expr) -> Option<Vec<String>> {
    match &ty.kind {
        ExprKind::Var(n) => Some(vec![n.clone()]),
        ExprKind::Member { object, field } => {
            let mut parts = member_attr_parts(object)?;
            parts.push(field.clone());
            Some(parts)
        }
        _ => None,
    }
}

/// 按属性链解析布局注解（等同编译期 getattr）：`C.layout` / `….C.layout`。
pub fn resolve_layout_from_expr(ty: &Expr) -> Option<Arc<LayoutObject>> {
    let parts = member_attr_parts(ty)?;
    match parts.as_slice() {
        [.., mod_name, attr] if mod_name == "C" && attr == "layout" => Some(c_abi_layout()),
        _ => None,
    }
}

/// 用布局策略为 struct 定义计算本地布局。
pub fn apply_layout(layout: &LayoutObject, def: &StructDef) -> Result<NativeStructLayout> {
    match layout.strategy {
        LayoutStrategy::CAbi => layout_from_struct_def(def, "<Layout>"),
    }
}

/// 旧名别名（内部）。
type FieldLayout = NativeFieldLayout;
type StructLayout = NativeStructLayout;
/// 兼容旧公开名。
pub type CFieldLayout = NativeFieldLayout;
pub type CStructLayout = NativeStructLayout;

#[must_use]
pub fn align_up(off: usize, align: usize) -> usize {
    let a = align.max(1);
    (off + a - 1) & !(a - 1)
}

/// 由字段 ABI 列表计算 C ABI 布局（与 Struct / `LayoutStrategy::CAbi` 共用）。
#[must_use]
pub fn layout_from_abi_fields(fields_in: &[(String, AbiType)]) -> NativeStructLayout {
    let mut fields = Vec::new();
    let mut offset = 0usize;
    let mut max_align = 1usize;
    for (name, abi) in fields_in {
        let (size, align) = abi_size_align(abi);
        offset = align_up(offset, align);
        max_align = max_align.max(align);
        fields.push(NativeFieldLayout {
            name: name.clone(),
            abi: abi.clone(),
            offset,
            size,
        });
        offset += size;
    }
    let total = align_up(offset, max_align.max(1));
    NativeStructLayout {
        fields,
        size: total,
        align: max_align.max(1),
    }
}

pub fn layout_from_struct_def(def: &StructDef, layout_id: &str) -> Result<NativeStructLayout> {
    let mut pairs = Vec::new();
    for (i, fname) in def.fields.iter().enumerate() {
        let info = def.field_types.get(i).ok_or_else(|| {
            RuntimeError::type_err(format!("{layout_id}: missing type for field '{fname}'"))
        })?;
        let ty = info.type_expr.as_ref().ok_or_else(|| {
            RuntimeError::type_err(format!(
                "{layout_id}: field '{fname}' needs a type annotation"
            ))
        })?;
        let abi = abi_from_field_type(ty, layout_id)?;
        pairs.push((fname.clone(), abi));
    }
    Ok(layout_from_abi_fields(&pairs))
}

fn abi_from_field_type(ty: &Expr, layout_id: &str) -> Result<AbiType> {
    if let Some(val) = crate::types::static_type_value_from_expr(ty) {
        return abi_from_type_value(&val, layout_id);
    }
    match &ty.kind {
        ExprKind::Var(n) => AbiType::from_type_name(n),
        ExprKind::Member { .. } => Err(RuntimeError::type_err(format!(
            "{layout_id}: cannot resolve field type member path (use getattr-reachable type, e.g. C.types.int)"
        ))),
        other => Err(RuntimeError::type_err(format!(
            "{layout_id}: unsupported field type {other:?}"
        ))),
    }
}

fn abi_from_type_value(val: &Value, layout_id: &str) -> Result<AbiType> {
    if let Some(name) = crate::types::type_value_base(val) {
        if ptr_registry::is_ptr_type_name(name) {
            return Ok(AbiType::Pointer);
        }
        return AbiType::from_type_name(name);
    }
    if let Value::TypeSpec(spec) = val {
        if ptr_registry::is_ptr_type_name(&spec.name) {
            return Ok(AbiType::Pointer);
        }
        return AbiType::from_type_name(&spec.name);
    }
    Err(RuntimeError::type_err(format!(
        "{layout_id}: unsupported field type {}",
        crate::types::type_value_display(val)
    )))
}

pub fn builtin_load(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(RuntimeError::type_err(format!(
            "{}(Type, ptr) requires type and pointer",
            builtin_repr("load")
        )));
    }
    let Value::TypeRef(name) = &args[0] else {
        return Err(RuntimeError::type_err(format!(
            "{}: first argument must be TypeRef",
            builtin_repr("load")
        )));
    };
    let base = expect_ptr("load", &args[1])?;
    let def = vm.struct_defs.get(name).ok_or_else(|| {
        RuntimeError::type_err(format!("{}: unknown type '{name}'", builtin_repr("load")))
    })?;
    let layout = def.native_layout.as_ref().ok_or_else(|| {
        RuntimeError::type_err(format!(
            "{}: '{name}' has no native layout",
            builtin_repr("load")
        ))
    })?;
    ptr_registry::check_access(base, 0, layout.size)?;
    let mut slots = Vec::with_capacity(layout.fields.len());
    for f in &layout.fields {
        slots.push(read_field(base, f)?);
    }
    // 按 StructDef.fields 顺序对齐（布局字段名应一致）
    let mut ordered = Vec::with_capacity(def.fields.len());
    for fname in &def.fields {
        let idx = layout
            .fields
            .iter()
            .position(|f| f.name == *fname)
            .ok_or_else(|| {
                RuntimeError::attr_err(format!("{}: missing field '{fname}'", builtin_repr("load")))
            })?;
        ordered.push(slots[idx].clone());
    }
    Ok(Value::Struct(Arc::new(StructInstance {
        def,
        slots: crate::shared::SyncCell::new(ordered),
        generic_args: Vec::new(),
    })))
}

/// `p[i]` / `p[i]=`：依赖 registry 中的 elem 类型。
pub fn ptr_index_get(vm: &mut Vm, addr: usize, index: i64) -> Result<Value> {
    let (entry, elem) = ptr_registry::require_elem(addr)?;
    if index < 0 {
        return Err(RuntimeError::index_err("pointer index must be >= 0"));
    }
    let (stride, _) = elem_stride(vm, &elem)?;
    let off = (index as usize)
        .checked_mul(stride)
        .ok_or_else(|| RuntimeError::value_err("pointer index overflow"))?;
    ptr_registry::check_access(addr, off, stride)?;
    let _ = entry;
    if let Some(def) = vm.struct_defs.get(&elem) {
        if def.native_layout.is_some() {
            return builtin_load(vm, &[Value::type_ref(elem), Value::Ptr(addr + off)]);
        }
    }
    read_scalar_at(addr + off, &elem)
}

pub fn ptr_index_set(vm: &mut Vm, addr: usize, index: i64, val: Value) -> Result<()> {
    let (_entry, elem) = ptr_registry::require_elem(addr)?;
    if index < 0 {
        return Err(RuntimeError::index_err("pointer index must be >= 0"));
    }
    let (stride, _) = elem_stride(vm, &elem)?;
    let off = (index as usize)
        .checked_mul(stride)
        .ok_or_else(|| RuntimeError::value_err("pointer index overflow"))?;
    ptr_registry::check_access(addr, off, stride)?;
    if let Some(def) = vm.struct_defs.get(&elem) {
        if def.native_layout.is_some() {
            builtin_store(vm, &[Value::Ptr(addr + off), val])?;
            return Ok(());
        }
    }
    write_scalar_at(addr + off, &elem, &val)
}

fn elem_stride(vm: &Vm, elem: &str) -> Result<(usize, usize)> {
    if let Some(def) = vm.struct_defs.get(elem) {
        if let Some(layout) = &def.native_layout {
            return Ok((layout.size, layout.align));
        }
    }
    let (sz, al) = abi_size_align(&AbiType::from_type_name(elem)?);
    Ok((sz, al))
}

fn read_scalar_at(addr: usize, type_name: &str) -> Result<Value> {
    let abi = AbiType::from_type_name(type_name)?;
    let size = abi_size_align(&abi).0;
    read_field(
        addr,
        &CFieldLayout {
            name: String::new(),
            abi,
            offset: 0,
            size,
        },
    )
}

fn write_scalar_at(addr: usize, type_name: &str, val: &Value) -> Result<()> {
    let abi = AbiType::from_type_name(type_name)?;
    let size = abi_size_align(&abi).0;
    write_field(
        addr,
        &CFieldLayout {
            name: String::new(),
            abi,
            offset: 0,
            size,
        },
        val,
    )
}

pub fn builtin_store(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(RuntimeError::type_err(format!(
            "{}(ptr, struct_instance) requires 2 arguments",
            builtin_repr("store")
        )));
    }
    let base = expect_ptr("store", &args[0])?;
    let Value::Struct(inst) = &args[1] else {
        return Err(RuntimeError::type_err(format!(
            "{}: second argument must be a struct instance",
            builtin_repr("store")
        )));
    };
    let layout = inst.def.native_layout.as_ref().ok_or_else(|| {
        RuntimeError::type_err(format!(
            "{}: '{}' has no native layout",
            builtin_repr("store"),
            inst.def.name
        ))
    })?;
    ptr_registry::check_access(base, 0, layout.size)?;
    let slots = inst.slots.borrow();
    for f in &layout.fields {
        let idx = inst
            .def
            .fields
            .iter()
            .position(|n| n == &f.name)
            .ok_or_else(|| {
                RuntimeError::attr_err(format!("{}: no field '{}'", builtin_repr("store"), f.name))
            })?;
        write_field(base, f, &slots[idx])?;
    }
    let _ = vm;
    Ok(Value::None)
}

fn read_field(base: usize, f: &FieldLayout) -> Result<Value> {
    let addr = base + f.offset;
    Ok(match &f.abi {
        AbiType::I32 => Value::Sized(SizedNum::I32(unsafe {
            std::ptr::read_unaligned(addr as *const i32)
        })),
        AbiType::U32 => Value::Sized(SizedNum::U32(unsafe {
            std::ptr::read_unaligned(addr as *const u32)
        })),
        AbiType::I64 => Value::Sized(SizedNum::I64(unsafe {
            std::ptr::read_unaligned(addr as *const i64)
        })),
        AbiType::U64 => Value::Sized(SizedNum::U64(unsafe {
            std::ptr::read_unaligned(addr as *const u64)
        })),
        AbiType::I16 => Value::Sized(SizedNum::I16(unsafe {
            std::ptr::read_unaligned(addr as *const i16)
        })),
        AbiType::U16 => Value::Sized(SizedNum::U16(unsafe {
            std::ptr::read_unaligned(addr as *const u16)
        })),
        AbiType::I8 | AbiType::Bool => Value::Sized(SizedNum::I8(unsafe {
            std::ptr::read_unaligned(addr as *const i8)
        })),
        AbiType::U8 => Value::Sized(SizedNum::U8(unsafe {
            std::ptr::read_unaligned(addr as *const u8)
        })),
        AbiType::F32 => Value::Sized(SizedNum::F32(unsafe {
            std::ptr::read_unaligned(addr as *const f32)
        })),
        AbiType::F64 => Value::Sized(SizedNum::F64(unsafe {
            std::ptr::read_unaligned(addr as *const f64)
        })),
        AbiType::Pointer | AbiType::CharPtr | AbiType::WCharPtr | AbiType::Usize => {
            Value::Ptr(unsafe { std::ptr::read_unaligned(addr as *const usize) })
        }
        AbiType::Isize => Value::Sized(SizedNum::Isize(unsafe {
            std::ptr::read_unaligned(addr as *const isize)
        })),
        AbiType::Void => Value::None,
        AbiType::CStruct { name, .. } => {
            return Err(RuntimeError::type_err(format!(
                "nested C struct field `{name}` is not supported"
            )));
        }
    })
}

fn write_field(base: usize, f: &FieldLayout, v: &Value) -> Result<()> {
    let addr = base + f.offset;
    match &f.abi {
        AbiType::I32 => unsafe {
            std::ptr::write_unaligned(addr as *mut i32, expect_i64("i32", v)? as i32);
        },
        AbiType::U32 => unsafe {
            std::ptr::write_unaligned(addr as *mut u32, expect_i64("u32", v)? as u32);
        },
        AbiType::I64 => unsafe {
            std::ptr::write_unaligned(addr as *mut i64, expect_i64("i64", v)?);
        },
        AbiType::U64 | AbiType::Usize => unsafe {
            std::ptr::write_unaligned(addr as *mut u64, expect_usize("u64", v)? as u64);
        },
        AbiType::I16 => unsafe {
            std::ptr::write_unaligned(addr as *mut i16, expect_i64("i16", v)? as i16);
        },
        AbiType::U16 => unsafe {
            std::ptr::write_unaligned(addr as *mut u16, expect_i64("u16", v)? as u16);
        },
        AbiType::I8 | AbiType::Bool => unsafe {
            std::ptr::write_unaligned(addr as *mut i8, expect_i64("i8", v)? as i8);
        },
        AbiType::U8 => unsafe {
            std::ptr::write_unaligned(addr as *mut u8, expect_i64("u8", v)? as u8);
        },
        AbiType::F64 => unsafe {
            let x = match v {
                Value::Sized(SizedNum::F64(f)) => *f,
                Value::Num(n) => n.to_f64_checked()?,
                _ => expect_i64("f64", v)? as f64,
            };
            std::ptr::write_unaligned(addr as *mut f64, x);
        },
        AbiType::F32 => unsafe {
            let x = match v {
                Value::Sized(SizedNum::F32(f)) => *f,
                Value::Num(n) => n.to_f64_checked()? as f32,
                _ => expect_i64("f32", v)? as f32,
            };
            std::ptr::write_unaligned(addr as *mut f32, x);
        },
        AbiType::Pointer | AbiType::CharPtr | AbiType::WCharPtr => unsafe {
            std::ptr::write_unaligned(addr as *mut usize, expect_ptr_or_usize("ptr", v)?);
        },
        AbiType::Isize => unsafe {
            std::ptr::write_unaligned(addr as *mut isize, expect_i64("isize", v)? as isize);
        },
        AbiType::Void => {}
        AbiType::CStruct { name, .. } => {
            return Err(RuntimeError::type_err(format!(
                "nested C struct field `{name}` is not supported"
            )));
        }
    }
    Ok(())
}

pub(crate) fn pack_c_struct(inst: &crate::value::StructInstance) -> Result<Vec<u8>> {
    let layout = inst.def.native_layout.as_ref().ok_or_else(|| {
        RuntimeError::type_err(format!("'{}' has no native layout", inst.def.name))
    })?;
    let mut buf = vec![0u8; layout.size.max(1)];
    let base = buf.as_mut_ptr() as usize;
    let slots = inst.slots.borrow();
    for f in &layout.fields {
        let idx = inst
            .def
            .fields
            .iter()
            .position(|n| n == &f.name)
            .ok_or_else(|| RuntimeError::attr_err(format!("no field '{}'", f.name)))?;
        write_field(base, f, &slots[idx])?;
    }
    Ok(buf)
}

pub(crate) fn unpack_c_struct(vm: &Vm, name: &str, buf: &[u8]) -> Result<Value> {
    let def = vm
        .struct_defs
        .get(name)
        .ok_or_else(|| RuntimeError::type_err(format!("unknown type '{name}'")))?;
    let layout = def
        .native_layout
        .as_ref()
        .ok_or_else(|| RuntimeError::type_err(format!("'{name}' has no native layout")))?;
    if buf.len() < layout.size {
        return Err(RuntimeError::value_err(format!(
            "C struct '{name}' return buffer too small: {} < {}",
            buf.len(),
            layout.size
        )));
    }
    let base = buf.as_ptr() as usize;
    let mut ordered = Vec::with_capacity(def.fields.len());
    for fname in &def.fields {
        let idx = layout
            .fields
            .iter()
            .position(|f| f.name == *fname)
            .ok_or_else(|| RuntimeError::attr_err(format!("missing field '{fname}'")))?;
        ordered.push(read_field(base, &layout.fields[idx])?);
    }
    Ok(Value::Struct(Arc::new(crate::value::StructInstance {
        def,
        slots: crate::shared::SyncCell::new(ordered),
        generic_args: Vec::new(),
    })))
}

// ----- sync callbacks -------------------------------------------------------

struct CallbackOwned {
    /// Closure must outlive the code pointer; 'static via leak of self-ref bundle.
    _keep: (*mut Closure<'static>, *mut CallbackData),
}

// SAFETY: 仅通过 CALLBACKS 互斥访问；裸指针指向堆上唯一所有者。
unsafe impl Send for CallbackOwned {}
unsafe impl Sync for CallbackOwned {}

struct CallbackData {
    callable: Value,
    arg_abis: Vec<AbiType>,
    ret_abi: AbiType,
}

static CALLBACKS: std::sync::LazyLock<Mutex<HashMap<usize, CallbackOwned>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static CALLBACK_IDS: AtomicUsize = AtomicUsize::new(1);

pub fn builtin_callback(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(RuntimeError::type_err(format!(
            "{}(callable, arg_types, ret_type) requires 3 arguments",
            builtin_repr("callback")
        )));
    }
    let callable = args[0].clone();
    if !matches!(
        callable,
        Value::Function(_) | Value::Builtin(_) | Value::GenericFunction(_)
    ) {
        return Err(RuntimeError::type_err(format!(
            "{}: first argument must be callable",
            builtin_repr("callback")
        )));
    }
    let Value::List(arg_types) = &args[1] else {
        return Err(RuntimeError::type_err(format!(
            "{}: arg_types must be a list of TypeRefs",
            builtin_repr("callback")
        )));
    };
    let mut arg_abis = Vec::new();
    for t in arg_types.borrow().iter() {
        let Value::TypeRef(n) = t else {
            return Err(RuntimeError::type_err(format!(
                "{}: arg_types elements must be TypeRefs",
                builtin_repr("callback")
            )));
        };
        arg_abis.push(AbiType::from_type_name(n)?);
    }
    let ret_abi = match &args[2] {
        Value::TypeRef(n) => AbiType::from_type_name(n)?,
        _ => {
            return Err(RuntimeError::type_err(format!(
                "{}: ret_type must be a TypeRef",
                builtin_repr("callback")
            )))
        }
    };

    let arg_ffi: Vec<FfiType> = arg_abis.iter().map(AbiType::ffi_type).collect();
    let cif = Cif::new(arg_ffi, ret_abi.ffi_type());

    let data = Box::new(CallbackData {
        callable,
        arg_abis,
        ret_abi,
    });
    let data_ptr = Box::into_raw(data);

    unsafe extern "C" fn trampoline(
        _cif: &ffi_low::ffi_cif,
        ret: &mut u64,
        args: *const *const c_void,
        userdata: &mut CallbackData,
    ) {
        let Ok(vm) = active_vm() else {
            // 无活动 VM：无法安全回调；返回零并尽量不静默到不可观测。
            eprintln!("optive FFI callback: no active VM; returning 0");
            *ret = 0;
            return;
        };
        let mut call_args = Vec::with_capacity(userdata.arg_abis.len());
        for (i, abi) in userdata.arg_abis.iter().enumerate() {
            let p = unsafe { *args.add(i) };
            call_args.push(unsafe { decode_cb_arg(p.cast_mut(), abi.clone()) });
        }
        let result = vm.call_value(userdata.callable.clone(), call_args);
        let val = match result {
            Ok(v) => v,
            Err(e) => {
                eprintln!("optive FFI callback error: {}", e.message());
                Value::None
            }
        };
        *ret = encode_cb_ret_u64(&val, userdata.ret_abi.clone());
    }

    let callback: CallbackMut<CallbackData, u64> = trampoline;
    let data_ref: &'static mut CallbackData = unsafe { &mut *data_ptr };
    let closure = Closure::new_mut(cif, callback, data_ref);
    let code = {
        let f: &unsafe extern "C" fn() = closure.code_ptr();
        (*f) as *mut c_void as usize
    };

    // Extend closure lifetime: leak box holding closure
    let boxed = Box::new(closure);
    let closure_ptr = Box::into_raw(boxed);
    // SAFETY: Closure<'a> with 'a tied to leaked CallbackData — both leaked until free.
    let static_closure: *mut Closure<'static> = closure_ptr;

    let id = CALLBACK_IDS.fetch_add(1, Ordering::Relaxed);
    CALLBACKS.lock().insert(
        id,
        CallbackOwned {
            _keep: (static_closure, data_ptr),
        },
    );

    Ok(Value::List(Shared::new(vec![
        Value::Ptr(code),
        Value::Num(Num::from_i64(id as i64)),
    ])))
}

pub fn builtin_callback_free(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(format!(
            "{}(id) requires id from {}",
            builtin_repr("callback_free"),
            builtin_repr("callback")
        )));
    }
    let id = expect_usize("callback_free", &args[0])?;
    if let Some(owned) = CALLBACKS.lock().remove(&id) {
        unsafe {
            drop(Box::from_raw(owned._keep.0));
            drop(Box::from_raw(owned._keep.1));
        }
    }
    Ok(Value::None)
}

unsafe fn decode_cb_arg(p: *mut c_void, abi: AbiType) -> Value {
    // Prefer Value::Num so Optive arithmetic (`+`, etc.) works without casts.
    match abi {
        AbiType::I32 => Value::Num(Num::from_i64(i64::from(unsafe { *(p as *const i32) }))),
        AbiType::U32 => Value::Num(Num::from_i64(i64::from(unsafe { *(p as *const u32) }))),
        AbiType::I64 => Value::Num(Num::from_i64(unsafe { *(p as *const i64) })),
        AbiType::U64 => {
            let n = unsafe { *(p as *const u64) };
            if i64::try_from(n).is_ok() {
                Value::Num(Num::from_i64(n as i64))
            } else {
                Value::Sized(SizedNum::U64(n))
            }
        }
        AbiType::I16 => Value::Num(Num::from_i64(i64::from(unsafe { *(p as *const i16) }))),
        AbiType::U16 => Value::Num(Num::from_i64(i64::from(unsafe { *(p as *const u16) }))),
        AbiType::I8 => Value::Num(Num::from_i64(i64::from(unsafe { *(p as *const i8) }))),
        AbiType::U8 => Value::Num(Num::from_i64(i64::from(unsafe { *(p as *const u8) }))),
        AbiType::Bool => Value::Bool(unsafe { *(p as *const u8) } != 0),
        // Floats stay sized; convert with num.(f64.(_)) if needed in script.
        AbiType::F32 => Value::Sized(SizedNum::F32(unsafe { *(p as *const f32) })),
        AbiType::F64 => Value::Sized(SizedNum::F64(unsafe { *(p as *const f64) })),
        AbiType::Pointer | AbiType::CharPtr | AbiType::WCharPtr | AbiType::Usize => {
            Value::Ptr(unsafe { *(p as *const usize) })
        }
        AbiType::Isize => Value::Num(Num::from_i64(unsafe { *(p as *const isize) } as i64)),
        AbiType::Void => Value::None,
        AbiType::CStruct { .. } => Value::None,
    }
}

fn encode_cb_ret_u64(v: &Value, abi: AbiType) -> u64 {
    match abi {
        AbiType::Void => 0,
        AbiType::Bool | AbiType::I8 | AbiType::U8 => u64::from(v.is_truthy()),
        AbiType::I16
        | AbiType::U16
        | AbiType::I32
        | AbiType::U32
        | AbiType::I64
        | AbiType::Isize => expect_i64("cb", v).unwrap_or(0) as u64,
        AbiType::Pointer | AbiType::CharPtr | AbiType::WCharPtr | AbiType::Usize => {
            expect_ptr_or_usize("cb", v).unwrap_or(0) as u64
        }
        AbiType::U64 => match v {
            Value::Sized(SizedNum::U64(n)) => *n,
            _ => expect_i64("cb", v).unwrap_or(0) as u64,
        },
        // libffi 结果槽按 CIF 类型解读；对 f32/f64 写入 IEEE 比特（非整型转换）。
        AbiType::F32 => {
            let f = match v {
                Value::Sized(SizedNum::F32(x)) => *x,
                Value::Sized(SizedNum::F64(x)) => *x as f32,
                Value::Num(n) => n.to_f64_checked().unwrap_or(0.0) as f32,
                _ => 0.0,
            };
            u64::from(f.to_bits())
        }
        AbiType::F64 => {
            let f = match v {
                Value::Sized(SizedNum::F64(x)) => *x,
                Value::Sized(SizedNum::F32(x)) => f64::from(*x),
                Value::Num(n) => n.to_f64_checked().unwrap_or(0.0),
                _ => 0.0,
            };
            f.to_bits()
        }
        AbiType::CStruct { .. } => 0,
    }
}

fn expect_usize(name: &str, v: &Value) -> Result<usize> {
    expect_usize_label(&builtin_repr(name), v)
}

fn expect_usize_label(ctx: &str, v: &Value) -> Result<usize> {
    match v {
        Value::Ptr(p) => Ok(*p),
        Value::Sized(SizedNum::Usize(x)) => Ok(*x),
        Value::Sized(SizedNum::Isize(x)) => Ok(*x as usize),
        Value::Sized(s) => s
            .to_i64()
            .map(|n| n as usize)
            .ok_or_else(|| RuntimeError::type_err(format!("{ctx}: expected integer"))),
        Value::Num(n) => n
            .to_i64()
            .map(|x| x as usize)
            .ok_or_else(|| RuntimeError::type_err(format!("{ctx}: expected integer"))),
        _ => Err(RuntimeError::type_err(format!(
            "{ctx}: expected integer/ptr, got {}",
            v.type_name()
        ))),
    }
}

fn expect_i64(name: &str, v: &Value) -> Result<i64> {
    expect_i64_label(&builtin_repr(name), v)
}

fn expect_i64_label(ctx: &str, v: &Value) -> Result<i64> {
    match v {
        Value::Sized(s) => s
            .to_i64()
            .ok_or_else(|| RuntimeError::type_err(format!("{ctx}: expected integer"))),
        Value::Num(n) => n
            .to_i64()
            .ok_or_else(|| RuntimeError::type_err(format!("{ctx}: expected integer"))),
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Ptr(p) => Ok(*p as i64),
        _ => Err(RuntimeError::type_err(format!(
            "{ctx}: expected integer, got {}",
            v.type_name()
        ))),
    }
}

fn expect_ptr(name: &str, v: &Value) -> Result<usize> {
    expect_ptr_label(&builtin_repr(name), v)
}

fn expect_ptr_label(ctx: &str, v: &Value) -> Result<usize> {
    match v {
        Value::Ptr(p) => Ok(*p),
        Value::Num(n) => Ok(n.to_i64().unwrap_or(0) as usize),
        Value::Sized(SizedNum::Usize(x)) => Ok(*x),
        _ => Err(RuntimeError::type_err(format!(
            "{ctx}: expected ptr, got {}",
            v.type_name()
        ))),
    }
}

fn expect_ptr_or_usize(name: &str, v: &Value) -> Result<usize> {
    expect_ptr(name, v).or_else(|_| expect_usize(name, v))
}

fn expect_ptr_or_usize_label(ctx: &str, v: &Value) -> Result<usize> {
    expect_ptr_label(ctx, v).or_else(|_| expect_usize_label(ctx, v))
}
