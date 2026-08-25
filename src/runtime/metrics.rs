//! 运行时热点采样（opcode / 调用 / 分配 / GC）。不进入 `dispatch_hot_u8`。
//!
//! `OPTIVE_METRICS=1` 时在协作预算耗尽的慢路径采样。报告走 stderr 或 [`Metrics::report`]。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::shared::Shared;
use crate::vm::Vm;

static ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Default, Clone)]
pub struct Metrics {
    pub samples: u64,
    pub opcodes: BTreeMap<String, u64>,
    pub calls: BTreeMap<String, u64>,
    pub gc_collects: usize,
    pub gc_cleared: usize,
    pub gc_stw_ns: u64,
}

impl Metrics {
    #[must_use]
    pub fn report(&self) -> String {
        let mut ops: Vec<_> = self.opcodes.iter().collect();
        ops.sort_by(|a, b| b.1.cmp(a.1));
        let mut calls: Vec<_> = self.calls.iter().collect();
        calls.sort_by(|a, b| b.1.cmp(a.1));
        let top_ops: Vec<String> = ops
            .into_iter()
            .take(8)
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let top_calls: Vec<String> = calls
            .into_iter()
            .take(8)
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        format!(
            "samples={} gc_collects={} gc_cleared={} stw_ns={} opcodes[{}] calls[{}]",
            self.samples,
            self.gc_collects,
            self.gc_cleared,
            self.gc_stw_ns,
            top_ops.join(","),
            top_calls.join(",")
        )
    }
}

#[must_use]
pub fn env_enabled() -> bool {
    match std::env::var("OPTIVE_METRICS") {
        Ok(v) => {
            let t = v.trim();
            !(t.is_empty()
                || t == "0"
                || t.eq_ignore_ascii_case("off")
                || t.eq_ignore_ascii_case("false"))
        }
        Err(_) => ENABLED.load(Ordering::Relaxed),
    }
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn attach(vm: &mut Vm) {
    vm.metrics = Some(Shared::new(Metrics::default()));
    vm.metrics_active = true;
}

pub fn sample(vm: &Vm) {
    let Some(m) = &vm.metrics else {
        return;
    };
    let mut st = m.borrow_mut();
    st.samples += 1;
    if let Some(op) = vm.code.get(vm.pc) {
        let name = format!("{op:?}");
        let short = name.split('(').next().unwrap_or(&name);
        *st.opcodes.entry(short.to_string()).or_default() += 1;
    }
    if let Some(f) = vm.func_stack.last() {
        *st.calls.entry(f.name.clone()).or_default() += 1;
    }
    st.gc_collects = vm.gc.total_collects.load(Ordering::Relaxed);
    st.gc_cleared = vm.gc.total_cleared.load(Ordering::Relaxed);
    st.gc_stw_ns = vm.gc.total_stw_ns.load(Ordering::Relaxed);
}

pub fn take_report(vm: &Vm) -> Option<String> {
    vm.metrics.as_ref().map(|m| m.borrow().report())
}
