//! Ahead-of-run module builder and interface emitter.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::deps;
use super::lock::ROOT_PARENT;
use super::manifest::find_project;
use super::resolve::DepMap;
use optive::ast::{ModuleRef, Program, Stmt, UseItem};
use optive::bc_cache;
use optive::compiler::bundle::{BundleBinding, BundleImage, BundleModule};
use optive::compiler::const_eval::{CompileTimeModule, ConstContext, ConstImports};
use optive::compiler::module_interface::{
    self, compile_time_values, interface_digest_of, ModuleInterface, INTERFACE_FORMAT,
};
use optive::compiler::module_resolve::{
    self, import_bind_name, is_builtin_root, locate_file_import, locate_package_entry,
    locate_package_module,
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Emit {
    #[default]
    All,
    Bytecode,
    Interface,
}

impl Emit {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "all" => Ok(Self::All),
            "bytecode" => Ok(Self::Bytecode),
            "interface" => Ok(Self::Interface),
            _ => Err("--emit must be `bytecode`, `interface`, or `all`".into()),
        }
    }

    fn write_bytecode(self) -> bool {
        matches!(self, Self::All | Self::Bytecode)
    }

    fn write_interface(self) -> bool {
        matches!(self, Self::All | Self::Interface)
    }
}

struct BuildOptions {
    explain: bool,
    clean: bool,
    bundle: bool,
    all_modules: bool,
    emit: Emit,
    jobs: usize,
    executable: bool,
    target: Option<TargetSpec>,
    output: Option<PathBuf>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            explain: false,
            clean: false,
            bundle: false,
            all_modules: false,
            emit: Emit::default(),
            jobs: default_jobs(),
            executable: false,
            target: None,
            output: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TargetSpec {
    Triple {
        raw: String,
        components: Vec<String>,
    },
    Json(PathBuf),
}

impl TargetSpec {
    fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if value.is_empty() {
            return Err("--target cannot be empty".into());
        }
        let path = PathBuf::from(value);
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            return Ok(Self::Json(path));
        }
        let components: Vec<_> = value.split('-').map(str::to_string).collect();
        if components.len() < 2 || components.iter().any(String::is_empty) {
            return Err(
                "--target must be a hyphen-separated target or a .json target specification".into(),
            );
        }
        Ok(Self::Triple {
            raw: value.to_string(),
            components,
        })
    }

    fn cargo_arg(&self) -> String {
        match self {
            Self::Triple { raw, .. } => raw.clone(),
            Self::Json(path) => path.to_string_lossy().into_owned(),
        }
    }
}

#[derive(Serialize)]
struct BundleEntry {
    package_id: String,
    module: String,
    interface: String,
    bytecode: Option<String>,
    dependencies: Vec<String>,
}

#[derive(Serialize)]
struct Bundle {
    format: u32,
    compiler: String,
    entries: Vec<BundleEntry>,
}

#[derive(Clone, Deserialize, Serialize)]
struct BuildState {
    format: u32,
    artifacts: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct ModuleKey {
    package_id: String,
    relative: String,
}

struct ModuleNode {
    key: ModuleKey,
    root: PathBuf,
    file: PathBuf,
    source: String,
    program: Program,
    dependencies: Vec<ModuleKey>,
}

pub fn cmd_build(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (opts, path) = parse_options(args)?;
    if (opts.bundle || opts.executable) && !opts.emit.write_bytecode() {
        return Err("--bundle/--exe requires bytecode; use --emit bytecode or --emit all".into());
    }
    if opts.target.is_some() && !opts.executable {
        return Err("--target requires --exe".into());
    }
    if opts.output.is_some() && !opts.executable {
        return Err("--output requires --exe".into());
    }
    let project = find_project(path.as_deref())?;
    let ensured = deps::ensure_for_run(&project)?;
    let optive_dir = project.root.join(".optive");
    let live = optive_dir.join("build");
    clean_temp_build_dirs(&optive_dir);
    if opts.clean && live.is_dir() {
        fs::remove_dir_all(&live)?;
    }
    let staging = optive_dir.join(format!("build.staging-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(staging.join("modules"))?;
    fs::create_dir_all(staging.join("interfaces"))?;

    let build_result =
        (|| -> Result<(usize, usize, Option<String>), Box<dyn std::error::Error>> {
            let mut roots = BTreeMap::from([(ROOT_PARENT.to_string(), project.root.clone())]);
            let mut seen = BTreeSet::new();
            for binding in ensured.dep_map.values() {
                if seen.insert(binding.id.clone()) {
                    roots.insert(binding.id.clone(), binding.path.clone());
                }
            }

            let mut modules = BTreeMap::new();
            for (package_id, root) in &roots {
                for file in source_files(root)? {
                    let relative = normalized_relative(root, &file)?;
                    let source = fs::read_to_string(&file)?;
                    let program = optive::parse_program(&source)?;
                    let key = ModuleKey {
                        package_id: package_id.clone(),
                        relative,
                    };
                    modules.insert(
                        key.clone(),
                        ModuleNode {
                            key,
                            root: root.clone(),
                            file,
                            source,
                            program,
                            dependencies: Vec::new(),
                        },
                    );
                }
            }
            let keys: Vec<_> = modules.keys().cloned().collect();
            for key in keys {
                let dependencies = resolve_dependencies(
                    modules.get(&key).expect("module exists"),
                    &modules,
                    &roots,
                    &ensured.dep_map,
                )?;
                modules.get_mut(&key).expect("module exists").dependencies = dependencies;
            }
            if !opts.all_modules {
                let keep = reachable_modules(
                    &modules,
                    &roots,
                    project.manifest.package.entry.as_deref(),
                    &project.manifest.package.name,
                )?;
                modules.retain(|key, _| keep.contains(key));
            }
            topological_order(&modules)?;
            let waves = ready_waves(&modules);

            let mut graph_entries = Vec::new();
            let mut interfaces: HashMap<ModuleKey, ModuleInterface> = HashMap::new();
            let mut compiled_bytecode: HashMap<ModuleKey, Vec<u8>> = HashMap::new();
            let mut live_artifacts = BTreeSet::new();
            let mut built = 0usize;
            let mut skipped = 0usize;
            let mut standalone_artifact = None;
            let prev = live.is_dir().then_some(live.as_path());
            for wave in waves {
                let outputs = build_wave(
                    &wave,
                    &modules,
                    &interfaces,
                    &roots,
                    &ensured.dep_map,
                    prev,
                    &opts,
                )?;
                for output in outputs {
                    let object_id = object_id_of(&output.key);
                    if opts.emit.write_bytecode() {
                        live_artifacts.insert(format!("modules/{object_id}.tivc"));
                        if let Some(bytes) = &output.bytecode {
                            atomic_write(
                                &staging.join("modules").join(format!("{object_id}.tivc")),
                                bytes,
                            )?;
                        }
                    }
                    if opts.emit.write_interface() {
                        live_artifacts.insert(format!("interfaces/{object_id}.tivi"));
                        atomic_write(
                            &staging.join("interfaces").join(format!("{object_id}.tivi")),
                            &serde_json::to_vec_pretty(&output.interface)?,
                        )?;
                    }
                    if output.hit {
                        skipped += 1;
                    } else {
                        built += 1;
                    }
                    if opts.explain {
                        println!(
                            "{} {}:{}",
                            if output.hit { "HIT " } else { "BUILD" },
                            output.key.package_id,
                            output.key.relative
                        );
                    }
                    graph_entries.push(BundleEntry {
                        package_id: output.key.package_id.clone(),
                        module: output.key.relative.clone(),
                        interface: output.interface.interface_digest.clone(),
                        bytecode: None,
                        dependencies: modules
                            .get(&output.key)
                            .expect("module exists")
                            .dependencies
                            .iter()
                            .map(display_key)
                            .collect(),
                    });
                    if let Some(bytes) = output.bytecode {
                        compiled_bytecode.insert(output.key.clone(), bytes);
                    }
                    interfaces.insert(output.key, output.interface);
                }
            }
            graph_entries
                .sort_by(|a, b| (&a.package_id, &a.module).cmp(&(&b.package_id, &b.module)));
            let graph = Bundle {
                format: 1,
                compiler: env!("CARGO_PKG_VERSION").to_string(),
                entries: graph_entries,
            };
            atomic_write(
                &staging.join("graph.json"),
                &serde_json::to_vec_pretty(&graph)?,
            )?;
            live_artifacts.insert("graph.json".into());
            if opts.bundle || opts.executable {
                let image = make_bundle_image(
                    &project,
                    &modules,
                    &interfaces,
                    &compiled_bytecode,
                    &roots,
                    &ensured.dep_map,
                )?;
                let bundle = image.encode()?;
                if opts.bundle {
                    atomic_write(&staging.join("app.tivb"), &bundle)?;
                    live_artifacts.insert("app.tivb".into());
                }
                if opts.executable {
                    let runner = runner_path(opts.target.as_ref())?;
                    let runner_bytes = fs::read(&runner)?;
                    let executable_name = standalone_name(&runner);
                    let executable = optive::compiler::standalone::attach(&runner_bytes, &bundle);
                    let destination = staging.join(&executable_name);
                    atomic_write(&destination, &executable)?;
                    fs::set_permissions(&destination, fs::metadata(&runner)?.permissions())?;
                    live_artifacts.insert(executable_name.clone());
                    standalone_artifact = Some(executable_name);
                }
            }
            atomic_write(
                &staging.join("state.json"),
                &serde_json::to_vec_pretty(&BuildState {
                    format: 1,
                    artifacts: live_artifacts,
                })?,
            )?;
            commit_build(&live, &staging)?;
            Ok((built, skipped, standalone_artifact))
        })();

    if build_result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    let (built, skipped, standalone_artifact) = build_result?;
    if let Some(output) = &opts.output {
        let artifact = standalone_artifact
            .as_deref()
            .ok_or("standalone build did not produce an executable")?;
        let source = live.join(artifact);
        let destination = if output.is_dir() {
            output.join(
                source
                    .file_name()
                    .ok_or("standalone output has no file name")?,
            )
        } else {
            output.clone()
        };
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
    }
    println!(
        "Built {built} module(s), reused {skipped}; output {}",
        live.display()
    );
    Ok(())
}

fn parse_options(args: &[String]) -> Result<(BuildOptions, Option<PathBuf>), String> {
    let mut opts = BuildOptions::default();
    let mut path = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--explain" => opts.explain = true,
            "--clean" => opts.clean = true,
            "--bundle" => opts.bundle = true,
            "--exe" => opts.executable = true,
            "--all-modules" => opts.all_modules = true,
            "--jobs" => {
                i += 1;
                opts.jobs = parse_jobs(args.get(i).ok_or("--jobs requires a value")?)?;
            }
            "--emit" => {
                i += 1;
                opts.emit = Emit::parse(args.get(i).ok_or("--emit requires a value")?)?;
            }
            "--target" => {
                i += 1;
                opts.target = Some(TargetSpec::parse(
                    args.get(i).ok_or("--target requires a value")?,
                )?);
            }
            "--output" => {
                i += 1;
                opts.output = Some(PathBuf::from(
                    args.get(i).ok_or("--output requires a value")?,
                ));
            }
            value if value.starts_with("--jobs=") => {
                opts.jobs = parse_jobs(value.trim_start_matches("--jobs="))?;
            }
            value if value.starts_with("--emit=") => {
                opts.emit = Emit::parse(value.trim_start_matches("--emit="))?;
            }
            value if value.starts_with("--target=") => {
                opts.target = Some(TargetSpec::parse(value.trim_start_matches("--target="))?);
            }
            value if value.starts_with("--output=") => {
                opts.output = Some(PathBuf::from(value.trim_start_matches("--output=")));
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown build option: {value}"))
            }
            value if path.is_none() => path = Some(PathBuf::from(value)),
            _ => return Err("build accepts at most one project path".into()),
        }
        i += 1;
    }
    Ok((opts, path))
}

fn parse_jobs(value: &str) -> Result<usize, String> {
    let jobs = value
        .parse::<usize>()
        .map_err(|_| "--jobs must be a positive integer".to_string())?;
    if jobs == 0 {
        return Err("--jobs must be at least 1".into());
    }
    Ok(jobs)
}

fn runner_path(target: Option<&TargetSpec>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let Some(target) = target else {
        return Ok(std::env::current_exe()?);
    };
    let manifest_dir = std::env::var_os("OPTIVE_RUNNER_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let manifest = manifest_dir.join("Cargo.toml");
    if !manifest.is_file() {
        return Err(format!(
            "cannot cross-build the Optive runner: {} is missing; set OPTIVE_RUNNER_MANIFEST_DIR",
            manifest.display()
        )
        .into());
    }
    let target_arg = target.cargo_arg();
    let output = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--bin",
            "Optive",
            "--target",
            &target_arg,
            "--message-format=json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()?;
    if !output.status.success() {
        let rendered = cargo_rendered_diagnostics(&output.stdout);
        return Err(format!(
            "failed to build runner for `{target_arg}`:\n{}{}",
            String::from_utf8_lossy(&output.stderr),
            rendered
        )
        .into());
    }
    cargo_executable(&output.stdout)
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!("Cargo did not report an executable for target `{target_arg}`").into()
        })
}

fn cargo_executable(stdout: &[u8]) -> Option<String> {
    for line in String::from_utf8_lossy(stdout).lines().rev() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message.get("reason").and_then(serde_json::Value::as_str) == Some("compiler-artifact")
            && message
                .pointer("/target/name")
                .and_then(serde_json::Value::as_str)
                == Some("Optive")
        {
            if let Some(executable) = message
                .get("executable")
                .and_then(serde_json::Value::as_str)
            {
                return Some(executable.to_string());
            }
        }
    }
    None
}

fn cargo_rendered_diagnostics(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|message| {
            message
                .get("message")
                .and_then(|message| message.get("rendered"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn standalone_name(runner: &Path) -> String {
    runner
        .extension()
        .and_then(|extension| extension.to_str())
        .map_or_else(|| "app".to_string(), |extension| format!("app.{extension}"))
}

fn default_jobs() -> usize {
    thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}

fn object_id_of(key: &ModuleKey) -> String {
    digest(format!("{}\0{}", key.package_id, key.relative).as_bytes())
}

struct ModuleOutput {
    key: ModuleKey,
    interface: ModuleInterface,
    bytecode: Option<Vec<u8>>,
    hit: bool,
}

fn ready_waves(modules: &BTreeMap<ModuleKey, ModuleNode>) -> Vec<Vec<ModuleKey>> {
    let mut indegree = BTreeMap::new();
    let mut dependents: BTreeMap<ModuleKey, Vec<ModuleKey>> = BTreeMap::new();
    for (key, node) in modules {
        indegree.insert(key.clone(), node.dependencies.len());
        for dependency in &node.dependencies {
            dependents
                .entry(dependency.clone())
                .or_default()
                .push(key.clone());
        }
    }
    let mut ready: Vec<_> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(key, _)| key.clone())
        .collect();
    ready.sort();
    let mut waves = Vec::new();
    while !ready.is_empty() {
        let wave = std::mem::take(&mut ready);
        for key in &wave {
            if let Some(children) = dependents.get(key) {
                for child in children {
                    if let Some(degree) = indegree.get_mut(child) {
                        *degree -= 1;
                        if *degree == 0 {
                            ready.push(child.clone());
                        }
                    }
                }
            }
        }
        ready.sort();
        waves.push(wave);
    }
    waves
}

fn build_wave(
    wave: &[ModuleKey],
    modules: &BTreeMap<ModuleKey, ModuleNode>,
    interfaces: &HashMap<ModuleKey, ModuleInterface>,
    roots: &BTreeMap<String, PathBuf>,
    dep_map: &DepMap,
    prev: Option<&Path>,
    opts: &BuildOptions,
) -> Result<Vec<ModuleOutput>, Box<dyn std::error::Error>> {
    if opts.jobs <= 1 || wave.len() <= 1 {
        return wave
            .iter()
            .map(|key| {
                build_one(
                    modules.get(key).expect("wave module exists"),
                    interfaces,
                    modules,
                    roots,
                    dep_map,
                    prev,
                    opts,
                )
                .map_err(|error| error.into())
            })
            .collect();
    }
    thread::scope(|scope| {
        let handles: Vec<_> = wave
            .iter()
            .map(|key| {
                scope.spawn(|| {
                    build_one(
                        modules.get(key).expect("wave module exists"),
                        interfaces,
                        modules,
                        roots,
                        dep_map,
                        prev,
                        opts,
                    )
                })
            })
            .collect();
        let mut outputs = Vec::new();
        let mut errors = Vec::new();
        for handle in handles {
            match handle.join().expect("build worker") {
                Ok(output) => outputs.push(output),
                Err(error) => errors.push(error),
            }
        }
        if let Some(error) = errors.into_iter().min() {
            return Err(error.into());
        }
        outputs.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(outputs)
    })
}

fn build_one(
    node: &ModuleNode,
    interfaces: &HashMap<ModuleKey, ModuleInterface>,
    modules: &BTreeMap<ModuleKey, ModuleNode>,
    roots: &BTreeMap<String, PathBuf>,
    dep_map: &DepMap,
    prev: Option<&Path>,
    opts: &BuildOptions,
) -> Result<ModuleOutput, String> {
    let imported = imported_const_imports(node, interfaces, modules, roots, dep_map)
        .map_err(|error| error.to_string())?;
    let consts = ConstContext::build_with_imports(&node.program, imported.clone())
        .map_err(|error| error.to_string())?;
    let exports =
        module_interface::exports_of(&node.program, &consts).map_err(|error| error.to_string())?;
    let imports = imports_of(&node.program.stmts);
    let source_digest = digest(node.source.as_bytes());
    let interface_digest = interface_digest_of(&node.key.package_id, &node.key.relative, &exports);
    let dependency_interfaces: Vec<_> = node
        .dependencies
        .iter()
        .map(|dependency| {
            interfaces
                .get(dependency)
                .expect("dependency built first")
                .interface_digest
                .as_str()
        })
        .collect();
    let build_digest = digest(
        format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            optive::versions::bytecode_cache_version(),
            INTERFACE_FORMAT,
            node.key.package_id,
            node.key.relative,
            source_digest,
            dependency_interfaces.join("\0")
        )
        .as_bytes(),
    );
    let object_id = object_id_of(&node.key);
    let prev_tivi = prev.map(|dir| dir.join("interfaces").join(format!("{object_id}.tivi")));
    let prev_tivc = prev.map(|dir| dir.join("modules").join(format!("{object_id}.tivc")));
    let cached_interface = prev_tivi
        .as_ref()
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<ModuleInterface>(&bytes).ok());
    let cached_bytecode = prev_tivc.as_ref().and_then(|path| fs::read(path).ok());
    let hit = cached_interface
        .as_ref()
        .is_some_and(|interface| interface.build_digest == build_digest)
        && (!opts.emit.write_bytecode() || cached_bytecode.is_some());
    if hit {
        return Ok(ModuleOutput {
            key: node.key.clone(),
            interface: cached_interface.expect("cache hit has interface"),
            bytecode: cached_bytecode,
            hit: true,
        });
    }
    let compiled = optive::compile_with_const_imports(&node.source, imported)
        .map_err(|error| error.to_string())?;
    let bytecode = if opts.emit.write_bytecode() || opts.bundle {
        Some(bc_cache::encode_program(&compiled).map_err(|_| {
            format!(
                "module cannot be encoded as bytecode: {}",
                node.file.display()
            )
        })?)
    } else {
        None
    };
    Ok(ModuleOutput {
        key: node.key.clone(),
        interface: ModuleInterface {
            format: INTERFACE_FORMAT,
            package_id: node.key.package_id.clone(),
            module: node.key.relative.clone(),
            source_digest,
            build_digest,
            interface_digest,
            imports,
            exports,
        },
        bytecode,
        hit: false,
    })
}

fn make_bundle_image(
    project: &super::manifest::Project,
    modules: &BTreeMap<ModuleKey, ModuleNode>,
    interfaces: &HashMap<ModuleKey, ModuleInterface>,
    bytecode: &HashMap<ModuleKey, Vec<u8>>,
    roots: &BTreeMap<String, PathBuf>,
    dep_map: &DepMap,
) -> Result<BundleImage, Box<dyn std::error::Error>> {
    let mut image = BundleImage {
        compiler: env!("CARGO_PKG_VERSION").to_string(),
        entry_package: ROOT_PARENT.to_string(),
        entry_module: String::new(),
        ..BundleImage::default()
    };
    if let Some(root) = roots.get(ROOT_PARENT) {
        if let Some(file) = locate_package_entry(
            root,
            &project.manifest.package.name,
            project.manifest.package.entry.as_deref(),
            |path| Ok::<_, String>(path.is_file()),
        )? {
            if let Ok(key) = find_module_key(ROOT_PARENT.to_string(), &file, modules) {
                image.entry_module = key.relative;
            }
        }
    }
    if image.entry_module.is_empty() {
        if let Some(key) = modules.keys().find(|key| key.package_id == ROOT_PARENT) {
            image.entry_module = key.relative.clone();
        }
    }
    for ((parent, name), binding) in dep_map {
        image.bindings.push(BundleBinding {
            parent: parent.clone(),
            name: name.clone(),
            package_id: binding.id.clone(),
        });
    }
    image.bindings.sort();
    for (package_id, root) in roots {
        let declared = declared_entry(root)?;
        if let Some(file) = locate_package_entry(root, package_id, declared.as_deref(), |path| {
            Ok::<_, String>(path.is_file())
        })? {
            if let Ok(key) = find_module_key(package_id.clone(), &file, modules) {
                image
                    .package_entries
                    .insert(package_id.clone(), key.relative);
            }
        }
    }
    for (key, interface) in interfaces {
        let bytes = bytecode
            .get(key)
            .ok_or_else(|| format!("bundle is missing bytecode for {}", display_key(key)))?;
        image.modules.insert(
            (key.package_id.clone(), key.relative.clone()),
            BundleModule {
                interface_digest: interface.interface_digest.clone(),
                interface: serde_json::to_vec(interface)?,
                bytecode: bytes.clone(),
                dependencies: modules
                    .get(key)
                    .map(|node| node.dependencies.iter().map(display_key).collect())
                    .unwrap_or_default(),
            },
        );
    }
    Ok(image)
}

fn clean_temp_build_dirs(optive_dir: &Path) {
    let Ok(entries) = fs::read_dir(optive_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("build.staging-") || name.starts_with("build.prev-") {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn commit_build(live: &Path, staging: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let backup = live.with_file_name(format!("build.prev-{}", std::process::id()));
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    if live.exists() {
        fs::rename(live, &backup)?;
    }
    match fs::rename(staging, live) {
        Ok(()) => {
            let _ = fs::remove_dir_all(backup);
            Ok(())
        }
        Err(error) => {
            if backup.exists() && !live.exists() {
                let _ = fs::rename(&backup, live);
            }
            Err(error.into())
        }
    }
}

fn source_files(root: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
        let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name();
                if !matches!(name.to_str(), Some(".git" | ".optive" | "target")) {
                    walk(&path, files)?;
                }
            } else if path.extension().is_some_and(|e| e == "tive") {
                files.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(root, &mut files)?;
    Ok(files)
}

fn normalized_relative(root: &Path, file: &Path) -> Result<String, Box<dyn std::error::Error>> {
    Ok(file
        .strip_prefix(root)?
        .to_string_lossy()
        .replace('\\', "/"))
}

fn display_key(key: &ModuleKey) -> String {
    format!("{}:{}", key.package_id, key.relative)
}

fn resolve_dependencies(
    node: &ModuleNode,
    modules: &BTreeMap<ModuleKey, ModuleNode>,
    roots: &BTreeMap<String, PathBuf>,
    dep_map: &DepMap,
) -> Result<Vec<ModuleKey>, Box<dyn std::error::Error>> {
    let mut dependencies = BTreeSet::new();
    for located in &node.program.stmts {
        let resolved = match &located.stmt {
            Stmt::Import {
                path,
                path_is_string,
                ..
            } => {
                if *path_is_string {
                    resolve_file_reference(node, path, modules)?
                } else {
                    resolve_qualified(
                        node,
                        &path.split('.').collect::<Vec<_>>(),
                        modules,
                        roots,
                        dep_map,
                    )?
                }
            }
            Stmt::Use { module, .. } => resolve_module_ref(node, module, modules, roots, dep_map)?,
            _ => None,
        };
        if let Some(key) = resolved {
            dependencies.insert(key);
        }
    }
    Ok(dependencies.into_iter().collect())
}

fn resolve_module_ref(
    node: &ModuleNode,
    module: &ModuleRef,
    modules: &BTreeMap<ModuleKey, ModuleNode>,
    roots: &BTreeMap<String, PathBuf>,
    dep_map: &DepMap,
) -> Result<Option<ModuleKey>, Box<dyn std::error::Error>> {
    match module {
        ModuleRef::Qualified(parts) => resolve_qualified(
            node,
            &parts.iter().map(String::as_str).collect::<Vec<_>>(),
            modules,
            roots,
            dep_map,
        ),
        ModuleRef::FilePath { path, .. } => resolve_file_reference(node, path, modules),
    }
}

fn resolve_qualified(
    node: &ModuleNode,
    parts: &[&str],
    modules: &BTreeMap<ModuleKey, ModuleNode>,
    roots: &BTreeMap<String, PathBuf>,
    dep_map: &DepMap,
) -> Result<Option<ModuleKey>, Box<dyn std::error::Error>> {
    let Some(first) = parts.first().copied() else {
        return Err("empty module reference".into());
    };
    if is_builtin_root(first) {
        return Ok(None);
    }
    let (package_id, root, module_parts, entry_name) =
        if let Some(binding) = dep_map.get(&(node.key.package_id.clone(), first.to_string())) {
            (
                binding.id.clone(),
                binding.path.as_path(),
                &parts[1..],
                Some(first),
            )
        } else {
            (
                node.key.package_id.clone(),
                roots
                    .get(&node.key.package_id)
                    .ok_or("missing package root")?
                    .as_path(),
                parts,
                None,
            )
        };
    let file = if module_parts.is_empty() {
        let declared = declared_entry(root)?;
        locate_package_entry(
            root,
            entry_name.unwrap_or(first),
            declared.as_deref(),
            |path| Ok::<_, String>(path.is_file()),
        )?
    } else {
        locate_package_module(root, module_parts, |path| Ok::<_, String>(path.is_file()))?
    };
    let Some(file) = file else {
        return Err(format!(
            "unresolved module `{}` imported by {}",
            parts.join("."),
            display_key(&node.key)
        )
        .into());
    };
    find_module_key(package_id, &file, modules).map(Some)
}

fn resolve_file_reference(
    node: &ModuleNode,
    path: &str,
    modules: &BTreeMap<ModuleKey, ModuleNode>,
) -> Result<Option<ModuleKey>, Box<dyn std::error::Error>> {
    let base = node.file.parent().unwrap_or(&node.root);
    let file = locate_file_import(base, path, |candidate| Ok::<_, String>(candidate.is_file()))?
        .ok_or_else(|| {
            format!(
                "unresolved file module `{path}` imported by {}",
                display_key(&node.key)
            )
        })?;
    find_module_key(node.key.package_id.clone(), &file, modules).map(Some)
}

fn declared_entry(root: &Path) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let manifest = root.join("Optive.toml");
    if !manifest.is_file() {
        return Ok(None);
    }
    Ok(module_resolve::declared_package_entry(
        &fs::read_to_string(manifest)?,
    )?)
}

fn find_module_key(
    package_id: String,
    file: &Path,
    modules: &BTreeMap<ModuleKey, ModuleNode>,
) -> Result<ModuleKey, Box<dyn std::error::Error>> {
    let canonical = file.canonicalize()?;
    modules
        .iter()
        .find(|(key, node)| {
            key.package_id == package_id
                && node.file.canonicalize().ok().as_ref() == Some(&canonical)
        })
        .map(|(key, _)| key.clone())
        .ok_or_else(|| format!("resolved module was not scanned: {}", file.display()).into())
}

fn reachable_modules(
    modules: &BTreeMap<ModuleKey, ModuleNode>,
    roots: &BTreeMap<String, PathBuf>,
    declared_entry: Option<&str>,
    package_name: &str,
) -> Result<BTreeSet<ModuleKey>, Box<dyn std::error::Error>> {
    let mut starts = Vec::new();
    if let Some(root) = roots.get(ROOT_PARENT) {
        if let Some(file) = locate_package_entry(root, package_name, declared_entry, |path| {
            Ok::<_, String>(path.is_file())
        })? {
            if let Ok(key) = find_module_key(ROOT_PARENT.to_string(), &file, modules) {
                starts.push(key);
            }
        }
    }
    if starts.is_empty() {
        starts.extend(
            modules
                .keys()
                .filter(|key| key.package_id == ROOT_PARENT)
                .cloned(),
        );
    }
    let mut keep = BTreeSet::new();
    let mut queue = VecDeque::from(starts);
    while let Some(key) = queue.pop_front() {
        if !keep.insert(key.clone()) {
            continue;
        }
        if let Some(node) = modules.get(&key) {
            queue.extend(node.dependencies.iter().cloned());
        }
    }
    Ok(keep)
}

fn topological_order(
    modules: &BTreeMap<ModuleKey, ModuleNode>,
) -> Result<Vec<ModuleKey>, Box<dyn std::error::Error>> {
    fn visit(
        key: &ModuleKey,
        modules: &BTreeMap<ModuleKey, ModuleNode>,
        temporary: &mut BTreeSet<ModuleKey>,
        permanent: &mut BTreeSet<ModuleKey>,
        stack: &mut Vec<ModuleKey>,
        order: &mut Vec<ModuleKey>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if permanent.contains(key) {
            return Ok(());
        }
        if !temporary.insert(key.clone()) {
            let start = stack.iter().position(|item| item == key).unwrap_or(0);
            let mut cycle: Vec<_> = stack[start..].iter().map(display_key).collect();
            cycle.push(display_key(key));
            return Err(format!("module dependency cycle: {}", cycle.join(" -> ")).into());
        }
        stack.push(key.clone());
        for dependency in &modules.get(key).expect("module exists").dependencies {
            visit(dependency, modules, temporary, permanent, stack, order)?;
        }
        stack.pop();
        temporary.remove(key);
        permanent.insert(key.clone());
        order.push(key.clone());
        Ok(())
    }
    let mut order = Vec::new();
    let mut temporary = BTreeSet::new();
    let mut permanent = BTreeSet::new();
    for key in modules.keys() {
        visit(
            key,
            modules,
            &mut temporary,
            &mut permanent,
            &mut Vec::new(),
            &mut order,
        )?;
    }
    Ok(order)
}

fn imported_const_imports(
    node: &ModuleNode,
    interfaces: &HashMap<ModuleKey, ModuleInterface>,
    modules: &BTreeMap<ModuleKey, ModuleNode>,
    roots: &BTreeMap<String, PathBuf>,
    dep_map: &DepMap,
) -> Result<ConstImports, Box<dyn std::error::Error>> {
    let mut imported = ConstImports::default();
    for located in &node.program.stmts {
        match &located.stmt {
            Stmt::Use { module, items } => {
                let Some(key) = resolve_module_ref(node, module, modules, roots, dep_map)? else {
                    continue;
                };
                let dependency = interfaces.get(&key).ok_or_else(|| {
                    format!("dependency {} was not built first", display_key(&key))
                })?;
                import_use_items(items, dependency, &mut imported.values)?;
            }
            Stmt::Import {
                path,
                path_is_string,
                alias,
            } => {
                let resolved = if *path_is_string {
                    resolve_file_reference(node, path, modules)?
                } else {
                    resolve_qualified(
                        node,
                        &path.split('.').collect::<Vec<_>>(),
                        modules,
                        roots,
                        dep_map,
                    )?
                };
                let Some(key) = resolved else {
                    continue;
                };
                let dependency = interfaces.get(&key).ok_or_else(|| {
                    format!("dependency {} was not built first", display_key(&key))
                })?;
                let bind = import_bind_name(path, *path_is_string, alias.as_deref());
                imported.modules.insert(
                    bind.clone(),
                    CompileTimeModule::new(
                        bind,
                        compile_time_values(&dependency.exports)?
                            .into_iter()
                            .collect(),
                    ),
                );
            }
            _ => {}
        }
    }
    Ok(imported)
}

fn import_use_items(
    items: &[UseItem],
    interface: &ModuleInterface,
    values: &mut HashMap<String, optive::value::Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let exported = compile_time_values(&interface.exports)?;
    for item in items {
        if let Some(value) = exported.get(&item.name) {
            values.insert(
                item.alias.clone().unwrap_or_else(|| item.name.clone()),
                value.clone(),
            );
        }
    }
    Ok(())
}

fn imports_of(stmts: &[optive::ast::LocatedStmt]) -> Vec<String> {
    let mut imports = BTreeSet::new();
    for located in stmts {
        match &located.stmt {
            Stmt::Import { path, .. } => {
                imports.insert(path.clone());
            }
            Stmt::Use { module, .. } => {
                let name = match module {
                    ModuleRef::Qualified(parts) => parts.join("."),
                    ModuleRef::FilePath { path, attrs } => std::iter::once(path.as_str())
                        .chain(attrs.iter().map(String::as_str))
                        .collect::<Vec<_>>()
                        .join("."),
                };
                imports.insert(name);
            }
            _ => {}
        }
    }
    imports.into_iter().collect()
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&tmp, bytes)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(tmp, path)
}

#[cfg(test)]
mod target_tests {
    use super::*;

    #[test]
    fn triple_keeps_every_hyphen_component() {
        let target = TargetSpec::parse("armv7-unknown-linux-gnueabihf").unwrap();
        assert_eq!(
            target,
            TargetSpec::Triple {
                raw: "armv7-unknown-linux-gnueabihf".into(),
                components: ["armv7", "unknown", "linux", "gnueabihf"]
                    .map(str::to_string)
                    .to_vec(),
            }
        );
        assert_eq!(target.cargo_arg(), "armv7-unknown-linux-gnueabihf");
    }

    #[test]
    fn target_accepts_variable_component_count_and_json() {
        assert!(matches!(
            TargetSpec::parse("wasm32-wasip1").unwrap(),
            TargetSpec::Triple { components, .. } if components.len() == 2
        ));
        assert!(matches!(
            TargetSpec::parse("targets/custom-platform.json").unwrap(),
            TargetSpec::Json(_)
        ));
        assert!(TargetSpec::parse("not-a--target").is_err());
    }
}
