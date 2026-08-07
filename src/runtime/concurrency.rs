//! Channel / Mutex / RWMutex / WaitGroup / Semaphore / Once / Barrier / Cond
//! 构造与方法绑定。


use crate::error::RuntimeError;
use crate::shared::Shared;
use crate::value::{
    expect_i64, ChannelInner, MutexInner, Num, SyncGuardInner, SyncInner, Value,
};
use crate::Result;
use std::sync::Arc;

/// 创建方法的辅助宏：克隆捕获变量并包装为 `Value::Builtin`。
/// 使用方式：`method!(捕获变量, vm_param, { 闭包体 })`
macro_rules! method {
    ($cap:ident, $vm:ident, |$arg:ident| $body:block) => {{
        let $cap = $cap.clone();
        Ok(Value::Builtin(Arc::new(move |$vm: &mut crate::vm::Vm, $arg: &[Value]| $body)))
    }};
}

pub fn get_channel_method(ch: &Shared<ChannelInner>, field: &str) -> Result<Value> {
    match field {
        "send" => method!(ch, vm, |args| {
            if args.len() != 1 {
                return Err(RuntimeError::type_err("Channel.send requires 1 argument"));
            }
            vm.channel_send(&ch, args[0].clone())?;
            Ok(Value::None)
        }),
        "recv" => method!(ch, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("Channel.recv takes no arguments"));
            }
            vm.channel_recv(&ch)
        }),
        "close" => method!(ch, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("Channel.close takes no arguments"));
            }
            ch.borrow_mut().closed = true;
            vm.mn.notify_all();
            Ok(Value::None)
        }),
        _ => Err(RuntimeError::attr_err(format!(
            "Channel has no method {field}"
        ))),
    }
}

pub fn get_mutex_method(m: &Shared<MutexInner>, field: &str) -> Result<Value> {
    match field {
        "lock" => method!(m, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("Mutex.lock takes no arguments"));
            }
            vm.mutex_lock(&m)
        }),
        _ => Err(RuntimeError::attr_err(format!(
            "Mutex has no method {field}"
        ))),
    }
}

pub fn get_mutex_guard_method(g: &Shared<crate::value::MutexGuardInner>, field: &str) -> Result<Value> {
    match field {
        "get" => method!(g, _vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("MutexGuard.get takes no arguments"));
            }
            Ok(g.borrow().mutex().borrow().value.clone())
        }),
        "set" => method!(g, _vm, |args| {
            if args.len() != 1 {
                return Err(RuntimeError::type_err("MutexGuard.set requires 1 argument"));
            }
            g.borrow().mutex().borrow_mut().value = args[0].clone();
            Ok(Value::None)
        }),
        "__enter__" => method!(g, _vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("__enter__ takes no arguments"));
            }
            Ok(Value::MutexGuard(g.clone()))
        }),
        "__exit__" => method!(g, vm, |args| {
            let _ = args;
            g.borrow().release();
            vm.mn.notify_all();
            Ok(Value::None)
        }),
        "unlock" => method!(g, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err(
                    "MutexGuard.unlock takes no arguments",
                ));
            }
            g.borrow().release();
            vm.mn.notify_all();
            Ok(Value::None)
        }),
        _ => Err(RuntimeError::attr_err(format!(
            "MutexGuard has no method {field}"
        ))),
    }
}

pub fn get_sync_method(s: &Shared<SyncInner>, field: &str) -> Result<Value> {
    let kind = match &*s.borrow() {
        SyncInner::RWMutex { .. } => "RWMutex",
        SyncInner::WaitGroup { .. } => "WaitGroup",
        SyncInner::Semaphore { .. } => "Semaphore",
        SyncInner::Once { .. } => "Once",
        SyncInner::Barrier { .. } => "Barrier",
        SyncInner::Cond { .. } => "Cond",
        SyncInner::TaskGroup { .. } => "TaskGroup",
        SyncInner::TimeoutCtx { .. } => "TimeoutCtx",
    };
    match (kind, field) {
        // --- RWMutex ---
        ("RWMutex", "read") => method!(s, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("RWMutex.read takes no arguments"));
            }
            vm.rwmutex_read(&s)
        }),
        ("RWMutex", "write") => method!(s, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("RWMutex.write takes no arguments"));
            }
            vm.rwmutex_write(&s)
        }),
        // --- WaitGroup ---
        ("WaitGroup", "add") => method!(s, _vm, |args| {
            if args.len() != 1 {
                return Err(RuntimeError::type_err("WaitGroup.add requires 1 argument"));
            }
            let n = expect_i64("WaitGroup.add", &args[0])?;
            match &mut *s.borrow_mut() {
                SyncInner::WaitGroup { count } => {
                    *count = count.saturating_add(n);
                    if *count < 0 {
                        *count = 0;
                        return Err(RuntimeError::value_err(
                            "WaitGroup.add: count became negative",
                        ));
                    }
                }
                _ => unreachable!(),
            }
            Ok(Value::None)
        }),
        ("WaitGroup", "done") => method!(s, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("WaitGroup.done takes no arguments"));
            }
            let hit_zero = match &mut *s.borrow_mut() {
                SyncInner::WaitGroup { count } => {
                    if *count <= 0 {
                        return Err(RuntimeError::value_err(
                            "WaitGroup.done: count already zero",
                        ));
                    }
                    *count -= 1;
                    *count == 0
                }
                _ => unreachable!(),
            };
            if hit_zero {
                vm.mn.notify_all();
            }
            Ok(Value::None)
        }),
        ("WaitGroup", "wait") => method!(s, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("WaitGroup.wait takes no arguments"));
            }
            vm.waitgroup_wait(&s)
        }),
        // --- Semaphore ---
        ("Semaphore", "acquire") => method!(s, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err(
                    "Semaphore.acquire takes no arguments",
                ));
            }
            vm.semaphore_acquire(&s)
        }),
        ("Semaphore", "release") => method!(s, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err(
                    "Semaphore.release takes no arguments",
                ));
            }
            match &mut *s.borrow_mut() {
                SyncInner::Semaphore { permits } => *permits += 1,
                _ => unreachable!(),
            }
            vm.mn.notify_all();
            Ok(Value::None)
        }),
        // --- Once ---
        ("Once", "run") | ("Once", "do") => method!(s, vm, |args| {
            if args.len() != 1 {
                return Err(RuntimeError::type_err(
                    "Once.run requires 1 callable argument",
                ));
            }
            vm.once_do(&s, args[0].clone())
        }),
        // --- Barrier ---
        ("Barrier", "wait") => method!(s, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("Barrier.wait takes no arguments"));
            }
            vm.barrier_wait(&s)
        }),
        // --- Cond ---
        ("Cond", "wait") => method!(s, vm, |args| {
            // Cond.wait(mutex_guard) — 释放 Mutex 再等待，唤醒后重新加锁
            if args.len() != 1 {
                return Err(RuntimeError::type_err(
                    "Cond.wait requires 1 MutexGuard argument",
                ));
            }
            let Value::MutexGuard(guard) = &args[0] else {
                return Err(RuntimeError::type_err(
                    "Cond.wait expects a MutexGuard",
                ));
            };
            vm.cond_wait(&s, guard)
        }),
        ("Cond", "signal") => method!(s, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("Cond.signal takes no arguments"));
            }
            match &mut *s.borrow_mut() {
                SyncInner::Cond { signals, waiters } => {
                    if *waiters > 0 {
                        *signals += 1;
                    }
                }
                _ => unreachable!(),
            }
            vm.mn.notify_all();
            Ok(Value::None)
        }),
        ("Cond", "broadcast") => method!(s, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err(
                    "Cond.broadcast takes no arguments",
                ));
            }
            match &mut *s.borrow_mut() {
                SyncInner::Cond { signals, waiters } => {
                    *signals += *waiters;
                }
                _ => unreachable!(),
            }
            vm.mn.notify_all();
            Ok(Value::None)
        }),
        // --- TaskGroup ---
        ("TaskGroup", "__enter__") => method!(s, _vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("__enter__ takes no arguments"));
            }
            Ok(Value::Sync(s.clone()))
        }),
        ("TaskGroup", "__exit__") => method!(s, vm, |args| {
            let _ = args;
            vm.taskgroup_wait(&s)
        }),
        ("TaskGroup", "run") => method!(s, vm, |args| {
            if args.len() != 1 {
                return Err(RuntimeError::type_err(
                    "TaskGroup.run requires 1 callable argument",
                ));
            }
            vm.taskgroup_run(&s, args[0].clone())
        }),
        ("TaskGroup", "wait") => method!(s, vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("TaskGroup.wait takes no arguments"));
            }
            vm.taskgroup_wait(&s)
        }),
        // --- TimeoutCtx ---
        ("TimeoutCtx", "__enter__") => method!(s, _vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("__enter__ takes no arguments"));
            }
            Ok(Value::Sync(s.clone()))
        }),
        ("TimeoutCtx", "__exit__") => method!(s, _vm, |args| {
            let _ = (&s, args);
            Ok(Value::None)
        }),
        ("TimeoutCtx", "expired") | ("TimeoutCtx", "cancelled") => method!(s, _vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err(
                    "TimeoutCtx.expired takes no arguments",
                ));
            }
            match &*s.borrow() {
                SyncInner::TimeoutCtx { deadline } => {
                    Ok(Value::Bool(std::time::Instant::now() >= *deadline))
                }
                _ => unreachable!(),
            }
        }),
        _ => Err(RuntimeError::attr_err(format!(
            "{kind} has no method {field}"
        ))),
    }
}

pub fn get_sync_guard_method(g: &Shared<SyncGuardInner>, field: &str) -> Result<Value> {
    match field {
        "get" => method!(g, _vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err(
                    "RWMutexGuard.get takes no arguments",
                ));
            }
            let mu = {
                let guard = g.borrow();
                match &*guard {
                    SyncGuardInner::Read { mu } | SyncGuardInner::Write { mu } => mu.clone(),
                }
            };
            let inner = mu.borrow();
            match &*inner {
                SyncInner::RWMutex { value, .. } => Ok(value.clone()),
                _ => Err(RuntimeError::msg("internal: bad RWMutex guard")),
            }
        }),
        "set" => method!(g, _vm, |args| {
            if args.len() != 1 {
                return Err(RuntimeError::type_err(
                    "RWMutexGuard.set requires 1 argument",
                ));
            }
            let mu = {
                let guard = g.borrow();
                match &*guard {
                    SyncGuardInner::Write { mu } => mu.clone(),
                    SyncGuardInner::Read { .. } => {
                        return Err(RuntimeError::type_err(
                            "RWMutexReadGuard is read-only; use write()",
                        ));
                    }
                }
            };
            let mut inner = mu.borrow_mut();
            match &mut *inner {
                SyncInner::RWMutex { value, .. } => {
                    *value = args[0].clone();
                    Ok(Value::None)
                }
                _ => Err(RuntimeError::msg("internal: bad RWMutex guard")),
            }
        }),
        "__enter__" => method!(g, _vm, |args| {
            if !args.is_empty() {
                return Err(RuntimeError::type_err("__enter__ takes no arguments"));
            }
            Ok(Value::SyncGuard(g.clone()))
        }),
        "__exit__" => method!(g, vm, |args| {
            let _ = args;
            let (mu, is_write) = {
                let guard = g.borrow();
                match &*guard {
                    SyncGuardInner::Read { mu } => (mu.clone(), false),
                    SyncGuardInner::Write { mu } => (mu.clone(), true),
                }
            };
            let mut inner = mu.borrow_mut();
            if let SyncInner::RWMutex {
                readers, writer, ..
            } = &mut *inner
            {
                if is_write {
                    *writer = false;
                } else if *readers > 0 {
                    *readers -= 1;
                }
            }
            drop(inner);
            vm.mn.notify_all();
            Ok(Value::None)
        }),
        _ => Err(RuntimeError::attr_err(format!(
            "RWMutexGuard has no method {field}"
        ))),
    }
}

pub fn construct_channel(args: &[Value]) -> Result<Value> {
    let capacity = match args {
        [] => None,
        [Value::Num(n)] => {
            let v = n.to_i64().ok_or_else(|| {
                RuntimeError::type_err("Channel capacity must be a non-negative integer")
            })?;
            if v < 0 {
                return Err(RuntimeError::type_err(
                    "Channel capacity must be a non-negative integer",
                ));
            }
            Some(v as usize)
        }
        _ => {
            return Err(RuntimeError::type_err(
                "Channel() takes 0 or 1 numeric capacity argument",
            ))
        }
    };
    Ok(Value::Channel(Shared::new(ChannelInner::new(
        capacity,
    ))))
}

pub fn construct_mutex(args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err("Mutex() requires 1 argument"));
    }
    Ok(Value::Mutex(Shared::new(MutexInner::new(
        args[0].clone(),
    ))))
}

pub fn construct_rwmutex(args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err("RWMutex() requires 1 argument"));
    }
    Ok(Value::Sync(Shared::new(SyncInner::RWMutex {
        value: args[0].clone(),
        readers: 0,
        writer: false,
    })))
}

pub fn construct_waitgroup(args: &[Value]) -> Result<Value> {
    let count = match args {
        [] => 0i64,
        [Value::Num(n)] => n
            .to_i64()
            .ok_or_else(|| RuntimeError::type_err("WaitGroup count must be an integer"))?,
        _ => {
            return Err(RuntimeError::type_err(
                "WaitGroup() takes 0 or 1 numeric argument",
            ))
        }
    };
    if count < 0 {
        return Err(RuntimeError::value_err(
            "WaitGroup count must be non-negative",
        ));
    }
    Ok(Value::Sync(Shared::new(SyncInner::WaitGroup {
        count,
    })))
}

pub fn construct_semaphore(args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(
            "Semaphore() requires 1 numeric argument",
        ));
    }
    let permits = expect_i64("Semaphore", &args[0])?;
    if permits < 0 {
        return Err(RuntimeError::value_err(
            "Semaphore permits must be non-negative",
        ));
    }
    Ok(Value::Sync(Shared::new(SyncInner::Semaphore {
        permits,
    })))
}

pub fn construct_once(args: &[Value]) -> Result<Value> {
    if !args.is_empty() {
        return Err(RuntimeError::type_err("Once() takes no arguments"));
    }
    Ok(Value::Sync(Shared::new(SyncInner::Once {
        phase: crate::value::OncePhase::Idle,
        value: Value::None,
    })))
}

pub fn construct_barrier(args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(
            "Barrier() requires 1 numeric party count",
        ));
    }
    let n = expect_i64("Barrier", &args[0])?;
    if n <= 0 {
        return Err(RuntimeError::value_err(
            "Barrier party count must be positive",
        ));
    }
    Ok(Value::Sync(Shared::new(SyncInner::Barrier {
        n,
        waiting: 0,
        generation: 0,
    })))
}

pub fn construct_cond(args: &[Value]) -> Result<Value> {
    if !args.is_empty() {
        return Err(RuntimeError::type_err("Cond() takes no arguments"));
    }
    Ok(Value::Sync(Shared::new(SyncInner::Cond {
        signals: 0,
        waiters: 0,
    })))
}

pub fn construct_taskgroup(args: &[Value]) -> Result<Value> {
    if !args.is_empty() {
        return Err(RuntimeError::type_err("taskgroup() takes no arguments"));
    }
    Ok(Value::Sync(Shared::new(SyncInner::TaskGroup {
        count: 0,
        first_error: None,
    })))
}

pub fn construct_timeout_ctx(args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(RuntimeError::type_err(
            "with_timeout() requires 1 numeric seconds argument",
        ));
    }
    let secs = match &args[0] {
        Value::Num(n) => n.to_f64_checked()?,
        _ => {
            return Err(RuntimeError::type_err(
                "with_timeout() expects num seconds",
            ))
        }
    };
    if !secs.is_finite() {
        return Err(RuntimeError::type_err(
            "with_timeout() expects a finite number of seconds",
        ));
    }
    // 负数视为 0；过大有限值钳到可构造 Duration / Instant 的上界。
    let secs = secs.max(0.0).min(MAX_TIMEOUT_SECS);
    let dur = std::time::Duration::from_secs_f64(secs);
    let now = std::time::Instant::now();
    let deadline = now.checked_add(dur).unwrap_or(now);
    Ok(Value::Sync(Shared::new(SyncInner::TimeoutCtx { deadline })))
}

/// `from_secs_f64` / `Instant` 加法的安全上界（约 100 年）。
pub(crate) const MAX_TIMEOUT_SECS: f64 = 86400.0 * 365.0 * 100.0;

/// deadline 毫秒值的安全转换：整数直取；超大浮点钳到 i64 端点而非回绕成 0。
fn num_deadline_ms(n: &Num) -> Result<i64> {
    if let Some(v) = n.to_i64() {
        return Ok(v);
    }
    let f = n.to_f64_checked().map_err(|_| {
        RuntimeError::type_err("sleep deadline expects a numeric millisecond value")
    })?;
    if f.is_nan() {
        return Err(RuntimeError::type_err("sleep deadline must not be NaN"));
    }
    Ok(f.clamp(i64::MIN as f64, i64::MAX as f64) as i64)
}

pub fn deadline_from_secs(secs: &Value) -> Result<Value> {
    let secs_f = match secs {
        Value::Num(n) => n.to_f64_checked()?,
        _ => {
            return Err(RuntimeError::type_err(
                "sleep/deadline expects num seconds",
            ))
        }
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let add_ms = (secs_f * 1000.0).round() as i64;
    Ok(Value::Num(Num::Small(now_ms.saturating_add(add_ms))))
}

pub fn poll_deadline_ready(deadline: &Value) -> Result<bool> {
    let target = match deadline {
        Value::Num(n) => num_deadline_ms(n)?,
        _ => {
            return Err(RuntimeError::type_err(
                "select sleep deadline expects num milliseconds",
            ))
        }
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    Ok(now_ms >= target)
}

/// select 空转：睡到最近截止时间（单次最多 `cap_ms`），便于与通道 case 交错轮询。
pub fn sleep_until_nearest_deadline(deadlines: &[Value], cap_ms: u64) -> Result<()> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let mut min_remain: Option<u64> = None;
    for d in deadlines {
        let Value::Num(n) = d else {
            continue;
        };
        let target = num_deadline_ms(n)?;
        if now_ms >= target {
            return Ok(());
        }
        let remain = (target - now_ms) as u64;
        min_remain = Some(min_remain.map_or(remain, |m| m.min(remain)));
    }
    if let Some(remain) = min_remain {
        let slice = remain.min(cap_ms).max(1);
        std::thread::sleep(std::time::Duration::from_millis(slice));
    }
    Ok(())
}
