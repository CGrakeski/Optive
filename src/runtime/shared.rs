//! M:N 共享堆单元：`Arc` + `parking_lot::RwLock`，API 对齐原 `Shared<T>`。
//!
//! 考量：
//! - 方法名保留 `borrow` / `borrow_mut`，降低 ~300 处调用点改动；
//! - 使用 `parking_lot`（无 poison、更快的无竞争路径）；
//! - 单线程时仍正确，便于阶段 A 先绿测再上调度器。
//! - `SharedMap`：跨 worker 共享的全局绑定表（模块 init 时换新 Arc，避免 clear 误伤）。

use std::sync::{Arc, Weak};

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use rustc_hash::FxHashMap;

/// 线程安全的共享可变堆对象（原 `Shared<T>`）。
#[derive(Default)]
pub struct Shared<T: ?Sized> {
    inner: Arc<RwLock<T>>,
}

impl<T> Shared<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(RwLock::new(value)),
        }
    }

    #[inline]
    pub fn borrow(&self) -> RwLockReadGuard<'_, T> {
        self.inner.read()
    }

    #[inline]
    pub fn borrow_mut(&self) -> RwLockWriteGuard<'_, T> {
        crate::gc::write_barrier_addr(Arc::as_ptr(&self.inner) as usize);
        self.inner.write()
    }

    #[inline]
    pub fn try_borrow(&self) -> Option<RwLockReadGuard<'_, T>> {
        self.inner.try_read()
    }

    #[inline]
    pub fn try_borrow_mut(&self) -> Option<RwLockWriteGuard<'_, T>> {
        crate::gc::write_barrier_addr(Arc::as_ptr(&self.inner) as usize);
        self.inner.try_write()
    }

    #[inline]
    pub fn as_ptr(&self) -> *const RwLock<T> {
        Arc::as_ptr(&self.inner)
    }

    #[inline]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.inner, &b.inner)
    }

    #[inline]
    pub fn downgrade(&self) -> WeakShared<T> {
        WeakShared {
            inner: Arc::downgrade(&self.inner),
        }
    }

    #[inline]
    pub fn strong_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }

    #[inline]
    pub fn try_unwrap(self) -> Result<T, Self> {
        match Arc::try_unwrap(self.inner) {
            Ok(lock) => Ok(lock.into_inner()),
            Err(inner) => Err(Self { inner }),
        }
    }
}

impl<T: ?Sized> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> std::fmt::Debug for Shared<T>
where
    T: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Shared").field(&*self.borrow()).finish()
    }
}

/// `Shared` 的弱引用（GC 跟踪用）。
pub struct WeakShared<T: ?Sized> {
    inner: Weak<RwLock<T>>,
}

impl<T: ?Sized> Clone for WeakShared<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> WeakShared<T> {
    #[inline]
    pub fn upgrade(&self) -> Option<Shared<T>> {
        self.inner.upgrade().map(|inner| Shared { inner })
    }
}

/// 结构体槽位等「嵌在 Arc 载荷内」的可变字段：API 同 `RefCell`。
#[derive(Default)]
pub struct SyncCell<T> {
    inner: RwLock<T>,
}

impl<T> SyncCell<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            inner: RwLock::new(value),
        }
    }

    #[inline]
    pub fn borrow(&self) -> RwLockReadGuard<'_, T> {
        self.inner.read()
    }

    #[inline]
    pub fn borrow_mut(&self) -> RwLockWriteGuard<'_, T> {
        // 脏卡：锁地址在 track_struct 时登记为 Struct 别名，terminate 可 O(1) 解析。
        crate::gc::write_barrier_addr(self.lock_addr());
        self.inner.write()
    }

    /// `RwLock` 身份地址（Struct 槽位写屏障别名用）。
    #[inline]
    pub fn lock_addr(&self) -> usize {
        &self.inner as *const RwLock<T> as usize
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T: Clone> Clone for SyncCell<T> {
    fn clone(&self) -> Self {
        Self::new(self.borrow().clone())
    }
}

/// 线程安全的字符串→Value 映射（VM `globals`）。`Clone` 为 Arc 共享。
#[derive(Clone, Default)]
pub struct SharedMap {
    inner: Arc<RwLock<FxHashMap<String, crate::value::Value>>>,
}

impl SharedMap {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn get(&self, key: &str) -> Option<crate::value::Value> {
        self.inner.read().get(key).cloned()
    }

    #[inline]
    pub fn insert(
        &self,
        key: String,
        value: crate::value::Value,
    ) -> Option<crate::value::Value> {
        self.inner.write().insert(key, value)
    }

    /// 按已有键就地写入；若值为 `Cell` 则写单元格内容。键不存在返回 `false`（不分配）。
    #[inline]
    pub fn set_inplace(&self, key: &str, value: crate::value::Value) -> bool {
        let mut g = self.inner.write();
        let Some(slot) = g.get_mut(key) else {
            return false;
        };
        if let crate::value::Value::Cell(cell) = slot {
            *cell.borrow_mut() = value;
        } else {
            *slot = value;
        }
        true
    }

    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        self.inner.read().contains_key(key)
    }

    #[inline]
    pub fn remove(&self, key: &str) -> Option<crate::value::Value> {
        self.inner.write().remove(key)
    }

    #[inline]
    pub fn clear(&self) {
        self.inner.write().clear();
    }

    #[inline]
    pub fn or_insert_with(
        &self,
        key: String,
        f: impl FnOnce() -> crate::value::Value,
    ) {
        self.inner.write().entry(key).or_insert_with(f);
    }

    #[inline]
    pub fn values(&self) -> Vec<crate::value::Value> {
        self.inner.read().values().cloned().collect()
    }

    #[inline]
    pub fn keys(&self) -> Vec<String> {
        self.inner.read().keys().cloned().collect()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// 深拷贝内部表（模块快照等需要隔离内容时用）。
    #[inline]
    pub fn deep_clone(&self) -> FxHashMap<String, crate::value::Value> {
        self.inner.read().clone()
    }

    #[inline]
    pub fn replace_with(&self, map: FxHashMap<String, crate::value::Value>) {
        *self.inner.write() = map;
    }

    #[inline]
    pub fn with_mut<R>(
        &self,
        f: impl FnOnce(&mut FxHashMap<String, crate::value::Value>) -> R,
    ) -> R {
        f(&mut self.inner.write())
    }

    #[inline]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.inner, &b.inner)
    }
}

/// 线程安全的 `String → T` 注册表（`struct_defs` / `functions` 等）。
/// `Clone` 为 Arc 共享，使 M:N helper 能看见主线程 `load_program` 后注册的定义。
pub struct SharedTable<T> {
    inner: Arc<RwLock<FxHashMap<String, T>>>,
}

impl<T> Default for SharedTable<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(FxHashMap::default())),
        }
    }
}

impl<T> SharedTable<T> {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        self.inner.read().contains_key(key)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    #[inline]
    pub fn remove(&self, key: &str) -> Option<T> {
        self.inner.write().remove(key)
    }

    #[inline]
    pub fn clear(&self) {
        self.inner.write().clear();
    }

    #[inline]
    pub fn with_ref<R>(&self, f: impl FnOnce(&FxHashMap<String, T>) -> R) -> R {
        f(&self.inner.read())
    }

    #[inline]
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut FxHashMap<String, T>) -> R) -> R {
        f(&mut self.inner.write())
    }

    #[inline]
    pub fn replace_with(&self, map: FxHashMap<String, T>) {
        *self.inner.write() = map;
    }

    #[inline]
    pub fn keys(&self) -> Vec<String> {
        self.inner.read().keys().cloned().collect()
    }
}

impl<T: Clone> SharedTable<T> {
    #[inline]
    pub fn get(&self, key: &str) -> Option<T> {
        self.inner.read().get(key).cloned()
    }

    #[inline]
    pub fn insert(&self, key: String, value: T) -> Option<T> {
        self.inner.write().insert(key, value)
    }

    #[inline]
    pub fn extend(&self, iter: impl IntoIterator<Item = (String, T)>) {
        self.inner.write().extend(iter);
    }

    /// 深拷贝内容（模块 init 快照 / diff 基线）。
    #[inline]
    pub fn snapshot_map(&self) -> FxHashMap<String, T> {
        self.inner.read().clone()
    }

    #[inline]
    pub fn values(&self) -> Vec<T> {
        self.inner.read().values().cloned().collect()
    }

    #[inline]
    pub fn entry_or_insert_with(&self, key: String, f: impl FnOnce() -> T) -> T {
        self.inner
            .write()
            .entry(key)
            .or_insert_with(f)
            .clone()
    }
}

impl<T> Clone for SharedTable<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> SharedTable<T> {
    #[inline]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.inner, &b.inner)
    }
}

/// 线程安全的 `Vec`（`script_globals` 等）。
#[derive(Clone, Default)]
pub struct SharedVec<T> {
    inner: Arc<RwLock<Vec<T>>>,
}

impl<T> SharedVec<T> {
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Vec::new())),
        }
    }

    #[inline]
    pub fn from_vec(v: Vec<T>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(v)),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    #[inline]
    pub fn with_ref<R>(&self, f: impl FnOnce(&Vec<T>) -> R) -> R {
        f(&self.inner.read())
    }

    #[inline]
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut Vec<T>) -> R) -> R {
        f(&mut self.inner.write())
    }

    #[inline]
    pub fn replace(&self, v: Vec<T>) {
        *self.inner.write() = v;
    }
}

impl<T: Clone> SharedVec<T> {
    #[inline]
    pub fn get(&self, idx: usize) -> Option<T> {
        self.inner.read().get(idx).cloned()
    }

    #[inline]
    pub fn set(&self, idx: usize, value: T) {
        if let Some(slot) = self.inner.write().get_mut(idx) {
            *slot = value;
        }
    }

    #[inline]
    pub fn clone_vec(&self) -> Vec<T> {
        self.inner.read().clone()
    }
}
