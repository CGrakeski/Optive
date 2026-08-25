//! toml / yaml 经 serde_json 中转。

use crate::value::{DictMap, ModuleObject, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use crate::shared::Shared;

use super::{builtin, expect_arity, expect_text, float_from_f64, submodule, value_key_to_value};

pub(super) fn serde_json_to_optive(v: &serde_json::Value) -> Result<Value> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Value::None,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Num(Num::Small(i))
            } else if let Some(u) = n.as_u64() {
                if let Ok(i) = i64::try_from(u) {
                    Value::Num(Num::Small(i))
                } else {
                    Value::Num(Num::from_bigint(num_bigint::BigInt::from(u)))
                }
            } else if let Some(f) = n.as_f64() {
                Value::Num(float_from_f64(f)?)
            } else {
                Value::None
            }
        }
        J::String(s) => Value::Text(s.clone()),
        J::Array(a) => {
            let items: Result<Vec<_>> = a.iter().map(serde_json_to_optive).collect();
            Value::List(Shared::new(items?))
        }
        J::Object(o) => {
            let mut d = DictMap::new();
            for (k, val) in o {
                d.insert(ValueKey::Text(k.clone()), serde_json_to_optive(val)?);
            }
            Value::Dict(Shared::new(d))
        }
    })
}

/// Optive `Value` → `serde_json::Value`（toml/yaml stringify 中转）。
pub(super) fn optive_to_serde_json(v: &Value) -> Result<serde_json::Value> {
    use serde_json::{Map, Number, Value as J};
    Ok(match v {
        Value::None => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Num(n) => {
            if let Some(i) = n.to_i64() {
                J::Number(i.into())
            } else if let Ok(f) = n.to_f64_checked() {
                Number::from_f64(f).map(J::Number).unwrap_or(J::Null)
            } else {
                J::String(n.to_string())
            }
        }
        Value::Text(s) => J::String(s.clone()),
        Value::List(l) => {
            let items: Result<Vec<_>> = l.borrow().iter().map(optive_to_serde_json).collect();
            J::Array(items?)
        }
        Value::Tuple(t) => {
            let items: Result<Vec<_>> = t.iter().map(optive_to_serde_json).collect();
            J::Array(items?)
        }
        Value::Dict(d) => {
            let mut map = Map::new();
            for (k, val) in d.borrow().iter() {
                let key = match value_key_to_value(k) {
                    Value::Text(s) => s,
                    other => other.print_string(),
                };
                if map.contains_key(&key) {
                    return Err(crate::error::RuntimeError::value_err(format!(
                        "duplicate JSON object key `{key}`"
                    )));
                }
                map.insert(key, optive_to_serde_json(val)?);
            }
            J::Object(map)
        }
        Value::Set(s) => {
            let items: Result<Vec<_>> = s
                .borrow()
                .iter()
                .map(|k| optive_to_serde_json(&value_key_to_value(k)))
                .collect();
            J::Array(items?)
        }
        other => J::String(other.print_string()),
    })
}

// --- std.toml ---

pub(super) fn toml_parse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("parse", args, 1)?;
    let s = expect_text("parse", args, 0)?;
    let toml_val: toml::Value = toml::from_str(&s)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("toml parse: {e}")))?;
    let jv = serde_json::to_value(&toml_val)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("toml convert: {e}")))?;
    serde_json_to_optive(&jv)
}

pub(super) fn toml_stringify(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("stringify", args, 1)?;
    let jv = optive_to_serde_json(&args[0])?;
    let toml_val = toml::Value::try_from(&jv)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("toml stringify: {e}")))?;
    toml::to_string(&toml_val)
        .map(Value::Text)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("toml stringify: {e}")))
}

pub(super) fn build_toml_module() -> Shared<ModuleObject> {
    submodule(
        "toml",
        &[
            ("parse", builtin(toml_parse)),
            ("stringify", builtin(toml_stringify)),
        ],
    )
}

// --- std.yaml ---

pub(super) fn yaml_parse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("parse", args, 1)?;
    let s = expect_text("parse", args, 0)?;
    let yaml_val: serde_yaml::Value = serde_yaml::from_str(&s)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("yaml parse: {e}")))?;
    let jv = serde_json::to_value(&yaml_val)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("yaml convert: {e}")))?;
    serde_json_to_optive(&jv)
}

pub(super) fn yaml_stringify(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("stringify", args, 1)?;
    let jv = optive_to_serde_json(&args[0])?;
    serde_yaml::to_string(&jv)
        .map(Value::Text)
        .map_err(|e| crate::error::RuntimeError::value_err(format!("yaml stringify: {e}")))
}

pub(super) fn build_yaml_module() -> Shared<ModuleObject> {
    submodule(
        "yaml",
        &[
            ("parse", builtin(yaml_parse)),
            ("stringify", builtin(yaml_stringify)),
        ],
    )
}
