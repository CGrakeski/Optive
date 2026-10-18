//! Blocking native work only. Jobs never receive a VM or GC-managed Values.
//! Language fibers retain their arguments and build Values on resumption.

use std::any::Any;
use std::sync::{mpsc, Arc, OnceLock};

use parking_lot::Mutex;

use crate::{Result, RuntimeError};

type Job = Box<dyn FnOnce() + Send>;
type NativeResult = Result<Box<dyn Any + Send>>;

pub(crate) struct Pending {
    result: Mutex<Option<NativeResult>>,
}

impl Pending {
    pub(crate) fn take<T: Send + 'static>(&self) -> Option<Result<T>> {
        self.result.lock().take().map(|result| {
            result.and_then(|value| {
                value.downcast::<T>().map(|value| *value).map_err(|_| {
                    RuntimeError::msg("internal error: native wait result type mismatch")
                })
            })
        })
    }
}

fn pool() -> Result<&'static mpsc::SyncSender<Job>> {
    static POOL: OnceLock<std::result::Result<mpsc::SyncSender<Job>, String>> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::env::var("OPTIVE_IO_THREADS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(8)
            .clamp(1, 128);
        let (sender, receiver) = mpsc::sync_channel::<Job>(256);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..threads {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("optive-io-{index}"))
                .spawn(move || loop {
                    let job = receiver.lock().recv();
                    match job {
                        Ok(job) => job(),
                        Err(_) => break,
                    }
                })
                .map_err(|error| format!("cannot start I/O worker: {error}"))?;
        }
        Ok(sender)
    })
    .as_ref()
    .map_err(|error| RuntimeError::io_err(error.clone()))
}

pub(crate) fn submit<T: Send + 'static>(
    job: impl FnOnce() -> Result<T> + Send + 'static,
    wake: impl FnOnce() + Send + 'static,
) -> Result<Arc<Pending>> {
    let pending = Arc::new(Pending {
        result: Mutex::new(None),
    });
    let output = pending.clone();
    pool()?
        .try_send(Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                .unwrap_or_else(|_| Err(RuntimeError::io_err("native I/O worker panicked")))
                .map(|value| Box::new(value) as Box<dyn Any + Send>);
            *output.result.lock() = Some(result);
            wake();
        }))
        .map_err(|error| RuntimeError::io_err(format!("cannot submit native I/O: {error}")))?;
    Ok(pending)
}
