//! `std.log`：级别日志，写 stderr。

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::shared::Shared;
use crate::value::{ModuleObject, Value};
use crate::vm::Vm;
use crate::Result;

use super::{expect_arity, expect_text, named_builtin, submodule};

const LV_DEBUG: u8 = 0;
const LV_INFO: u8 = 1;
const LV_WARN: u8 = 2;
const LV_ERROR: u8 = 3;
const LV_UNSET: u8 = 255;

static LEVEL: AtomicU8 = AtomicU8::new(LV_UNSET);

pub(super) fn build_log_module() -> Shared<ModuleObject> {
    submodule(
        "log",
        &[
            ("debug", named_builtin("debug", log_debug)),
            ("info", named_builtin("info", log_info)),
            ("warn", named_builtin("warn", log_warn)),
            ("error", named_builtin("error", log_error)),
            ("set_level", named_builtin("set_level", log_set_level)),
            ("get_level", named_builtin("get_level", log_get_level)),
        ],
    )
}

fn current_level() -> u8 {
    let v = LEVEL.load(Ordering::Relaxed);
    if v != LV_UNSET {
        return v;
    }
    let from_env = std::env::var("OPTIVE_LOG")
        .ok()
        .and_then(|s| parse_level(&s))
        .unwrap_or(LV_INFO);
    LEVEL.store(from_env, Ordering::Relaxed);
    from_env
}

fn parse_level(s: &str) -> Option<u8> {
    match s.trim().to_ascii_lowercase().as_str() {
        "debug" => Some(LV_DEBUG),
        "info" => Some(LV_INFO),
        "warn" | "warning" => Some(LV_WARN),
        "error" => Some(LV_ERROR),
        _ => None,
    }
}

fn level_name(lv: u8) -> &'static str {
    match lv {
        LV_DEBUG => "debug",
        LV_INFO => "info",
        LV_WARN => "warn",
        _ => "error",
    }
}

fn log_debug(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    emit(LV_DEBUG, "DEBUG", args)
}
fn log_info(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    emit(LV_INFO, "INFO", args)
}
fn log_warn(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    emit(LV_WARN, "WARN", args)
}
fn log_error(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    emit(LV_ERROR, "ERROR", args)
}

fn emit(min: u8, tag: &str, args: &[Value]) -> Result<Value> {
    if current_level() <= min {
        let msg = args
            .iter()
            .map(Value::display_string)
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("{} {tag} {msg}", utc_stamp());
    }
    Ok(Value::None)
}

fn log_set_level(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("set_level", args, 1)?;
    let name = expect_text("set_level", args, 0)?;
    let lv = parse_level(&name).ok_or_else(|| {
        crate::error::RuntimeError::value_err(format!(
            "set_level: unknown level `{name}` (debug|info|warn|error)"
        ))
    })?;
    LEVEL.store(lv, Ordering::Relaxed);
    Ok(Value::None)
}

fn log_get_level(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    Ok(Value::Text(level_name(current_level()).into()))
}

fn utc_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, hh, mm, ss) = unix_to_utc(secs);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn unix_to_utc(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let tod = secs % 86400;
    let hh = (tod / 3600) as u32;
    let mm = ((tod % 3600) / 60) as u32;
    let ss = (tod % 60) as u32;
    let days = (secs / 86400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = (y + i64::from(mo <= 2)) as i32;
    (y, mo, d, hh, mm, ss)
}
