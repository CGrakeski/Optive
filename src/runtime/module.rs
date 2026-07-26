use crate::codegen::Generator;
use crate::error::RuntimeError;
use crate::opcode::FunctionObject;
use crate::parser::Parser;
use crate::std_modules;
use crate::value::{ModuleObject, Value};
use crate::vm::{DepPackage, Vm};
use crate::Result;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub fn install_std(vm: &mut Vm) {
    let std_mod = std_modules::build_std_module();
    vm.register_builtin_module("std", std_mod.clone());
    vm.globals.insert("std".into(), Value::Module(std_mod));
}

pub fn find_module(vm: &mut Vm, module_name: &str) -> Result<Value> {
    if module_name.is_empty() {
        return Err(RuntimeError::msg("empty module name"));
    }
    if let Some(stripped) = module_name.strip_prefix("@str:") {
        return load_string_module(vm, stripped);
    }
    if let Some(cached) = vm.module_cache.get(module_name) {
        return Ok(Value::Module(cached.clone()));
    }
    if let Some(mod_val) = resolve_builtin_path(vm, module_name) {
        vm.module_cache
            .insert(module_name.to_string(), mod_val.clone());
        return Ok(Value::Module(mod_val));
    }
    load_user_module(vm, module_name)
}

fn resolve_builtin_path(vm: &Vm, module_name: &str) -> Option<Rc<RefCell<ModuleObject>>> {
    let parts: Vec<&str> = module_name.split('.').collect();
    if parts.is_empty() {
        return None;
    }
    let root = vm.builtin_modules.get(parts[0])?;
    let mut current = root.clone();
    for part in parts.iter().skip(1) {
        let next = current.borrow().children.get(*part)?.clone();
        current = next;
    }
    Some(current)
}

fn run_module_source(
    vm: &mut Vm,
    source: &str,
    package_name: &str,
    import_base: PathBuf,
    package_id: String,
    package_root: Option<PathBuf>,
) -> Result<HashMap<String, Value>> {
    let program = Parser::parse(source).map_err(|e| RuntimeError::msg(e.to_string()))?;
    let compiled = Generator::new().compile(&program)?;
    let snap = vm.snapshot_for_module_init();
    let exports = vm.begin_module_init(&snap, package_name);
    let prev_base = vm.import_base.clone();
    let prev_pkg = vm.current_package_id.clone();
    let prev_root = vm.package_root.clone();
    vm.import_base = import_base;
    vm.current_package_id = package_id;
    vm.package_root = package_root;
    vm.load_program(compiled)?;
    let run_result = vm.run();
    vm.import_base = prev_base;
    vm.current_package_id = prev_pkg;
    vm.package_root = prev_root;
    run_result?;
    let module_env = Rc::new(vm.snapshot_module_global_env());
    let new_functions: HashMap<String, Rc<FunctionObject>> = vm
        .functions
        .iter()
        .filter(|(k, _)| !snap.functions.contains_key(*k))
        .map(|(k, v)| {
            let mut func = (**v).clone();
            func.module_env = Some(module_env.clone());
            (k.clone(), Rc::new(func))
        })
        .collect();
    for (k, v) in &new_functions {
        vm.functions.insert(k.clone(), v.clone());
    }
    let mut export_map = exports.borrow().clone();
    for (name, val) in export_map.iter_mut() {
        if let Value::Function(_) = val {
            if let Some(f) = new_functions.get(name.as_str()) {
                *val = Value::Function(f.clone());
            }
        }
    }
    let new_macros: HashMap<_, _> = vm
        .macros
        .iter()
        .filter(|(k, _)| !snap.macros.contains_key(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let new_struct_defs: HashMap<_, _> = vm
        .struct_defs
        .iter()
        .filter(|(k, _)| !snap.struct_defs.contains_key(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    vm.finish_module_init(snap, new_functions, new_macros, new_struct_defs);
    Ok(export_map)
}

fn load_user_module(vm: &mut Vm, module_name: &str) -> Result<Value> {
    let path_components: Vec<&str> = module_name.split('.').collect();
    if path_components.is_empty() {
        return Err(RuntimeError::value_err(format!(
            "invalid module name: {module_name}"
        )));
    }
    let first = path_components[0];
    let last = path_components
        .last()
        .copied()
        .ok_or_else(|| RuntimeError::value_err(format!("invalid module name: {module_name}")))?;

    // 1) 当前包声明的依赖
    if let Some(binding) = vm
        .dep_map
        .get(&(vm.current_package_id.clone(), first.to_string()))
        .cloned()
    {
        return load_from_package(vm, module_name, &path_components, &binding);
    }

    // 2) 当前包内相对包根的模块（依赖包或根项目）
    if let Some(root) = vm.package_root.clone() {
        if let Some(file_path) = locate_under_root(&root, &path_components) {
            return load_file_as_module(vm, module_name, last, &file_path, vm.current_package_id.clone(), Some(root));
        }
    }

    // 3) 根包：传统搜索路径（项目本地模块）
    if vm.current_package_id == "__root__" {
        if let Ok(file_path) = locate_module_file(&path_components) {
            let import_base = file_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| vm.import_base.clone());
            return load_file_as_module(
                vm,
                module_name,
                last,
                &file_path,
                "__root__".into(),
                vm.package_root.clone().or(Some(import_base)),
            );
        }
    }

    // 4) 未声明依赖
    if !vm.dep_map.is_empty() {
        return Err(RuntimeError::msg(format!(
            "undeclared dependency `{first}` (not in this package's Optive.toml [dependencies]); \
             transitive installs are not importable"
        )));
    }

    Err(RuntimeError::msg(format!(
        "Module not found: {}",
        path_components.join(".")
    )))
}

fn load_from_package(
    vm: &mut Vm,
    module_name: &str,
    path_components: &[&str],
    binding: &DepPackage,
) -> Result<Value> {
    let logical = path_components[0];
    let file_path = if path_components.len() == 1 {
        resolve_package_entry_file(&binding.path, logical).ok_or_else(|| {
            RuntimeError::msg(format!(
                "package `{logical}` has no entry (tried [package].entry, main.tive, {logical}.tive)"
            ))
        })?
    } else {
        locate_under_root(&binding.path, &path_components[1..]).ok_or_else(|| {
            RuntimeError::msg(format!(
                "Module not found: {} (under package root {})",
                path_components.join("."),
                binding.path.display()
            ))
        })?
    };
    let last = path_components.last().copied().unwrap_or(logical);
    load_file_as_module(
        vm,
        module_name,
        last,
        &file_path,
        binding.id.clone(),
        Some(binding.path.clone()),
    )
}

fn load_file_as_module(
    vm: &mut Vm,
    module_name: &str,
    last: &str,
    file_path: &Path,
    package_id: String,
    package_root: Option<PathBuf>,
) -> Result<Value> {
    let source = read_module_file(file_path)?;
    let placeholder = Rc::new(RefCell::new(ModuleObject::new_user(
        last.to_string(),
        module_name.to_string(),
    )));
    vm.module_cache
        .insert(module_name.to_string(), placeholder.clone());
    let import_base = file_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| vm.import_base.clone());
    match run_module_source(
        vm,
        &source,
        module_name,
        import_base,
        package_id,
        package_root,
    ) {
        Ok(exports) => {
            placeholder.borrow_mut().exports = exports;
            Ok(Value::Module(placeholder))
        }
        Err(e) => {
            vm.module_cache.remove(module_name);
            Err(e)
        }
    }
}

fn resolve_package_entry_file(package_root: &Path, logical_name: &str) -> Option<PathBuf> {
    for name in ["Optive.toml", "optive.toml"] {
        let p = package_root.join(name);
        if p.is_file() {
            if let Ok(text) = fs::read_to_string(&p) {
                if let Ok(val) = text.parse::<toml::Value>() {
                    if let Some(entry) = val
                        .get("package")
                        .and_then(|p| p.get("entry"))
                        .and_then(|e| e.as_str())
                    {
                        let ep = package_root.join(entry);
                        if ep.is_file() {
                            return Some(ep);
                        }
                    }
                }
            }
            break;
        }
    }
    let main = package_root.join("main.tive");
    if main.is_file() {
        return Some(main);
    }
    let named = package_root.join(format!("{logical_name}.tive"));
    if named.is_file() {
        return Some(named);
    }
    None
}

fn locate_under_root(root: &Path, path_components: &[&str]) -> Option<PathBuf> {
    if path_components.is_empty() {
        return None;
    }
    let last = *path_components.last()?;
    let prefix = &path_components[..path_components.len() - 1];
    let mut dir = root.to_path_buf();
    for part in prefix {
        dir.push(part);
    }
    let file_candidate = dir.join(format!("{last}.tive"));
    if file_candidate.is_file() {
        return Some(file_candidate);
    }
    let package_candidate = dir.join(last).join("main.tive");
    if package_candidate.is_file() {
        return Some(package_candidate);
    }
    None
}

fn read_module_file(file_path: &Path) -> Result<String> {
    fs::read_to_string(file_path).map_err(|e| {
        RuntimeError::msg(format!(
            "failed to read module file {}: {e}",
            file_path.display()
        ))
    })
}

fn locate_module_file(path_components: &[&str]) -> Result<PathBuf> {
    let last = path_components
        .last()
        .copied()
        .ok_or_else(|| RuntimeError::value_err("empty module path"))?;
    let prefix = &path_components[..path_components.len() - 1];
    for base in module_search_paths(None) {
        let mut dir = base.clone();
        for part in prefix {
            dir.push(part);
        }
        let file_candidate = dir.join(format!("{last}.tive"));
        if file_candidate.is_file() {
            return Ok(file_candidate);
        }
        let package_candidate = dir.join(last).join("main.tive");
        if package_candidate.is_file() {
            return Ok(package_candidate);
        }
    }
    Err(RuntimeError::msg(format!(
        "Module not found: {}",
        path_components.join(".")
    )))
}

/// 解析 import/use 字符串路径对应的脚本路径。
pub fn resolve_import_path(path: &str, base_dir: &Path) -> Result<PathBuf> {
    let path_obj = Path::new(path);
    if path_obj.is_absolute() {
        if path_obj.is_file() {
            return Ok(path_obj.to_path_buf());
        }
        return Err(RuntimeError::msg(format!("Module file not found: {path}")));
    }
    if path.starts_with("./")
        || path.starts_with("../")
        || path.contains('/')
        || path.contains('\\')
    {
        let candidate = base_dir.join(path);
        if candidate.is_file() {
            return Ok(candidate.canonicalize().unwrap_or(candidate));
        }
        if !path.ends_with(".tive") {
            let with_ext = candidate.with_extension("tive");
            if with_ext.is_file() {
                return Ok(with_ext.canonicalize().unwrap_or(with_ext));
            }
        }
        return Err(RuntimeError::msg(format!("Module file not found: {path}")));
    }
    locate_string_module(path, Some(base_dir))
}

pub fn locate_string_module(path: &str, base_dir: Option<&Path>) -> Result<PathBuf> {
    let path_obj = Path::new(path);
    if path_obj.is_file() {
        return Ok(path_obj.to_path_buf());
    }
    if let Some(base) = base_dir {
        let candidate = base.join(path);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    for base in module_search_paths(base_dir) {
        let candidate = base.join(path);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(RuntimeError::msg(format!("Module file not found: {path}")))
}

fn module_search_paths(base_dir: Option<&Path>) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut paths = Vec::new();
    if let Some(base) = base_dir {
        paths.push(base.to_path_buf());
    }
    if let Ok(env_paths) = std::env::var("OPTIVE_PATH") {
        #[cfg(windows)]
        let sep = ';';
        #[cfg(not(windows))]
        let sep = ':';
        for part in env_paths.split(sep) {
            let part = part.trim();
            if !part.is_empty() {
                paths.push(PathBuf::from(part));
            }
        }
    }
    paths.push(cwd.clone());
    paths.push(cwd.join("examples"));
    paths.push(cwd.join("std"));
    paths.push(cwd.join("modules"));
    paths.push(cwd.join("lib"));
    // 不再把 OPTIVE_DEPS / deps 当作全局可 import 搜索路径（避免幽灵依赖）。
    // LOCAL_DEPS 调试时由 DepMap 注入具体包根。
    paths
}

fn load_string_module(vm: &mut Vm, path: &str) -> Result<Value> {
    let cache_key = format!("@str:{path}");
    if let Some(cached) = vm.module_cache.get(&cache_key) {
        return Ok(Value::Module(cached.clone()));
    }
    let file_path = resolve_import_path(path, &vm.import_base)?;
    let canonical = file_path.to_string_lossy().to_string();
    let cache_key = format!("@str:{canonical}");
    if let Some(cached) = vm.module_cache.get(&cache_key) {
        return Ok(Value::Module(cached.clone()));
    }
    let source = read_module_file(&file_path)?;
    let alias = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module")
        .to_string();
    let import_base = file_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| vm.import_base.clone());
    let exports = run_module_source(
        vm,
        &source,
        &alias,
        import_base,
        vm.current_package_id.clone(),
        vm.package_root.clone(),
    )?;
    let module = Rc::new(RefCell::new(ModuleObject {
        name: alias.clone(),
        full_name: canonical.clone(),
        exports,
        children: HashMap::new(),
        is_user: true,
    }));
    vm.module_cache.insert(cache_key, module.clone());
    Ok(Value::Module(module))
}
