//! Optive 的环收集垃圾回收器。
//!
//! VM 堆由 `Rc<RefCell<...>>` 容器（`List`、`Dict`、`Iterator`、`Cell`）以及
//! `Rc<StructInstance>` 构成。纯引用计数无法回收「列表包含自身」或两个结构体
//! 通过 `Cell` 互相引用这类环。
//!
//! 本模块实现简单的标记-清扫：目标是**打断环**而非直接释放内存。每个堆容器在
//! 创建时以 `Weak` 登记到 `GcTracker`。`collect` 从根出发标记所有可达容器（按
//! 指针地址），然后清扫跟踪表：未标记对象清空其内部（从而打断环）并从跟踪表
//! 移除。环被打断后，真正的 `Rc` 释放仍由正常引用计数完成。

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use rustc_hash::FxHashSet;

use crate::value::{DictMap, IteratorKind, IteratorState, SetMap, StructInstance, Value};
use crate::vm::Vm;

/// 指向某一跟踪堆对象的弱引用，按种类标记以便清扫时清空内部。
pub(crate) enum TrackedWeak {
    List(Weak<RefCell<Vec<Value>>>),
    Dict(Weak<RefCell<DictMap>>),
    Set(Weak<RefCell<SetMap>>),
    Iter(Weak<RefCell<IteratorState>>),
    Cell(Weak<RefCell<Value>>),
    Struct(Weak<StructInstance>),
}

#[derive(Default)]
pub struct GcTracker {
    weaks: Vec<TrackedWeak>,
    /// 已登记对象的指针地址，避免同一 Rc 被重复跟踪。
    addrs: FxHashSet<usize>,
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

    pub fn track_list(&mut self, rc: &Rc<RefCell<Vec<Value>>>) {
        let addr = Rc::as_ptr(rc) as usize;
        if self.track_addr(addr) {
            self.weaks.push(TrackedWeak::List(Rc::downgrade(rc)));
        }
    }

    pub fn track_dict(&mut self, rc: &Rc<RefCell<DictMap>>) {
        let addr = Rc::as_ptr(rc) as usize;
        if self.track_addr(addr) {
            self.weaks.push(TrackedWeak::Dict(Rc::downgrade(rc)));
        }
    }

    pub fn track_set(&mut self, rc: &Rc<RefCell<SetMap>>) {
        let addr = Rc::as_ptr(rc) as usize;
        if self.track_addr(addr) {
            self.weaks.push(TrackedWeak::Set(Rc::downgrade(rc)));
        }
    }

    pub fn track_iter(&mut self, rc: &Rc<RefCell<IteratorState>>) {
        let addr = Rc::as_ptr(rc) as usize;
        if self.track_addr(addr) {
            self.weaks.push(TrackedWeak::Iter(Rc::downgrade(rc)));
        }
    }

    pub fn track_cell(&mut self, rc: &Rc<RefCell<Value>>) {
        let addr = Rc::as_ptr(rc) as usize;
        if self.track_addr(addr) {
            self.weaks.push(TrackedWeak::Cell(Rc::downgrade(rc)));
        }
    }

    pub fn track_struct(&mut self, rc: &Rc<StructInstance>) {
        let addr = Rc::as_ptr(rc) as usize;
        if self.track_addr(addr) {
            self.weaks.push(TrackedWeak::Struct(Rc::downgrade(rc)));
        }
    }

    /// 当前已登记的堆对象数量。
    pub fn tracked_count(&self) -> usize {
        self.weaks.len()
    }

    /// 标记-清扫：从 `vm` 根可达对象做标记，再清空不可达对象内部以打断环。
    /// 返回被清空内部的对象个数。
    ///
    /// 注意：调用方需先把 tracker 从 `vm.gc` 取出，再传入 `&Vm`，避免
    /// `&Vm` 与 `&mut self` 别名冲突。
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
                let addr = Rc::as_ptr(&rc) as usize;
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
                let addr = Rc::as_ptr(&rc) as usize;
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
                let addr = Rc::as_ptr(&rc) as usize;
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
                let addr = Rc::as_ptr(&rc) as usize;
                if marked.contains(&addr) {
                    return true;
                }
                // 清空迭代器内部状态，打断对源容器的引用环。
                *rc.borrow_mut() = IteratorState {
                    kind: IteratorKind::List {
                        items: Vec::new(),
                        index: 0,
                    },
                };
                removed_addrs.push(addr);
                cleared += 1;
                false
            }
            TrackedWeak::Cell(weak) => {
                let Some(rc) = weak.upgrade() else {
                    return false;
                };
                let addr = Rc::as_ptr(&rc) as usize;
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
                let addr = Rc::as_ptr(&rc) as usize;
                if marked.contains(&addr) {
                    return true;
                }
                // 结构体字段槽清空为 None，打断环。
                let n = rc.slots.borrow().len();
                *rc.slots.borrow_mut() = vec![Value::None; n];
                removed_addrs.push(addr);
                cleared += 1;
                false
            }
        });
        for addr in removed_addrs {
            self.addrs.remove(&addr);
        }
        // 死弱引用也会让 retain 返回 false；重建地址集以保持一致。
        if self.weaks.len() != self.addrs.len() {
            self.addrs.clear();
            for w in &self.weaks {
                let addr = match w {
                    TrackedWeak::List(w) => w.upgrade().map(|rc| Rc::as_ptr(&rc) as usize),
                    TrackedWeak::Dict(w) => w.upgrade().map(|rc| Rc::as_ptr(&rc) as usize),
                    TrackedWeak::Set(w) => w.upgrade().map(|rc| Rc::as_ptr(&rc) as usize),
                    TrackedWeak::Iter(w) => w.upgrade().map(|rc| Rc::as_ptr(&rc) as usize),
                    TrackedWeak::Cell(w) => w.upgrade().map(|rc| Rc::as_ptr(&rc) as usize),
                    TrackedWeak::Struct(w) => w.upgrade().map(|rc| Rc::as_ptr(&rc) as usize),
                };
                if let Some(a) = addr {
                    self.addrs.insert(a);
                }
            }
        }
        cleared
    }
}

/// 标记单个值的容器（若有），并将其子节点压入工作队列。
/// 用指针地址去重，同一对象不会被重复访问。
pub fn mark_value(v: &Value, marked: &mut FxHashSet<usize>, worklist: &mut Vec<Value>) {
    match v {
        Value::List(rc) => {
            let addr = Rc::as_ptr(rc) as usize;
            if marked.insert(addr) {
                for child in rc.borrow().iter() {
                    worklist.push(child.clone());
                }
            }
        }
        Value::Dict(rc) => {
            let addr = Rc::as_ptr(rc) as usize;
            if marked.insert(addr) {
                for (_k, child) in rc.borrow().iter() {
                    worklist.push(child.clone());
                }
            }
        }
        Value::Set(rc) => {
            let addr = Rc::as_ptr(rc) as usize;
            if marked.insert(addr) {
                for k in rc.borrow().iter() {
                    worklist.push(crate::value::value_key_to_value(k));
                }
            }
        }
        Value::Iterator(rc) => {
            let addr = Rc::as_ptr(rc) as usize;
            if marked.insert(addr) {
                mark_iterator_children(&rc.borrow(), worklist);
            }
        }
        Value::Cell(rc) => {
            let addr = Rc::as_ptr(rc) as usize;
            if marked.insert(addr) {
                worklist.push(rc.borrow().clone());
            }
        }
        Value::Struct(rc) => {
            let addr = Rc::as_ptr(rc) as usize;
            if marked.insert(addr) {
                for child in rc.slots.borrow().iter() {
                    worklist.push(child.clone());
                }
            }
        }
        _ => {}
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
