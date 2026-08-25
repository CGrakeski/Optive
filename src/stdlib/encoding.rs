//! `std.encoding`：base64 / hex / url / gzip。

use std::sync::Arc;

use crate::shared::Shared;
use crate::value::{ModuleObject, Value};
use crate::vm::Vm;
use crate::Result;

use super::{builtin, expect_arity, expect_text, io_map, submodule};

// ---------------------------------------------------------------------------
// std.encoding —— base64 / hex / url / gzip 编解码
// ---------------------------------------------------------------------------

pub(super) fn enc_input_bytes(v: &Value) -> Result<Vec<u8>> {
    match v {
        Value::Text(s) => Ok(s.as_bytes().to_vec()),
        Value::Bytes(b) => Ok(b.as_ref().clone()),
        _ => Err(crate::error::RuntimeError::type_err(
            "expected text or bytes",
        )),
    }
}

pub(super) fn enc_base64_encode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("base64_encode", args, 1)?;
    let data = enc_input_bytes(&args[0])?;
    use base64::Engine;
    Ok(Value::Text(
        base64::engine::general_purpose::STANDARD.encode(&data),
    ))
}

pub(super) fn enc_base64_decode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("base64_decode", args, 1)?;
    let s = expect_text("base64_decode", args, 0)?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .map_err(|e| crate::error::RuntimeError::value_err(format!("base64_decode: {e}")))?;
    Ok(Value::Bytes(Arc::new(bytes)))
}

pub(super) fn enc_hex_encode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("hex_encode", args, 1)?;
    let data = enc_input_bytes(&args[0])?;
    Ok(Value::Text(hex::encode(&data)))
}

pub(super) fn enc_hex_decode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("hex_decode", args, 1)?;
    let s = expect_text("hex_decode", args, 0)?;
    let bytes = hex::decode(s)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("hex_decode: {e}")))?;
    Ok(Value::Bytes(Arc::new(bytes)))
}

/// URL 百分号编码：保留 unreserved 字符（A-Za-z0-9-._~），其余 %XX。
pub(super) fn enc_url_encode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("url_encode", args, 1)?;
    let s = expect_text("url_encode", args, 0)?;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(Value::Text(out))
}

pub(super) fn enc_url_decode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("url_decode", args, 1)?;
    let s = expect_text("url_decode", args, 0)?;
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            if i + 2 >= bytes.len() {
                return Err(crate::error::RuntimeError::value_err(
                    "url_decode: incomplete %XX escape",
                ));
            }
            let hi = hex_val(bytes[i + 1])?;
            let lo = hex_val(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else if b == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(b);
            i += 1;
        }
    }
    Ok(Value::Text(String::from_utf8(out).map_err(|e| {
        crate::error::RuntimeError::value_err(format!("url_decode: {e}"))
    })?))
}

pub(super) fn hex_val(b: u8) -> Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(crate::error::RuntimeError::value_err(format!(
            "url_decode: invalid hex digit '{}'",
            b as char
        ))),
    }
}

pub(super) fn enc_gzip_encode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("gzip_encode", args, 1)?;
    let data = enc_input_bytes(&args[0])?;
    use flate2::write::GzEncoder;
    use std::io::Write;
    let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(&data)
        .map_err(|e| io_map("gzip_encode", e))?;
    let out = encoder.finish().map_err(|e| io_map("gzip_encode", e))?;
    Ok(Value::Bytes(Arc::new(out)))
}

pub(super) fn enc_gzip_decode(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("gzip_decode", args, 1)?;
    let data = enc_input_bytes(&args[0])?;
    use flate2::read::GzDecoder;
    use std::io::Read;
    let mut dec = GzDecoder::new(&data[..]);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|e| io_map("gzip_decode", e))?;
    Ok(Value::Bytes(Arc::new(out)))
}

pub(super) fn build_encoding_module() -> Shared<ModuleObject> {
    submodule(
        "encoding",
        &[
            ("base64_encode", builtin(enc_base64_encode)),
            ("base64_decode", builtin(enc_base64_decode)),
            ("hex_encode", builtin(enc_hex_encode)),
            ("hex_decode", builtin(enc_hex_decode)),
            ("url_encode", builtin(enc_url_encode)),
            ("url_decode", builtin(enc_url_decode)),
            ("gzip_encode", builtin(enc_gzip_encode)),
            ("gzip_decode", builtin(enc_gzip_decode)),
        ],
    )
}
