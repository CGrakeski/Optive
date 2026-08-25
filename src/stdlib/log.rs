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

fn log_debug(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    emit(vm, LV_DEBUG, "DEBUG", args)
}
fn log_info(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    emit(vm, LV_INFO, "INFO", args)
}
fn log_warn(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    emit(vm, LV_WARN, "WARN", args)
}
fn log_error(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    emit(vm, LV_ERROR, "ERROR", args)
}

fn emit(vm: &Vm, min: u8, tag: &str, args: &[Value]) -> Result<Value> {
    if current_level() <= min {
        let msg = args
            .iter()
            .map(Value::display_string)
            .collect::<Vec<_>>()
            .join(" ");
        vm.write_output(
            crate::vm::OutputStream::Stderr,
            &format!("{} {tag} {msg}\n", utc_stamp()),
        );
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
    let (y, mo, d, hh, mm, ss) = super::time::utc_parts_from_secs(secs as i64);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}
