//! Safe in-memory ZIP, TAR and gzip helpers.

use std::io::{Cursor, Read, Write};
use std::sync::Arc;

use flate2::{read::GzDecoder, write::GzEncoder, Compression};

use crate::error::RuntimeError;
use crate::shared::Shared;
use crate::value::{ModuleObject, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use super::{builtin, expect_arity, expect_int, expect_text, submodule};

const DEFAULT_LIMIT: usize = 64 * 1024 * 1024;

pub(super) fn build_archive_module() -> Shared<ModuleObject> {
    submodule(
        "archive",
        &[
            ("gzip", builtin(gzip)),
            ("gunzip", builtin(gunzip)),
            ("zip", builtin(make_zip)),
            ("zip_list", builtin(zip_list)),
            ("zip_read", builtin(zip_read)),
            ("tar", builtin(make_tar)),
            ("tar_list", builtin(tar_list)),
            ("tar_read", builtin(tar_read)),
        ],
    )
}

fn input_bytes(name: &str, value: &Value) -> Result<Vec<u8>> {
    match value {
        Value::Bytes(bytes) => Ok(bytes.as_ref().clone()),
        Value::Text(text) => Ok(text.as_bytes().to_vec()),
        other => Err(RuntimeError::type_err(format!(
            "{name} expects bytes or text, got {}",
            other.type_name()
        ))),
    }
}

fn safe_path(path: &str) -> Result<()> {
    let normalized = path.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.as_bytes().get(1) == Some(&b':')
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part == ".")
    {
        return Err(RuntimeError::value_err(format!(
            "unsafe archive entry path '{path}'"
        )));
    }
    Ok(())
}

fn input_entries(name: &str, value: &Value) -> Result<Vec<(String, Vec<u8>)>> {
    let Value::Dict(map) = value else {
        return Err(RuntimeError::type_err(format!(
            "{name} expects a dict of path to bytes/text"
        )));
    };
    let mut out = Vec::with_capacity(map.borrow().len());
    for (key, value) in map.borrow().iter() {
        let ValueKey::Text(path) = key else {
            return Err(RuntimeError::type_err("archive paths must be text"));
        };
        safe_path(path)?;
        out.push((path.clone(), input_bytes(name, value)?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn checked_limit(args: &[Value], index: usize, name: &str) -> Result<usize> {
    if let Some(_) = args.get(index) {
        let limit = expect_int(name, args, index)?;
        if limit < 0 {
            return Err(RuntimeError::value_err(format!(
                "{name} output limit cannot be negative"
            )));
        }
        usize::try_from(limit)
            .map_err(|_| RuntimeError::value_err(format!("{name} output limit is too large")))
    } else {
        Ok(DEFAULT_LIMIT)
    }
}

fn read_limited(reader: impl Read, limit: usize, name: &str) -> Result<Vec<u8>> {
    let mut reader = reader.take((limit as u64).saturating_add(1));
    let mut out = Vec::new();
    reader
        .read_to_end(&mut out)
        .map_err(|e| RuntimeError::value_err(format!("{name}: {e}")))?;
    if out.len() > limit {
        return Err(RuntimeError::value_err(format!(
            "{name} output exceeds {limit} bytes"
        )));
    }
    Ok(out)
}

fn gzip(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !(1..=2).contains(&args.len()) {
        return Err(RuntimeError::type_err(
            "archive.gzip requires data and optional level",
        ));
    }
    let level = if args.len() == 2 {
        let level = expect_int("archive.gzip", args, 1)?;
        if !(0..=9).contains(&level) {
            return Err(RuntimeError::value_err(
                "gzip level must be between 0 and 9",
            ));
        }
        level as u32
    } else {
        6
    };
    let mut writer = GzEncoder::new(Vec::new(), Compression::new(level));
    writer
        .write_all(&input_bytes("archive.gzip", &args[0])?)
        .map_err(|e| RuntimeError::io_err(format!("archive.gzip: {e}")))?;
    Ok(Value::Bytes(Arc::new(writer.finish().map_err(|e| {
        RuntimeError::io_err(format!("archive.gzip: {e}"))
    })?)))
}

fn gunzip(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !(1..=2).contains(&args.len()) {
        return Err(RuntimeError::type_err(
            "archive.gunzip requires data and optional output limit",
        ));
    }
    let limit = checked_limit(args, 1, "archive.gunzip")?;
    let input = input_bytes("archive.gunzip", &args[0])?;
    Ok(Value::Bytes(Arc::new(read_limited(
        GzDecoder::new(input.as_slice()),
        limit,
        "archive.gunzip",
    )?)))
}

fn make_zip(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("archive.zip", args, 1)?;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (path, data) in input_entries("archive.zip", &args[0])? {
        writer
            .start_file(path, options)
            .map_err(|e| RuntimeError::io_err(format!("archive.zip: {e}")))?;
        writer
            .write_all(&data)
            .map_err(|e| RuntimeError::io_err(format!("archive.zip: {e}")))?;
    }
    let out = writer
        .finish()
        .map_err(|e| RuntimeError::io_err(format!("archive.zip: {e}")))?
        .into_inner();
    Ok(Value::Bytes(Arc::new(out)))
}

fn open_zip(value: &Value) -> Result<zip::ZipArchive<Cursor<Vec<u8>>>> {
    zip::ZipArchive::new(Cursor::new(input_bytes("archive zip", value)?))
        .map_err(|e| RuntimeError::value_err(format!("invalid zip archive: {e}")))
}

fn zip_list(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("archive.zip_list", args, 1)?;
    let mut archive = open_zip(&args[0])?;
    let mut out = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| RuntimeError::value_err(format!("invalid zip entry: {e}")))?;
        safe_path(entry.name())?;
        out.push(Value::Text(entry.name().to_string()));
    }
    Ok(Value::List(Shared::new(out)))
}

fn zip_read(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !(2..=3).contains(&args.len()) {
        return Err(RuntimeError::type_err(
            "archive.zip_read requires archive, path and optional output limit",
        ));
    }
    let name = expect_text("archive.zip_read", args, 1)?;
    safe_path(&name)?;
    let limit = checked_limit(args, 2, "archive.zip_read")?;
    let mut archive = open_zip(&args[0])?;
    let entry = archive
        .by_name(&name)
        .map_err(|e| RuntimeError::value_err(format!("zip entry '{name}': {e}")))?;
    Ok(Value::Bytes(Arc::new(read_limited(
        entry,
        limit,
        "archive.zip_read",
    )?)))
}

fn make_tar(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("archive.tar", args, 1)?;
    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        for (path, data) in input_entries("archive.tar", &args[0])? {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, data.as_slice())
                .map_err(|e| RuntimeError::io_err(format!("archive.tar: {e}")))?;
        }
        builder
            .finish()
            .map_err(|e| RuntimeError::io_err(format!("archive.tar: {e}")))?;
    }
    Ok(Value::Bytes(Arc::new(out)))
}

fn read_tar(value: &Value, limit: usize) -> Result<Vec<(String, Vec<u8>)>> {
    let input = input_bytes("archive tar", value)?;
    let mut archive = tar::Archive::new(input.as_slice());
    let mut out = Vec::new();
    let mut total = 0usize;
    for entry in archive
        .entries()
        .map_err(|e| RuntimeError::value_err(format!("invalid tar archive: {e}")))?
    {
        let entry =
            entry.map_err(|e| RuntimeError::value_err(format!("invalid tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| RuntimeError::value_err(format!("invalid tar path: {e}")))?
            .to_string_lossy()
            .replace('\\', "/");
        safe_path(&path)?;
        if entry.header().entry_type().is_file() {
            let remaining = limit.saturating_sub(total);
            let data = read_limited(entry, remaining, "archive.tar_read")?;
            total = total.saturating_add(data.len());
            out.push((path, data));
        }
    }
    Ok(out)
}

fn tar_list(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("archive.tar_list", args, 1)?;
    let names = read_tar(&args[0], DEFAULT_LIMIT)?
        .into_iter()
        .map(|(path, _)| Value::Text(path))
        .collect();
    Ok(Value::List(Shared::new(names)))
}

fn tar_read(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !(2..=3).contains(&args.len()) {
        return Err(RuntimeError::type_err(
            "archive.tar_read requires archive, path and optional output limit",
        ));
    }
    let name = expect_text("archive.tar_read", args, 1)?;
    safe_path(&name)?;
    let limit = checked_limit(args, 2, "archive.tar_read")?;
    read_tar(&args[0], limit)?
        .into_iter()
        .find(|(path, _)| path == &name)
        .map(|(_, data)| Value::Bytes(Arc::new(data)))
        .ok_or_else(|| RuntimeError::value_err(format!("tar entry '{name}' not found")))
}
