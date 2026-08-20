//! 护照指针登记表：本方 alloc 可验证；外来须 `unsafe_ptr` 才允许 peek。

use std::collections::HashMap;
use std::sync::LazyLock;

use parking_lot::Mutex;

use crate::error::RuntimeError;
use crate::value::builtin_repr;
use crate::ffi::{abi_size_align, AbiType};
use crate::value::Value;
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtrKind {
    /// `alloc` / `cstring` 等，free 时释放。
    Owned,
    /// `unsafe_ptr`：允许 peek，不负责 free。
    ForeignUnsafe,
}

#[derive(Debug, Clone)]
pub struct PtrEntry {
    pub addr: usize,
    pub nbytes: usize,
    pub align: usize,
    /// 元素类型名（如 `i32`、`Point`）；无则禁止 `p[i]`。
    pub elem: Option<String>,
    pub kind: PtrKind,
}

static REGISTRY: LazyLock<Mutex<HashMap<usize, PtrEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn register(entry: PtrEntry) {
    if entry.addr == 0 {
        return;
    }
    REGISTRY.lock().insert(entry.addr, entry);
}

pub fn unregister(addr: usize) -> Option<PtrEntry> {
    if addr == 0 {
        return None;
    }
    REGISTRY.lock().remove(&addr)
}

pub fn lookup(addr: usize) -> Option<PtrEntry> {
    if addr == 0 {
        return None;
    }
    REGISTRY.lock().get(&addr).cloned()
}

/// 是否在登记表中（Owned 或 `ForeignUnsafe`）—— peek 门槛。
#[must_use]
pub fn is_registered(addr: usize) -> bool {
    lookup(addr).is_some()
}

/// Optive 分配器意义上的「活」：由 `alloc*` 分配且尚未 `free`。
/// **不含** `unsafe_ptr` 外来指针；**不**探测 OS 堆悬垂。
#[must_use]
pub fn is_live(addr: usize) -> bool {
    matches!(lookup(addr), Some(e) if e.kind == PtrKind::Owned)
}

/// 读/写下标或 peek 前：必须登记；Owned 校验 `[offset, offset+len)` 不越界。
pub fn check_access(addr: usize, offset: usize, len: usize) -> Result<PtrEntry> {
    let Some(e) = lookup(addr) else {
        return Err(RuntimeError::value_err(
            format!("pointer not registered (use {} / {} before peek)", builtin_repr("alloc"), builtin_repr("unsafe_ptr")),
        ));
    };
    if e.kind == PtrKind::Owned {
        let end = offset
            .checked_add(len)
            .ok_or_else(|| RuntimeError::value_err("pointer access overflow"))?;
        if end > e.nbytes {
            return Err(RuntimeError::index_err(format!(
                "pointer access out of bounds: offset {offset}+{len} > {}",
                e.nbytes
            )));
        }
    }
    Ok(e)
}

pub fn require_elem(addr: usize) -> Result<(PtrEntry, String)> {
    let e = lookup(addr).ok_or_else(|| {
        RuntimeError::value_err(
            format!("pointer not registered (use {} / {} for typed indexing)", builtin_repr("alloc_array"), builtin_repr("cast_ptr")),
        )
    })?;
    let elem = e.elem.clone().ok_or_else(|| {
        RuntimeError::type_err(
            format!("untyped ptr cannot be indexed (use {}(T,n) or {}(p, T))", builtin_repr("alloc_array"), builtin_repr("cast_ptr")),
        )
    })?;
    Ok((e, elem))
}

pub fn set_elem(addr: usize, elem: Option<String>) -> Result<()> {
    let mut g = REGISTRY.lock();
    let e = g.get_mut(&addr).ok_or_else(|| {
        RuntimeError::value_err(format!("{}: pointer not registered", builtin_repr("cast_ptr")))
    })?;
    e.elem = elem;
    Ok(())
}

/// 将类型名解析为元素步长（标量 ABI 或带 `native_layout` 的结构体由调用方传入 size）。
pub fn scalar_stride(type_name: &str) -> Result<usize> {
    let abi = AbiType::from_type_name(type_name)?;
    let (sz, _) = abi_size_align(&abi);
    if sz == 0 {
        return Err(RuntimeError::type_err(format!(
            "type '{type_name}' has zero size"
        )));
    }
    Ok(sz)
}

#[must_use]
pub fn is_ptr_type_name(name: &str) -> bool {
    matches!(name, "ptr" | "pointer" | "void*" | "void_ptr")
}

/// 从 TypeSpec/注解名取出 pointee（`ptr` / `void*` + 单参）。
#[must_use]
pub fn pointee_from_generic(name: &str, params: &[Value]) -> Option<String> {
    if !is_ptr_type_name(name) || params.len() != 1 {
        return None;
    }
    match &params[0] {
        Value::TypeRef(n) | Value::Text(n) => Some(n.clone()),
        Value::TypeSpec(_) => None,
        _ => None,
    }
}

pub fn alloc_owned(nbytes: usize, align: usize, elem: Option<String>) -> Result<usize> {
    if nbytes == 0 {
        return Ok(0);
    }
    let layout = std::alloc::Layout::from_size_align(nbytes, align.max(1))
        .map_err(|e| RuntimeError::value_err(format!("alloc: invalid layout: {e}")))?;
    let p = unsafe { std::alloc::alloc_zeroed(layout) };
    if p.is_null() {
        return Err(RuntimeError::msg("alloc: out of memory"));
    }
    let addr = p as usize;
    register(PtrEntry {
        addr,
        nbytes,
        align: align.max(1),
        elem,
        kind: PtrKind::Owned,
    });
    Ok(addr)
}

pub fn free_owned(addr: usize) -> Result<()> {
    if addr == 0 {
        return Ok(());
    }
    let Some(e) = unregister(addr) else {
        return Err(RuntimeError::value_err(
            format!("{}: pointer not registered as owned (already freed or foreign)", builtin_repr("free")),
        ));
    };
    if e.kind != PtrKind::Owned {
        // 放回并报错
        register(e);
        return Err(RuntimeError::value_err(
            format!("{}: cannot free foreign/unsafe pointer", builtin_repr("free")),
        ));
    }
    if e.nbytes == 0 {
        return Ok(());
    }
    let layout = std::alloc::Layout::from_size_align(e.nbytes, e.align.max(1))
        .map_err(|e| RuntimeError::value_err(format!("{}: invalid layout: {e}", builtin_repr("free"))))?;
    unsafe { std::alloc::dealloc(addr as *mut u8, layout) };
    Ok(())
}

/// 仅移除登记（不 `dealloc`）。外来指针测毕或误登记时用。
#[must_use]
pub fn unregister_only(addr: usize) -> bool {
    unregister(addr).is_some()
}
