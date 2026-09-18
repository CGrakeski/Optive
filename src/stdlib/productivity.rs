//! Small, composable batteries for command-line programs and identifiers.

use std::sync::Arc;

use ring::rand::{SecureRandom, SystemRandom};

use crate::error::RuntimeError;
use crate::shared::Shared;
use crate::value::{DictMap, ModuleObject, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use super::{builtin, expect_arity, expect_int, expect_text, submodule};

pub(super) fn build_productivity_modules() -> Vec<(String, Shared<ModuleObject>)> {
    vec![
        (
            "cli".into(),
            submodule(
                "cli",
                &[
                    ("parse", builtin(cli_parse)),
                    ("args", builtin(super::os::os_args)),
                ],
            ),
        ),
        (
            "console".into(),
            submodule(
                "console",
                &[
                    ("style", builtin(console_style)),
                    ("strip_ansi", builtin(console_strip_ansi)),
                    ("width", builtin(console_width)),
                ],
            ),
        ),
        (
            "process".into(),
            submodule(
                "process",
                &[
                    ("run", builtin(super::os::os_run)),
                    ("capture", builtin(super::os::os_capture)),
                    ("args", builtin(super::os::os_args)),
                    ("exit", builtin(super::os::os_exit)),
                ],
            ),
        ),
        (
            "uri".into(),
            submodule(
                "uri",
                &[
                    ("encode", builtin(uri_encode)),
                    ("decode", builtin(uri_decode)),
                    ("query_encode", builtin(query_encode)),
                    ("query_decode", builtin(query_decode)),
                ],
            ),
        ),
        (
            "secrets".into(),
            submodule(
                "secrets",
                &[
                    ("bytes", builtin(secret_bytes)),
                    ("hex", builtin(secret_hex)),
                    ("token", builtin(secret_token)),
                ],
            ),
        ),
        (
            "uuid".into(),
            submodule(
                "uuid",
                &[
                    ("v4", builtin(uuid_v4)),
                    ("is_valid", builtin(uuid_is_valid)),
                    ("normalize", builtin(uuid_normalize)),
                ],
            ),
        ),
    ]
}

fn cli_parse(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() > 1 {
        return Err(RuntimeError::type_err(
            "cli.parse takes zero or one argument",
        ));
    }
    let values = if let Some(value) = args.first() {
        super::value_to_list(value)?
    } else if let Value::List(v) = super::os::os_args(vm, &[])? {
        v.borrow().iter().skip(1).cloned().collect()
    } else {
        unreachable!()
    };
    let mut out = DictMap::new();
    let mut positional = Vec::new();
    let mut end_options = false;
    for value in values {
        let arg = value.print_string();
        if !end_options && arg == "--" {
            end_options = true;
            continue;
        }
        if !end_options && arg.starts_with("--") {
            let raw = &arg[2..];
            let (key, value) = raw
                .split_once('=')
                .map_or((raw, Value::Bool(true)), |(k, v)| {
                    (k, Value::Text(v.into()))
                });
            if key.is_empty() {
                return Err(RuntimeError::value_err("cli.parse: empty option name"));
            }
            out.insert(ValueKey::Text(key.replace('-', "_")), value);
        } else if !end_options && arg.starts_with('-') && arg.len() > 1 {
            for flag in arg[1..].chars() {
                out.insert(ValueKey::Text(flag.to_string()), Value::Bool(true));
            }
        } else {
            positional.push(Value::Text(arg));
        }
    }
    out.insert(
        ValueKey::Text("_".into()),
        Value::List(Shared::new(positional)),
    );
    Ok(Value::Dict(Shared::new(out)))
}

fn console_style(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !(1..=2).contains(&args.len()) {
        return Err(RuntimeError::type_err(
            "console.style requires text and optional style",
        ));
    }
    let text = expect_text("console.style", args, 0)?;
    let style = args.get(1).map_or_else(
        || Ok("reset".into()),
        |v| match v {
            Value::Text(s) => Ok(s.clone()),
            _ => Err(RuntimeError::type_err("console style must be text")),
        },
    )?;
    let code = match style.as_str() {
        "bold" => "1",
        "dim" => "2",
        "red" => "31",
        "green" => "32",
        "yellow" => "33",
        "blue" => "34",
        "magenta" => "35",
        "cyan" => "36",
        "reset" => "0",
        _ => {
            return Err(RuntimeError::value_err(format!(
                "unknown console style '{style}'"
            )))
        }
    };
    Ok(Value::Text(format!("\u{1b}[{code}m{text}\u{1b}[0m")))
}

fn console_strip_ansi(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let text = expect_text("console.strip_ansi", args, 0)?;
    let re = regex::Regex::new("\\x1b\\[[0-9;]*m").map_err(|e| RuntimeError::msg(e.to_string()))?;
    Ok(Value::Text(re.replace_all(&text, "").into_owned()))
}

fn console_width(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    use unicode_width::UnicodeWidthStr;
    let text = expect_text("console.width", args, 0)?;
    Ok(Value::Num(Num::Small(text.width() as i64)))
}

fn uri_encode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Text(percent_encode(
        &expect_text("uri.encode", args, 0)?,
        false,
    )))
}
fn query_encode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    Ok(Value::Text(percent_encode(
        &expect_text("uri.query_encode", args, 0)?,
        true,
    )))
}

fn percent_encode(text: &str, query: bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else if query && b == b' ' {
            out.push('+');
        } else {
            out.push('%');
            out.push(char::from(HEX[(b >> 4) as usize]));
            out.push(char::from(HEX[(b & 15) as usize]));
        }
    }
    out
}

fn uri_decode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    decode_percent(&expect_text("uri.decode", args, 0)?, false)
}
fn query_decode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    decode_percent(&expect_text("uri.query_decode", args, 0)?, true)
}

fn decode_percent(text: &str, query: bool) -> Result<Value> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(RuntimeError::value_err("incomplete percent escape"));
            }
            let h = |b: u8| (b as char).to_digit(16).map(|n| n as u8);
            let Some(a) = h(bytes[i + 1]) else {
                return Err(RuntimeError::value_err("invalid percent escape"));
            };
            let Some(b) = h(bytes[i + 2]) else {
                return Err(RuntimeError::value_err("invalid percent escape"));
            };
            out.push(a * 16 + b);
            i += 3;
        } else {
            out.push(if query && bytes[i] == b'+' {
                b' '
            } else {
                bytes[i]
            });
            i += 1;
        }
    }
    String::from_utf8(out)
        .map(Value::Text)
        .map_err(|_| RuntimeError::value_err("decoded URI is not UTF-8"))
}

fn secure_random(len: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0; len];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| RuntimeError::msg("secure random source failed"))?;
    Ok(bytes)
}
fn secret_bytes(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let n = expect_int("secrets.bytes", args, 0)?;
    if !(0..=1_048_576).contains(&n) {
        return Err(RuntimeError::value_err(
            "secret byte count must be between 0 and 1048576",
        ));
    }
    Ok(Value::Bytes(Arc::new(secure_random(n as usize)?)))
}
fn secret_hex(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let n = expect_int("secrets.hex", args, 0)?;
    if !(0..=1_048_576).contains(&n) {
        return Err(RuntimeError::value_err(
            "secret byte count must be between 0 and 1048576",
        ));
    }
    Ok(Value::Text(hex::encode(secure_random(n as usize)?)))
}
fn secret_token(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let n = expect_int("secrets.token", args, 0)?;
    if !(0..=1_048_576).contains(&n) {
        return Err(RuntimeError::value_err(
            "secret byte count must be between 0 and 1048576",
        ));
    }
    use base64::Engine;
    Ok(Value::Text(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secure_random(n as usize)?),
    ))
}

fn uuid_v4(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("uuid.v4", args, 0)?;
    let mut b = secure_random(16)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    Ok(Value::Text(format_uuid(&b)))
}
fn uuid_is_valid(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("uuid.is_valid", args, 0)?;
    Ok(Value::Bool(parse_uuid(&s).is_some()))
}
fn uuid_normalize(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("uuid.normalize", args, 0)?;
    parse_uuid(&s)
        .map(|b| Value::Text(format_uuid(&b)))
        .ok_or_else(|| RuntimeError::value_err("invalid UUID"))
}
fn parse_uuid(s: &str) -> Option<Vec<u8>> {
    let compact: String = s
        .trim()
        .trim_matches('{')
        .trim_matches('}')
        .chars()
        .filter(|c| *c != '-')
        .collect();
    if compact.len() != 32 {
        return None;
    }
    hex::decode(compact).ok().filter(|b| b.len() == 16)
}
fn format_uuid(b: &[u8]) -> String {
    let h = hex::encode(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}
