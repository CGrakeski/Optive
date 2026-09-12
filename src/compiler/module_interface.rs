//! Canonical `.tivi` module interface schema and const-value encoding.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ast::{
    EnumMemberDecl, Expr, FuncParam, Program, ProtocolMember, Stmt, StructField, VariantCaseDecl,
    Visibility,
};
use crate::compiler::const_eval::ConstContext;
use crate::fmt;
use crate::value::{value_key_to_value, FrozenValue, Num, Value, ValueKey};

pub const INTERFACE_FORMAT: u32 = 3;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModuleInterface {
    pub format: u32,
    pub package_id: String,
    pub module: String,
    pub source_digest: String,
    pub build_digest: String,
    pub interface_digest: String,
    pub imports: Vec<String>,
    pub exports: Vec<Export>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Export {
    pub name: String,
    pub kind: String,
    pub visibility: String,
    pub compile_time: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamSig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_params: Vec<TypeParamSig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldSig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<VariantSig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<MethodSig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ParamSig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mutable: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub variadic: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TypeParamSig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FieldSig {
    pub name: String,
    pub mutable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VariantSig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldSig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MethodSig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamSig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
}

#[must_use]
pub fn visibility_name(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Default => "default",
        Visibility::Exported => "export",
        Visibility::Internal => "internal",
    }
}

pub fn exports_of(program: &Program, consts: &ConstContext) -> Result<Vec<Export>, String> {
    let mut result = Vec::new();
    for located in &program.stmts {
        let Some(mut export) = export_of(&located.stmt, consts)? else {
            continue;
        };
        if export.visibility == "internal" {
            continue;
        }
        if export.compile_time && export.kind == "value" {
            export.value = consts
                .value(&export.name)
                .map(const_value_json)
                .transpose()?;
        }
        result.push(export);
    }
    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(result)
}

fn export_of(stmt: &Stmt, _consts: &ConstContext) -> Result<Option<Export>, String> {
    Ok(match stmt {
        Stmt::VarDecl {
            visibility,
            is_const,
            name,
            type_expr,
            ..
        } => Some(Export {
            name: name.clone(),
            kind: "value".into(),
            visibility: visibility_name(*visibility).into(),
            compile_time: *is_const,
            value: None,
            params: Vec::new(),
            return_type: type_expr.as_ref().map(type_text),
            type_params: Vec::new(),
            fields: Vec::new(),
            variants: Vec::new(),
            methods: Vec::new(),
            effects: Vec::new(),
            doc: None,
        }),
        Stmt::FuncDecl {
            visibility,
            is_const,
            name,
            params,
            return_type,
            type_params,
            ..
        } => Some(Export {
            name: name.clone(),
            kind: "function".into(),
            visibility: visibility_name(*visibility).into(),
            compile_time: *is_const,
            value: None,
            params: params.iter().map(param_sig).collect(),
            return_type: return_type.as_ref().map(type_text),
            type_params: type_params.iter().map(type_param_sig).collect(),
            fields: Vec::new(),
            variants: Vec::new(),
            methods: Vec::new(),
            effects: if *is_const {
                vec!["pure".into()]
            } else {
                Vec::new()
            },
            doc: None,
        }),
        Stmt::StructDecl {
            visibility,
            name,
            type_params,
            fields,
            methods,
            ..
        } => Some(Export {
            name: name.clone(),
            kind: "struct".into(),
            visibility: visibility_name(*visibility).into(),
            compile_time: false,
            value: None,
            params: Vec::new(),
            return_type: None,
            type_params: type_params.iter().map(type_param_sig).collect(),
            fields: fields.iter().map(field_sig).collect(),
            variants: Vec::new(),
            methods: methods
                .iter()
                .map(|method| MethodSig {
                    name: method.name.clone(),
                    params: method.params.iter().map(param_sig).collect(),
                    return_type: method.return_type.as_ref().map(type_text),
                })
                .collect(),
            effects: Vec::new(),
            doc: None,
        }),
        Stmt::EnumDecl {
            visibility,
            name,
            members,
            methods,
            ..
        } => Some(Export {
            name: name.clone(),
            kind: "enum".into(),
            visibility: visibility_name(*visibility).into(),
            compile_time: false,
            value: None,
            params: Vec::new(),
            return_type: None,
            type_params: Vec::new(),
            fields: Vec::new(),
            variants: members.iter().map(enum_variant_sig).collect(),
            methods: methods
                .iter()
                .map(|method| MethodSig {
                    name: method.name.clone(),
                    params: method.params.iter().map(param_sig).collect(),
                    return_type: None,
                })
                .collect(),
            effects: Vec::new(),
            doc: None,
        }),
        Stmt::VariantDecl {
            visibility,
            name,
            type_params,
            cases,
            ..
        } => Some(Export {
            name: name.clone(),
            kind: "variant".into(),
            visibility: visibility_name(*visibility).into(),
            compile_time: false,
            value: None,
            params: Vec::new(),
            return_type: None,
            type_params: type_params.iter().map(type_param_sig).collect(),
            fields: Vec::new(),
            variants: cases.iter().map(variant_case_sig).collect(),
            methods: Vec::new(),
            effects: Vec::new(),
            doc: None,
        }),
        Stmt::ProtocolDecl {
            visibility,
            name,
            members,
            ..
        } => Some(Export {
            name: name.clone(),
            kind: "protocol".into(),
            visibility: visibility_name(*visibility).into(),
            compile_time: false,
            value: None,
            params: Vec::new(),
            return_type: None,
            type_params: Vec::new(),
            fields: protocol_fields(members),
            variants: Vec::new(),
            methods: protocol_methods(members),
            effects: Vec::new(),
            doc: None,
        }),
        _ => None,
    })
}

fn param_sig(param: &FuncParam) -> ParamSig {
    ParamSig {
        name: param.name.clone(),
        type_name: param.type_expr.as_ref().map(type_text),
        mutable: false,
        variadic: param.is_variadic || param.is_kwvariadic,
    }
}

fn type_param_sig((name, constraint): &(String, Option<Expr>)) -> TypeParamSig {
    TypeParamSig {
        name: name.clone(),
        constraint: constraint.as_ref().map(type_text),
    }
}

fn field_sig(field: &StructField) -> FieldSig {
    FieldSig {
        name: field.name.clone(),
        mutable: field.mutable,
        type_name: field.type_expr.as_ref().map(type_text),
    }
}

fn enum_variant_sig(member: &EnumMemberDecl) -> VariantSig {
    VariantSig {
        name: member.name.clone(),
        fields: Vec::new(),
    }
}

fn variant_case_sig(case: &VariantCaseDecl) -> VariantSig {
    VariantSig {
        name: case.name.clone(),
        fields: case.fields.iter().map(field_sig).collect(),
    }
}

fn protocol_fields(members: &[ProtocolMember]) -> Vec<FieldSig> {
    members
        .iter()
        .filter_map(|member| match member {
            ProtocolMember::Field { name, mutable } => Some(FieldSig {
                name: name.clone(),
                mutable: *mutable,
                type_name: None,
            }),
            ProtocolMember::Method { .. } => None,
        })
        .collect()
}

fn protocol_methods(members: &[ProtocolMember]) -> Vec<MethodSig> {
    members
        .iter()
        .filter_map(|member| match member {
            ProtocolMember::Method { name, params } => Some(MethodSig {
                name: name.clone(),
                params: params.iter().map(param_sig).collect(),
                return_type: None,
            }),
            ProtocolMember::Field { .. } => None,
        })
        .collect()
}

fn type_text(expr: &Expr) -> String {
    fmt::format_expr(expr).trim().to_string()
}

pub fn interface_digest_of(package_id: &str, module: &str, exports: &[Export]) -> String {
    digest_bytes(&canonical_json_bytes(&serde_json::json!({
        "exports": exports,
        "module": module,
        "package_id": package_id,
        "schema": INTERFACE_FORMAT,
    })))
}

pub fn compile_time_values(
    exports: &[Export],
) -> Result<std::collections::BTreeMap<String, Value>, String> {
    let mut values = std::collections::BTreeMap::new();
    for export in exports {
        if export.compile_time {
            if let Some(encoded) = &export.value {
                values.insert(export.name.clone(), const_value_from_json(encoded)?);
            }
        }
    }
    Ok(values)
}

#[must_use]
pub fn digest_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn canonical_json_bytes(value: &serde_json::Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut Vec<u8>) {
    match value {
        serde_json::Value::Null => out.extend_from_slice(b"null"),
        serde_json::Value::Bool(true) => out.extend_from_slice(b"true"),
        serde_json::Value::Bool(false) => out.extend_from_slice(b"false"),
        serde_json::Value::Number(number) => out.extend_from_slice(number.to_string().as_bytes()),
        serde_json::Value::String(text) => {
            out.extend_from_slice(serde_json::to_string(text).expect("string json").as_bytes());
        }
        serde_json::Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            out.push(b'{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(serde_json::to_string(key).expect("key json").as_bytes());
                out.push(b':');
                write_canonical(&map[key], out);
            }
            out.push(b'}');
        }
    }
}

pub fn const_value_from_json(value: &serde_json::Value) -> Result<Value, String> {
    let kind = value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing const kind")?;
    let payload = value.get("value");
    Ok(match kind {
        "none" => Value::None,
        "bool" => Value::Bool(
            payload
                .and_then(serde_json::Value::as_bool)
                .ok_or("bad bool const")?,
        ),
        "int" => Value::Num(
            Num::from_literal(&payload.ok_or("bad int const")?.to_string())
                .map_err(|error| error.to_string())?,
        ),
        "num" => Value::Num(
            Num::from_literal(
                payload
                    .and_then(serde_json::Value::as_str)
                    .ok_or("bad num const")?,
            )
            .map_err(|error| error.to_string())?,
        ),
        "text" => Value::Text(
            payload
                .and_then(serde_json::Value::as_str)
                .ok_or("bad text const")?
                .to_string(),
        ),
        "bytes" => Value::Bytes(Arc::new(
            hex::decode(
                payload
                    .and_then(serde_json::Value::as_str)
                    .ok_or("bad bytes const")?,
            )
            .map_err(|error| error.to_string())?,
        )),
        "tuple" => Value::Tuple(decode_value_array(payload)?.into()),
        "frozen_list" => Value::Frozen(Arc::new(FrozenValue::List(
            decode_value_array(payload)?.into(),
        ))),
        "frozen_dict" => {
            let mut entries = Vec::new();
            for entry in payload
                .and_then(serde_json::Value::as_array)
                .ok_or("bad frozen dict")?
            {
                let pair = entry
                    .as_array()
                    .filter(|pair| pair.len() == 2)
                    .ok_or("bad frozen dict entry")?;
                let key = ValueKey::from_value(&const_value_from_json(&pair[0])?)
                    .map_err(|e| e.to_string())?;
                entries.push((key, const_value_from_json(&pair[1])?));
            }
            entries.sort_by(|(left, _), (right, _)| key_ord(left, right));
            Value::Frozen(Arc::new(FrozenValue::Dict(entries.into())))
        }
        "frozen_set" => {
            let mut keys = decode_value_array(payload)?
                .iter()
                .map(ValueKey::from_value)
                .collect::<crate::Result<Vec<_>>>()
                .map_err(|error| error.to_string())?;
            keys.sort_by(key_ord);
            keys.dedup();
            Value::Frozen(Arc::new(FrozenValue::Set(keys.into())))
        }
        _ => return Err(format!("unknown const kind `{kind}`")),
    })
}

fn decode_value_array(value: Option<&serde_json::Value>) -> Result<Vec<Value>, String> {
    value
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "bad const array".to_string())?
        .iter()
        .map(const_value_from_json)
        .collect()
}

pub fn const_value_json(value: &Value) -> Result<serde_json::Value, String> {
    Ok(match value {
        Value::None => serde_json::json!({ "kind": "none" }),
        Value::Bool(value) => serde_json::json!({ "kind": "bool", "value": value }),
        Value::Num(Num::Small(value)) => serde_json::json!({ "kind": "int", "value": value }),
        Value::Num(value) => serde_json::json!({ "kind": "num", "value": value.to_string() }),
        Value::Text(value) => serde_json::json!({ "kind": "text", "value": value }),
        Value::Bytes(value) => {
            serde_json::json!({ "kind": "bytes", "value": hex::encode(value.as_slice()) })
        }
        Value::Tuple(values) => serde_json::json!({
            "kind": "tuple",
            "value": values.iter().map(const_value_json).collect::<Result<Vec<_>, _>>()?,
        }),
        Value::Frozen(frozen) => match frozen.as_ref() {
            FrozenValue::List(values) => serde_json::json!({
                "kind": "frozen_list",
                "value": values.iter().map(const_value_json).collect::<Result<Vec<_>, _>>()?,
            }),
            FrozenValue::Dict(values) => {
                let mut entries = values
                    .iter()
                    .map(|(key, value)| {
                        Ok((
                            key.clone(),
                            serde_json::json!([
                                const_value_json(&value_key_to_value(key))?,
                                const_value_json(value)?
                            ]),
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                entries.sort_by(|(left, _), (right, _)| key_ord(left, right));
                serde_json::json!({
                    "kind": "frozen_dict",
                    "value": entries.into_iter().map(|(_, value)| value).collect::<Vec<_>>(),
                })
            }
            FrozenValue::Set(values) => {
                let mut items = values
                    .iter()
                    .map(|key| Ok((key.clone(), const_value_json(&value_key_to_value(key))?)))
                    .collect::<Result<Vec<_>, String>>()?;
                items.sort_by(|(left, _), (right, _)| key_ord(left, right));
                serde_json::json!({
                    "kind": "frozen_set",
                    "value": items.into_iter().map(|(_, value)| value).collect::<Vec<_>>(),
                })
            }
        },
        other => {
            return Err(format!(
                "const interface cannot encode {}",
                other.type_name()
            ))
        }
    })
}

fn key_ord(left: &ValueKey, right: &ValueKey) -> std::cmp::Ordering {
    canonical_key(left).cmp(&canonical_key(right))
}

fn canonical_key(key: &ValueKey) -> (u8, String) {
    match key {
        ValueKey::Bool(false) => (0, "false".into()),
        ValueKey::Bool(true) => (0, "true".into()),
        ValueKey::NumInt(value) => (1, value.to_string()),
        ValueKey::Text(value) => (2, value.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_object_keys() {
        let value = serde_json::json!({"b": 1, "a": 2});
        assert_eq!(
            String::from_utf8(canonical_json_bytes(&value)).unwrap(),
            r#"{"a":2,"b":1}"#
        );
    }
}
