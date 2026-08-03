//! 数值 `enum` 与代数 `variant` 支持。

use std::collections::HashMap;

use crate::ast::{EnumMemberDecl, EnumMethodDecl, Expr, ExprKind, StructField, TypeExpr, VariantCaseDecl};
use crate::error::RuntimeError;
use crate::opcode::{FunctionObject, Instruction};
use crate::shared::Shared;
use crate::value::{
    EnumDef, EnumMemberData, EnumMemberInfo, FieldTypeInfo, Num, StructDef, Value, VariantCaseDef,
    VariantDef, VariantInstance,
};
use crate::Result;
use std::sync::Arc;

pub fn default_enum_values(members: &[EnumMemberDecl]) -> Result<Vec<EnumMemberInfo>> {
    let mut out = Vec::new();
    let mut next: i64 = 0;
    for m in members {
        let value = if let Some(expr) = &m.value {
            eval_const_num(expr)?
        } else {
            let v = Num::from_i64(next);
            next += 1;
            v
        };
        if m.value.is_some() {
            next = value
                .to_i64()
                .ok_or_else(|| RuntimeError::type_err("enum value must be integer"))?
                + 1;
        }
        out.push(EnumMemberInfo {
            name: m.name.clone(),
            value,
        });
    }
    Ok(out)
}

pub fn eval_const_num(expr: &Expr) -> Result<Num> {
    match &expr.kind {
        ExprKind::Number(n) => Num::from_literal(n),
        ExprKind::Unary {
            op: crate::ast::UnaryOp::Neg,
            operand,
        } => {
            let inner = eval_const_num(operand)?;
            match inner {
                Num::Small(n) => Ok(Num::Small(-n)),
                Num::Int(n) => Ok(Num::from_bigint(-n.as_ref())),
                Num::Rat(r) => Ok(Num::from_rational(-r.as_ref())),
            }
        }
        _ => Err(RuntimeError::type_err("enum member value must be integer literal")),
    }
}

pub fn build_enum_def(name: &str, members: Vec<EnumMemberInfo>) -> Arc<EnumDef> {
    Arc::new(EnumDef { name: name.to_string(), members })
}

pub fn enum_member_value(def: &Arc<EnumDef>, index: usize) -> Value {
    let type_name = enum_member_type_name(def, &def.members[index].name);
    Value::EnumMember(Arc::new(EnumMemberData {
        def: def.clone(),
        member_index: index,
        type_name,
    }))
}

pub fn enum_member_type_name(def: &EnumDef, member_name: &str) -> String {
    format!("{}.{}", def.name, member_name)
}

pub fn enum_member_numeric_value(member: &EnumMemberData) -> Num {
    member.def.members[member.member_index].value.clone()
}

pub fn case_struct_name(variant_name: &str, case_name: &str) -> String {
    format!("{variant_name}.{case_name}")
}

pub fn variant_case_fields(case: &VariantCaseDecl) -> Vec<StructField> {
    case.fields.clone()
}

pub fn build_variant_def(
    name: &str,
    type_params: Vec<(String, Option<TypeExpr>)>,
    cases: &[VariantCaseDecl],
) -> (Arc<VariantDef>, Vec<(String, Arc<StructDef>)>) {
    let mut case_defs = Vec::new();
    let mut struct_defs = Vec::new();
    for case in cases {
        let struct_name = case_struct_name(name, &case.name);
        let fields: Vec<String> = case.fields.iter().map(|f| f.name.clone()).collect();
        let mutable_fields = case.fields.iter().map(|f| f.mutable).collect();
        let field_types = case
            .fields
            .iter()
            .map(|f| FieldTypeInfo {
                type_expr: f.type_expr.clone(),
                strict: f.type_strong,
            })
            .collect();
        case_defs.push(VariantCaseDef {
            name: case.name.clone(),
            struct_name: struct_name.clone(),
        });
        struct_defs.push((
            struct_name.clone(),
            Arc::new(StructDef {
                name: struct_name,
                base: None,
                fields,
                mutable_fields,
                typed: true,
                field_types,
                type_params: type_params.clone(),
                c_layout: None,
            }),
        ));
    }
    (
        Arc::new(VariantDef {
            name: name.to_string(),
            type_params,
            cases: case_defs,
        }),
        struct_defs,
    )
}

pub fn wrap_variant(
    inst_name: &str,
    def: &Arc<VariantDef>,
    generic_args: Vec<TypeExpr>,
    case_idx: usize,
    payload: Value,
) -> Value {
    Value::Variant(Arc::new(VariantInstance {
        inst_name: inst_name.to_string(),
        def: def.clone(),
        generic_args,
        case_idx,
        payload,
    }))
}

pub fn enum_name_of(vm: &mut crate::vm::Vm, enum_name: &str, args: &[Value]) -> Result<Value> {
    let n = if args.len() == 2 {
        &args[1]
    } else if args.len() == 1 {
        &args[0]
    } else {
        return Err(RuntimeError::type_err("name_of expects (cls, n) or (n)"));
    };
    let def = vm
        .enum_defs
        .get(enum_name)
        .ok_or_else(|| RuntimeError::msg(format!("unknown enum: {enum_name}")))?;
    for m in def.members.iter() {
        if Value::Num(m.value.clone()).eq(n)? {
            return Ok(Value::Text(m.name.clone()));
        }
    }
    Ok(Value::None)
}

pub fn builtin_enum_method_entries(
    enum_name: &str,
    def: &Arc<EnumDef>,
) -> Vec<(String, Arc<FunctionObject>)> {
    let members_name = format!("{enum_name}.members");
    let def_members = def.clone();
    let members_body = vec![
        Instruction::Push(Value::List(Shared::new(
            def_members
                .members
                .iter()
                .enumerate()
                .map(|(i, _)| enum_member_value(&def_members, i))
                .collect(),
        ))),
        Instruction::Ret,
    ];
    vec![(
        members_name,
        Arc::new(FunctionObject::new(
            format!("{enum_name}.members"),
            vec![crate::ast::FuncParam {
                name: "cls".into(),
                is_variadic: false,
                is_kwvariadic: false,
                implicit: false,
                type_expr: None,
                type_strong: false,
                default_expr: None,
            }],
            members_body,
        )),
    )]
}

pub fn install_builtin_enum_methods(
    enum_name: &str,
    def: &Arc<EnumDef>,
    functions: &mut HashMap<String, Arc<FunctionObject>>,
) {
    for (name, func) in builtin_enum_method_entries(enum_name, def) {
        functions.insert(name, func);
    }
}

pub fn finalize_enum_from_dict(
    vm: &mut crate::vm::Vm,
    enum_name: &str,
    member_names: &[String],
    values: &crate::value::DictMap,
) -> Result<()> {
    use crate::value::ValueKey;

    let mut member_infos = Vec::with_capacity(member_names.len());
    for name in member_names {
        let key = ValueKey::Text(name.clone());
        let val = values.get(&key).ok_or_else(|| {
            RuntimeError::msg(format!("__generate__ result missing key `{name}`"))
        })?;
        let num = match val {
            Value::Num(n) if n.to_i64().is_some() => n.clone(),
            _ => {
                return Err(RuntimeError::type_err(format!(
                    "__generate__ value for `{name}` must be integer num"
                )));
            }
        };
        member_infos.push(EnumMemberInfo {
            name: name.clone(),
            value: num,
        });
    }
    if values.len() != member_names.len() {
        return Err(RuntimeError::msg(
            "__generate__ result has extra keys not in enum declaration",
        ));
    }
    let def = build_enum_def(enum_name, member_infos);
    vm.register_enum_def(enum_name.to_string(), def);
    Ok(())
}

pub fn validate_enum_method(method: &EnumMethodDecl) -> Result<()> {
    if method.name == "__generate__" {
        if method.params.len() != 1 || method.params[0].name != "all" {
            return Err(RuntimeError::msg(
                "__generate__ must have exactly one parameter named `all`",
            ));
        }
        return Ok(());
    }
    if method.params.first().map(|p| p.name.as_str()) != Some("cls") {
        return Err(RuntimeError::msg(format!(
            "enum method `{}` must have `cls` as first parameter",
            method.name
        )));
    }
    Ok(())
}
