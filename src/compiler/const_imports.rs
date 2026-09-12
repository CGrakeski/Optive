//! Resolve compile-time import namespaces from source modules.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::ast::{ModuleRef, Program, Stmt, UseItem};
use crate::compiler::const_eval::{CompileTimeModule, ConstContext, ConstImports};
use crate::compiler::module_interface::{
    self, canonical_json_bytes, const_value_json, digest_bytes,
};
use crate::compiler::module_resolve::{
    self, import_bind_name, locate_file_import, locate_package_entry, locate_package_module,
};
use crate::parser::Parser;
use crate::value::Value;

pub trait ConstImportSource {
    fn package_id(&self) -> &str;
    fn package_root(&self) -> Option<&Path>;
    fn import_base(&self) -> &Path;
    fn is_builtin(&self, name: &str) -> bool;
    fn dep(&self, parent: &str, name: &str) -> Option<(PathBuf, String)>;
    fn is_file(&self, path: &Path) -> bool;
    fn read(&self, path: &Path) -> Result<String, String>;
}

struct ImportCx<'a, S: ConstImportSource> {
    source: &'a S,
    package_id: String,
    package_root: Option<PathBuf>,
    import_base: PathBuf,
}

impl<'a, S: ConstImportSource> ImportCx<'a, S> {
    fn from_source(source: &'a S) -> Self {
        Self {
            source,
            package_id: source.package_id().to_string(),
            package_root: source.package_root().map(Path::to_path_buf),
            import_base: source.import_base().to_path_buf(),
        }
    }

    fn is_builtin(&self, name: &str) -> bool {
        self.source.is_builtin(name)
    }

    fn dep(&self, name: &str) -> Option<(PathBuf, String)> {
        self.source.dep(&self.package_id, name)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.source.is_file(path)
    }

    fn read(&self, path: &Path) -> Result<String, String> {
        self.source.read(path)
    }
}

pub fn collect(
    program: &Program,
    source: &impl ConstImportSource,
    visiting: &mut BTreeSet<String>,
) -> Result<ConstImports, String> {
    collect_in(program, &ImportCx::from_source(source), visiting)
}

pub fn fingerprint(imports: &ConstImports) -> String {
    let mut values = serde_json::Map::new();
    for (name, value) in &imports.values {
        if let Ok(encoded) = const_value_json(value) {
            values.insert(name.clone(), encoded);
        }
    }
    let mut modules = serde_json::Map::new();
    for (name, module) in &imports.modules {
        let mut fields = serde_json::Map::new();
        for (field, value) in &module.values {
            if let Ok(encoded) = const_value_json(value) {
                fields.insert(field.clone(), encoded);
            }
        }
        modules.insert(name.clone(), serde_json::Value::Object(fields));
    }
    digest_bytes(&canonical_json_bytes(&serde_json::json!({
        "modules": modules,
        "values": values,
    })))
}

fn collect_in<S: ConstImportSource>(
    program: &Program,
    cx: &ImportCx<'_, S>,
    visiting: &mut BTreeSet<String>,
) -> Result<ConstImports, String> {
    let mut imported = ConstImports::default();
    for located in &program.stmts {
        match &located.stmt {
            Stmt::Use { module, items } => {
                if let Some(values) = load_module_consts(cx, &module_ref_parts(module), visiting)? {
                    import_use_items(items, &values, &mut imported.values);
                }
            }
            Stmt::Import {
                path,
                path_is_string,
                alias,
            } => {
                let resolved = if *path_is_string {
                    load_file_consts(cx, path, visiting)?
                } else {
                    load_module_consts(cx, &path.split('.').collect::<Vec<_>>(), visiting)?
                };
                if let Some(values) = resolved {
                    let bind = import_bind_name(path, *path_is_string, alias.as_deref());
                    imported
                        .modules
                        .insert(bind.clone(), CompileTimeModule::new(bind, values));
                }
            }
            _ => {}
        }
    }
    Ok(imported)
}

fn module_ref_parts(module: &ModuleRef) -> Vec<&str> {
    match module {
        ModuleRef::Qualified(parts) => parts.iter().map(String::as_str).collect(),
        ModuleRef::FilePath { path, attrs } => std::iter::once(path.as_str())
            .chain(attrs.iter().map(String::as_str))
            .collect(),
    }
}

fn load_module_consts<S: ConstImportSource>(
    cx: &ImportCx<'_, S>,
    parts: &[&str],
    visiting: &mut BTreeSet<String>,
) -> Result<Option<HashMap<String, Value>>, String> {
    let Some(first) = parts.first().copied() else {
        return Ok(None);
    };
    if cx.is_builtin(first) {
        return Ok(None);
    }
    let (package_id, root, module_parts, entry_name) =
        if let Some((root, package_id)) = cx.dep(first) {
            (package_id, root, &parts[1..], Some(first))
        } else if let Some(root) = cx.package_root.clone() {
            (cx.package_id.clone(), root, parts, None)
        } else {
            return Ok(None);
        };
    let file = if module_parts.is_empty() {
        let declared = declared_entry(cx, &root);
        locate_package_entry(
            &root,
            entry_name.unwrap_or(first),
            declared.as_deref(),
            |path| Ok::<_, String>(cx.is_file(path)),
        )?
    } else {
        locate_package_module(&root, module_parts, |path| {
            Ok::<_, String>(cx.is_file(path))
        })?
    };
    let Some(file) = file else {
        return Ok(None);
    };
    consts_of_file(cx.source, &package_id, Some(root), &file, visiting)
}

fn load_file_consts<S: ConstImportSource>(
    cx: &ImportCx<'_, S>,
    path: &str,
    visiting: &mut BTreeSet<String>,
) -> Result<Option<HashMap<String, Value>>, String> {
    let file = locate_file_import(&cx.import_base, path, |candidate| {
        Ok::<_, String>(cx.is_file(candidate))
    })?;
    let Some(file) = file else {
        return Ok(None);
    };
    consts_of_file(
        cx.source,
        &cx.package_id,
        cx.package_root.clone(),
        &file,
        visiting,
    )
}

fn consts_of_file<S: ConstImportSource>(
    source: &S,
    package_id: &str,
    package_root: Option<PathBuf>,
    file: &Path,
    visiting: &mut BTreeSet<String>,
) -> Result<Option<HashMap<String, Value>>, String> {
    let key = format!("{package_id}:{}", file.to_string_lossy().replace('\\', "/"));
    if !visiting.insert(key.clone()) {
        return Ok(Some(HashMap::new()));
    }
    let text = source.read(file)?;
    let program = Parser::parse(&text).map_err(|error| error.to_string())?;
    let nested = ImportCx {
        source,
        package_id: package_id.to_string(),
        package_root,
        import_base: file
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
    };
    let imported = collect_in(&program, &nested, visiting)?;
    visiting.remove(&key);
    let consts =
        ConstContext::build_with_imports(&program, imported).map_err(|error| error.to_string())?;
    Ok(Some(
        module_interface::compile_time_values(&module_interface::exports_of(&program, &consts)?)?
            .into_iter()
            .collect(),
    ))
}

fn declared_entry<S: ConstImportSource>(cx: &ImportCx<'_, S>, root: &Path) -> Option<String> {
    let manifest = root.join("Optive.toml");
    if !cx.is_file(&manifest) {
        return None;
    }
    let text = cx.read(&manifest).ok()?;
    module_resolve::declared_package_entry(&text).ok().flatten()
}

fn import_use_items(
    items: &[UseItem],
    values: &HashMap<String, Value>,
    dest: &mut HashMap<String, Value>,
) {
    for item in items {
        if let Some(value) = values.get(&item.name) {
            dest.insert(
                item.alias.clone().unwrap_or_else(|| item.name.clone()),
                value.clone(),
            );
        }
    }
}
