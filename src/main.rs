#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod cli;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use std::borrow::Cow;

use optive::custom::{self, CliMsg, Diag, ReplMsg};
use optive::{repl_needs_continuation, run_source_in_vm, vm::Vm};
use rustyline::error::ReadlineError;
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::history::DefaultHistory;
use rustyline::{Completer, Editor, Helper, Hinter, Validator};

use cli::color;
use cli::repl_highlight::{self, LineHighlightCache};
use cli::resolve::{EnsureResult};
use cli::main_index;
use optive::caps::Capabilities;
use crate::cli::debug_cmd::inject_dep_map;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Windows 上 rustyline 按原始 prompt 算宽度且不忽略 ANSI；
/// 颜色只能放在 Highlighter，不能塞进 `readline` 的 prompt 字符串。
#[derive(Helper, Completer, Hinter, Validator)]
struct ReplHelper {
    colored_prompt: String,
    line_cache: LineHighlightCache,
}

impl Highlighter for ReplHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        self.line_cache.get_or_highlight(line)
    }

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

    fn highlight_char(&self, _line: &str, _pos: usize, _kind: CmdKind) -> bool {
        repl_highlight::highlight_enabled()
    }
}

fn take_custom_arg(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut custom = None;
    let mut out = Vec::with_capacity(args.len());
    if let Some(first) = args.first() {
        out.push(first.clone());
    }
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if let Some(rest) = a.strip_prefix("--custom=") {
            custom = Some(rest.to_string());
            i += 1;
            continue;
        }
        if a == "--custom" {
            if i + 1 < args.len() {
                custom = Some(args[i + 1].clone());
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        out.push(args[i].clone());
        i += 1;
    }
    (custom, out)
}

fn init_custom(cli_override: Option<&str>) {
    if let Err(e) = custom::init_from_env_and_cwd(cli_override) {
        color::eprint_error(format!("Error: {e}"));
        process::exit(2);
    }
}

fn t_cli(msg: CliMsg) -> String {
    custom::render(&Diag::Cli(msg))
}

fn t_repl(msg: ReplMsg) -> String {
    custom::render(&Diag::Repl(msg))
}

fn main() {
    let raw_args: Vec<String> = env::args().collect();
    let (color_choice, args) = color::take_color_args(&raw_args);
    color::init(color_choice);
    let (custom_override, args) = take_custom_arg(&args);
    init_custom(custom_override.as_deref());

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
            "search" => {
                if let Err(e) = cmd_search(&args[2..]) {
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
            "publish" => {
                if args.len() != 3 {
                    color::eprint_error("usage: Optive publish <version>");
                    process::exit(2);
                }
                if let Err(e) = cli::publish::publish(&args[2]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "up" => {
                let (caps, rest) = parse_caps_or_exit(&args);
                let (path, script_args) = parse_project_path_and_script_args(&rest);
                if let Err(e) = cmd_up(path.as_deref(), caps, &script_args) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "run" => {
                let (caps, rest) = parse_caps_or_exit(&args);
                let (path, script_args) = parse_project_path_and_script_args(&rest);
                if let Err(e) = cmd_run(path.as_deref(), caps, &script_args) {
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
            "test" => {
                let (caps, rest) = parse_caps_or_exit(&args);
                let (path, script_args) = parse_project_path_and_script_args(&rest);
                if let Err(e) = cli::test_cmd::cmd_test(path.as_deref(), caps, &script_args) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "custom" => {
                if let Err(e) = cli::custom_cmd::run(&args[2..]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "index" => {
                match args.get(2).map(String::as_str) {
                    Some("sync") if args.len() == 3 => {
                        if let Err(e) = main_index::sync_index() {
                            color::eprint_error(format!("Sync failed: {e}"));
                            process::exit(1);
                        }
                    }
                    Some("change") if args.len() == 4 => {
                        if let Err(e) = main_index::change_index(&args[3]) {
                            color::eprint_error(format!("Change failed: {e}"));
                            process::exit(1);
                        }
                    }
                    _ => {
                        color::eprint_error(
                            "usage: Optive index sync | Optive index change <url>",
                        );
                        process::exit(2);
                    }
                }
                return;
            }
            path => {
                if path.ends_with(".tive") || Path::new(path).is_file() {
                    let (caps, _rest) = parse_caps_or_exit(&args);
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

fn parse_project_path_and_script_args(rest: &Vec<String>) -> (Option<PathBuf>, Vec<String>) {
    let (path, script_args) = match split_project_and_script_args(&rest) {
        Ok(v) => v,
        Err(e) => {
            color::eprint_error(format!("Error: {e}"));
            process::exit(2);
        }
    };
    (path, script_args)
}

fn parse_caps_or_exit(args: &[String]) -> (Capabilities, Vec<String>) {
    let (caps, rest) = match cli::caps::parse_caps(&args[2..]) {
        Ok(v) => v,
        Err(e) => {
            color::eprint_error(format!("Error: {e}"));
            process::exit(2);
        }
    };
    (caps, rest)
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

fn cmd_run(
    path: Option<&Path>,
    caps: Capabilities,
    script_args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
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
    run_script_path_with_deps(
        &entry,
        &project.root,
        &ensured,
        caps,
        Some(build_script_argv(&entry_display, script_args)),
    )?;
    Ok(())
}

fn cmd_up(
    path: Option<&Path>,
    caps: Capabilities,
    script_args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
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
    run_script_path_with_deps(
        &entry,
        &project.root,
        &ensured,
        caps,
        Some(build_script_argv(&entry_display, script_args)),
    )?;
    Ok(())
}

/// 拆分 `run`/`up` 剩余参数：可选项目路径 + `--` 后的脚本参数。
///
/// - `Optive run -- a b` → `path=None（cwd），script_args`=[a,b]
/// - `Optive run . -- a` → path=Some(.), `script_args`=[a]
/// - `Optive run .` → path=Some(.), `script_args`=[]
/// - `--` 前多于一个操作数 → 用法错误
fn split_project_and_script_args(
    rest: &[String],
) -> Result<(Option<PathBuf>, Vec<String>), String> {
    if let Some(dash) = rest.iter().position(|a| a == "--") {
        if dash > 1 {
            return Err(format!(
                "too many arguments before '--' (expected at most one project path); got: {}",
                rest[..dash].join(" ")
            ));
        }
        let path = if dash == 0 {
            None
        } else {
            Some(PathBuf::from(&rest[0]))
        };
        let script_args = rest[dash + 1..].to_vec();
        Ok((path, script_args))
    } else if rest.is_empty() {
        Ok((None, Vec::new()))
    } else if rest.len() == 1 {
        Ok((Some(PathBuf::from(&rest[0])), Vec::new()))
    } else {
        Err(format!(
            "too many arguments (expected project path or 'run -- <script args>'); got: {}",
            rest.join(" ")
        ))
    }
}

fn build_script_argv(entry_display: &str, script_args: &[String]) -> Vec<String> {
    let exe = env::args()
        .next()
        .unwrap_or_else(|| "Optive".to_string());
    let mut argv = Vec::with_capacity(2 + script_args.len());
    argv.push(exe);
    argv.push(entry_display.to_string());
    argv.extend(script_args.iter().cloned());
    argv
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
        color::status_line("Wrote Optive.lock");
    }
    Ok(())
}

fn cmd_add(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // args[0] == "add"
    let mut target = None;
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
                if target.is_some() {
                    return Err(
                        "usage: Optive add <git-url|pack[@version]> [--name N] [--branch B|--tag T]"
                            .into(),
                    );
                }
                target = Some(s.to_string());
            }
            other => return Err(format!("unknown add flag: {other}").into()),
        }
        i += 1;
    }
    let target = target.ok_or(
        "usage: Optive add <git-url|pack[@version]> [--name N] [--branch B|--tag T]",
    )?;
    let project = cli::manifest::find_project(None)?;
    let msg = cli::commands::cmd_add(
        &project,
        cli::commands::AddOptions {
            target,
            name,
            branch,
            tag,
        },
    )?;
    color::status_line(&msg);
    Ok(())
}

fn cmd_search(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let query = args.first().map(String::as_str);
    let hits = cli::registry::search_packs(query)?;
    const SOFT_LIMIT: usize = 200;
    if hits.is_empty() {
        if let Some(q) = query {
            println!("(no packs matching `{q}`)");
        } else {
            println!("(index is empty)");
        }
        return Ok(());
    }
    let show = hits.len().min(SOFT_LIMIT);
    let name_width = hits[..show]
        .iter()
        .map(|(n, _)| n.len())
        .max()
        .unwrap_or(0);
    for (name, url) in hits.iter().take(show) {
        println!("{name:<name_width$}  {url}");
    }
    if hits.len() > SOFT_LIMIT {
        eprintln!(
            "... {} more; narrow with a query (e.g. Optive search foo)",
            hits.len() - SOFT_LIMIT
        );
    }
    Ok(())
}

fn cmd_remove(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let project = cli::manifest::find_project(None)?;
    let msg = cli::commands::cmd_remove(&project, name)?;
    color::status_line(&msg);
    Ok(())
}

fn cmd_cache(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        Some("gc") => {
            let dry = args.iter().any(|a| a == "--dry-run");
            cli::doctor::cache_gc(dry)?;
            Ok(())
        }
        _ => Err("usage: Optive cache gc [--dry-run]".into()),
    }
}

fn cmd_deps(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        None => cli::doctor::list_deps(false),
        Some("-v" | "--verbose") if args.len() == 1 => cli::doctor::list_deps(true),
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
    let ver = cli::repo_meta::project_version_label(&project.root);
    let headline = match ver {
        Some(v) if v.starts_with("(unreleased)") => format!(
            "Project {} {} ({})",
            project.manifest.package.name,
            v,
            project.root.display()
        ),
        Some(v) => format!(
            "Project {} {} ({})",
            project.manifest.package.name,
            v,
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



fn run_script_path_with_deps(
    path: &Path,
    project_root: &Path,
    ensured: &EnsureResult,
    caps: Capabilities,
    argv_override: Option<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = path.to_string_lossy().to_string();
    run_in_vm(&source, &file, caps, argv_override, |vm| {
        inject_dep_map(vm, ensured, project_root);
    })
}

fn run_script_file(path: &str, caps: Capabilities) {
    if let Err(e) = run_script_path(Path::new(path), caps) {
        color::eprint_error(e.to_string());
        process::exit(1);
    }
}

fn run_script_path(path: &Path, caps: Capabilities) -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = path.to_string_lossy().to_string();
    run_in_vm(&source, &file, caps, None, |_| {})
}

fn run_inline_source(source: &str, caps: Capabilities) -> Result<(), Box<dyn std::error::Error>> {
    run_in_vm(source, "<string>", caps, None, |_| {})
}

/// 公共 VM 执行入口：创建 VM、设置能力、运行源码、打印非 None 结果。
fn run_in_vm(
    source: &str,
    file: &str,
    caps: Capabilities,
    argv_override: Option<Vec<String>>,
    setup: impl FnOnce(&mut Vm),
) -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = Vm::new();
    vm.caps = caps;
    vm.argv_override = argv_override;
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
    println!("{} {VERSION}", t_cli(CliMsg::HelpTitle));
    println!();
    println!("{}", t_cli(CliMsg::HelpUsageHeader));
    println!("{}", t_cli(CliMsg::HelpRepl));
    println!("{}", t_cli(CliMsg::HelpRunScript));
    println!("{}", t_cli(CliMsg::HelpRunCode));
    println!("{}", t_cli(CliMsg::HelpNew));
    println!("{}", t_cli(CliMsg::HelpRun));
    println!("{}", t_cli(CliMsg::HelpUp));
    println!("{}", t_cli(CliMsg::HelpAdd));
    println!("{}", t_cli(CliMsg::HelpSearch));
    println!("{}", t_cli(CliMsg::HelpRemove));
    println!("{}", t_cli(CliMsg::HelpUpdate));
    println!("{}", t_cli(CliMsg::HelpPublish));
    println!("{}", t_cli(CliMsg::HelpCache));
    println!("{}", t_cli(CliMsg::HelpDeps));
    println!("{}", t_cli(CliMsg::HelpDepsDoctor));
    println!("{}", t_cli(CliMsg::HelpEnv));
    println!("{}", t_cli(CliMsg::HelpChange));
    println!("{}", t_cli(CliMsg::HelpFmt));
    println!("{}", t_cli(CliMsg::HelpDebug));
    println!("{}", t_cli(CliMsg::HelpTest));
    println!("{}", t_cli(CliMsg::HelpIndex));
    println!("{}", t_cli(CliMsg::HelpIndexChange));
    println!("{}", t_cli(CliMsg::HelpCustom));
    println!();
    println!("{}", t_cli(CliMsg::HelpCapsHeader));
    println!("{}", t_cli(CliMsg::HelpSandbox));
    println!("{}", t_cli(CliMsg::HelpNoNetwork));
    println!("{}", t_cli(CliMsg::HelpNoFfi));
    println!("{}", t_cli(CliMsg::HelpAllowFfi));
    println!("{}", t_cli(CliMsg::HelpAllowPath));
    println!("{}", t_cli(CliMsg::HelpH));
    println!("{}", t_cli(CliMsg::HelpV));
    println!();
    println!("{}", t_cli(CliMsg::HelpEnvHeader));
    println!("{}", t_cli(CliMsg::HelpOptiveHome));
    println!("{}", t_cli(CliMsg::HelpLocalDeps));
    println!("{}", t_cli(CliMsg::HelpOptiveCustomEnv));
    println!("{}", t_cli(CliMsg::HelpOptiveIndexUrl));
    println!();
    println!("{}", t_cli(CliMsg::HelpFiles));
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
    println!("{}", t_repl(ReplMsg::HelpTitle));
    println!("{}", t_repl(ReplMsg::HelpHelp));
    println!("{}", t_repl(ReplMsg::HelpQuit));
    println!("{}", t_repl(ReplMsg::HelpCtrlC));
    println!("{}", t_repl(ReplMsg::HelpCtrlD));
}

fn repl() {
    let mut rl: Editor<ReplHelper, DefaultHistory> = match Editor::new() {
        Ok(mut e) => {
            e.set_helper(Some(ReplHelper {
                colored_prompt: String::new(),
                line_cache: LineHighlightCache::default(),
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
    let pack = custom::active_pack();
    let primary = pack.repl_prompt().to_string();
    let continuation = pack.repl_continuation().to_string();

    loop {
        let prompt = if accumulator.is_empty() {
            primary.as_str()
        } else {
            continuation.as_str()
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
