//! `std.text`：文本处理（含类型桥接与扩展函数）。

use std::collections::HashMap;
use std::sync::Arc;

use crate::shared::Shared;
use crate::value::{ModuleObject, Num, Value};
use crate::vm::Vm;
use crate::Result;

use super::{builtin, expect_arity, expect_int, expect_text, submodule, value_to_list};

pub(super) fn build_text_module() -> Shared<ModuleObject> {
    let entries: &[(&str, Value)] = &[
        // 类型 / 构造
        ("Text", Value::type_ref("text")),
        ("Bytes", Value::type_ref("bytes")),
        ("Builder", builtin(text_builder_new)),
        // 基础 API（原 std.text）
        ("upper", builtin(text_upper)),
        ("lower", builtin(text_lower)),
        ("strip", builtin(text_strip)),
        ("split", builtin(text_split)),
        ("contains", builtin(text_contains)),
        ("len", builtin(text_len)),
        ("replace", builtin(text_replace)),
        ("startswith", builtin(text_startswith)),
        ("endswith", builtin(text_endswith)),
        ("find", builtin(text_find)),
        ("join", builtin(text_join)),
        ("repeat", builtin(text_repeat)),
        ("count", builtin(text_count)),
        ("lines", builtin(text_lines)),
        ("is_digit", builtin(text_is_digit)),
        ("is_alpha", builtin(text_is_alpha)),
        ("is_space", builtin(text_is_space)),
        ("ord", builtin(text_ord)),
        ("chr", builtin(text_chr)),
        // 扩展
        ("lstrip", builtin(text_lstrip)),
        ("rstrip", builtin(text_rstrip)),
        ("trim", builtin(text_strip)),
        ("trim_start", builtin(text_lstrip)),
        ("trim_end", builtin(text_rstrip)),
        ("rsplit", builtin(text_rsplit)),
        ("split_once", builtin(text_split_once)),
        ("rsplit_once", builtin(text_rsplit_once)),
        ("split_ws", builtin(text_split_ws)),
        ("partition", builtin(text_partition)),
        ("rpartition", builtin(text_rpartition)),
        ("ljust", builtin(text_ljust)),
        ("rjust", builtin(text_rjust)),
        ("center", builtin(text_center)),
        ("zfill", builtin(text_zfill)),
        ("title", builtin(text_title)),
        ("capitalize", builtin(text_capitalize)),
        ("swapcase", builtin(text_swapcase)),
        ("removeprefix", builtin(text_removeprefix)),
        ("removesuffix", builtin(text_removesuffix)),
        ("is_alnum", builtin(text_is_alnum)),
        ("is_lower", builtin(text_is_lower)),
        ("is_upper", builtin(text_is_upper)),
        ("is_ascii", builtin(text_is_ascii)),
        ("is_empty", builtin(text_is_empty)),
        ("substring", builtin(text_substring)),
        ("slice", builtin(text_substring)),
        ("reverse", builtin(text_reverse)),
        ("chars", builtin(text_chars)),
        ("codepoints", builtin(text_codepoints)),
        ("from_chars", builtin(text_from_chars)),
        ("from_codepoints", builtin(text_from_codepoints)),
        ("to_bytes", builtin(text_to_bytes)),
        ("from_bytes", builtin(text_from_bytes)),
        ("byte_len", builtin(text_byte_len)),
        ("rfind", builtin(text_rfind)),
        ("replace_n", builtin(text_replace_n)),
        ("cmp", builtin(text_cmp)),
    ];

    submodule("text", entries)
}

fn text_upper(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Text(expect_text("upper", args, 0)?.to_uppercase()))
}

fn text_lower(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Text(expect_text("lower", args, 0)?.to_lowercase()))
}

fn text_strip(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Text(
        expect_text("strip", args, 0)?.trim().to_string(),
    ))
}

/// `split(s)` → 按 `" "`；`split(s, sep)`；`split(s, sep, maxsplit)`（maxsplit≥0）。
/// 按空白折叠拆分请用 `split_ws`。
fn text_split(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() || args.len() > 3 {
        return Err(crate::error::RuntimeError::type_err(
            "split requires 1 to 3 arguments (s[, sep[, maxsplit]])",
        ));
    }
    let s = expect_text("split", args, 0)?;
    let sep = if args.len() >= 2 {
        expect_text("split", args, 1)?
    } else {
        " ".into()
    };
    if sep.is_empty() {
        return Err(crate::error::RuntimeError::value_err(
            "split: empty separator",
        ));
    }
    let maxsplit = if args.len() == 3 {
        expect_int("split", args, 2)?
    } else {
        -1
    };
    let parts: Vec<Value> = if maxsplit < 0 {
        s.split(&sep).map(|p| Value::Text(p.to_string())).collect()
    } else {
        s.splitn(maxsplit as usize + 1, &sep)
            .map(|p| Value::Text(p.to_string()))
            .collect()
    };
    Ok(Value::List(Shared::new(parts)))
}

fn text_contains(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "contains requires 2 arguments",
        ));
    }
    let hay = expect_text("contains", args, 0)?;
    let needle = args[1].print_string();
    Ok(Value::Bool(hay.contains(&needle)))
}

fn text_len(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Num(Num::Small(
        expect_text("len", args, 0)?.chars().count() as i64,
    )))
}

fn text_replace(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(crate::error::RuntimeError::type_err(
            "replace requires 3 arguments",
        ));
    }
    let s = expect_text("replace", args, 0)?;
    let from = expect_text("replace", args, 1)?;
    let to = expect_text("replace", args, 2)?;
    Ok(Value::Text(s.replace(&from, &to)))
}

fn text_startswith(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    text_affix_check("startswith", args, |s, affix| s.starts_with(affix))
}

fn text_endswith(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    text_affix_check("endswith", args, |s, affix| s.ends_with(affix))
}

fn text_affix_check(
    name: &str,
    args: &[Value],
    check: impl FnOnce(&str, &str) -> bool,
) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(format!(
            "{name} requires 2 arguments"
        )));
    }
    let s = expect_text(name, args, 0)?;
    let affix = expect_text(name, args, 1)?;
    Ok(Value::Bool(check(&s, &affix)))
}

fn text_find(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "find requires 2 arguments",
        ));
    }
    let s = expect_text("find", args, 0)?;
    let needle = expect_text("find", args, 1)?;
    Ok(match s.find(&needle) {
        Some(i) => Value::Num(Num::Small(s[..i].chars().count() as i64)),
        None => Value::Num(Num::Small(-1)),
    })
}

fn text_join(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "join requires 2 arguments",
        ));
    }
    let sep = expect_text("join", args, 0)?;
    let parts = value_to_list(&args[1])?;
    let joined = parts
        .iter()
        .map(Value::print_string)
        .collect::<Vec<_>>()
        .join(&sep);
    Ok(Value::Text(joined))
}

fn text_repeat(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "repeat requires 2 arguments",
        ));
    }
    let s = expect_text("repeat", args, 0)?;
    let n = expect_int("repeat", args, 1)?.max(0) as usize;
    Ok(Value::Text(s.repeat(n)))
}

macro_rules! define_text_char_preds {
    ($(($fn_name:ident, $api:literal, $pred:expr)),+ $(,)?) => {
        $(
            fn $fn_name(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
                text_char_predicate($api, args, $pred)
            }
        )+
    };
}

define_text_char_preds! {
    (text_is_digit, "is_digit", |c| c.is_ascii_digit()),
    (text_is_alpha, "is_alpha", |c| c.is_ascii_alphabetic()),
    (text_is_space, "is_space", char::is_whitespace),
}

fn text_char_predicate(name: &str, args: &[Value], pred: impl Fn(char) -> bool) -> Result<Value> {
    let s = expect_text(name, args, 0)?;
    Ok(Value::Bool(!s.is_empty() && s.chars().all(pred)))
}

fn text_count(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "count requires 2 arguments (s, sub)",
        ));
    }
    let s = expect_text("count", args, 0)?;
    let sub = expect_text("count", args, 1)?;
    if sub.is_empty() {
        return Ok(Value::Num(Num::Small((s.chars().count() + 1) as i64)));
    }
    Ok(Value::Num(Num::Small(s.matches(&sub).count() as i64)))
}

fn text_lines(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("lines", args, 0)?;
    let lines: Vec<Value> = s.lines().map(|l| Value::Text(l.to_string())).collect();
    Ok(Value::List(Shared::new(lines)))
}

fn text_ord(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("ord", args, 0)?;
    let ch = s
        .chars()
        .next()
        .ok_or_else(|| crate::error::RuntimeError::type_err("ord requires a non-empty text"))?;
    Ok(Value::Num(Num::Small(i64::from(ch as u32))))
}

fn text_chr(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let n = expect_int("chr", args, 0)?;
    if !(0..=0x0010_FFFF).contains(&n) {
        return Err(crate::error::RuntimeError::value_err(
            "chr code point out of range",
        ));
    }
    let Some(ch) = char::from_u32(n as u32) else {
        return Err(crate::error::RuntimeError::value_err(
            "chr invalid code point",
        ));
    };
    Ok(Value::Text(ch.to_string()))
}

fn text_lstrip(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Text(
        expect_text("lstrip", args, 0)?.trim_start().to_string(),
    ))
}

fn text_rstrip(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Text(
        expect_text("rstrip", args, 0)?.trim_end().to_string(),
    ))
}

fn text_split_ws(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("split_ws", args, 0)?;
    let parts: Vec<Value> = s
        .split_whitespace()
        .map(|p| Value::Text(p.to_string()))
        .collect();
    Ok(Value::List(Shared::new(parts)))
}

fn text_rsplit(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 || args.len() > 3 {
        return Err(crate::error::RuntimeError::type_err(
            "rsplit requires 2 or 3 arguments (s, sep[, maxsplit])",
        ));
    }
    let s = expect_text("rsplit", args, 0)?;
    let sep = expect_text("rsplit", args, 1)?;
    if sep.is_empty() {
        return Err(crate::error::RuntimeError::value_err(
            "rsplit: empty separator",
        ));
    }
    let maxsplit = if args.len() == 3 {
        expect_int("rsplit", args, 2)?
    } else {
        -1
    };
    let parts: Vec<Value> = if maxsplit < 0 {
        s.rsplit(&sep).map(|p| Value::Text(p.to_string())).collect()
    } else {
        s.rsplitn(maxsplit as usize + 1, &sep)
            .map(|p| Value::Text(p.to_string()))
            .collect()
    };
    let parts: Vec<Value> = parts.into_iter().rev().collect();
    Ok(Value::List(Shared::new(parts)))
}

fn text_split_once(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("split_once", args, 2)?;
    let s = expect_text("split_once", args, 0)?;
    let sep = expect_text("split_once", args, 1)?;
    match s.split_once(&sep) {
        Some((a, b)) => Ok(Value::Tuple(Arc::from(vec![
            Value::Text(a.to_string()),
            Value::Text(b.to_string()),
        ]))),
        None => Ok(Value::None),
    }
}

fn text_rsplit_once(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("rsplit_once", args, 2)?;
    let s = expect_text("rsplit_once", args, 0)?;
    let sep = expect_text("rsplit_once", args, 1)?;
    match s.rsplit_once(&sep) {
        Some((a, b)) => Ok(Value::Tuple(Arc::from(vec![
            Value::Text(a.to_string()),
            Value::Text(b.to_string()),
        ]))),
        None => Ok(Value::None),
    }
}

fn text_partition(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("partition", args, 2)?;
    let s = expect_text("partition", args, 0)?;
    let sep = expect_text("partition", args, 1)?;
    let (a, b, c) = match s.split_once(&sep) {
        Some((left, right)) => (left, sep.as_str(), right),
        None => (s.as_str(), "", ""),
    };
    Ok(Value::Tuple(Arc::from(vec![
        Value::Text(a.to_string()),
        Value::Text(b.to_string()),
        Value::Text(c.to_string()),
    ])))
}

fn text_rpartition(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("rpartition", args, 2)?;
    let s = expect_text("rpartition", args, 0)?;
    let sep = expect_text("rpartition", args, 1)?;
    let (a, b, c) = match s.rsplit_once(&sep) {
        Some((left, right)) => (left, sep.as_str(), right),
        None => ("", "", s.as_str()),
    };
    Ok(Value::Tuple(Arc::from(vec![
        Value::Text(a.to_string()),
        Value::Text(b.to_string()),
        Value::Text(c.to_string()),
    ])))
}

fn pad_with(
    name: &str,
    args: &[Value],
    place: impl FnOnce(&str, usize, char) -> String,
) -> Result<Value> {
    if args.len() < 2 || args.len() > 3 {
        return Err(crate::error::RuntimeError::type_err(format!(
            "{name} requires 2 or 3 arguments (s, width[, fill])"
        )));
    }
    let s = expect_text(name, args, 0)?;
    let width = expect_int(name, args, 1)?.max(0) as usize;
    let fill = if args.len() == 3 {
        let f = expect_text(name, args, 2)?;
        f.chars().next().ok_or_else(|| {
            crate::error::RuntimeError::value_err(format!("{name}: fill must be non-empty"))
        })?
    } else {
        ' '
    };
    Ok(Value::Text(place(&s, width, fill)))
}

fn text_ljust(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    pad_with("ljust", args, |s, width, fill| {
        let n = s.chars().count();
        if n >= width {
            s.to_string()
        } else {
            format!("{s}{}", fill.to_string().repeat(width - n))
        }
    })
}

fn text_rjust(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    pad_with("rjust", args, |s, width, fill| {
        let n = s.chars().count();
        if n >= width {
            s.to_string()
        } else {
            format!("{}{s}", fill.to_string().repeat(width - n))
        }
    })
}

fn text_center(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    pad_with("center", args, |s, width, fill| {
        let n = s.chars().count();
        if n >= width {
            return s.to_string();
        }
        let pad = width - n;
        let left = pad / 2;
        let right = pad - left;
        format!(
            "{}{s}{}",
            fill.to_string().repeat(left),
            fill.to_string().repeat(right)
        )
    })
}

fn text_zfill(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("zfill", args, 2)?;
    let s = expect_text("zfill", args, 0)?;
    let width = expect_int("zfill", args, 1)?.max(0) as usize;
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= width {
        return Ok(Value::Text(s));
    }
    let (sign, body) = if matches!(chars.first(), Some('+' | '-')) {
        (chars[0].to_string(), chars[1..].iter().collect::<String>())
    } else {
        (String::new(), s.clone())
    };
    let body_len = body.chars().count();
    let zeros = width.saturating_sub(sign.chars().count() + body_len);
    Ok(Value::Text(format!("{sign}{}{body}", "0".repeat(zeros))))
}

fn text_title(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("title", args, 0)?;
    let mut out = String::with_capacity(s.len());
    let mut new_word = true;
    for c in s.chars() {
        if c.is_alphabetic() {
            if new_word {
                out.extend(c.to_uppercase());
                new_word = false;
            } else {
                out.extend(c.to_lowercase());
            }
        } else {
            out.push(c);
            new_word = true;
        }
    }
    Ok(Value::Text(out))
}

fn text_capitalize(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("capitalize", args, 0)?;
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return Ok(Value::Text(String::new()));
    };
    let mut out: String = first.to_uppercase().collect();
    out.extend(chars.flat_map(|c| c.to_lowercase()));
    Ok(Value::Text(out))
}

fn text_swapcase(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("swapcase", args, 0)?;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_lowercase() {
            out.extend(c.to_uppercase());
        } else if c.is_uppercase() {
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    Ok(Value::Text(out))
}

fn text_removeprefix(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("removeprefix", args, 2)?;
    let s = expect_text("removeprefix", args, 0)?;
    let prefix = expect_text("removeprefix", args, 1)?;
    Ok(Value::Text(
        s.strip_prefix(&prefix).unwrap_or(&s).to_string(),
    ))
}

fn text_removesuffix(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("removesuffix", args, 2)?;
    let s = expect_text("removesuffix", args, 0)?;
    let suffix = expect_text("removesuffix", args, 1)?;
    Ok(Value::Text(
        s.strip_suffix(&suffix).unwrap_or(&s).to_string(),
    ))
}

fn text_is_alnum(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    text_char_predicate("is_alnum", args, |c| c.is_ascii_alphanumeric())
}

fn text_is_lower(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("is_lower", args, 0)?;
    Ok(Value::Bool(
        !s.is_empty() && s.chars().any(|c| c.is_alphabetic()) && s == s.to_lowercase(),
    ))
}

fn text_is_upper(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("is_upper", args, 0)?;
    Ok(Value::Bool(
        !s.is_empty() && s.chars().any(|c| c.is_alphabetic()) && s == s.to_uppercase(),
    ))
}

fn text_is_ascii(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Bool(expect_text("is_ascii", args, 0)?.is_ascii()))
}

fn text_is_empty(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Bool(expect_text("is_empty", args, 0)?.is_empty()))
}

fn normalize_char_index(idx: i64, len: usize) -> Result<usize> {
    let len_i = len as i64;
    let i = if idx < 0 { len_i + idx } else { idx };
    if i < 0 || i > len_i {
        return Err(crate::error::RuntimeError::index_err(format!(
            "string index out of range: {idx}"
        )));
    }
    Ok(i as usize)
}

fn text_substring(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() < 2 || args.len() > 3 {
        return Err(crate::error::RuntimeError::type_err(
            "substring requires 2 or 3 arguments (s, start[, end])",
        ));
    }
    let s = expect_text("substring", args, 0)?;
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = normalize_char_index(expect_int("substring", args, 1)?, len)?;
    let end = if args.len() == 3 {
        normalize_char_index(expect_int("substring", args, 2)?, len)?
    } else {
        len
    };
    if start > end {
        return Ok(Value::Text(String::new()));
    }
    Ok(Value::Text(chars[start..end].iter().collect()))
}

fn text_reverse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("reverse", args, 0)?;
    Ok(Value::Text(s.chars().rev().collect()))
}

fn text_chars(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("chars", args, 0)?;
    let chars: Vec<Value> = s.chars().map(|c| Value::Text(c.to_string())).collect();
    Ok(Value::List(Shared::new(chars)))
}

fn text_codepoints(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("codepoints", args, 0)?;
    let cps: Vec<Value> = s
        .chars()
        .map(|c| Value::Num(Num::Small(i64::from(c as u32))))
        .collect();
    Ok(Value::List(Shared::new(cps)))
}

fn text_from_chars(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("from_chars", args, 1)?;
    let parts = value_to_list(&args[0])?;
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        let t = match p {
            Value::Text(s) => s.as_str(),
            _ => {
                return Err(crate::error::RuntimeError::type_err(format!(
                    "from_chars: item {i} must be text"
                )))
            }
        };
        if t.chars().count() != 1 {
            return Err(crate::error::RuntimeError::value_err(format!(
                "from_chars: item {i} must be a single character"
            )));
        }
        out.push_str(t);
    }
    Ok(Value::Text(out))
}

fn text_from_codepoints(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("from_codepoints", args, 1)?;
    let parts = value_to_list(&args[0])?;
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        let n = crate::value::expect_i64("from_codepoints", p)?;
        if !(0..=0x0010_FFFF).contains(&n) {
            return Err(crate::error::RuntimeError::value_err(format!(
                "from_codepoints: item {i} out of range"
            )));
        }
        let Some(ch) = char::from_u32(n as u32) else {
            return Err(crate::error::RuntimeError::value_err(format!(
                "from_codepoints: item {i} invalid code point"
            )));
        };
        out.push(ch);
    }
    Ok(Value::Text(out))
}

fn text_to_bytes(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("to_bytes", args, 0)?;
    Ok(Value::Bytes(Arc::new(s.into_bytes())))
}

fn text_from_bytes(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("from_bytes", args, 1)?;
    let bytes = match &args[0] {
        Value::Bytes(b) => b.as_ref().clone(),
        Value::Text(s) => s.as_bytes().to_vec(),
        _ => {
            return Err(crate::error::RuntimeError::type_err(
                "from_bytes: argument must be bytes or text",
            ))
        }
    };
    let s = String::from_utf8(bytes).map_err(|e| {
        crate::error::RuntimeError::value_err(format!("from_bytes: invalid UTF-8 ({e})"))
    })?;
    Ok(Value::Text(s))
}

fn text_byte_len(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Num(Num::Small(
        expect_text("byte_len", args, 0)?.len() as i64,
    )))
}

fn text_rfind(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("rfind", args, 2)?;
    let s = expect_text("rfind", args, 0)?;
    let needle = expect_text("rfind", args, 1)?;
    Ok(match s.rfind(&needle) {
        Some(i) => Value::Num(Num::Small(s[..i].chars().count() as i64)),
        None => Value::Num(Num::Small(-1)),
    })
}

fn text_replace_n(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("replace_n", args, 4)?;
    let s = expect_text("replace_n", args, 0)?;
    let from = expect_text("replace_n", args, 1)?;
    let to = expect_text("replace_n", args, 2)?;
    let n = expect_int("replace_n", args, 3)?;
    if n < 0 {
        return Ok(Value::Text(s.replace(&from, &to)));
    }
    Ok(Value::Text(s.replacen(&from, &to, n as usize)))
}

fn text_cmp(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("cmp", args, 2)?;
    let a = expect_text("cmp", args, 0)?;
    let b = expect_text("cmp", args, 1)?;
    let ord = match a.cmp(&b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    };
    Ok(Value::Num(Num::Small(ord)))
}

/// `std.text.Builder([initial])` → 可变字符串缓冲。
fn text_builder_new(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() > 1 {
        return Err(crate::error::RuntimeError::type_err(
            "Builder requires 0 or 1 argument",
        ));
    }
    let initial = if args.is_empty() {
        String::new()
    } else {
        args[0].print_string()
    };
    let buf = Shared::new(initial);

    let buf_a = buf.clone();
    let append = Value::builtin("append", move |_vm, call_args| {
        if call_args.len() != 1 {
            return Err(crate::error::RuntimeError::type_err(
                "Builder.append requires 1 argument",
            ));
        }
        buf_a.borrow_mut().push_str(&call_args[0].print_string());
        Ok(Value::None)
    });

    let buf_c = buf.clone();
    let clear = Value::builtin("clear", move |_vm, call_args| {
        if !call_args.is_empty() {
            return Err(crate::error::RuntimeError::type_err(
                "Builder.clear requires 0 arguments",
            ));
        }
        buf_c.borrow_mut().clear();
        Ok(Value::None)
    });

    let buf_l = buf.clone();
    let len = Value::builtin("len", move |_vm, call_args| {
        if !call_args.is_empty() {
            return Err(crate::error::RuntimeError::type_err(
                "Builder.len requires 0 arguments",
            ));
        }
        Ok(Value::Num(
            Num::Small(buf_l.borrow().chars().count() as i64),
        ))
    });

    let buf_t = buf.clone();
    let to_text = Value::builtin("to_text", move |_vm, call_args| {
        if !call_args.is_empty() {
            return Err(crate::error::RuntimeError::type_err(
                "Builder.to_text requires 0 arguments",
            ));
        }
        Ok(Value::Text(buf_t.borrow().clone()))
    });

    let buf_b = buf;
    let to_bytes = Value::builtin("to_bytes", move |_vm, call_args| {
        if !call_args.is_empty() {
            return Err(crate::error::RuntimeError::type_err(
                "Builder.to_bytes requires 0 arguments",
            ));
        }
        Ok(Value::Bytes(Arc::new(buf_b.borrow().as_bytes().to_vec())))
    });

    let mut exports = HashMap::new();
    exports.insert("append".into(), append);
    exports.insert("clear".into(), clear);
    exports.insert("len".into(), len);
    exports.insert("to_text".into(), to_text);
    exports.insert("to_bytes".into(), to_bytes);

    Ok(Value::Module(Shared::new(ModuleObject {
        name: "Builder".into(),
        exports,
        children: HashMap::new(),
        is_user: false,
    })))
}
