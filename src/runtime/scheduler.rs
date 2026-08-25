//! M:N work-stealing 调度核心。
//!
//! - `OPTIVE_WORKERS`：OS worker 数；未设置或 `1` 时保持 M:1 协作语义（现有测试顺序不变）。
//! - `>1` 时额外线程从全局 Injector / 彼此 Stealer 取任务，真正并行跑纤程。
//! - 阻塞等待（channel/mutex）在无就绪任务时 `Condvar` 休眠，由 `notify` 唤醒。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use parking_lot::{Condvar, Mutex};
use rustc_hash::FxHashMap;

use crate::shared::{Shared, WeakShared};
use crate::value::{TaskInner, Value};

/// 挂起纤程快照存于此（跨 worker 迁移）。
pub(crate) type FiberStore = Mutex<FxHashMap<usize, crate::vm::TaskFiber>>;

pub struct MnScheduler {
    pub(crate) injector: Injector<Shared<TaskInner>>,
    /// 构造 worker 时登记的 stealer；主线程 local worker 的 stealer 也在内。
    stealers: Mutex<Vec<Stealer<Shared<TaskInner>>>>,
    pub(crate) fibers: FiberStore,
    wake_mu: Mutex<()>,
    wake_cv: Condvar,
    pub shutdown: AtomicBool,
    /// 配置的 worker 总数（含主线程）；helper 全部启动失败时可收缩为 1。
    worker_count: AtomicUsize,
    /// 正在执行任务的线程数（取任务 +1 / 跑完 -1）；M:N 死锁检测用。
    busy_count: AtomicUsize,
    /// 已启动的辅助线程数。
    helpers_started: AtomicUsize,
    /// helper 线程成功认领并开跑的任务次数（诊断 M:N 是否真在干活）。
    helper_runs: AtomicUsize,
    /// 主线程发布的脚本全局槽快照；helper 任务开始时拷到本地，避免热路径抢 SharedMap。
    script_snap: Mutex<(Vec<String>, Vec<Value>)>,
    /// GC stop-the-world：请求置位后各 worker 在安全点停住。
    pub(crate) stw_requested: AtomicBool,
    stw_parked: AtomicUsize,
    /// STW 超时次数（可观测性）。
    stw_failures: AtomicUsize,
    /// Helper 请求 primary 做一次 GC（避免双端同时 `begin_stw` 死锁）。
    gc_requested: AtomicBool,
    /// Helper 在 STW 安全点发布的根快照（按线程 id）。
    pub(crate) parked_roots: Mutex<FxHashMap<usize, Vec<Value>>>,
    /// 曾入队的任务弱引用（injector 不可遍历时的根补充）。
    /// 按任务指针去重：挂起后重入队是热路径，绝不能每次 push 一条 Weak。
    pub(crate) scheduled_tasks: Mutex<FxHashMap<usize, WeakShared<TaskInner>>>,
}

/// `wait_brief` 单次等待时长。
const WAIT_BRIEF_MS: u64 = 2;

impl MnScheduler {
    #[must_use]
    pub fn new(worker_count: usize) -> Arc<Self> {
        Arc::new(Self {
            injector: Injector::new(),
            stealers: Mutex::new(Vec::new()),
            fibers: Mutex::new(FxHashMap::default()),
            wake_mu: Mutex::new(()),
            wake_cv: Condvar::new(),
            shutdown: AtomicBool::new(false),
            worker_count: AtomicUsize::new(worker_count.max(1)),
            busy_count: AtomicUsize::new(0),
            helpers_started: AtomicUsize::new(0),
            helper_runs: AtomicUsize::new(0),
            script_snap: Mutex::new((Vec::new(), Vec::new())),
            stw_requested: AtomicBool::new(false),
            stw_parked: AtomicUsize::new(0),
            stw_failures: AtomicUsize::new(0),
            gc_requested: AtomicBool::new(false),
            parked_roots: Mutex::new(FxHashMap::default()),
            scheduled_tasks: Mutex::new(FxHashMap::default()),
        })
    }

    pub fn request_gc(&self) {
        self.gc_requested.store(true, Ordering::Release);
        self.notify_one();
    }

    #[inline]
    pub fn gc_request_pending(&self) -> bool {
        self.gc_requested.load(Ordering::Relaxed)
    }

    /// 若有挂起的 GC 请求则清除并返回 true。
    pub fn take_gc_request(&self) -> bool {
        self.gc_requested.swap(false, Ordering::AcqRel)
    }

    /// 解释循环 / 阻塞等待处调用：若 GC 请求 STW 则停在此直到结束。
    pub fn poll_safepoint(&self) {
        self.poll_safepoint_with_roots(None);
    }

    /// 携带可选根快照进入 STW 停车（helper 应用此路径）。
    pub fn poll_safepoint_with_roots(&self, roots: Option<Vec<Value>>) {
        if !self.stw_requested.load(Ordering::Acquire) {
            return;
        }
        if let Some(roots) = roots {
            let tid = thread_id_key();
            self.parked_roots.lock().insert(tid, roots);
        }
        self.stw_parked.fetch_add(1, Ordering::Release);
        self.notify_all();
        while self.stw_requested.load(Ordering::Acquire) {
            if self.is_shutdown() {
                break;
            }
            std::thread::yield_now();
        }
        self.stw_parked.fetch_sub(1, Ordering::Release);
        let tid = thread_id_key();
        self.parked_roots.lock().remove(&tid);
    }

    /// 取出所有已停车 worker 发布的根（STW 期间由主线程调用）。
    pub fn take_parked_roots(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for roots in self.parked_roots.lock().values() {
            out.extend(roots.iter().cloned());
        }
        out
    }

    pub fn note_scheduled_task(&self, task: &Shared<TaskInner>) {
        let key = task.as_ptr() as usize;
        let mut v = self.scheduled_tasks.lock();
        if let Some(w) = v.get(&key) {
            if w.upgrade().is_some() {
                return;
            }
        }
        v.insert(key, task.downgrade());
        // 偶尔修剪已结束任务的死弱引用
        if v.len() > 4096 {
            v.retain(|_, w| w.upgrade().is_some());
        }
    }

    pub fn scheduled_task_values(&self) -> Vec<Value> {
        let mut v = self.scheduled_tasks.lock();
        v.retain(|_, w| w.upgrade().is_some());
        v.values()
            .filter_map(|w| w.upgrade().map(Value::Task))
            .collect()
    }

    /// 主线程 GC：拦住其它 worker。成功返回 `true`；超时则取消 STW 并返回 `false`。
    pub fn begin_stw(&self) -> bool {
        if self.worker_count.load(Ordering::Acquire) <= 1 {
            return true;
        }
        self.stw_requested.store(true, Ordering::Release);
        self.notify_all();
        let need = self.helpers_started.load(Ordering::Acquire);
        let timeout = stw_timeout();
        let start = std::time::Instant::now();
        while self.stw_parked.load(Ordering::Acquire) < need {
            if start.elapsed() > timeout {
                self.stw_requested.store(false, Ordering::Release);
                self.notify_all();
                return false;
            }
            std::thread::yield_now();
        }
        true
    }

    /// 记录一次 STW 超时，返回累计失败次数。
    pub fn note_stw_failure(&self) -> usize {
        self.stw_failures.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn end_stw(&self) {
        self.stw_requested.store(false, Ordering::Release);
        self.notify_all();
    }

    pub fn register_stealer(&self, stealer: Stealer<Shared<TaskInner>>) {
        self.stealers.lock().push(stealer);
    }

    pub fn push_task(&self, task: Shared<TaskInner>) {
        self.note_scheduled_task(&task);
        self.injector.push(task);
        // 一次 `go` 可能对应多个空闲 helper；`notify_one` 会漏叫醒。
        self.notify_all();
    }

    pub fn notify_one(&self) {
        self.wake_cv.notify_one();
    }

    pub fn notify_all(&self) {
        self.wake_cv.notify_all();
    }

    /// 无本地/全局任务时短等；有任务或关机则返回。
    pub fn wait_brief(&self) {
        self.poll_safepoint();
        let mut guard = self.wake_mu.lock();
        self.wake_cv
            .wait_for(&mut guard, Duration::from_millis(WAIT_BRIEF_MS));
    }

    /// 成功取到一个任务（随 `scheduler_run_task` 配对 `note_task_done`）。
    pub fn note_task_taken(&self) {
        self.busy_count.fetch_add(1, Ordering::AcqRel);
    }

    pub fn note_task_done(&self) {
        self.busy_count.fetch_sub(1, Ordering::AcqRel);
    }

    /// 正在执行任务的线程数。
    pub fn busy(&self) -> usize {
        self.busy_count.load(Ordering::Acquire)
    }

    /// 全局 injector 与所有已注册 stealer 队列均为空。
    pub fn queues_empty(&self) -> bool {
        if !self.injector.is_empty() {
            return false;
        }
        let stealers = self.stealers.lock();
        stealers.iter().all(crossbeam_deque::Stealer::is_empty)
    }

    pub fn worker_count(&self) -> usize {
        self.worker_count.load(Ordering::Acquire)
    }

    /// 全部 helper 启动失败时收缩为单 worker（回退 M:1 语义）。
    pub fn shrink_to_single(&self) {
        self.worker_count.store(1, Ordering::Release);
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.notify_all();
    }

    /// 从 local worker → injector → 其他 stealer 取一个任务。
    pub fn steal_task(&self, local: &Worker<Shared<TaskInner>>) -> Option<Shared<TaskInner>> {
        if let Some(t) = local.pop() {
            return Some(t);
        }
        loop {
            match self.injector.steal() {
                Steal::Success(t) => return Some(t),
                Steal::Empty => break,
                Steal::Retry => continue,
            }
        }
        let stealers = self.stealers.lock().clone();
        for s in &stealers {
            loop {
                match s.steal() {
                    Steal::Success(t) => return Some(t),
                    Steal::Empty => break,
                    Steal::Retry => continue,
                }
            }
        }
        None
    }

    pub fn mark_helper_started(&self) {
        self.helpers_started.fetch_add(1, Ordering::Release);
    }

    pub fn note_helper_run(&self) {
        self.helper_runs.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn helper_runs(&self) -> usize {
        self.helper_runs.load(Ordering::Relaxed)
    }

    pub fn publish_script_globals(&self, names: Vec<String>, vals: Vec<Value>) {
        *self.script_snap.lock() = (names, vals);
    }

    #[must_use]
    pub fn snapshot_script_globals(&self) -> (Vec<String>, Vec<Value>) {
        self.script_snap.lock().clone()
    }
}

/// 默认 1（M:1）；`OPTIVE_WORKERS` 可覆盖。`mn` feature 下也可用 `0` 表示 `num_cpus`。
#[must_use]
pub fn configured_workers() -> usize {
    match std::env::var("OPTIVE_WORKERS") {
        Ok(s) => {
            let n: usize = s.parse().unwrap_or(1);
            if n == 0 {
                num_cpus::get().max(1)
            } else {
                n
            }
        }
        Err(_) => 1,
    }
}

#[must_use]
pub fn new_local_worker() -> Worker<Shared<TaskInner>> {
    Worker::new_fifo()
}

fn thread_id_key() -> usize {
    // 稳定且足够唯一：用线程 id 的哈希。
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    std::thread::current().id().hash(&mut h);
    h.finish() as usize
}

fn stw_timeout() -> Duration {
    static CACHED: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        let ms = std::env::var("OPTIVE_STW_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n: &u64| n > 0)
            .unwrap_or(2_000);
        Duration::from_millis(ms)
    })
}
