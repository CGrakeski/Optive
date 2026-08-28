//! `std.sqlite`：脚本侧文件 / 内存数据库。不碰宿主 `index.db`。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{params_from_iter, types::ValueRef, Connection};

use crate::error::RuntimeError;
use crate::shared::Shared;
use crate::value::{DictMap, ModuleObject, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use super::{expect_arity, expect_text, exports, named_builtin, submodule};

pub(super) fn build_sqlite_module() -> Shared<ModuleObject> {
    submodule("sqlite", &[("open", named_builtin("open", sqlite_open))])
}

fn sqlite_open(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("open", args, 1)?;
    let path = expect_text("open", args, 0)?;
    let checked = if path == ":memory:" {
        std::path::PathBuf::from(&path)
    } else if vm.caps.fs_restricted() {
        return Err(RuntimeError::io_err(
            "sqlite.open: file databases are disabled in sandbox because SQLite cannot open an \
             already-authorized file handle; use ':memory:'",
        ));
    } else {
        std::path::PathBuf::from(&path)
    };
    let conn = Connection::open(&checked)
        .map_err(|e| RuntimeError::io_err(format!("sqlite.open {}: {e}", checked.display())))?;
    Ok(wrap_db(conn))
}

fn wrap_db(conn: Connection) -> Value {
    let inner = Arc::new(Mutex::new(Some(conn)));
    let exec_h = inner.clone();
    let query_h = inner.clone();
    let close_h = inner;
    Value::Module(Shared::new(ModuleObject {
        name: "SqliteDb".into(),
        exports: exports(&[
            (
                "execute",
                Value::builtin("execute", move |_vm, args| {
                    if args.is_empty() || args.len() > 2 {
                        return Err(RuntimeError::type_err(
                            "execute requires (sql) or (sql, params)",
                        ));
                    }
                    let sql = expect_text("execute", args, 0)?;
                    let binds = if args.len() == 2 {
                        sql_params(&args[1])?
                    } else {
                        Vec::new()
                    };
                    let mut guard = exec_h.lock();
                    let db = guard
                        .as_mut()
                        .ok_or_else(|| RuntimeError::io_err("execute: database is closed"))?;
                    let n = db
                        .execute(&sql, params_from_iter(binds.iter()))
                        .map_err(|e| RuntimeError::io_err(format!("execute: {e}")))?;
                    Ok(Value::Num(Num::Small(n as i64)))
                }),
            ),
            (
                "query",
                Value::builtin("query", move |_vm, args| {
                    if args.is_empty() || args.len() > 2 {
                        return Err(RuntimeError::type_err(
                            "query requires (sql) or (sql, params)",
                        ));
                    }
                    let sql = expect_text("query", args, 0)?;
                    let binds = if args.len() == 2 {
                        sql_params(&args[1])?
                    } else {
                        Vec::new()
                    };
                    let mut guard = query_h.lock();
                    let db = guard
                        .as_mut()
                        .ok_or_else(|| RuntimeError::io_err("query: database is closed"))?;
                    let mut stmt = db
                        .prepare(&sql)
                        .map_err(|e| RuntimeError::io_err(format!("query: {e}")))?;
                    let names: Vec<String> = stmt
                        .column_names()
                        .into_iter()
                        .map(str::to_string)
                        .collect();
                    let mut rows_out = Vec::new();
                    let mut rows = stmt
                        .query(params_from_iter(binds.iter()))
                        .map_err(|e| RuntimeError::io_err(format!("query: {e}")))?;
                    while let Some(row) = rows
                        .next()
                        .map_err(|e| RuntimeError::io_err(format!("query: {e}")))?
                    {
                        let mut d = DictMap::new();
                        for (i, name) in names.iter().enumerate() {
                            let v = row
                                .get_ref(i)
                                .map_err(|e| RuntimeError::io_err(format!("query: {e}")))?;
                            d.insert(ValueKey::Text(name.clone()), sql_value(v));
                        }
                        rows_out.push(Value::Dict(Shared::new(d)));
                    }
                    Ok(Value::List(Shared::new(rows_out)))
                }),
            ),
            (
                "close",
                Value::builtin("close", move |_vm, _| {
                    let _ = close_h.lock().take();
                    Ok(Value::None)
                }),
            ),
        ]),
        children: HashMap::new(),
        is_user: false,
        live_globals: None,
    }))
}

fn sql_params(v: &Value) -> Result<Vec<rusqlite::types::Value>> {
    let items = match v {
        Value::List(l) => l.borrow().clone(),
        Value::Tuple(t) => t.iter().cloned().collect(),
        _ => {
            return Err(RuntimeError::type_err(
                "sqlite params must be a list or tuple",
            ));
        }
    };
    items.iter().map(optive_to_sql).collect()
}

fn optive_to_sql(v: &Value) -> Result<rusqlite::types::Value> {
    Ok(match v {
        Value::None => rusqlite::types::Value::Null,
        Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
        Value::Num(n) => match n.to_i64() {
            Some(i) => rusqlite::types::Value::Integer(i),
            None => rusqlite::types::Value::Real(n.to_f64_checked().unwrap_or(0.0)),
        },
        Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
        Value::Bytes(b) => rusqlite::types::Value::Blob(b.as_ref().clone()),
        other => {
            return Err(RuntimeError::type_err(format!(
                "unsupported sqlite param type: {}",
                other.type_name()
            )));
        }
    })
}

fn sql_value(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::None,
        ValueRef::Integer(i) => Value::Num(Num::Small(i)),
        ValueRef::Real(f) => num_rational::BigRational::from_float(f)
            .map(Num::from_rational)
            .map(Value::Num)
            .unwrap_or(Value::None),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Bytes(Arc::new(b.to_vec())),
    }
}
