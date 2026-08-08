//! Optive 的环收集垃圾回收器（M:N 就绪）。
//!
//! VM 堆由 `Shared<T>` 容器（`List`、`Dict`、`Iterator`、`Cell`）以及
//! `Arc<StructInstance>` 构成。纯引用计数无法回收环引用。
//!
//! 标记-清扫打断环：创建时以 `WeakShared` 登记；`collect` 从根标记可达对象，
//! 清空不可达对象内部。真正释放仍由 `Arc` 引用计数完成。

use std::sync::Arc;
use std::sync::Weak as ArcWeak;

use rustc_hash::FxHashSet;

use crate::shared::{Shared, WeakShared};
use crate::value::{DictMap, IteratorKind, IteratorState, SetMap, StructInstance, Value};
use crate::vm::Vm;

/// 指向某一跟踪堆对象的弱引用，按种类标记以便清扫时清空内部。
pub(crate) enum TrackedWeak {
    List(WeakShared<Vec<Value>>),
    Dict(WeakShared<DictMap>),
    Set(WeakShared<SetMap>),
    Iter(WeakShared<IteratorState>),
    Cell(WeakShared<Value>),
    Struct(ArcWeak<StructInstance>),
}

#[derive(Default)]
pub struct GcTracker {
    weaks: Vec<TrackedWeak>,
    /// 已登记对象的指针地址，避免同一 Shared/Arc 被重复跟踪。
    addrs: FxHashSet<usize>,
}

fn track_shared_into<T>(
    tracker: &mut GcTracker,
    rc: &Shared<T>,
    wrap: impl FnOnce(WeakShared<T>) -> TrackedWeak,
) {
    let addr = rc.as_ptr() as usize;
    if tracker.track_addr(addr) {
        tracker.weaks.push(wrap(rc.downgrade()));
    }
}

impl GcTracker {
    pub fn new() -> Self {
        Self {
            weaks: Vec::new(),
            addrs: FxHashSet::default(),
        }
    }

    fn track_addr(&mut self, addr: usize) -> bool {
        self.addrs.insert(addr)
    }

    pub fn track_list(&mut self, rc: &Shared<Vec<Value>>) {
        track_shared_into(self, rc, TrackedWeak::List);
    }

    pub fn track_dict(&mut self, rc: &Shared<DictMap>) {
        track_shared_into(self, rc, TrackedWeak::Dict);
    }

    pub fn track_set(&mut self, rc: &Shared<SetMap>) {
        track_shared_into(self, rc, TrackedWeak::Set);
    }

    pub fn track_iter(&mut self, rc: &Shared<IteratorState>) {
        track_shared_into(self, rc, TrackedWeak::Iter);
    }

    pub fn track_cell(&mut self, rc: &Shared<Value>) {
        track_shared_into(self, rc, TrackedWeak::Cell);
    }

    pub fn track_struct(&mut self, rc: &Arc<StructInstance>) {
        let addr = Arc::as_ptr(rc) as usize;
        if self.track_addr(addr) {
            self.weaks.push(TrackedWeak::Struct(Arc::downgrade(rc)));
        }
    }

    pub fn tracked_count(&self) -> usize {
        self.weaks.len()
    }

    /// 去掉已失效的 Weak 跟踪项（不打断环、不做标记）。供 M:N helper 限制表大小。
    pub fn prune_dead(&mut self) -> usize {
        let before = self.weaks.len();
        self.weaks.retain(|w| match w {
            TrackedWeak::List(weak) => weak.upgrade().is_some(),
            TrackedWeak::Dict(weak) => weak.upgrade().is_some(),
            TrackedWeak::Set(weak) => weak.upgrade().is_some(),
            TrackedWeak::Iter(weak) => weak.upgrade().is_some(),
            TrackedWeak::Cell(weak) => weak.upgrade().is_some(),
            TrackedWeak::Struct(weak) => weak.strong_count() > 0,
        });
        // 按存活条目重建地址集，避免死指针占坑。
        self.addrs.clear();
        for w in &self.weaks {
            let addr = match w {
                TrackedWeak::List(weak) => weak.upgrade().map(|rc| rc.as_ptr() as usize),
                TrackedWeak::Dict(weak) => weak.upgrade().map(|rc| rc.as_ptr() as usize),
                TrackedWeak::Set(weak) => weak.upgrade().map(|rc| rc.as_ptr() as usize),
                TrackedWeak::Iter(weak) => weak.upgrade().map(|rc| rc.as_ptr() as usize),
                TrackedWeak::Cell(weak) => weak.upgrade().map(|rc| rc.as_ptr() as usize),
                TrackedWeak::Struct(weak) => weak.upgrade().map(|rc| Arc::as_ptr(&rc) as usize),
            };
            if let Some(addr) = addr {
                self.addrs.insert(addr);
            }
        }
        before.saturating_sub(self.weaks.len())
    }

    /// 标记-清扫：从 `vm` 根可达对象做标记，再清空不可达对象内部以打断环。
    pub fn collect(&mut self, vm: &Vm) -> usize {
        let mut marked: FxHashSet<usize> = FxHashSet::default();
        vm.gc_mark_roots(&mut marked);
        self.sweep(&marked)
    }

    fn sweep(&mut self, marked: &FxHashSet<usize>) -> usize {
        let mut cleared = 0usize;
        let mut removed_addrs: Vec<usize> = Vec::new();
        self.weaks.retain(|w| match w {
            TrackedWeak::List(weak) => {
                let Some(rc) = weak.upgrade() else {
                    return false;
                };
                let addr = rc.as_ptr() as usize;
                if marked.contains(&addr) {
                    return true;
                }
                rc.borrow_mut().clear();
                removed_addrs.push(addr);
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
                removed_addrs.push(addr);
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
                removed_addrs.push(addr);
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
                // 置空迭代器状态，打断对源容器的引用。
                rc.borrow_mut().kind = IteratorKind::List {
                    items: Vec::new(),
                    index: 0,
                };
                removed_addrs.push(addr);
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
                removed_addrs.push(addr);
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
                removed_addrs.push(addr);
                cleared += 1;
                false
            }
        });
        for addr in removed_addrs {
            self.addrs.remove(&addr);
        }
        cleared
    }
}

/// 将 `val` 及其子对象加入标记工作表（供 `Vm::gc_mark_roots` / 递归标记使用）。
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
                for (k, v) in d.iter() {
                    let _ = k;
                    worklist.push(v.clone());
                }
            }
        }
        Value::Set(rc) => {
            // Set 仅容纳可哈希标量键，不会形成容器环；登记地址即可。
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
                // Task 载荷在状态机里；保守地不深入，避免与调度器竞态。
                let _ = t;
            }
        }
        Value::Channel(c) => {
            let addr = c.as_ptr() as usize;
            if marked.insert(addr) {
                for v in c.borrow().queue.iter() {
                    worklist.push(v.clone());
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
        Value::Num(crate::value::Num::Int(_))
        | Value::Num(crate::value::Num::Rat(_))
        | Value::Num(crate::value::Num::Small(_))
        | Value::None
        | Value::Bool(_)
        | Value::Sized(_)
        | Value::Ptr(_)
        | Value::DllHandle(_)
        | Value::Text(_)
        | Value::TypeRef(_)
        | Value::Bytes(_)
        | Value::GenericFunction(_)
        | Value::Macro(_)
        | Value::Builtin(_)
        | Value::RuntimeAst(_)
        | Value::TypeSpec(_)
        | Value::EnumMember(_) => {}
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
        | IteratorKind::GenExpr { source, .. } => {
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
        IteratorKind::Channel { .. } => {}
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
