//! Optive 的环收集垃圾回收器（M:N 就绪）。
//!
//! VM 堆由 `Shared<T>` 容器（`List`、`Dict`、`Iterator`、`Cell`）以及
//! `Arc<StructInstance>` 构成。纯引用计数无法回收环引用。
//!
//! 标记-清扫打断环：创建时以 `WeakShared` 登记；`collect` 从根标记可达对象，
//! 清空不可达对象内部。真正释放仍由 `Arc` 引用计数完成。
//!
//! 模式（`OPTIVE_GC_MODE`）：
//! - `stw`（默认）：完整 stop-the-world 标记+清扫
//! - `concurrent`：短 STW 握手 + 并发三色标记（脏卡写屏障）+ 并行 marker + 短 STW 清扫

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Weak as ArcWeak;
use std::time::Instant;

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::shared::{Shared, WeakShared};
use crate::value::{DictMap, IteratorKind, IteratorState, SetMap, StructInstance, Value};
use crate::vm::Vm;

/// 指向某一跟踪堆对象的弱引用，按种类标记以便清扫时清空内部。
#[derive(Clone)]
pub(crate) enum TrackedWeak {
    List(WeakShared<Vec<Value>>),
    Dict(WeakShared<DictMap>),
    Set(WeakShared<SetMap>),
    Iter(WeakShared<IteratorState>),
    Cell(WeakShared<Value>),
    Struct(ArcWeak<StructInstance>),
}

#[derive(Default)]
struct TrackerData {
    weaks: Vec<TrackedWeak>,
    /// 主地址 / 别名（如 Struct 的 `SyncCell` 锁）→ `weaks` 下标；sweep/prune 后重建。
    by_addr: FxHashMap<usize, usize>,
}

/// GC 运行模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GcMode {
    Stw,
    Concurrent,
}

impl GcMode {
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var("OPTIVE_GC_MODE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "concurrent" | "conc" | "parallel" => Self::Concurrent,
            _ => Self::Stw,
        }
    }
}

/// 跨 worker 共享的环收集器。
pub struct SharedGc {
    data: Mutex<TrackerData>,
    /// 防止重叠 collect。
    pub(crate) collect_lock: Mutex<()>,
    mode: GcMode,
    /// 并发标记进行中（写屏障 fast-path）。
    pub(crate) marking: AtomicBool,
    /// 并发标记期间被 mutator 写过的容器地址（脏卡）。
    dirty: Mutex<FxHashSet<usize>>,
    /// 灰对象工作表。
    gray: Mutex<Vec<Value>>,
    /// 已标记地址（黑 ∪ 灰）。
    marked: Mutex<FxHashSet<usize>>,
    /// FFI 卸荷等非协作线程临时钉住的 Value。
    ffi_pins: Mutex<Vec<Value>>,
    /// 进行中的原生 FFI 调用数（卸荷线程不进 safepoint）。
    ffi_inflight: AtomicUsize,
    /// 最近一次 STW 握手+清扫墙钟（纳秒），供基准。
    pub last_stw_ns: AtomicU64,
    /// 最近一次完整 collect 墙钟（纳秒）。
    pub last_collect_ns: AtomicU64,
    /// STW 握手失败后回退/重试计数。
    pub stw_fallback_count: AtomicUsize,
    /// 累计：成功 collect 次数 / STW 纳秒 / collect 纳秒 / 清扫对象数。
    pub total_collects: AtomicUsize,
    pub total_stw_ns: AtomicU64,
    pub total_collect_ns: AtomicU64,
    pub total_cleared: AtomicUsize,
    /// 并行 marker 线程数（concurrent 模式；含协调线程本地工作）。
    marker_threads: usize,
}

impl Default for SharedGc {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedGc {
    #[must_use]
    pub fn new() -> Self {
        Self::with_mode(GcMode::from_env())
    }

    #[must_use]
    pub fn with_mode(mode: GcMode) -> Self {
        let markers = std::env::var("OPTIVE_GC_MARKERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n: &usize| n > 0)
            .unwrap_or_else(|| num_cpus::get().clamp(1, 4));
        Self {
            data: Mutex::new(TrackerData::default()),
            collect_lock: Mutex::new(()),
            mode,
            marking: AtomicBool::new(false),
            dirty: Mutex::new(FxHashSet::default()),
            gray: Mutex::new(Vec::new()),
            marked: Mutex::new(FxHashSet::default()),
            ffi_pins: Mutex::new(Vec::new()),
            ffi_inflight: AtomicUsize::new(0),
            last_stw_ns: AtomicU64::new(0),
            last_collect_ns: AtomicU64::new(0),
            stw_fallback_count: AtomicUsize::new(0),
            total_collects: AtomicUsize::new(0),
            total_stw_ns: AtomicU64::new(0),
            total_collect_ns: AtomicU64::new(0),
            total_cleared: AtomicUsize::new(0),
            marker_threads: markers,
        }
    }

    /// 一次成功收集结束后记账（由 `Vm::gc_collect` 调用）。
    pub fn note_collect_stats(&self, stw_ns: u64, collect_ns: u64, cleared: usize) {
        self.last_stw_ns.store(stw_ns, Ordering::Relaxed);
        self.last_collect_ns.store(collect_ns, Ordering::Relaxed);
        self.total_collects.fetch_add(1, Ordering::Relaxed);
        self.total_stw_ns.fetch_add(stw_ns, Ordering::Relaxed);
        self.total_collect_ns
            .fetch_add(collect_ns, Ordering::Relaxed);
        self.total_cleared.fetch_add(cleared, Ordering::Relaxed);
    }

    pub const fn mode(&self) -> GcMode {
        self.mode
    }

    pub fn is_marking(&self) -> bool {
        self.marking.load(Ordering::Acquire)
    }

    pub fn tracked_count(&self) -> usize {
        self.data.lock().weaks.len()
    }

    fn track_new(data: &mut TrackerData, primary: usize, weak: TrackedWeak, alias: Option<usize>) -> bool {
        if data.by_addr.contains_key(&primary) {
            return false;
        }
        let idx = data.weaks.len();
        data.weaks.push(weak);
        data.by_addr.insert(primary, idx);
        if let Some(a) = alias {
            data.by_addr.insert(a, idx);
        }
        true
    }

    pub fn track_list(&self, rc: &Shared<Vec<Value>>) {
        let addr = rc.as_ptr() as usize;
        let mut data = self.data.lock();
        Self::track_new(
            &mut data,
            addr,
            TrackedWeak::List(rc.downgrade()),
            None,
        );
        drop(data);
        self.shade_new_alloc(Value::List(rc.clone()));
    }

    pub fn track_dict(&self, rc: &Shared<DictMap>) {
        let addr = rc.as_ptr() as usize;
        let mut data = self.data.lock();
        Self::track_new(
            &mut data,
            addr,
            TrackedWeak::Dict(rc.downgrade()),
            None,
        );
        drop(data);
        self.shade_new_alloc(Value::Dict(rc.clone()));
    }

    pub fn track_set(&self, rc: &Shared<SetMap>) {
        let addr = rc.as_ptr() as usize;
        let mut data = self.data.lock();
        Self::track_new(
            &mut data,
            addr,
            TrackedWeak::Set(rc.downgrade()),
            None,
        );
        drop(data);
        self.shade_new_alloc(Value::Set(rc.clone()));
    }

    pub fn track_iter(&self, rc: &Shared<IteratorState>) {
        let addr = rc.as_ptr() as usize;
        let mut data = self.data.lock();
        Self::track_new(
            &mut data,
            addr,
            TrackedWeak::Iter(rc.downgrade()),
            None,
        );
        drop(data);
        self.shade_new_alloc(Value::Iterator(rc.clone()));
    }

    pub fn track_cell(&self, rc: &Shared<Value>) {
        let addr = rc.as_ptr() as usize;
        let mut data = self.data.lock();
        Self::track_new(
            &mut data,
            addr,
            TrackedWeak::Cell(rc.downgrade()),
            None,
        );
        drop(data);
        self.shade_new_alloc(Value::Cell(rc.clone()));
    }

    pub fn track_struct(&self, rc: &Arc<StructInstance>) {
        let addr = Arc::as_ptr(rc) as usize;
        let alias = rc.slots.lock_addr();
        let mut data = self.data.lock();
        Self::track_new(
            &mut data,
            addr,
            TrackedWeak::Struct(Arc::downgrade(rc)),
            Some(alias),
        );
        drop(data);
        self.shade_new_alloc(Value::Struct(rc.clone()));
    }

    /// 并发标记中新分配的对象染灰，避免漂泊白对象。
    fn shade_new_alloc(&self, v: Value) {
        if !self.is_marking() {
            return;
        }
        self.gray.lock().push(v);
    }

    /// 写屏障：mutator 弄脏容器地址，终止阶段重扫。
    #[inline]
    pub fn note_dirty_addr(&self, addr: usize) {
        if !self.is_marking() {
            return;
        }
        self.dirty.lock().insert(addr);
    }

    /// FFI / 卸荷线程钉住值，直至调用结束。
    pub fn ffi_pin(&self, v: Value) {
        self.ffi_pins.lock().push(v);
    }

    pub fn ffi_unpin_last(&self) {
        let mut pins = self.ffi_pins.lock();
        let _ = pins.pop();
    }

    pub fn ffi_pins_snapshot(&self) -> Vec<Value> {
        self.ffi_pins.lock().clone()
    }

    pub fn ffi_enter(&self) {
        self.ffi_inflight.fetch_add(1, Ordering::AcqRel);
    }

    pub fn ffi_leave(&self) {
        self.ffi_inflight.fetch_sub(1, Ordering::AcqRel);
    }

    /// 等待非协作 FFI 结束（最多 `OPTIVE_STW_TIMEOUT_MS`）。
    pub fn wait_ffi_quiescent(&self) -> bool {
        let ms = std::env::var("OPTIVE_STW_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n: &u64| n > 0)
            .unwrap_or(2_000);
        let start = Instant::now();
        while self.ffi_inflight.load(Ordering::Acquire) > 0 {
            if start.elapsed().as_millis() as u64 > ms {
                return false;
            }
            std::thread::yield_now();
        }
        true
    }

    /// 去掉已失效的 Weak（不打断环）。
    pub fn prune_dead(&self) -> usize {
        let mut data = self.data.lock();
        let before = data.weaks.len();
        data.weaks.retain(|w| match w {
            TrackedWeak::List(weak) => weak.upgrade().is_some(),
            TrackedWeak::Dict(weak) => weak.upgrade().is_some(),
            TrackedWeak::Set(weak) => weak.upgrade().is_some(),
            TrackedWeak::Iter(weak) => weak.upgrade().is_some(),
            TrackedWeak::Cell(weak) => weak.upgrade().is_some(),
            TrackedWeak::Struct(weak) => weak.strong_count() > 0,
        });
        reindex_by_addr(&mut data);
        before.saturating_sub(data.weaks.len())
    }

    /// STW 模式：标记+清扫（调用方已持 STW）。
    pub fn collect_stw_inner(&self, vm: &Vm) -> usize {
        let stw_t0 = Instant::now();
        let mut marked = FxHashSet::default();
        let mut worklist = Vec::new();
        vm.gc_push_all_roots(&mut worklist);
        while let Some(v) = worklist.pop() {
            mark_value(&v, &mut marked, &mut worklist);
        }
        let cleared = self.sweep(&marked);
        self.last_stw_ns
            .store(stw_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
        cleared
    }

    /// 分阶段并发收集：由 Vm 控制 STW 边界。
    pub fn concurrent_prepare_roots(&self, vm: &Vm) {
        let mut marked = self.marked.lock();
        marked.clear();
        let mut gray = self.gray.lock();
        gray.clear();
        self.dirty.lock().clear();
        vm.gc_push_all_roots(&mut gray);
        self.set_marking(true);
    }

    /// 并发/并行 drain 灰表；可在无 STW 下运行。
    pub fn concurrent_mark_drain(&self) {
        let markers = self.marker_threads.max(1);
        if markers == 1 {
            self.mark_drain_local();
            return;
        }
        std::thread::scope(|scope| {
            for _ in 0..markers {
                scope.spawn(|| {
                    let mut local_work = Vec::new();
                    loop {
                        if local_work.is_empty() {
                            let mut gray = self.gray.lock();
                            if gray.is_empty() {
                                break;
                            }
                            let take = (gray.len() / 2).clamp(1, 64);
                            let start = gray.len() - take;
                            local_work.extend(gray.drain(start..));
                        }
                        let mut marked = self.marked.lock();
                        while let Some(v) = local_work.pop() {
                            mark_value(&v, &mut marked, &mut local_work);
                        }
                    }
                });
            }
        });
        self.mark_drain_local();
    }

    fn mark_drain_local(&self) {
        let mut local_work = Vec::new();
        loop {
            {
                let mut gray = self.gray.lock();
                if gray.is_empty() && local_work.is_empty() {
                    break;
                }
                local_work.append(&mut gray);
            }
            let mut marked = self.marked.lock();
            while let Some(v) = local_work.pop() {
                mark_value(&v, &mut marked, &mut local_work);
            }
        }
    }

    /// 将脏卡批量转入灰表（可在 STW 外调用，缩短终止窗）。
    pub fn concurrent_flush_dirty_to_gray(&self) -> usize {
        const BATCH: usize = 512;
        let dirties: Vec<usize> = {
            let mut d = self.dirty.lock();
            if d.is_empty() {
                return 0;
            }
            if d.len() <= BATCH {
                d.drain().collect()
            } else {
                let take: Vec<_> = d.iter().copied().take(BATCH).collect();
                for a in &take {
                    d.remove(a);
                }
                take
            }
        };
        let n = dirties.len();
        let mut to_gray = Vec::with_capacity(n);
        {
            let data = self.data.lock();
            for addr in dirties {
                if let Some(v) = tracked_value_at(&data, addr) {
                    to_gray.push(v);
                }
            }
        }
        if !to_gray.is_empty() {
            self.gray.lock().extend(to_gray);
        }
        n
    }

    /// 终止：重扫脏卡直至不动点，关闭屏障，返回 marked 快照供 sweep。
    /// 若脏卡风暴在轮次上限后仍未收敛，返回 `None`，由调用方回退完整 STW。
    pub fn concurrent_terminate(&self, vm: &Vm) -> Option<FxHashSet<usize>> {
        let _ = self.wait_ffi_quiescent();
        {
            let mut gray = self.gray.lock();
            vm.gc_push_all_roots(&mut gray);
            gray.extend(self.ffi_pins_snapshot());
        }
        const MAX_ROUNDS: usize = 48;
        let mut converged = false;
        for _ in 0..MAX_ROUNDS {
            while self.concurrent_flush_dirty_to_gray() > 0 {
                self.mark_drain_local();
            }
            self.mark_drain_local();
            if self.dirty.lock().is_empty() && self.gray.lock().is_empty() {
                converged = true;
                break;
            }
        }
        if !converged {
            self.set_marking(false);
            self.dirty.lock().clear();
            self.gray.lock().clear();
            self.marked.lock().clear();
            return None;
        }
        self.set_marking(false);
        self.dirty.lock().clear();
        self.gray.lock().clear();
        Some(std::mem::take(&mut *self.marked.lock()))
    }

    #[inline]
    pub(crate) fn set_marking(&self, on: bool) {
        self.marking.store(on, Ordering::Release);
    }

    pub fn sweep_marked(&self, marked: &FxHashSet<usize>) -> usize {
        self.sweep(marked)
    }

    fn sweep(&self, marked: &FxHashSet<usize>) -> usize {
        let mut data = self.data.lock();
        let mut cleared = 0usize;
        data.weaks.retain(|w| match w {
            TrackedWeak::List(weak) => {
                let Some(rc) = weak.upgrade() else {
                    return false;
                };
                let addr = rc.as_ptr() as usize;
                if marked.contains(&addr) {
                    return true;
                }
                rc.borrow_mut().clear();
                cleared += 1;
                false
            }
            TrackedWeak::Dict(weak) => {
                let Some(rc) = weak.upgrade() else {
                    return false;
                };
                let addr = rc.as_ptr() as usize;
                if marked.contains(&addr) {
                    return true;
                }
                rc.borrow_mut().clear();
                cleared += 1;
                false
            }
            TrackedWeak::Set(weak) => {
                let Some(rc) = weak.upgrade() else {
                    return false;
                };
                let addr = rc.as_ptr() as usize;
                if marked.contains(&addr) {
                    return true;
                }
                rc.borrow_mut().clear();
                cleared += 1;
                false
            }
            TrackedWeak::Iter(weak) => {
                let Some(rc) = weak.upgrade() else {
                    return false;
                };
                let addr = rc.as_ptr() as usize;
                if marked.contains(&addr) {
                    return true;
                }
                rc.borrow_mut().kind = IteratorKind::List {
                    items: Vec::new(),
                    index: 0,
                };
                cleared += 1;
                false
            }
            TrackedWeak::Cell(weak) => {
                let Some(rc) = weak.upgrade() else {
                    return false;
                };
                let addr = rc.as_ptr() as usize;
                if marked.contains(&addr) {
                    return true;
                }
                *rc.borrow_mut() = Value::None;
                cleared += 1;
                false
            }
            TrackedWeak::Struct(weak) => {
                let Some(rc) = weak.upgrade() else {
                    return false;
                };
                let addr = Arc::as_ptr(&rc) as usize;
                if marked.contains(&addr) {
                    return true;
                }
                rc.slots.borrow_mut().clear();
                cleared += 1;
                false
            }
        });
        reindex_by_addr(&mut data);
        cleared
    }
}

fn reindex_by_addr(data: &mut TrackerData) {
    data.by_addr.clear();
    for (i, w) in data.weaks.iter().enumerate() {
        let (primary, alias) = match w {
            TrackedWeak::List(weak) => {
                let Some(rc) = weak.upgrade() else {
                    continue;
                };
                (rc.as_ptr() as usize, None)
            }
            TrackedWeak::Dict(weak) => {
                let Some(rc) = weak.upgrade() else {
                    continue;
                };
                (rc.as_ptr() as usize, None)
            }
            TrackedWeak::Set(weak) => {
                let Some(rc) = weak.upgrade() else {
                    continue;
                };
                (rc.as_ptr() as usize, None)
            }
            TrackedWeak::Iter(weak) => {
                let Some(rc) = weak.upgrade() else {
                    continue;
                };
                (rc.as_ptr() as usize, None)
            }
            TrackedWeak::Cell(weak) => {
                let Some(rc) = weak.upgrade() else {
                    continue;
                };
                (rc.as_ptr() as usize, None)
            }
            TrackedWeak::Struct(weak) => {
                let Some(rc) = weak.upgrade() else {
                    continue;
                };
                (
                    Arc::as_ptr(&rc) as usize,
                    Some(rc.slots.lock_addr()),
                )
            }
        };
        data.by_addr.insert(primary, i);
        if let Some(a) = alias {
            data.by_addr.insert(a, i);
        }
    }
}

fn tracked_value_at(data: &TrackerData, addr: usize) -> Option<Value> {
    let &idx = data.by_addr.get(&addr)?;
    let w = data.weaks.get(idx)?;
    match w {
        TrackedWeak::List(weak) => weak.upgrade().map(Value::List),
        TrackedWeak::Dict(weak) => weak.upgrade().map(Value::Dict),
        TrackedWeak::Set(weak) => weak.upgrade().map(Value::Set),
        TrackedWeak::Iter(weak) => weak.upgrade().map(Value::Iterator),
        TrackedWeak::Cell(weak) => weak.upgrade().map(Value::Cell),
        TrackedWeak::Struct(weak) => weak.upgrade().map(Value::Struct),
    }
}

thread_local! {
    /// 当前 OS 线程绑定的 SharedGc（主线程 / helper / FFI 卸荷线程各自安装）。
    static CURRENT_GC: RefCell<Option<Arc<SharedGc>>> = const { RefCell::new(None) };
}

pub fn install_current_gc(gc: Arc<SharedGc>) {
    CURRENT_GC.with(|c| {
        *c.borrow_mut() = Some(gc);
    });
}

pub fn clear_current_gc() {
    CURRENT_GC.with(|c| {
        *c.borrow_mut() = None;
    });
}

/// 写屏障：非标记期仅 TLS + 原子 load；标记期才登记脏卡。
#[inline]
pub fn write_barrier_addr(addr: usize) {
    CURRENT_GC.with(|c| {
        let guard = c.borrow();
        let Some(gc) = guard.as_ref() else {
            return;
        };
        if !gc.marking.load(Ordering::Relaxed) {
            return;
        }
        gc.note_dirty_addr(addr);
    });
}

#[inline]
pub fn ffi_enter() {
    CURRENT_GC.with(|c| {
        if let Some(gc) = c.borrow().as_ref() {
            gc.ffi_enter();
        }
    });
}

#[inline]
pub fn ffi_leave() {
    CURRENT_GC.with(|c| {
        if let Some(gc) = c.borrow().as_ref() {
            gc.ffi_leave();
        }
    });
}

/// 兼容旧名：本地非共享 tracker 已移除，保留类型别名文档用。
pub type GcTracker = SharedGc;

/// 将 `val` 及其子对象加入标记工作表。
pub fn mark_value(val: &Value, marked: &mut FxHashSet<usize>, worklist: &mut Vec<Value>) {
    match val {
        Value::List(rc) => {
            let addr = rc.as_ptr() as usize;
            if marked.insert(addr) {
                for v in rc.borrow().iter() {
                    worklist.push(v.clone());
                }
            }
        }
        Value::Dict(rc) => {
            let addr = rc.as_ptr() as usize;
            if marked.insert(addr) {
                let d = rc.borrow();
                for (_k, v) in d.iter() {
                    worklist.push(v.clone());
                }
            }
        }
        Value::Set(rc) => {
            let addr = rc.as_ptr() as usize;
            marked.insert(addr);
        }
        Value::Iterator(rc) => {
            let addr = rc.as_ptr() as usize;
            if marked.insert(addr) {
                mark_iterator_children(&rc.borrow(), worklist);
            }
        }
        Value::Cell(rc) => {
            let addr = rc.as_ptr() as usize;
            if marked.insert(addr) {
                worklist.push(rc.borrow().clone());
            }
        }
        Value::Struct(rc) => {
            let addr = Arc::as_ptr(rc) as usize;
            if marked.insert(addr) {
                for v in rc.slots.borrow().iter() {
                    worklist.push(v.clone());
                }
            }
        }
        Value::Tuple(t) => {
            for v in t.iter() {
                worklist.push(v.clone());
            }
        }
        Value::Variant(v) => {
            worklist.push(v.payload.clone());
        }
        Value::Module(m) => {
            let addr = m.as_ptr() as usize;
            if marked.insert(addr) {
                let mo = m.borrow();
                for v in mo.exports.values() {
                    worklist.push(v.clone());
                }
                for c in mo.children.values() {
                    worklist.push(Value::Module(c.clone()));
                }
            }
        }
        Value::Dispatch(d) => {
            let addr = d.as_ptr() as usize;
            if marked.insert(addr) {
                for v in d.borrow().handlers.borrow().iter() {
                    worklist.push(v.clone());
                }
            }
        }
        Value::Task(t) => {
            let addr = t.as_ptr() as usize;
            if marked.insert(addr) {
                let inner = t.borrow();
                match &inner.state {
                    crate::value::TaskState::Pending { callable, args } => {
                        worklist.push(callable.clone());
                        worklist.extend(args.iter().cloned());
                    }
                    crate::value::TaskState::Done(v) | crate::value::TaskState::Failed(v) => {
                        worklist.push(v.clone());
                    }
                    crate::value::TaskState::Running | crate::value::TaskState::Suspended => {}
                }
                if let Some(tg) = &inner.task_group {
                    worklist.push(Value::Sync(tg.clone()));
                }
            }
        }
        Value::Channel(c) => {
            let addr = c.as_ptr() as usize;
            if marked.insert(addr) {
                for v in &c.borrow().queue {
                    worklist.push(v.clone());
                }
            }
        }
        Value::Stream(s) => {
            let addr = s.as_ptr() as usize;
            if marked.insert(addr) {
                match &*s.borrow() {
                    crate::value::StreamInner::Channel(c) => {
                        worklist.push(Value::Channel(c.clone()));
                    }
                    crate::value::StreamInner::Iter(it) => {
                        worklist.push(Value::Iterator(it.clone()));
                    }
                }
            }
        }
        Value::Mutex(m) => {
            let addr = m.as_ptr() as usize;
            if marked.insert(addr) {
                worklist.push(m.borrow().value.clone());
            }
        }
        Value::MutexGuard(g) => {
            let addr = g.as_ptr() as usize;
            if marked.insert(addr) {
                let m = g.borrow().mutex();
                let maddr = m.as_ptr() as usize;
                if marked.insert(maddr) {
                    worklist.push(m.borrow().value.clone());
                }
            }
        }
        Value::Sync(s) => {
            let addr = s.as_ptr() as usize;
            if marked.insert(addr) {
                mark_sync_children(&s.borrow(), worklist);
            }
        }
        Value::SyncGuard(g) => {
            let addr = g.as_ptr() as usize;
            if marked.insert(addr) {
                match &*g.borrow() {
                    crate::value::SyncGuardInner::Read { mu }
                    | crate::value::SyncGuardInner::Write { mu } => {
                        worklist.push(Value::Sync(mu.clone()));
                    }
                }
            }
        }
        Value::Function(f) => {
            if let Some(cap) = &f.captured {
                for v in cap.values() {
                    worklist.push(v.clone());
                }
            }
        }
        Value::Num(crate::value::Num::Int(_) | crate::value::Num::Rat(_) |
crate::value::Num::Small(_)) | Value::None | Value::Bool(_) | Value::Sized(_)
| Value::Ptr(_) | Value::DllHandle(_) | Value::Text(_) | Value::TypeRef(_) |
Value::Bytes(_) | Value::GenericFunction(_) | Value::Macro(_) |
Value::Builtin(_) | Value::RuntimeAst(_) | Value::TypeSpec(_) |
Value::EnumMember(_) => {}
    }
}

fn mark_iterator_children(state: &IteratorState, worklist: &mut Vec<Value>) {
    match &state.kind {
        IteratorKind::List { items, .. } => {
            for v in items {
                worklist.push(v.clone());
            }
        }
        IteratorKind::Range { .. } => {}
        IteratorKind::Zip { children } => {
            for c in children {
                worklist.push(Value::Iterator(c.clone()));
            }
        }
        IteratorKind::Map { source, .. }
        | IteratorKind::Filter { source, .. }
        | IteratorKind::GenExpr { source, .. }
        | IteratorKind::Take { source, .. } => {
            worklist.push(Value::Iterator(source.clone()));
        }
        IteratorKind::Repeat { value, .. } => {
            worklist.push(value.clone());
        }
        IteratorKind::Cycle { items, .. } => {
            for v in items {
                worklist.push(v.clone());
            }
        }
        IteratorKind::Channel { channel } => {
            worklist.push(Value::Channel(channel.clone()));
        }
        IteratorKind::Generator {
            locals,
            yield_from,
            ..
        } => {
            for v in locals {
                worklist.push(v.clone());
            }
            if let Some(yf) = yield_from {
                worklist.push(Value::Iterator(yf.clone()));
            }
        }
    }
}

fn mark_sync_children(inner: &crate::value::SyncInner, worklist: &mut Vec<Value>) {
    use crate::value::SyncInner;
    match inner {
        SyncInner::RWMutex { value, .. } => worklist.push(value.clone()),
        SyncInner::Once { value, phase: _ } => worklist.push(value.clone()),
        SyncInner::WaitGroup { .. }
        | SyncInner::Semaphore { .. }
        | SyncInner::Barrier { .. }
        | SyncInner::Cond { .. }
        | SyncInner::TimeoutCtx { .. } => {}
        SyncInner::Atomic { value } => worklist.push(value.clone()),
        SyncInner::TaskGroup {
            first_error,
            tasks,
            ..
        } => {
            if let Some(err) = first_error {
                worklist.push(err.clone());
            }
            for t in tasks {
                worklist.push(Value::Task(t.clone()));
            }
        }
    }
}
