//! 稳定窄范围 Rust embedding facade。
//!
//! [`crate::vm::Vm`] 的字段是实现细节，宿主不应依赖。本模块覆盖：
//! 构建 VM、能力、编译/运行、宿主内建、I/O、错误/traceback、取消与协作预算。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::caps::Capabilities;
use crate::error::RuntimeError;
use crate::value::Value;
use crate::vm::{ErrorStackFrame, OutputSink, Vm};
use crate::Result;

/// 当前稳定 embedding API 版本（与 [`crate::versions::EMBED_API_VERSION`] 相同）。
pub const API_VERSION: u16 = crate::versions::EMBED_API_VERSION;

#[derive(Clone)]
pub struct EngineBuilder {
    caps: Capabilities,
    workers: usize,
    suspend_budget: Option<usize>,
    output: Option<OutputSink>,
}

impl Default for EngineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            caps: Capabilities::full(),
            workers: 1,
            suspend_budget: None,
            output: None,
        }
    }

    #[must_use]
    pub fn capabilities(mut self, caps: Capabilities) -> Self {
        self.caps = caps;
        self
    }

    #[must_use]
    pub fn workers(mut self, n: usize) -> Self {
        self.workers = n.max(1);
        self
    }

    #[must_use]
    pub fn suspend_budget(mut self, n: usize) -> Self {
        self.suspend_budget = Some(n);
        self
    }

    #[must_use]
    pub fn output_sink(mut self, sink: OutputSink) -> Self {
        self.output = Some(sink);
        self
    }

    #[must_use]
    pub fn build(self) -> Engine {
        let mut vm = Vm::with_workers(self.workers);
        vm.install_caps(self.caps);
        if let Some(b) = self.suspend_budget {
            vm = vm.with_suspend_budget(b);
        }
        if let Some(sink) = self.output {
            vm.set_output_sink(sink);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        vm.set_host_cancel(cancel.clone());
        Engine { vm, cancel }
    }
}

pub struct Engine {
    vm: Vm,
    cancel: Arc<AtomicBool>,
}

impl Engine {
    #[must_use]
    pub fn builder() -> EngineBuilder {
        EngineBuilder::new()
    }

    #[must_use]
    pub fn new() -> Self {
        EngineBuilder::new().build()
    }

    pub fn eval(&mut self, source: &str) -> Result<Value> {
        self.eval_named(source, "<embed>")
    }

    pub fn eval_named(&mut self, source: &str, file: &str) -> Result<Value> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(RuntimeError::cancelled("host cancelled"));
        }
        crate::run_source_in_vm(&mut self.vm, source, file)
    }

    pub fn register_host(
        &mut self,
        name: &str,
        f: impl Fn(&mut Vm, &[Value]) -> Result<Value> + Send + Sync + 'static,
    ) {
        self.vm
            .globals
            .insert(name.to_string(), Value::builtin(name.to_string(), f));
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn reset_cancel(&self) {
        self.cancel.store(false, Ordering::Release);
    }

    #[must_use]
    pub fn traceback(&mut self) -> Vec<ErrorStackFrame> {
        self.vm.take_error_stack()
    }

    #[must_use]
    pub fn last_line(&self) -> usize {
        self.vm.current_line()
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_and_host_builtin() {
        let mut eng = Engine::new();
        eng.register_host("twice", |_vm, args| {
            let n = args[0].clone();
            Ok(n)
        });
        let v = eng.eval("1 + 2").unwrap();
        assert_eq!(v.display_string(), "3");
    }

    #[test]
    fn cancel_before_run() {
        let mut eng = Engine::new();
        eng.cancel();
        let err = eng.eval("1").unwrap_err();
        assert!(err.message().contains("cancel"), "{err}");
    }
}
