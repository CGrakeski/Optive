//! `std.io`：文件与行 I/O。

use std::sync::Arc;

use crate::value::Value;
use crate::vm::Vm;
use crate::Result;

use super::expect_text;

pub(super) fn io_read_file(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "read_file requires 1 argument",
        ));
    }
    let path = expect_text("read_file", args, 0)?;
    let content = vm.caps.read_to_string("read_file", &path)?;
    vm.request_cooperative_yield();
    Ok(Value::Text(content))
}

pub(super) fn io_write_file(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "write_file requires 2 arguments",
        ));
    }
    let path = expect_text("write_file", args, 0)?;
    let content = args[1].print_string();
    vm.caps.write("write_file", &path, content)?;
    vm.request_cooperative_yield();
    Ok(Value::None)
}

pub(super) fn io_append_file(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "append_file requires 2 arguments",
        ));
    }
    let path = expect_text("append_file", args, 0)?;
    let content = args[1].print_string();
    vm.caps.append("append_file", &path, content.as_bytes())?;
    vm.request_cooperative_yield();
    Ok(Value::None)
}

pub(super) fn io_read_bytes(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "read_bytes requires 1 argument",
        ));
    }
    let path = expect_text("read_bytes", args, 0)?;
    let bytes = vm.caps.read("read_bytes", &path)?;
    vm.request_cooperative_yield();
    Ok(Value::Bytes(Arc::new(bytes)))
}

pub(super) fn io_write_bytes(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "write_bytes requires 2 arguments",
        ));
    }
    let path = expect_text("write_bytes", args, 0)?;
    let bytes = match &args[1] {
        Value::Bytes(b) => b.as_ref().clone(),
        Value::Text(s) => s.as_bytes().to_vec(),
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "write_bytes: content must be bytes or text",
            ))
        }
    };
    vm.caps.write("write_bytes", &path, bytes)?;
    vm.request_cooperative_yield();
    Ok(Value::None)
}

pub(super) fn io_write_line(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let out = crate::value::args_join_space(args);
    vm.write_output(crate::vm::OutputStream::Stdout, &format!("{out}\n"));
    Ok(Value::None)
}

pub(super) fn io_eprint(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let out = crate::value::args_join_space(args);
    vm.write_output(crate::vm::OutputStream::Stderr, &format!("{out}\n"));
    Ok(Value::None)
}

pub(super) fn io_read_line(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let prompt = if args.is_empty() {
        String::new()
    } else {
        args[0].print_string()
    };
    crate::builtins::read_line_with_prompt(vm, &prompt)
}
