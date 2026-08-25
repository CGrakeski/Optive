//! `std.format`：字符串格式化。

use crate::value::Value;
use crate::vm::Vm;
use crate::Result;

use super::{expect_int, expect_num_f64, expect_text, value_to_list, DEFAULT_NUM_PRECISION};

pub(super) fn format_format(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(crate::error::RuntimeError::type_err(
            "format requires at least 1 argument",
        ));
    }
    let tmpl = expect_text("format", args, 0)?;
    let values = &args[1..];
    let mut result = String::new();
    let chars: Vec<char> = tmpl.chars().collect();
    let mut i = 0;
    let mut auto_idx = 0usize;
    while i < chars.len() {
        if chars[i] == '{' {
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                result.push('{');
                i += 2;
                continue;
            }
            let close = chars[i + 1..]
                .iter()
                .position(|&c| c == '}')
                .map(|p| i + 1 + p)
                .ok_or_else(|| crate::error::RuntimeError::value_err("format: unmatched '{'"))?;
            let inner: String = chars[i + 1..close].iter().collect();
            let idx = if inner.is_empty() {
                let n = auto_idx;
                auto_idx += 1;
                n
            } else {
                inner.parse::<usize>().map_err(|_| {
                    crate::error::RuntimeError::value_err(format!(
                        "format: invalid field {{{inner}}}"
                    ))
                })?
            };
            let v = values.get(idx).ok_or_else(|| {
                crate::error::RuntimeError::value_err(format!("format: missing argument {{{idx}}}"))
            })?;
            result.push_str(&v.print_string());
            i = close + 1;
        } else if chars[i] == '}' {
            if i + 1 < chars.len() && chars[i + 1] == '}' {
                result.push('}');
                i += 2;
                continue;
            }
            return Err(crate::error::RuntimeError::value_err(
                "format: unmatched '}'",
            ));
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }
    Ok(Value::Text(result))
}

pub(super) fn format_join(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "join requires 2 arguments",
        ));
    }
    let sep = expect_text("join", args, 0)?;
    let items = value_to_list(&args[1])?;
    let parts: Vec<String> = items
        .iter()
        .map(crate::runtime::value::Value::print_string)
        .collect();
    Ok(Value::Text(parts.join(&sep)))
}

pub(super) fn format_format_num(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(crate::error::RuntimeError::type_err(
            "format_num requires 1 or 2 arguments (n[, prec])",
        ));
    }
    let x = expect_num_f64("format_num", args, 0)?;
    let prec = if args.len() == 2 {
        expect_int("format_num", args, 1)?.max(0) as usize
    } else {
        DEFAULT_NUM_PRECISION
    };
    Ok(Value::Text(format!("{x:.prec$}")))
}

pub(super) fn format_pad(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 || args.len() > 3 {
        return Err(crate::error::RuntimeError::type_err(
            "pad requires 2 or 3 arguments (s, width[, fill])",
        ));
    }
    let s = expect_text("pad", args, 0)?;
    let width = expect_int("pad", args, 1)?.max(0) as usize;
    let fill = if args.len() == 3 {
        let f = expect_text("pad", args, 2)?;
        f.chars().next().unwrap_or(' ')
    } else {
        ' '
    };
    if s.chars().count() >= width {
        return Ok(Value::Text(s));
    }
    let pad_len = width - s.chars().count();
    let mut out = String::with_capacity(width);
    for _ in 0..pad_len {
        out.push(fill);
    }
    out.push_str(&s);
    Ok(Value::Text(out))
}

pub(super) fn format_indent(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "indent requires 2 arguments (text, n)",
        ));
    }
    let text = expect_text("indent", args, 0)?;
    let n = expect_int("indent", args, 1)?.max(0) as usize;
    let pad = " ".repeat(n);
    let trailing_nl = text.ends_with('\n');
    let out: Vec<String> = text
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{pad}{line}")
            }
        })
        .collect();
    let mut joined = out.join("\n");
    if trailing_nl {
        joined.push('\n');
    }
    Ok(Value::Text(joined))
}
