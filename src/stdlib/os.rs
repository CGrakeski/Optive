//! `std.os`：环境、进程与工作目录。

use crate::value::{DictMap, ModuleObject, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use crate::shared::Shared;

use super::{builtin, expect_text, io_map, submodule};

pub(super) fn build_os_module() -> Shared<ModuleObject> {
    submodule(
        "os",
        &[
            ("getenv", builtin(os_getenv)),
            ("setenv", builtin(os_setenv)),
            ("args", builtin(os_args)),
            ("exit", builtin(os_exit)),
            ("cwd", builtin(os_cwd)),
            ("chdir", builtin(os_chdir)),
            ("name", builtin(os_name)),
            ("run", builtin(os_run)),
            ("capture", builtin(os_capture)),
        ],
    )
}

pub(super) fn os_getenv(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let key = expect_text("getenv", args, 0)?;
    Ok(std::env::var(&key).map_or(Value::None, Value::Text))
}

pub(super) fn os_setenv(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "setenv requires 2 arguments",
        ));
    }
    vm.caps.check_env("setenv")?;
    let key = expect_text("setenv", args, 0)?;
    let val = args[1].print_string();
    std::env::set_var(key, val);
    Ok(Value::None)
}

pub(super) fn os_args(vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    let items: Vec<Value> = if let Some(override_args) = &vm.argv_override {
        override_args.iter().cloned().map(Value::Text).collect()
    } else {
        std::env::args().map(Value::Text).collect()
    };
    Ok(Value::List(Shared::new(items)))
}

pub(super) fn os_exit(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    // 与全局 `exit` 共用同一退出语义。
    crate::builtins::call_exit(vm, args)
}

pub(super) fn os_cwd(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    let cwd = std::env::current_dir().map_err(|e| io_map("cwd failed", e))?;
    Ok(Value::Text(cwd.to_string_lossy().to_string()))
}

pub(super) fn os_chdir(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let p = expect_text("chdir", args, 0)?;
    vm.caps.check_env("chdir")?;
    std::env::set_current_dir(&p).map_err(|e| io_map("chdir failed", e))?;
    Ok(Value::None)
}

pub(super) fn os_name(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    Ok(Value::Text(std::env::consts::OS.to_string()))
}

/// 解析 `run`/`capture` 的命令：`text` 或 `[prog, arg...]`。
pub(super) fn os_parse_cmdline(args: &[Value]) -> Result<(String, Vec<String>)> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "run/capture requires a command (text or list)",
        ));
    }
    match &args[0] {
        Value::Text(cmd) => {
            let extra: Vec<String> = match args.get(1) {
                None => Vec::new(),
                Some(Value::List(list)) => list
                    .borrow()
                    .iter()
                    .map(crate::runtime::value::Value::print_string)
                    .collect(),
                Some(Value::Tuple(t)) => t
                    .iter()
                    .map(crate::runtime::value::Value::print_string)
                    .collect(),
                Some(other) => {
                    return Err(crate::error::RuntimeError::type_err(format!(
                        "run/capture args must be a list, got {}",
                        other.type_name()
                    )))
                }
            };
            Ok((cmd.clone(), extra))
        }
        Value::List(list) => {
            let items = list.borrow();
            if items.is_empty() {
                return Err(crate::error::RuntimeError::value_err(
                    "run/capture command list must be non-empty",
                ));
            }
            let prog = items[0].print_string();
            let rest = items[1..]
                .iter()
                .map(crate::runtime::value::Value::print_string)
                .collect();
            Ok((prog, rest))
        }
        other => Err(crate::error::RuntimeError::type_err(format!(
            "run/capture expects text or list command, got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn os_spawn_result(
    vm: &mut Vm,
    prog: &str,
    args: &[String],
    capture: bool,
) -> Result<Value> {
    vm.caps.check_process("os.run")?;
    vm.request_cooperative_yield();
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args);
    if capture {
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
    }
    let output = if capture {
        cmd.output()
    } else {
        cmd.status().map(|status| std::process::Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
    .map_err(|e| io_map("os.run failed", e))?;
    vm.request_cooperative_yield();
    let code = output
        .status
        .code()
        .unwrap_or(i32::from(!output.status.success()));
    let mut map = DictMap::new();
    map.insert(
        ValueKey::Text("ok".into()),
        Value::Bool(output.status.success()),
    );
    map.insert(
        ValueKey::Text("status".into()),
        Value::Num(Num::Small(i64::from(code))),
    );
    map.insert(
        ValueKey::Text("stdout".into()),
        Value::Text(String::from_utf8_lossy(&output.stdout).into_owned()),
    );
    map.insert(
        ValueKey::Text("stderr".into()),
        Value::Text(String::from_utf8_lossy(&output.stderr).into_owned()),
    );
    Ok(Value::Dict(Shared::new(map)))
}

pub(super) fn os_run(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let (prog, rest) = os_parse_cmdline(args)?;
    os_spawn_result(vm, &prog, &rest, false)
}

pub(super) fn os_capture(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let (prog, rest) = os_parse_cmdline(args)?;
    os_spawn_result(vm, &prog, &rest, true)
}
