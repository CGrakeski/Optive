use super::{builtin, expect_function, expect_num_f64, expect_text, submodule, value_to_list};
use crate::shared::Shared;
use crate::value::{ModuleObject, Value};
use crate::vm::Vm;
use crate::Result;

pub(super) fn build_test_module() -> Shared<ModuleObject> {
    submodule(
        "test",
        &[
            ("assert_eq", builtin(test_assert_eq)),
            ("assert_true", builtin(test_assert_true)),
            ("assert_false", builtin(test_assert_false)),
            ("assert_ne", builtin(test_assert_ne)),
            ("assert_contains", builtin(test_assert_contains)),
            ("assert_approx", builtin(test_assert_approx)),
            ("assert_raises", builtin(test_assert_raises)),
            ("each", builtin(test_each)),
            ("tmp_dir", builtin(test_tmp_dir)),
        ],
    )
}

fn assertion_error(vm: &mut Vm, message: String) -> Result<Value> {
    let exc = crate::exceptions::make_exception(vm, "AssertionError", message)?;
    vm.throw_value(exc)?;
    Ok(Value::None)
}

fn test_assert_false(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "assert_false requires 1 argument",
        ));
    }
    if args[0].is_truthy() {
        return assertion_error(vm, format!("expected falsy value, got {}", args[0]));
    }
    Ok(Value::None)
}

fn test_assert_ne(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "assert_ne requires 2 arguments",
        ));
    }
    if args[0].print_string() == args[1].print_string() {
        return assertion_error(vm, format!("{} == {}", args[0], args[1]));
    }
    Ok(Value::None)
}

fn test_assert_contains(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "assert_contains requires (container, value)",
        ));
    }
    let found = match &args[0] {
        Value::Text(text) => text.contains(&args[1].print_string()),
        other => value_to_list(other)?
            .iter()
            .any(|v| v.print_string() == args[1].print_string()),
    };
    if !found {
        return assertion_error(vm, format!("{} does not contain {}", args[0], args[1]));
    }
    Ok(Value::None)
}

fn test_assert_approx(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !(2..=3).contains(&args.len()) {
        return Err(crate::error::RuntimeError::type_err(
            "assert_approx requires (actual, expected, tolerance?)",
        ));
    }
    let actual = expect_num_f64("assert_approx", args, 0)?;
    let expected = expect_num_f64("assert_approx", args, 1)?;
    let tolerance = if let Some(value) = args.get(2) {
        match value {
            Value::Num(n) => n.to_f64_checked().map_err(|_| {
                crate::error::RuntimeError::value_err("tolerance is outside f64 range")
            })?,
            _ => {
                return Err(crate::error::RuntimeError::type_err(
                    "tolerance must be numeric",
                ))
            }
        }
    } else {
        1e-9
    };
    if tolerance < 0.0 {
        return Err(crate::error::RuntimeError::value_err(
            "tolerance must be non-negative",
        ));
    }
    if (actual - expected).abs() > tolerance {
        return assertion_error(
            vm,
            format!("{actual} is not within {tolerance} of {expected}"),
        );
    }
    Ok(Value::None)
}

fn test_each(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err(
            "each requires 3 arguments (name, rows, fn)",
        ));
    }
    let name = expect_text("each", args, 0)?;
    let rows = value_to_list(&args[1])?;
    let func = expect_function("each", args, 2)?;
    let mut fails: Vec<String> = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        match vm.call_user_function_catching(func.clone(), vec![row])? {
            Ok(_) => vm.test_case_log.push(format!("{name}[{i}] ok")),
            Err(thrown) => {
                if crate::exceptions::struct_is_a(vm, &thrown, "AssertionError") {
                    let msg = thrown.display_string();
                    let detail = format!("{name}[{i}]: {msg}");
                    vm.test_case_log.push(format!("{name}[{i}] FAILED: {msg}"));
                    fails.push(detail);
                } else {
                    vm.throw_value(thrown)?;
                }
            }
        }
    }
    if !fails.is_empty() {
        let exc = crate::exceptions::make_exception(
            vm,
            "AssertionError",
            format!(
                "each `{name}`: {} row(s) failed: {}",
                fails.len(),
                fails.join("; ")
            ),
        )?;
        vm.throw_value(exc)?;
    }
    Ok(Value::None)
}

fn test_tmp_dir(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "tmp_dir takes no arguments",
        ));
    }
    let n = vm.test_tmp_dirs.len();
    let root = vm
        .caps
        .writable_root()
        .ok_or_else(|| crate::error::RuntimeError::io_err("tmp_dir: no writable sandbox root"))?;
    let dir = root
        .join(".optive")
        .join("tmp")
        .join(format!("optive_test_{}_{n}", std::process::id()));
    vm.caps.create_dir("tmp_dir", &dir, true)?;
    let dir = vm
        .caps
        .resolve_fs_path("tmp_dir display", &dir, crate::caps::FsAccess::Read)?;
    #[cfg(windows)]
    let path = super::strip_windows_extended_prefix(&dir.to_string_lossy()).replace('\\', "/");
    #[cfg(not(windows))]
    let path = dir.display().to_string();
    vm.test_tmp_dirs.push(dir);
    Ok(Value::Text(path))
}

fn test_assert_eq(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "assert_eq requires 2 arguments",
        ));
    }
    if args[0].print_string() != args[1].print_string() {
        let exc = crate::exceptions::make_exception(
            vm,
            "AssertionError",
            format!("{} != {}", args[0].print_string(), args[1].print_string()),
        )?;
        vm.throw_value(exc)?;
    }
    Ok(Value::None)
}

fn test_assert_true(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "assert_true requires 1 argument",
        ));
    }
    if !args[0].is_truthy() {
        let exc = crate::exceptions::make_exception(vm, "AssertionError", "assertion failed")?;
        vm.throw_value(exc)?;
    }
    Ok(Value::None)
}

fn test_assert_raises(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "assert_raises requires 2 arguments (function, exception_type)",
        ));
    }
    let func = expect_function("assert_raises", args, 0)?;
    let exc_type = match args[1].as_type_name_operand() {
        Some(name) => name.to_string(),
        None => match &args[1] {
            Value::TypeSpec(spec) => spec.name.clone(),
            other => other.type_name_string(),
        },
    };
    match vm.call_user_function_catching(func, vec![])? {
        Ok(returned) => {
            let exc = crate::exceptions::make_exception(
                vm,
                "AssertionError",
                format!("expected {exc_type}, but no exception was raised (got {returned})"),
            )?;
            vm.throw_value(exc)?;
            Ok(Value::None)
        }
        Err(thrown) => {
            if !crate::exceptions::struct_is_a(vm, &thrown, &exc_type) {
                let exc = crate::exceptions::make_exception(
                    vm,
                    "AssertionError",
                    format!("expected {}, got {}", exc_type, thrown.type_name()),
                )?;
                vm.throw_value(exc)?;
            }
            Ok(Value::None)
        }
    }
}
