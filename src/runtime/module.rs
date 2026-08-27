use crate::error::RuntimeError;
use crate::hot_code;
use crate::opcode::{FunctionObject, Instruction, ModuleGlobalEnv};
use crate::std_modules;
use crate::value::{ModuleObject, Value};
use crate::vm::{DepPackage, Vm};
use crate::Result;
use rustc_hash::FxHashMap;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::shared::Shared;
/// 已挂到其它已加载模块的函数（`use` 引入）不得换绑到当前模块。
fn module_env_already_finalized(func: &FunctionObject) -> bool {
    func.module_env.as_ref().is_some_and(|env| env.finalized)
}

/// 把模块快照挂到函数（含体内嵌套 `Push(Function)`），使 LoadGlobal/StoreGlobal 解析模块绑定。
fn function_with_module_env(
    func: &Arc<FunctionObject>,
    env: &Arc<ModuleGlobalEnv>,
) -> Arc<FunctionObject> {
    if module_env_already_finalized(func) {
        return func.clone();
    }
    let mut f = (**func).clone();
    f.module_env = Some(env.clone());
    let mut body = (*f.body).clone();
    let mut changed = false;
    for ins in &mut body {
        if let Instruction::Push(Value::Function(inner)) = ins {
            *ins = Instruction::Push(Value::Function(function_with_module_env(inner, env)));
            changed = true;
        }
    }
    if changed {
        f.body = Arc::new(body);
        f.hot = hot_code::HotCode::encode(&f.body);
    }
    Arc::new(f)
}

fn rebind_export_value(val: &Value, env: &Arc<ModuleGlobalEnv>) -> Value {
    match val {
        Value::Function(f) => Value::Function(function_with_module_env(f, env)),
        Value::Dispatch(table) => {
            let handlers = table.borrow().handlers.clone();
            let mut hs = handlers.borrow_mut();
            for h in hs.iter_mut() {
                if let Value::Function(f) = h {
                    *h = Value::Function(function_with_module_env(f, env));
                }
            }
            drop(hs);
            Value::Dispatch(table.clone())
        }
        other => other.clone(),
    }
}

pub fn install_std(vm: &mut Vm) {
    let std_mod = std_modules::build_std_module();
    vm.register_builtin_module("std", std_mod.clone());
    vm.globals
        .insert("std".into(), Value::Module(std_mod.clone()));
    if let Err(e) = install_std_macros(vm, &std_mod) {
        // 宏库加载失败不应让解释器起不来，但测试里应立刻暴露。
        eprintln!("warning: failed to install std.macros: {}", e.message());
    }
}

/// 编译嵌入的 `macros.tive`，挂到 `std.macros`。
fn install_std_macros(vm: &mut Vm, std_mod: &Shared<ModuleObject>) -> Result<()> {
    let source = include_str!("../stdlib/macros.tive");
    let exports = run_module_source(
        vm,
        source,
        "<std.macros>",
        "macros",
        PathBuf::from("<std.macros>"),
        String::new(),
        None,
    )?;
    let macros_mod = Shared::new(ModuleObject {
        name: "macros".into(),
        exports,
        children: HashMap::new(),
        is_user: false,
    });
    std_mod
        .borrow_mut()
        .children
        .insert("macros".into(), macros_mod);
    Ok(())
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

/// 按路径段解析用户模块；加载后挂到父 `children`，使 `a.b.c` 的 getattr 链命中。
pub fn find_module_segments(vm: &mut Vm, parts: &[String]) -> Result<Value> {
    if parts.is_empty() {
        return Err(RuntimeError::msg("empty module path"));
    }
    let first = &parts[0];
    // 根段：命中缓存 / builtin / 用户根模块。
    let mut cur_val = if let Some(cached) = vm.module_cache.get(first) {
        Value::Module(cached.clone())
    } else if let Some(root) = vm.builtin_modules.get(first.as_str()) {
        Value::Module(root.clone())
    } else {
        load_user_module_segments(vm, &parts[..1])?
    };
    // 逐级 getattr；若中间缺 children 且还没加载，尝试按需加载后挂上。
    for (i, seg) in parts.iter().enumerate().skip(1) {
        let Value::Module(m) = &cur_val else {
            return Err(RuntimeError::attr_err(format!(
                "module segment `{}` is not a module",
                seg
            )));
        };
        let next = { m.borrow().get_attr(seg) };
        if let Some(next) = next {
            cur_val = next;
            continue;
        }
        // 尝试按需加载剩余段对应文件，成功后挂到当前 children。
        let sub = load_user_module_segments(vm, &parts[..=i])?;
        if let Value::Module(subm) = &sub {
            m.borrow_mut().children.insert(seg.clone(), subm.clone());
        }
        cur_val = sub;
    }
    Ok(cur_val)
}

fn load_user_module_segments(vm: &mut Vm, parts: &[String]) -> Result<Value> {
    let dotted = parts.join(".");
    if let Some(cached) = vm.module_cache.get(&dotted) {
        return Ok(Value::Module(cached.clone()));
    }
    load_user_module(vm, &dotted)
}

fn resolve_builtin_path(vm: &Vm, module_name: &str) -> Option<Shared<ModuleObject>> {
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

/// 模块初始化期间临时切换源码与包解析上下文；任何 `Result::Err` 提前返回都会恢复。
struct ModuleContextGuard<'a> {
    vm: &'a mut Vm,
    source_file: String,
    current_source: Option<Arc<str>>,
    import_base: PathBuf,
    package_id: String,
    package_root: Option<PathBuf>,
    caps: crate::caps::Capabilities,
}

impl<'a> ModuleContextGuard<'a> {
    fn new(
        vm: &'a mut Vm,
        source: &str,
        source_file: &str,
        import_base: PathBuf,
        package_id: String,
        package_root: Option<PathBuf>,
    ) -> Self {
        let previous_source_file = std::mem::replace(&mut vm.source_file, source_file.to_string());
        let previous_source = vm.current_source.replace(Arc::from(source));
        let previous_base = std::mem::replace(&mut vm.import_base, import_base);
        let previous_package_id = std::mem::replace(&mut vm.current_package_id, package_id.clone());
        let previous_package_root = std::mem::replace(&mut vm.package_root, package_root.clone());
        let active = if package_id != "__root__" {
            package_root
                .as_ref()
                .map(|root| vm.host_caps.restrict_for_dependency(root))
                .unwrap_or_else(|| vm.host_caps.clone())
        } else {
            vm.host_caps.clone()
        };
        let previous_caps = std::mem::replace(&mut vm.caps, active);
        Self {
            vm,
            source_file: previous_source_file,
            current_source: previous_source,
            import_base: previous_base,
            package_id: previous_package_id,
            package_root: previous_package_root,
            caps: previous_caps,
        }
    }
}

impl Deref for ModuleContextGuard<'_> {
    type Target = Vm;

    fn deref(&self) -> &Self::Target {
        self.vm
    }
}

impl DerefMut for ModuleContextGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.vm
    }
}

impl Drop for ModuleContextGuard<'_> {
    fn drop(&mut self) {
        self.vm.source_file = std::mem::take(&mut self.source_file);
        self.vm.current_source = self.current_source.take();
        self.vm.import_base = std::mem::take(&mut self.import_base);
        self.vm.current_package_id = std::mem::take(&mut self.package_id);
        self.vm.package_root = self.package_root.take();
        self.vm.caps = std::mem::take(&mut self.caps);
    }
}

fn run_module_source(
    vm: &mut Vm,
    source: &str,
    source_file: &str,
    package_name: &str,
    import_base: PathBuf,
    package_id: String,
    package_root: Option<PathBuf>,
) -> Result<HashMap<String, Value>> {
    let mut context = ModuleContextGuard::new(
        vm,
        source,
        source_file,
        import_base,
        package_id,
        package_root,
    );
    let vm = &mut *context;
    let compiled = crate::compile_with_context(vm, source, source_file)?;
    let snap = vm.snapshot_for_module_init();
    let exports = vm.begin_module_init(&snap, package_name);
    let module_overload_keys: Vec<String> = compiled.overload_tables.keys().cloned().collect();
    vm.load_program(compiled)?;
    let run_result = vm.run();
    run_result?;
    let module_env = Arc::new(vm.snapshot_module_global_env());
    let new_functions: HashMap<String, Arc<FunctionObject>> = vm.functions.with_ref(|m| {
        m.iter()
            .filter(|(k, _)| !snap.functions.contains_key(*k))
            .map(|(k, v)| (k.clone(), function_with_module_env(v, &module_env)))
            .collect()
    });
    for (k, v) in &new_functions {
        vm.functions.insert(k.clone(), v.clone());
    }

    // 快照里的 Function 仍是「空 globals 的 module_env」。导出函数会被
    // function_with_module_env 换掉，但模块内非 export 函数（如 via_let）
    // 仍通过 LoadGlobal 从 module_env.globals 取出旧对象 → 函数体看不到
    // 本模块 let/use。此处把快照内的函数（及 Dispatch）换成已挂 env 的版本。
    {
        let mut g = module_env.globals.borrow_mut();
        let keys: Vec<String> = g.keys().cloned().collect();
        for name in keys {
            let Some(val) = g.get(&name).cloned() else {
                continue;
            };
            if let Some(f) = new_functions.get(name.as_str()) {
                g.insert(name, Value::Function(f.clone()));
                continue;
            }
            match &val {
                Value::Function(_) | Value::Dispatch(_) => {
                    g.insert(name, rebind_export_value(&val, &module_env));
                }
                _ => {}
            }
        }
    }

    let mut new_overloads: FxHashMap<String, Vec<Arc<FunctionObject>>> = FxHashMap::default();
    for name in module_overload_keys {
        if let Some(overloads) = vm.overload_tables.get(&name) {
            let patched: Vec<Arc<FunctionObject>> = overloads
                .iter()
                .map(|f| function_with_module_env(f, &module_env))
                .collect();
            new_overloads.insert(name, patched);
        }
    }

    let mut export_map = exports.borrow().clone();
    for (name, val) in &mut export_map {
        if matches!(val, Value::None) {
            if let Some(f) = new_functions.get(name.as_str()) {
                *val = Value::Function(f.clone());
                continue;
            }
        }
        if let Value::Function(_) = val {
            if let Some(f) = new_functions.get(name.as_str()) {
                *val = Value::Function(f.clone());
                continue;
            }
        }
        *val = rebind_export_value(val, &module_env);
    }
    let new_macros: HashMap<_, _> = vm.macros.with_ref(|m| {
        m.iter()
            .filter(|(k, _)| !snap.macros.contains_key(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    });
    let new_struct_defs: HashMap<_, _> = vm.struct_defs.with_ref(|m| {
        m.iter()
            .filter(|(k, _)| !snap.struct_defs.contains_key(*k))
            .map(|(k, v)| {
                // 模块新增 StructDef：其 methods/overloads 也需挂到模块 env，
                // 否则模块内 struct 方法体看不到本模块的 let/use 绑定。
                let rebound = rebound_struct_def_methods(v.clone(), &module_env);
                (k.clone(), rebound)
            })
            .collect()
    });
    vm.finish_module_init(
        snap,
        new_functions,
        new_macros,
        new_struct_defs,
        new_overloads,
    );
    Ok(export_map)
}

/// 将 `StructDef.methods` / `overloads` 里的函数重绑到模块全局 env。
fn rebound_struct_def_methods(
    def: Arc<crate::value::StructDef>,
    module_env: &Arc<ModuleGlobalEnv>,
) -> Arc<crate::value::StructDef> {
    if def.methods.is_empty() && def.overloads.is_empty() {
        return def;
    }
    let mut d = (*def).clone();
    for f in d.methods.values_mut() {
        *f = function_with_module_env(f, module_env);
    }
    for list in d.overloads.values_mut() {
        for f in list.iter_mut() {
            *f = function_with_module_env(f, module_env);
        }
    }
    Arc::new(d)
}

fn load_user_module(vm: &mut Vm, module_name: &str) -> Result<Value> {
    let path_components: Vec<&str> = module_name.split('.').collect();
    if path_components.is_empty()
        || path_components.iter().any(|part| {
            part.is_empty()
                || *part == ".."
                || part.contains('/')
                || part.contains('\\')
                || Path::new(part).is_absolute()
        })
    {
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
        if let Some(file_path) = locate_under_root(&vm.caps, &root, &path_components)? {
            return load_file_as_module(
                vm,
                module_name,
                last,
                &file_path,
                vm.current_package_id.clone(),
                Some(root),
            );
        }
    }

    // 3) 根包：传统搜索路径（项目本地模块）
    if vm.current_package_id == "__root__" {
        if let Ok(file_path) = locate_module_file(&vm.caps, &path_components) {
            let import_base = file_path
                .parent()
                .map_or_else(|| vm.import_base.clone(), std::path::Path::to_path_buf);
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
        "Module not found: {first} (searched under package root / project search paths)"
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
        resolve_package_entry_file(vm, &binding.path, logical)?.ok_or_else(|| {
            RuntimeError::msg(format!(
                "package `{logical}` has no entry (tried [package].entry, src/main.tive, main.tive, {logical}.tive)"
            ))
        })?
    } else {
        locate_under_root(&vm.caps, &binding.path, &path_components[1..])?.ok_or_else(|| {
            let rest = path_components[1..].join("/");
            RuntimeError::msg(format!(
                "Module not found: '{rest}' under package root {}",
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
    let source = read_module_file(vm, file_path)?;
    let placeholder = Shared::new(ModuleObject::new_user(last.to_string()));
    vm.module_cache
        .insert(module_name.to_string(), placeholder.clone());
    let import_base = file_path
        .parent()
        .map_or_else(|| vm.import_base.clone(), std::path::Path::to_path_buf);
    match run_module_source(
        vm,
        &source,
        &file_path.to_string_lossy(),
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

fn resolve_package_entry_file(
    vm: &Vm,
    package_root: &Path,
    logical_name: &str,
) -> Result<Option<PathBuf>> {
    for name in ["Optive.toml"] {
        let p = package_root.join(name);
        if !vm.caps.is_file("package manifest", &p)? {
            continue;
        }
        let checked_manifest =
            secure_package_path(package_root, Path::new(name), vm.caps.fs_restricted())?;
        let text = vm
            .caps
            .read_to_string("package manifest", &checked_manifest)
            .map_err(|e| RuntimeError::msg(format!("cannot read {}: {e}", p.display())))?;
        let val: toml::Value = text
            .parse()
            .map_err(|e| RuntimeError::msg(format!("invalid {}: {e}", p.display())))?;
        if let Some(entry) = val
            .get("package")
            .and_then(|pkg| pkg.get("entry"))
            .and_then(|e| e.as_str())
        {
            let ep = secure_package_path(package_root, Path::new(entry), vm.caps.fs_restricted())?;
            if vm.caps.is_file("package entry", &ep)? {
                return Ok(Some(ep));
            }
            return Err(RuntimeError::msg(format!(
                "{} declares entry `{}` but file is missing: {}",
                p.display(),
                entry,
                ep.display()
            )));
        }
        break;
    }
    for relative in [
        PathBuf::from("src/main.tive"),
        PathBuf::from("main.tive"),
        PathBuf::from(format!("{logical_name}.tive")),
    ] {
        let candidate = secure_package_path(package_root, &relative, vm.caps.fs_restricted())?;
        if vm.caps.is_file("package entry", &candidate)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn secure_package_path(package_root: &Path, relative: &Path, strict: bool) -> Result<PathBuf> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(RuntimeError::msg(format!(
            "package entry must be relative and must not contain '..': {}",
            relative.display()
        )));
    }
    let candidate = package_root.join(relative);
    if strict {
        let package_gate = crate::caps::Capabilities::sandbox(vec![package_root.to_path_buf()]);
        return package_gate.resolve_fs_path(
            "package entry",
            candidate,
            crate::caps::FsAccess::Read,
        );
    }
    if let (Ok(root), Ok(resolved)) = (package_root.canonicalize(), candidate.canonicalize()) {
        if !resolved.starts_with(root) {
            return Err(RuntimeError::msg(format!(
                "package entry escapes package root: {}",
                relative.display()
            )));
        }
    }
    Ok(candidate)
}

fn locate_under_root(
    caps: &crate::caps::Capabilities,
    root: &Path,
    path_components: &[&str],
) -> Result<Option<PathBuf>> {
    if path_components.is_empty() {
        return Ok(None);
    }
    let Some(last) = path_components.last().copied() else {
        return Ok(None);
    };
    let prefix = &path_components[..path_components.len() - 1];
    let mut dir = root.to_path_buf();
    for part in prefix {
        dir.push(part);
    }
    let file_candidate = dir.join(format!("{last}.tive"));
    if caps.lookup_is_file("module lookup", &file_candidate)? {
        return Ok(Some(file_candidate));
    }
    let package_candidate = dir.join(last).join("main.tive");
    if caps.lookup_is_file("module lookup", &package_candidate)? {
        return Ok(Some(package_candidate));
    }
    Ok(None)
}

fn read_module_file(vm: &Vm, file_path: &Path) -> Result<String> {
    vm.caps
        .read_to_string("module import", file_path)
        .map_err(|e| {
            RuntimeError::msg(format!(
                "failed to read module file {}: {e}",
                file_path.display()
            ))
        })
}

fn locate_module_file(
    caps: &crate::caps::Capabilities,
    path_components: &[&str],
) -> Result<PathBuf> {
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
        if caps.lookup_is_file("module lookup", &file_candidate)? {
            return Ok(file_candidate);
        }
        let package_candidate = dir.join(last).join("main.tive");
        if caps.lookup_is_file("module lookup", &package_candidate)? {
            return Ok(package_candidate);
        }
    }
    let first = path_components[0];
    let rest = path_components[1..].join("/");
    Err(RuntimeError::msg(format!(
        "Module not found: '{first}' (then '{rest}') under project search paths"
    )))
}

/// 解析 import/use 字符串路径对应的脚本路径。
pub fn resolve_import_path(path: &str, base_dir: &Path) -> Result<PathBuf> {
    resolve_import_path_with_caps(path, base_dir, &crate::caps::Capabilities::full())
}

fn resolve_import_path_with_caps(
    path: &str,
    base_dir: &Path,
    caps: &crate::caps::Capabilities,
) -> Result<PathBuf> {
    let path_obj = Path::new(path);
    if path_obj.is_absolute() {
        if caps.is_file("module import", path_obj)? {
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
        if caps.is_file("module import", &candidate)? {
            return Ok(candidate);
        }
        if !path.ends_with(".tive") {
            let with_ext = candidate.with_extension("tive");
            if caps.is_file("module import", &with_ext)? {
                return Ok(with_ext);
            }
        }
        return Err(RuntimeError::msg(format!("Module file not found: {path}")));
    }
    locate_string_module_with_caps(caps, path, Some(base_dir))
}

pub fn locate_string_module(path: &str, base_dir: Option<&Path>) -> Result<PathBuf> {
    locate_string_module_with_caps(&crate::caps::Capabilities::full(), path, base_dir)
}

fn locate_string_module_with_caps(
    caps: &crate::caps::Capabilities,
    path: &str,
    base_dir: Option<&Path>,
) -> Result<PathBuf> {
    // 相对路径必须先相对 import_base / 搜索路径拼接，再交给沙箱。
    // 若先用裸 `"foo.tive"` 做检查，会按进程 cwd 判定，沙箱根是临时目录时
    // 会误报 outside roots，连 import_base 下的符号链接都检查不到。
    if let Some(base) = base_dir {
        let candidate = base.join(path);
        if caps.lookup_is_file("module lookup", &candidate)? {
            return Ok(candidate);
        }
    }
    for base in module_search_paths(base_dir) {
        let candidate = base.join(path);
        if caps.lookup_is_file("module lookup", &candidate)? {
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

pub fn load_string_module(vm: &mut Vm, path: &str) -> Result<Value> {
    // `import "example-proj"` / `import "example-proj.src.lib"`：包名可含 `-`，
    // 不能写成点号标识符；字符串若不像文件路径则按依赖模块解析。
    if looks_like_package_spec(path) {
        let parts: Vec<String> = path
            .split('.')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if !parts.is_empty() {
            return find_module_segments(vm, &parts);
        }
    }
    let file_path = resolve_import_path_with_caps(path, &vm.import_base, &vm.caps)?;
    let canonical = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.clone())
        .to_string_lossy()
        .to_string();
    if let Some(cached) = vm.module_cache.get(&canonical) {
        return Ok(Value::Module(cached.clone()));
    }
    let source = read_module_file(vm, &file_path)?;
    let alias = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module")
        .to_string();
    let import_base = file_path
        .parent()
        .map_or_else(|| vm.import_base.clone(), std::path::Path::to_path_buf);
    let exports = run_module_source(
        vm,
        &source,
        &file_path.to_string_lossy(),
        &alias,
        import_base,
        vm.current_package_id.clone(),
        vm.package_root.clone(),
    )?;
    let module = Shared::new(ModuleObject {
        name: alias.clone(),
        exports,
        children: HashMap::new(),
        is_user: true,
    });
    vm.module_cache.insert(canonical, module.clone());
    Ok(Value::Module(module))
}

fn looks_like_package_spec(path: &str) -> bool {
    let p = path.trim();
    if p.is_empty() {
        return false;
    }
    if p.starts_with("./") || p.starts_with("../") {
        return false;
    }
    if p.contains('/') || p.contains('\\') {
        return false;
    }
    if p.ends_with(".tive") {
        return false;
    }
    true
}
