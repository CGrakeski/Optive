//! `std.json`：手写 JSON 解析 / 序列化。

use num_bigint::BigInt;

use crate::value::{DictMap, ModuleObject, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use crate::shared::Shared;

use super::{builtin, expect_text, float_from_f64, submodule, value_key_to_value};

const JSON_MAX_DEPTH: usize = 64;

pub(super) fn build_json_module() -> Shared<ModuleObject> {
    submodule(
        "json",
        &[
            ("parse", builtin(json_parse)),
            ("stringify", builtin(json_stringify)),
            ("parse_file", builtin(json_parse_file)),
            ("dump", builtin(json_dump)),
        ],
    )
}

pub(super) fn json_parse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let s = expect_text("parse", args, 0)?;
    let mut p = JsonParser {
        chars: s.chars().collect(),
        i: 0,
    };
    let v = p.parse_value()?;
    p.skip_ws();
    if p.i < p.chars.len() {
        return Err(crate::error::RuntimeError::msg(
            "json parse: trailing input",
        ));
    }
    Ok(v)
}

pub(super) fn json_stringify(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "stringify requires 1 argument",
        ));
    }
    Ok(Value::Text(json_stringify_value(&args[0], 0)?))
}

pub(super) fn json_parse_file(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(crate::error::RuntimeError::type_err(
            "parse_file requires 1 argument",
        ));
    }
    let path = expect_text("parse_file", args, 0)?;
    let text = vm.caps.read_to_string("json.parse_file", &path)?;
    json_parse(vm, &[Value::Text(text)])
}

pub(super) fn json_dump(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(crate::error::RuntimeError::type_err(
            "dump requires 2 arguments (path, value)",
        ));
    }
    let path = expect_text("dump", args, 0)?;
    let text = json_stringify_value(&args[1], 0)?;
    vm.caps.write("json.dump", &path, text)?;
    Ok(Value::None)
}

struct JsonParser {
    chars: Vec<char>,
    i: usize,
}

impl JsonParser {
    pub(super) fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    pub(super) fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }

    fn expect_char(&mut self, expected: char) -> Result<()> {
        if self.bump() == Some(expected) {
            Ok(())
        } else {
            Err(crate::error::RuntimeError::msg(format!(
                "json parse: expected '{expected}'"
            )))
        }
    }

    pub(super) fn read_u_escape(&mut self) -> Result<u32> {
        let mut hex = String::new();
        for _ in 0..4 {
            let c = self
                .bump()
                .ok_or_else(|| crate::error::RuntimeError::msg("json parse: bad \\u escape"))?;
            hex.push(c);
        }
        u32::from_str_radix(&hex, 16)
            .map_err(|_| crate::error::RuntimeError::msg("json parse: bad \\u escape"))
    }

    pub(super) fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\n' | '\r' | '\t')) {
            self.i += 1;
        }
    }

    pub(super) fn parse_value(&mut self) -> Result<Value> {
        self.parse_value_at(0)
    }

    fn parse_value_at(&mut self, depth: usize) -> Result<Value> {
        if depth >= JSON_MAX_DEPTH {
            return Err(crate::error::RuntimeError::value_err(
                "json parse: nesting exceeds maximum depth",
            ));
        }
        self.skip_ws();
        match self.peek() {
            Some('n') => self.parse_literal("null", Value::None),
            Some('t') => self.parse_literal("true", Value::Bool(true)),
            Some('f') => self.parse_literal("false", Value::Bool(false)),
            Some('"') => Ok(Value::Text(self.parse_string()?)),
            Some('[') => self.parse_array(depth),
            Some('{') => self.parse_object(depth),
            Some('-' | '0'..='9') => self.parse_number(),
            Some(c) => Err(crate::error::RuntimeError::msg(format!(
                "json parse: unexpected '{c}'"
            ))),
            None => Err(crate::error::RuntimeError::msg(
                "json parse: unexpected end",
            )),
        }
    }

    pub(super) fn parse_literal(&mut self, lit: &str, val: Value) -> Result<Value> {
        for ch in lit.chars() {
            if self.bump() != Some(ch) {
                return Err(crate::error::RuntimeError::msg(format!(
                    "json parse: expected {lit}"
                )));
            }
        }
        Ok(val)
    }

    pub(super) fn parse_string(&mut self) -> Result<String> {
        if self.bump() != Some('"') {
            return Err(crate::error::RuntimeError::msg(
                "json parse: expected string",
            ));
        }
        let mut out = String::new();
        loop {
            match self.bump() {
                Some('"') => return Ok(out),
                Some('\\') => match self.bump() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('b') => out.push('\u{0008}'),
                    Some('f') => out.push('\u{000c}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let code = self.read_u_escape()?;
                        if (0xD800..=0xDBFF).contains(&code) {
                            // 高代理：必须紧跟 \uXXXX 低代理
                            if self.bump() != Some('\\') || self.bump() != Some('u') {
                                return Err(crate::error::RuntimeError::value_err(
                                    "json parse: lonely high surrogate",
                                ));
                            }
                            let low = self.read_u_escape()?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return Err(crate::error::RuntimeError::value_err(
                                    "json parse: invalid low surrogate",
                                ));
                            }
                            let cp = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            out.push(char::from_u32(cp).ok_or_else(|| {
                                crate::error::RuntimeError::value_err("json parse: invalid unicode")
                            })?);
                        } else if (0xDC00..=0xDFFF).contains(&code) {
                            return Err(crate::error::RuntimeError::value_err(
                                "json parse: lonely low surrogate",
                            ));
                        } else {
                            out.push(char::from_u32(code).ok_or_else(|| {
                                crate::error::RuntimeError::value_err("json parse: invalid unicode")
                            })?);
                        }
                    }
                    _ => return Err(crate::error::RuntimeError::msg("json parse: bad escape")),
                },
                Some(c) => out.push(c),
                None => {
                    return Err(crate::error::RuntimeError::msg(
                        "json parse: unterminated string",
                    ))
                }
            }
        }
    }

    pub(super) fn parse_number(&mut self) -> Result<Value> {
        let start = self.i;
        if self.peek() == Some('-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some('0'..='9')) {
            self.i += 1;
        }
        let mut is_float = false;
        if self.peek() == Some('.') {
            is_float = true;
            self.i += 1;
            while matches!(self.peek(), Some('0'..='9')) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            is_float = true;
            self.i += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.i += 1;
            }
            while matches!(self.peek(), Some('0'..='9')) {
                self.i += 1;
            }
        }
        let s: String = self.chars[start..self.i].iter().collect();
        if is_float {
            let f: f64 = s
                .parse()
                .map_err(|_| crate::error::RuntimeError::msg("json parse: bad number"))?;
            Ok(Value::Num(float_from_f64(f)?))
        } else if let Ok(n) = s.parse::<i64>() {
            Ok(Value::Num(Num::Small(n)))
        } else {
            Ok(Value::Num(Num::from_bigint(
                BigInt::parse_bytes(s.as_bytes(), 10)
                    .ok_or_else(|| crate::error::RuntimeError::msg("json parse: bad number"))?,
            )))
        }
    }

    pub(super) fn parse_array(&mut self, depth: usize) -> Result<Value> {
        self.bump(); // [
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(']') {
            self.bump();
            return Ok(Value::List(Shared::new(items)));
        }
        loop {
            items.push(self.parse_value_at(depth + 1)?);
            self.skip_ws();
            match self.bump() {
                Some(',') => {
                    self.skip_ws();
                    continue;
                }
                Some(']') => break,
                _ => {
                    return Err(crate::error::RuntimeError::msg(
                        "json parse: expected ',' or ']'",
                    ))
                }
            }
        }
        Ok(Value::List(Shared::new(items)))
    }

    pub(super) fn parse_object(&mut self, depth: usize) -> Result<Value> {
        self.bump(); // {
        self.skip_ws();
        let mut map = DictMap::new();
        if self.peek() == Some('}') {
            self.bump();
            return Ok(Value::Dict(Shared::new(map)));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect_char(':')?;
            let val = self.parse_value_at(depth + 1)?;
            map.insert(ValueKey::from_value(&Value::Text(key))?, val);
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some('}') => break,
                _ => {
                    return Err(crate::error::RuntimeError::msg(
                        "json parse: expected ',' or '}'",
                    ))
                }
            }
        }
        Ok(Value::Dict(Shared::new(map)))
    }
}

pub(super) fn json_stringify_value(v: &Value, depth: usize) -> Result<String> {
    if depth >= JSON_MAX_DEPTH {
        return Err(crate::error::RuntimeError::value_err(
            "json.stringify: nesting exceeds maximum depth",
        ));
    }
    Ok(match v {
        Value::None => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => n.to_string(),
        Value::Text(s) => json_escape_string(s),
        Value::List(l) => {
            let parts: Result<Vec<_>> = l
                .borrow()
                .iter()
                .map(|item| json_stringify_value(item, depth + 1))
                .collect();
            format!("[{}]", parts?.join(","))
        }
        Value::Dict(d) => {
            let mut parts = Vec::new();
            for (k, val) in d.borrow().iter() {
                let key = match value_key_to_value(k) {
                    Value::Text(s) => s,
                    other => other.print_string(),
                };
                parts.push(format!(
                    "{}:{}",
                    json_escape_string(&key),
                    json_stringify_value(val, depth + 1)?
                ));
            }
            format!("{{{}}}", parts.join(","))
        }
        Value::Set(s) => {
            let parts: Result<Vec<_>> = s
                .borrow()
                .iter()
                .map(|k| json_stringify_value(&value_key_to_value(k), depth + 1))
                .collect();
            format!("[{}]", parts?.join(","))
        }
        other => json_escape_string(&other.print_string()),
    })
}

pub(super) fn json_escape_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
