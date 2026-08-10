//! FFI 卸荷线程池：阻塞型 `extern` 不占用 Optive OS worker。
//!
//! `OPTIVE_FFI_THREADS=0`（默认）关闭；>0 时任务纤程内的 call 提交到本池，
//! 经 `block_suspend` 挂起纤程，完成后由调度器重试 Builtin 取回结果。
//! 卸荷路径不设置 `active_vm`，同步回调会失败（首版有意禁止）。

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, OnceLock};
use std::thread;

use parking_lot::Mutex;

use crate::error::RuntimeError;
use crate::ffi::{
    invoke_native_call_sampled, ArgStorage, FfiCallable, RetStorage, AbiType,
};
use crate::gc::SharedGc;
use crate::Result;

pub(crate) type FfiPendingResult = std::result::Result<(RetStorage, i32), String>;

pub struct FfiPending {
    pub(crate) result: Mutex<Option<FfiPendingResult>>,
}

impl FfiPending {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(None),
        })
    }

    pub(crate) fn try_take(&self) -> Option<FfiPendingResult> {
        self.result.lock().take()
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.result.lock().is_some()
    }
}

struct Job {
    ffi: Arc<FfiCallable>,
    storage: Vec<ArgStorage>,
    ret_abi: AbiType,
    use_serial: bool,
    pending: Arc<FfiPending>,
    /// 卸荷线程安装，使 `ffi_enter/leave` 落到提交方的 `SharedGc`。
    gc: Arc<SharedGc>,
}

struct PoolState {
    tx: Mutex<Option<Sender<Job>>>,
    threads: usize,
}

static POOL: OnceLock<Mutex<PoolState>> = OnceLock::new();

fn pool_state() -> &'static Mutex<PoolState> {
    POOL.get_or_init(|| {
        Mutex::new(PoolState {
            tx: Mutex::new(None),
            threads: 0,
        })
    })
}

fn ensure_workers(n: usize) -> Result<()> {
    if n == 0 {
        return Ok(());
    }
    let state = pool_state();
    let mut guard = state.lock();
    if guard.threads >= n && guard.tx.lock().is_some() {
        return Ok(());
    }
    let (tx, rx) = mpsc::channel::<Job>();
    let rx = Arc::new(Mutex::new(rx));
    let mut spawned = 0usize;
    for i in 0..n {
        let rx = rx.clone();
        let result = thread::Builder::new()
            .name(format!("optive-ffi-{i}"))
            .spawn(move || {
                loop {
                    let job = {
                        let rx = rx.lock();
                        rx.recv()
                    };
                    let Ok(mut job) = job else {
                        break;
                    };
                    crate::gc::install_current_gc(job.gc.clone());
                    let out = invoke_native_call_sampled(
                        &job.ffi,
                        &mut job.storage,
                        job.ret_abi,
                        job.use_serial,
                    );
                    crate::gc::clear_current_gc();
                    let packed = out.map_err(|e| e.message().to_string());
                    *job.pending.result.lock() = Some(packed);
                    // 纤程已在 ready 队列自旋重试；唤醒可能在 wait_brief 的 worker。
                    // 通知走全局调度器较难：依赖任务已 enqueue + worker 轮询。
                }
            });
        match result {
            Ok(_) => spawned += 1,
            Err(e) => {
                // 一个都起不来才是致命错误；部分成功则降级继续。
                if spawned == 0 {
                    return Err(RuntimeError::msg(format!(
                        "failed to spawn FFI pool worker: {e}"
                    )));
                }
                eprintln!("optive: 仅 {spawned}/{n} 个 FFI 池线程启动成功：{e}");
                break;
            }
        }
    }
    *guard.tx.lock() = Some(tx);
    guard.threads = spawned;
    Ok(())
}

/// 按 `OPTIVE_FFI_THREADS` / 测试覆盖调整池大小（可增不可热缩）。
pub(crate) fn resize_pool(n: usize) -> Result<()> {
    ensure_workers(n)
}

/// 提交一次原生调用；调用方应随后 `block_suspend` 并在重试时 `try_take`。
pub(crate) fn submit_call(
    ffi: Arc<FfiCallable>,
    storage: Vec<ArgStorage>,
    ret_abi: AbiType,
    use_serial: bool,
    threads: usize,
    gc: Arc<SharedGc>,
) -> Result<Arc<FfiPending>> {
    if threads == 0 {
        return Err(RuntimeError::msg(
            "internal: FFI offload requested but ffi_threads=0",
        ));
    }
    ensure_workers(threads)?;
    let pending = FfiPending::new();
    let job = Job {
        ffi,
        storage,
        ret_abi,
        use_serial,
        pending: pending.clone(),
        gc,
    };
    let state = pool_state().lock();
    let tx_guard = state.tx.lock();
    let Some(tx) = tx_guard.as_ref() else {
        return Err(RuntimeError::msg("internal: FFI pool not started"));
    };
    tx.send(job).map_err(|_| RuntimeError::msg("FFI pool worker gone"))?;
    Ok(pending)
}
