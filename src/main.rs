mod cli;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use std::borrow::Cow;

use optive::{repl_needs_continuation, run_source_in_vm, vm::DepPackage, vm::Vm};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::history::DefaultHistory;
use rustyline::{Completer, Editor, Helper, Hinter, Validator};

use cli::color;
use cli::lock::ROOT_PARENT;
use cli::resolve::{DepBinding, EnsureResult};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPL_PRIMARY: &str = ">>> ";
const REPL_CONTINUE: &str = "... ";

/// Windows 上 rustyline 按原始 prompt 算宽度且不忽略 ANSI；
/// 颜色只能放在 Highlighter，不能塞进 `readline` 的 prompt 字符串。
#[derive(Helper, Completer, Hinter, Validator)]
struct ReplHelper {
    colored_prompt: String,
}

impl Highlighter for ReplHelper {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        default: bool,
    ) -> Cow<'b, str> {
        if default && !self.colored_prompt.is_empty() {
            Cow::Borrowed(self.colored_prompt.as_str())
        } else {
            Cow::Borrowed(prompt)
        }
    }
}

fn main() {
    let raw_args: Vec<String> = env::args().collect();
    let (color_choice, args) = color::take_color_args(&raw_args);
    color::init(color_choice);

    if args.len() > 1 {
        match args[1].as_str() {
            "-h" | "--help" => {
                print_help();
                return;
            }
            "-V" | "--version" => {
                println!("Optive {VERSION}");
                return;
            }
            "-c" | "--code" => {
                if args.len() < 3 {
                    color::eprint_error("usage: Optive -c <code>");
                    process::exit(2);
                }
                let (caps, _rest) = match cli::caps::parse_caps(&args[3..]) {
                    Ok(v) => v,
                    Err(e) => { color::eprint_error(format!("Error: {e}")); process::exit(2); }
                };
                // 允许多行：整段作为下一个参数（shell 引号内可含换行）。
                if let Err(e) = run_inline_source(&args[2], caps) {
                    color::eprint_error(e.to_string());
                    process::exit(1);
                }
                return;
            }
            "add" => {
                if let Err(e) = cmd_add(&args[1..]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "remove" => {
                if args.len() != 3 {
                    color::eprint_error("usage: Optive remove <name>");
                    process::exit(2);
                }
                if let Err(e) = cmd_remove(&args[2]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "update" => {
                if let Err(e) = cmd_update(&args[2..]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "up" => {
                let (caps, rest) = match cli::caps::parse_caps(&args[2..]) {
                    Ok(v) => v,
                    Err(e) => { color::eprint_error(format!("Error: {e}")); process::exit(2); }
                };
                let path = rest.first().map(Path::new);
                if let Err(e) = cmd_up(path, caps) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "run" => {
                let (caps, rest) = match cli::caps::parse_caps(&args[2..]) {
                    Ok(v) => v,
                    Err(e) => { color::eprint_error(format!("Error: {e}")); process::exit(2); }
                };
                let path = rest.first().map(Path::new);
                if let Err(e) = cmd_run(path, caps) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "new" => {
                if args.len() != 3 {
                    color::eprint_error("usage: Optive new <ProjectName>");
                    process::exit(2);
                }
                if let Err(e) = cmd_new(&args[2]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "cache" => {
                if let Err(e) = cmd_cache(&args[2..]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "deps" => {
                if let Err(e) = cmd_deps(&args[2..]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "env" => {
                cli::doctor::print_env();
                return;
            }
            "change" => {
                if let Err(e) = cmd_change(&args[2..]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "fmt" => {
                if let Err(e) = cmd_fmt(&args[2..]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "debug" => {
                if let Err(e) = cli::debug_cmd::cmd_debug(&args[2..]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            path => {
                if path.ends_with(".tive") || Path::new(path).is_file() {
                    let (caps, _rest) = match cli::caps::parse_caps(&args[2..]) {
                        Ok(v) => v,
                        Err(e) => { color::eprint_error(format!("Error: {e}")); process::exit(2); }
                    };
                    run_script_file(path, caps);
                    return;
                }
                color::eprint_error(format!("unknown command or file: {path}"));
                color::eprint_error("try: Optive --help");
                process::exit(2);
            }
        }
    }

    repl();
}

fn cmd_new(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let root = cli::new_project::create_project(&cwd, name)?;
    color::status_line(&format!("Created project {}", root.display()));
    println!("  Optive.toml");
    println!("  src/main.tive");
    println!("  .gitignore");
    println!();
    println!("Next:");
    println!("  cd {}", name.trim());
    println!("  Optive run");
    Ok(())
}

/// `Optive fmt <file> [-o|--out]`：默认写回；`-o` / `--out` 只打印到 stdout。
fn cmd_fmt(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut out_only = false;
    let mut file: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "-o" | "--out" => out_only = true,
            "-h" | "--help" => {
                println!("usage: Optive fmt <filename> [-o|--out]");
                println!("  default: write formatted source back to <filename>");
                println!("  -o, --out: print formatted source to stdout only");
                return Ok(());
            }
            s if s.starts_with('-') => {
                return Err(format!("unknown fmt flag: {s}").into());
            }
            s => {
                if file.is_some() {
                    return Err("usage: Optive fmt <filename> [-o|--out]".into());
                }
                file = Some(s);
            }
        }
    }
    let Some(path) = file else {
        return Err("usage: Optive fmt <filename> [-o|--out]".into());
    };
    let source = fs::read_to_string(path)?;
    let formatted = optive::fmt::format_source(&source).map_err(|e| {
        optive::diagnostics::format_parse_error(&source, path, &e)
    })?;
    if out_only {
        print!("{formatted}");
    } else {
        fs::write(path, formatted)?;
    }
    Ok(())
}

fn cmd_run(path: Option<&Path>, caps: optive::caps::Capabilities) -> Result<(), Box<dyn std::error::Error>> {
    let project = cli::manifest::find_project(path)?;
    print_project_header(&project);
    let ensured = cli::deps::ensure_for_run(&project)?;
    print_ensure_report(&ensured);
    env::set_current_dir(&project.root)?;
    let entry = project.entry_path()?;
    let entry_display = entry
        .strip_prefix(&project.root)
        .unwrap_or(&entry)
        .display()
        .to_string();
    color::status_line(&format!("Running {entry_display}"));
    run_script_path_with_deps(&entry, &project.root, &ensured, caps)?;
    Ok(())
}

fn cmd_up(path: Option<&Path>, caps: optive::caps::Capabilities) -> Result<(), Box<dyn std::error::Error>> {
    let project = cli::manifest::find_project(path)?;
    print_project_header(&project);
    color::status_line("Updating dependencies…");
    let ensured = cli::deps::ensure_for_update(&project, None)?;
    print_ensure_report(&ensured);
    env::set_current_dir(&project.root)?;
    let entry = project.entry_path()?;
    let entry_display = entry
        .strip_prefix(&project.root)
        .unwrap_or(&entry)
        .display()
        .to_string();
    color::status_line(&format!("Running {entry_display}"));
    run_script_path_with_deps(&entry, &project.root, &ensured, caps)?;
    Ok(())
}

fn cmd_update(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut dry_run = false;
    let mut verbose = false;
    let mut only: Option<String> = None;
    for a in args {
        match a.as_str() {
            "--dry-run" => dry_run = true,
            "-v" | "--verbose" => verbose = true,
            s if !s.starts_with('-') => {
                if only.is_some() {
                    return Err("usage: Optive update [name] [--dry-run] [-v]".into());
                }
                only = Some(s.to_string());
            }
            other => return Err(format!("unknown update flag: {other}").into()),
        }
    }
    let project = cli::manifest::find_project(None)?;
    print_project_header(&project);
    if dry_run {
        let lines = cli::resolve::dry_run_summary(&project, verbose)?;
        for line in lines {
            println!("{line}");
        }
        return Ok(());
    }
    let ensured = cli::deps::ensure_for_update(&project, only.as_deref())?;
    print_ensure_report(&ensured);
    if ensured.wrote_lock {
        color::status_line("Wrote optive.lock");
    }
    Ok(())
}

fn cmd_add(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // args[0] == "add"
    let mut url = None;
    let mut name = None;
    let mut branch = None;
    let mut tag = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                i += 1;
                name = Some(
                    args.get(i)
                        .ok_or("--name requires a value")?
                        .clone(),
                );
            }
            "--branch" => {
                i += 1;
                branch = Some(
                    args.get(i)
                        .ok_or("--branch requires a value")?
                        .clone(),
                );
            }
            "--tag" => {
                i += 1;
                tag = Some(args.get(i).ok_or("--tag requires a value")?.clone());
            }
            s if !s.starts_with('-') => {
                if url.is_some() {
                    return Err("usage: Optive add <git-url> [--name N] [--branch B|--tag T]".into());
                }
                url = Some(s.to_string());
            }
            other => return Err(format!("unknown add flag: {other}").into()),
        }
        i += 1;
    }
    let url = url.ok_or("usage: Optive add <git-url> [--name N] [--branch B|--tag T]")?;
    let project = cli::manifest::find_project(None)?;
    let msg = cli::commands::cmd_add(
        &project,
        cli::commands::AddOptions {
            url,
            name,
            branch,
            tag,
        },
    )?;
    color::status_line(&msg);
    Ok(())
}

fn cmd_remove(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let project = cli::manifest::find_project(None)?;
    let msg = cli::commands::cmd_remove(&project, name)?;
    color::status_line(&msg);
    Ok(())
}

fn cmd_cache(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(|s| s.as_str()) {
        Some("gc") => {
            let dry = args.iter().any(|a| a == "--dry-run");
            cli::doctor::cache_gc(dry)?;
            Ok(())
        }
        _ => Err("usage: Optive cache gc [--dry-run]".into()),
    }
}

fn cmd_deps(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(|s| s.as_str()) {
        None => cli::doctor::list_deps(false),
        Some("-v") | Some("--verbose") if args.len() == 1 => cli::doctor::list_deps(true),
        Some("list") => {
            let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
            cli::doctor::list_deps(verbose)
        }
        Some("doctor") => {
            let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
            let code = cli::doctor::doctor(verbose)?;
            if code != 0 {
                process::exit(code);
            }
            Ok(())
        }
        _ => Err("usage: Optive deps [-v] | Optive deps doctor [-v]".into()),
    }
}

fn cmd_change(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let spec = args.first().ok_or("usage: Optive change track_latest=true|false")?;
    if let Some(rest) = spec.strip_prefix("track_latest=") {
        let value = match rest {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            other => {
                return Err(format!("invalid track_latest value: {other}").into());
            }
        };
        let project = cli::manifest::find_project(None)?;
        let msg = cli::commands::cmd_change_track_latest(&project, value)?;
        color::status_line(&msg);
        Ok(())
    } else {
        Err("usage: Optive change track_latest=true|false".into())
    }
}

fn print_project_header(project: &cli::manifest::Project) {
    let headline = match &project.manifest.package.version {
        Some(ver) => format!(
            "Project {} v{} ({})",
            project.manifest.package.name,
            ver,
            project.root.display()
        ),
        None => format!(
            "Project {} ({})",
            project.manifest.package.name,
            project.root.display()
        ),
    };
    color::status_line(&headline);
    if let Some(desc) = &project.manifest.package.description {
        if !desc.is_empty() {
            color::status_line(desc);
        }
    }
}

fn print_ensure_report(ensured: &EnsureResult) {
    if !ensured.report.installed.is_empty() {
        color::status_line(&format!(
            "Installed: {}",
            ensured.report.installed.join(", ")
        ));
    }
    if !ensured.report.reused.is_empty() {
        color::status_line(&format!(
            "Reused packs: {}",
            ensured.report.reused.join(", ")
        ));
    }
}

fn inject_dep_map(vm: &mut Vm, ensured: &EnsureResult, project_root: &Path) {
    vm.dep_map.clear();
    for ((parent, name), DepBinding { path, id }) in &ensured.dep_map {
        vm.dep_map.insert(
            (parent.clone(), name.clone()),
            DepPackage {
                path: path.clone(),
                id: id.clone(),
            },
        );
    }
    vm.current_package_id = ROOT_PARENT.to_string();
    vm.package_root = Some(project_root.to_path_buf());
}

fn run_script_path_with_deps(
    path: &Path,
    project_root: &Path,
    ensured: &EnsureResult,
    caps: optive::caps::Capabilities,
) -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = path.to_string_lossy().to_string();
    run_in_vm(&source, &file, caps, |vm| inject_dep_map(vm, ensured, project_root))
}

fn run_script_file(path: &str, caps: optive::caps::Capabilities) {
    if let Err(e) = run_script_path(Path::new(path), caps) {
        color::eprint_error(e.to_string());
        process::exit(1);
    }
}

fn run_script_path(path: &Path, caps: optive::caps::Capabilities) -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = path.to_string_lossy().to_string();
    run_in_vm(&source, &file, caps, |_| {})
}

fn run_inline_source(source: &str, caps: optive::caps::Capabilities) -> Result<(), Box<dyn std::error::Error>> {
    run_in_vm(source, "<string>", caps, |_| {})
}

/// 公共 VM 执行入口：创建 VM、设置能力、运行源码、打印非 None 结果。
fn run_in_vm(
    source: &str,
    file: &str,
    caps: optive::caps::Capabilities,
    setup: impl FnOnce(&mut Vm),
) -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = Vm::new();
    vm.caps = caps;
    setup(&mut vm);
    match run_source_in_vm(&mut vm, source, file) {
        Ok(v) => {
            if !matches!(v, optive::value::Value::None) {
                println!("{}", v.display_string());
            }
            Ok(())
        }
        Err(e) => Err(e.to_string().into()),
    }
}

fn print_help() {
    println!("Optive {VERSION}");
    println!();
    println!("Usage:");
    println!("  Optive                         Start interactive REPL");
    println!("  Optive <script.tive>           Run a script");
    println!("  Optive -c <code>               Run code from argument (multi-line OK)");
    println!("  Optive new <ProjectName>       Create a new project");
    println!("  Optive run [path]              Ensure deps (strict lock) + run entry");
    println!("  Optive up [path]               update + run");
    println!("  Optive add <git-url> […]       Add dependency (default: pin tip commit)");
    println!("  Optive remove <name>           Remove dependency");
    println!("  Optive update [name] [--dry-run] [-v]");
    println!("  Optive cache gc [--dry-run]    Remove orphan packs");
    println!("  Optive deps [-v]               List project dependencies");
    println!("  Optive deps doctor [-v]        Diagnose deps / lock / orphans");
    println!("  Optive env                     Print OPTIVE_HOME and paths");
    println!("  Optive change track_latest=…   Toggle tip-following (warns)");
    println!("  Optive fmt <file> [-o|--out]   Format a .tive file (default: write back)");
    println!("  Optive debug [file|path]       Debug a script or project entry");
    println!();
    println!("Runtime capability flags (apply to run / up / debug / <script> / -c):");
    println!("  --sandbox[=DIR]          No network, no env, no FFI; fs limited to DIR (default: cwd)");
    println!("  --no-network            Disable std.http");
    println!("  --no-ffi                Disable C.frompath / extern");
    println!("  --allow-ffi             Allow native FFI (overrides sandbox default)");
    println!("  --allow-path DIR         Allow fs access under DIR (repeatable; combines with --sandbox)");
    println!("  Optive -h, --help              Show this help");
    println!("  Optive -V, --version           Show version");
    println!();
    println!("Env:");
    println!("  OPTIVE_HOME              Global pack/ + index.db root");
    println!("  OPTIVE_USE_LOCAL_DEPS=1  Debug: install into project deps/");
    println!();
    println!("Files: Optive.toml (intent), optive.lock (repro), Optive.cache (local)");
}

fn history_path() -> PathBuf {
    if let Some(p) = env::var_os("OPTIVE_HISTORY") {
        return PathBuf::from(p);
    }
    if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        return PathBuf::from(home).join(".optive_history");
    }
    PathBuf::from(".optive_history")
}

fn print_repl_help() {
    println!("Optive REPL");
    println!("  :help              Show this help");
    println!("  :quit / :exit      Exit (also quit / exit)");
    println!("  Ctrl-C             Cancel unfinished multi-line input");
    println!("  Ctrl-D             Exit");
}

fn repl() {
    let mut rl: Editor<ReplHelper, DefaultHistory> = match Editor::new() {
        Ok(mut e) => {
            e.set_helper(Some(ReplHelper {
                colored_prompt: String::new(),
            }));
            e
        }
        Err(e) => {
            color::eprint_error(format!("REPL init failed: {e}"));
            process::exit(1);
        }
    };
    let hist = history_path();
    let _ = rl.load_history(&hist);

    let mut vm = Vm::new();
    let mut accumulator = String::new();

    loop {
        let prompt = if accumulator.is_empty() {
            REPL_PRIMARY
        } else {
            REPL_CONTINUE
        };
        // 宽度按纯文本 prompt 计算；着色仅通过 Highlighter 绘制。
        if let Some(h) = rl.helper_mut() {
            h.colored_prompt = if color::enabled() {
                color::purple(prompt)
            } else {
                String::new()
            };
        }
        match rl.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim_end();
                let cmd = trimmed.trim();
                if accumulator.is_empty() {
                    match cmd {
                        ":help" | "help" => {
                            print_repl_help();
                            continue;
                        }
                        ":quit" | ":exit" | "quit" | "exit" => break,
                        _ => {}
                    }
                }
                if cmd.is_empty() {
                    if !accumulator.is_empty() {
                        accumulator.clear();
                    }
                    continue;
                }

                let _ = rl.add_history_entry(trimmed);

                if !accumulator.is_empty() {
                    accumulator.push('\n');
                }
                accumulator.push_str(trimmed);

                if repl_needs_continuation(&accumulator) {
                    continue;
                }

                let segment = accumulator.clone();
                accumulator.clear();

                match run_source_in_vm(&mut vm, &segment, "<repl>") {
                    Ok(v) => {
                        if !matches!(v, optive::value::Value::None) {
                            println!("{}", v.display_string());
                        }
                    }
                    Err(e) => {
                        color::eprint_error(e.to_string());
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                if !accumulator.is_empty() {
                    accumulator.clear();
                }
                continue;
            }
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                color::eprint_error(format!("REPL error: {e}"));
                break;
            }
        }
    }

    let _ = rl.save_history(&hist);
}
