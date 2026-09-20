#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod cli;

use std::env;
use std::path::{Path, PathBuf};
use std::process;

use std::borrow::Cow;

use optive::custom::{self, CliMsg, Diag, ReplMsg};
use optive::{repl_needs_continuation, run_source_in_vm, vm::Vm};
use rustyline::completion::{Completer as CompleterTrait, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::history::DefaultHistory;
use rustyline::{Context, Editor, Helper, Hinter, Result as ReadlineResult, Validator};

use crate::cli::debug_cmd::inject_dep_map;
use cli::color;
use cli::main_index;
use cli::repl_highlight::LineHighlightCache;
use cli::resolve::EnsureResult;
use optive::caps::Capabilities;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Windows 上 rustyline 按原始 prompt 算宽度且不忽略 ANSI；
/// 颜色只能放在 Highlighter，不能塞进 `readline` 的 prompt 字符串。
#[derive(Helper, Hinter, Validator)]
struct ReplHelper {
    colored_prompt: String,
    completion_prefix: String,
    line_cache: LineHighlightCache,
}

impl CompleterTrait for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> ReadlineResult<(usize, Vec<Self::Candidate>)> {
        Ok(repl_completions(&self.completion_prefix, line, pos))
    }
}

fn repl_completions(prefix: &str, line: &str, pos: usize) -> (usize, Vec<Pair>) {
    let pos = floor_char_boundary(line, pos.min(line.len()));
    let before = &line[..pos];
    let start = before
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_alphanumeric() && *ch != '_')
        .map_or(0, |(index, ch)| index + ch.len_utf8());

    let mut source = prefix.to_string();
    if !source.is_empty() && !source.ends_with('\n') {
        source.push('\n');
    }
    let lsp_line = source.bytes().filter(|byte| *byte == b'\n').count();
    source.push_str(line);
    let lsp_column = before.encode_utf16().count();
    let items = optive::lsp::completion(&source, lsp_line, lsp_column);
    let candidates = items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.to_string();
            let detail = item.get("detail").and_then(serde_json::Value::as_str);
            Some(Pair {
                display: detail.map_or_else(|| label.clone(), |text| format!("{label}\t{text}")),
                replacement: label,
            })
        })
        .collect();
    (start, candidates)
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
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

    fn highlight_char(&self, line: &str, pos: usize, kind: CmdKind) -> bool {
        self.line_cache.should_refresh(line, pos, kind)
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
    match std::env::current_exe()
        .map_err(|error| error.to_string())
        .and_then(|path| optive::compiler::standalone::read(&path))
    {
        Ok(Some(bundle)) => {
            color::init(color::ColorChoice::Auto);
            init_custom(None);
            let script_args = raw_args.iter().skip(1).cloned().collect::<Vec<_>>();
            if let Err(error) = cmd_run_bundle_bytes(&bundle, Capabilities::full(), &script_args) {
                color::eprint_error(error.to_string());
                process::exit(1);
            }
            return;
        }
        Ok(None) => {}
        Err(error) => {
            color::init(color::ColorChoice::Auto);
            color::eprint_error(format!("Error: {error}"));
            process::exit(1);
        }
    }
    let (color_choice, args) = color::take_color_args(&raw_args);
    color::init(color_choice);
    let (quiet, args) = color::take_quiet_arg(&args);
    color::set_quiet(quiet);
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
                    Err(e) => {
                        color::eprint_error(format!("Error: {e}"));
                        process::exit(2);
                    }
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
            "repl" => {
                let (caps, rest) = parse_caps_or_exit(&args);
                if rest.len() > 1 {
                    color::eprint_error("usage: Optive repl [path] [capability flags]");
                    process::exit(2);
                }
                if let Err(error) = cmd_repl_project(rest.first().map(Path::new), caps) {
                    color::eprint_error(format!("Error: {error}"));
                    process::exit(1);
                }
                return;
            }
            "build" => {
                if let Err(e) = cli::build_cmd::cmd_build(&args[2..]) {
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
            "init" => {
                if args.len() != 2 {
                    color::eprint_error("usage: Optive init");
                    process::exit(2);
                }
                if let Err(e) = cmd_init() {
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
                if let Err(e) = cli::fmt_cmd::cmd_fmt(&args[2..]) {
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
                let (opts, rest) = match cli::test_cmd::take_test_flags(&rest) {
                    Ok(v) => v,
                    Err(e) => {
                        color::eprint_error(format!("Error: {e}"));
                        process::exit(2);
                    }
                };
                let (path, script_args) = parse_project_path_and_script_args(&rest);
                if let Err(e) = cli::test_cmd::cmd_test(path.as_deref(), caps, &script_args, opts) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "check" => {
                if let Err(e) = cli::check::cmd_check_args(&args[2..]) {
                    color::eprint_error(format!("Error: {e}"));
                    process::exit(1);
                }
                return;
            }
            "dap" => {
                let launcher: optive::dap::LaunchBootstrap =
                    std::sync::Arc::new(cli::debug_cmd::prepare_dap_vm);
                if let Err(e) = optive::dap::run_stdio_with_launcher(launcher) {
                    color::eprint_error(format!("DAP: {e}"));
                    process::exit(1);
                }
                return;
            }
            "lsp" => {
                if let Err(e) = optive::lsp::run_stdio() {
                    color::eprint_error(format!("LSP: {e}"));
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
                        color::eprint_error("usage: Optive index sync | Optive index change <url>");
                        process::exit(2);
                    }
                }
                return;
            }
            path => {
                if path.starts_with("--") {
                    let (caps, rest) = parse_caps_from(&args[1..]);
                    if rest.is_empty() {
                        repl(caps);
                        return;
                    }
                    let script = &rest[0];
                    if script.ends_with(".tive") || Path::new(script).is_file() {
                        run_script_file(script, caps);
                        return;
                    }
                    color::eprint_error(format!("unknown command or file: {script}"));
                    color::eprint_error("try: Optive --help");
                    process::exit(2);
                }
                if path.ends_with(".tive") || Path::new(path).is_file() {
                    let (caps, _rest) = parse_caps_from(&args[2..]);
                    run_script_file(path, caps);
                    return;
                }
                color::eprint_error(format!("unknown command or file: {path}"));
                color::eprint_error("try: Optive --help");
                process::exit(2);
            }
        }
    }

    repl(Capabilities::full());
}

fn parse_project_path_and_script_args(rest: &[String]) -> (Option<PathBuf>, Vec<String>) {
    let (path, script_args) = match split_project_and_script_args(rest) {
        Ok(v) => v,
        Err(e) => {
            color::eprint_error(format!("Error: {e}"));
            process::exit(2);
        }
    };
    (path, script_args)
}

fn parse_caps_from(args: &[String]) -> (Capabilities, Vec<String>) {
    match cli::caps::parse_caps(args) {
        Ok(v) => v,
        Err(e) => {
            color::eprint_error(format!("Error: {e}"));
            process::exit(2);
        }
    }
}

fn parse_caps_or_exit(args: &[String]) -> (Capabilities, Vec<String>) {
    parse_caps_from(&args[2..])
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

fn cmd_init() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let (root, name) = cli::new_project::init_project(&cwd)?;
    color::status_line(&format!("Initialized project {name} in {}", root.display()));
    println!("  Optive.toml");
    println!("  src/main.tive");
    println!("  .gitignore");
    println!();
    println!("Next:");
    println!("  Optive run");
    Ok(())
}

fn cmd_run(
    path: Option<&Path>,
    mut caps: Capabilities,
    script_args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(bundle_path) = path.filter(|candidate| optive::bundle::is_bundle_path(candidate)) {
        return cmd_run_bundle(bundle_path, caps, script_args);
    }
    let project = cli::manifest::find_project(path)?;
    print_project_header(&project);
    let ensured = cli::deps::ensure_for_run(&project)?;
    print_ensure_report(&ensured);
    env::set_current_dir(&project.root)?;
    caps.configure_project_fs(
        &project.root,
        ensured.dep_map.values().map(|binding| binding.path.clone()),
    );
    let entry = project.entry_path_with_caps(&caps)?;
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

fn cmd_repl_project(
    path: Option<&Path>,
    mut caps: Capabilities,
) -> Result<(), Box<dyn std::error::Error>> {
    let project = cli::manifest::find_project(path)?;
    print_project_header(&project);
    let ensured = cli::deps::ensure_for_run(&project)?;
    print_ensure_report(&ensured);
    env::set_current_dir(&project.root)?;
    caps.configure_project_fs(
        &project.root,
        ensured.dep_map.values().map(|binding| binding.path.clone()),
    );
    color::status_line("Starting project REPL");
    repl_with_project(caps, Some((project.root, ensured)));
    Ok(())
}

fn cmd_run_bundle(
    path: &Path,
    caps: Capabilities,
    script_args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    cmd_run_bundle_bytes(&bytes, caps, script_args)
}

fn cmd_run_bundle_bytes(
    bytes: &[u8],
    caps: Capabilities,
    script_args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let image = optive::bundle::BundleImage::decode(bytes)?;
    let entry = image
        .get(&image.entry_package, &image.entry_module)
        .ok_or_else(|| {
            format!(
                "bundle entry {}:{} is missing",
                image.entry_package, image.entry_module
            )
        })?;
    let compiled = optive::bc_cache::decode_program(&entry.bytecode)
        .map_err(|error| format!("bundle entry bytecode is unreadable: {error}"))?;
    let entry_display = format!("bundle:{}:{}", image.entry_package, image.entry_module);
    color::status_line(&format!("Running {entry_display}"));
    let mut vm = Vm::new();
    vm.install_caps(caps);
    vm.argv_override = Some(build_script_argv(&entry_display, script_args));
    vm.current_package_id = image.entry_package.clone();
    vm.source_file = entry_display.clone();
    vm.bundle = Some(std::sync::Arc::new(image));
    vm.load_program(compiled)?;
    vm.run().map_err(|error| error.to_string())?;
    Ok(())
}

fn cmd_up(
    path: Option<&Path>,
    mut caps: Capabilities,
    script_args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let project = cli::manifest::find_project(path)?;
    print_project_header(&project);
    color::status_line("Updating dependencies…");
    let ensured = cli::deps::ensure_for_update(&project, None)?;
    print_ensure_report(&ensured);
    env::set_current_dir(&project.root)?;
    caps.configure_project_fs(
        &project.root,
        ensured.dep_map.values().map(|binding| binding.path.clone()),
    );
    let entry = project.entry_path_with_caps(&caps)?;
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
    let exe = env::args().next().unwrap_or_else(|| "Optive".to_string());
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
                name = Some(args.get(i).ok_or("--name requires a value")?.clone());
            }
            "--branch" => {
                i += 1;
                branch = Some(args.get(i).ok_or("--branch requires a value")?.clone());
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
    let target = target
        .ok_or("usage: Optive add <git-url|pack[@version]> [--name N] [--branch B|--tag T]")?;
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
    let name_width = hits[..show].iter().map(|(n, _)| n.len()).max().unwrap_or(0);
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
    let spec = args
        .first()
        .ok_or("usage: Optive change track_latest=true|false")?;
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
    mut caps: Capabilities,
    argv_override: Option<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    caps.configure_project_fs(
        project_root,
        ensured.dep_map.values().map(|binding| binding.path.clone()),
    );
    let source = caps
        .read_to_string("script entry", path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = path.to_string_lossy().to_string();
    run_in_vm(&source, &file, caps, argv_override, false, |vm| {
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
    let source = caps
        .read_to_string("script entry", path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = path.to_string_lossy().to_string();
    run_in_vm(&source, &file, caps, None, false, |_| {})
}

fn run_inline_source(source: &str, caps: Capabilities) -> Result<(), Box<dyn std::error::Error>> {
    run_in_vm(source, "<string>", caps, None, true, |_| {})
}

/// 公共 VM 执行入口：创建 VM、设置能力、运行源码。
/// `print_result` 仅给 `-c`：脚本 / `run` 的 stdout 只应由 `print()` 写入。
fn run_in_vm(
    source: &str,
    file: &str,
    caps: Capabilities,
    argv_override: Option<Vec<String>>,
    print_result: bool,
    setup: impl FnOnce(&mut Vm),
) -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = Vm::new();
    vm.install_caps(caps);
    vm.argv_override = argv_override;
    setup(&mut vm);
    match run_source_in_vm(&mut vm, source, file) {
        Ok(v) => {
            if print_result && !matches!(v, optive::value::Value::None) {
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
    println!("  Optive repl [path]              Start a project-aware REPL");
    println!("{}", t_cli(CliMsg::HelpRunScript));
    println!("{}", t_cli(CliMsg::HelpRunCode));
    println!("{}", t_cli(CliMsg::HelpNew));
    println!("{}", t_cli(CliMsg::HelpInit));
    println!("{}", t_cli(CliMsg::HelpRun));
    println!(
        "  Optive build [path] [--explain] [--clean] [--bundle|--exe] [--target TARGET] [--output PATH] [--all-modules] [--jobs N] [--emit bytecode|interface|all]"
    );
    println!("  Optive run <app.tivb>");
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
    println!("{}", t_cli(CliMsg::HelpCheck));
    println!("{}", t_cli(CliMsg::HelpLsp));
    println!("{}", t_cli(CliMsg::HelpDap));
    println!("{}", t_cli(CliMsg::HelpIndex));
    println!("{}", t_cli(CliMsg::HelpIndexChange));
    println!("{}", t_cli(CliMsg::HelpCustom));
    println!();
    println!("{}", t_cli(CliMsg::HelpCapsHeader));
    println!("{}", t_cli(CliMsg::HelpQuiet));
    println!("{}", t_cli(CliMsg::HelpSandbox));
    println!("{}", t_cli(CliMsg::HelpNoNetwork));
    println!("{}", t_cli(CliMsg::HelpNoFfi));
    println!("{}", t_cli(CliMsg::HelpAllowFfi));
    println!("{}", t_cli(CliMsg::HelpAllowPath));
    println!("{}", t_cli(CliMsg::HelpTrustDeps));
    println!("{}", t_cli(CliMsg::HelpAllowDepNetwork));
    println!("{}", t_cli(CliMsg::HelpAllowDepEnv));
    println!("{}", t_cli(CliMsg::HelpAllowDepProcess));
    println!("{}", t_cli(CliMsg::HelpAllowDepFfi));
    println!("{}", t_cli(CliMsg::HelpH));
    println!("{}", t_cli(CliMsg::HelpV));
    println!();
    println!("{}", t_cli(CliMsg::HelpEnvHeader));
    println!("{}", t_cli(CliMsg::HelpOptiveHome));
    println!("{}", t_cli(CliMsg::HelpLocalDeps));
    println!("{}", t_cli(CliMsg::HelpOptiveCustomEnv));
    println!("{}", t_cli(CliMsg::HelpOptiveIndexUrl));
    println!("{}", t_cli(CliMsg::HelpOptiveIndexPin));
    println!("{}", t_cli(CliMsg::HelpOptiveIndexPolicy));
    println!();
    println!("{}", t_cli(CliMsg::HelpFiles));
}

fn history_path() -> PathBuf {
    if let Some(p) = env::var_os("OPTIVE_HISTORY") {
        return PathBuf::from(p);
    }
    cli::home::optive_home().join("repl.history")
}

#[derive(Debug, Clone)]
struct ReplCell {
    id: usize,
    source: String,
}

fn new_repl_vm(caps: &Capabilities, project: Option<&(PathBuf, EnsureResult)>) -> Vm {
    let mut vm = Vm::new();
    vm.install_caps(caps.clone());
    if let Some((root, ensured)) = project {
        inject_dep_map(&mut vm, ensured, root);
    }
    vm
}

fn print_repl_check(source: &str) {
    if source.trim().is_empty() {
        println!("check: no cells");
        return;
    }
    let diagnostics = optive::lsp::diagnostics(source, "<repl:check>");
    if diagnostics.is_empty() {
        println!("check: ok");
        return;
    }
    for (line, column, message) in diagnostics {
        let metadata = optive::semantic::diagnostic_metadata(&message);
        let severity = match metadata.severity {
            optive::semantic::Severity::Error => "error",
            optive::semantic::Severity::Warning => "warning",
        };
        println!(
            "{severity}[{}] <repl:check>:{line}:{column}: {message}",
            metadata.code
        );
        if let Some(help) = metadata.help {
            println!("  help: {help}");
        }
    }
}

fn print_repl_vars(vm: &Vm) {
    let builtins: std::collections::HashSet<&str> = optive::api_registry::BUILTINS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let core_types: std::collections::HashSet<&str> =
        optive::type_registry::global_type_names().collect();
    let function_names: std::collections::HashSet<String> =
        vm.functions.keys().into_iter().collect();
    let mut values: Vec<_> = vm
        .debug_list_globals()
        .into_iter()
        .filter(|(name, value)| {
            !name.starts_with("__")
                && !builtins.contains(name.as_str())
                && !core_types.contains(name.as_str())
                && !function_names.contains(name)
                && !matches!(
                    value,
                    optive::value::Value::TypeRef(_) | optive::value::Value::TypeSpec(_)
                )
        })
        .collect();
    values.sort_by(|left, right| left.0.cmp(&right.0));
    if values.is_empty() {
        println!("<no user variables>");
    } else {
        for (name, value) in values {
            println!("{name} = {}", value.display_string());
        }
    }
}

fn print_repl_functions(vm: &Vm) {
    let mut names = vm.functions.keys();
    names.sort();
    if names.is_empty() {
        println!("<no user functions>");
    } else {
        for name in names {
            println!("{name}");
        }
    }
}

fn append_repl_cell_source(session_source: &mut String, source: &str) {
    if !session_source.is_empty() && !session_source.ends_with('\n') {
        session_source.push('\n');
    }
    session_source.push_str(source);
    session_source.push('\n');
}

fn print_repl_help() {
    println!("{}", t_repl(ReplMsg::HelpTitle));
    println!("{}", t_repl(ReplMsg::HelpHelp));
    println!("{}", t_repl(ReplMsg::HelpQuit));
    println!("{}", t_repl(ReplMsg::HelpCtrlC));
    println!("{}", t_repl(ReplMsg::HelpCtrlD));
    println!("  :cancel            Cancel unfinished multi-line input");
    println!("  :reset             Clear all user state and cells");
    println!("  :vars              List user variables");
    println!("  :functions         List user functions");
    println!("  :caps              Show active host capabilities");
    println!("  :check [code]      Statically check the session or extra code");
    println!("  :history           List executed cells");
    println!("  :show <cell>       Show one cell");
}

fn repl(caps: Capabilities) {
    repl_with_project(caps, None);
}

fn repl_with_project(caps: Capabilities, project: Option<(PathBuf, EnsureResult)>) {
    let mut rl: Editor<ReplHelper, DefaultHistory> = match Editor::new() {
        Ok(mut e) => {
            e.set_helper(Some(ReplHelper {
                colored_prompt: String::new(),
                completion_prefix: String::new(),
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
    if let Err(error) = rl.load_history(&hist) {
        if !matches!(error, ReadlineError::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound)
        {
            color::eprint_error(format!("REPL history load failed: {error}"));
        }
    }

    let mut vm = new_repl_vm(&caps, project.as_ref());
    let mut accumulator = String::new();
    let mut session_source = String::new();
    let mut cells: Vec<ReplCell> = Vec::new();
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
            h.completion_prefix.clone_from(&session_source);
            if !accumulator.is_empty() {
                if !h.completion_prefix.is_empty() && !h.completion_prefix.ends_with('\n') {
                    h.completion_prefix.push('\n');
                }
                h.completion_prefix.push_str(&accumulator);
                h.completion_prefix.push('\n');
            }
        }
        match rl.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim_end();
                let cmd = trimmed.trim();
                if !accumulator.is_empty() && cmd == ":cancel" {
                    accumulator.clear();
                    continue;
                }
                if accumulator.is_empty() {
                    match cmd {
                        ":help" | "help" => {
                            print_repl_help();
                            continue;
                        }
                        ":quit" | ":exit" | "quit" | "exit" => break,
                        ":reset" => {
                            vm = new_repl_vm(&caps, project.as_ref());
                            session_source.clear();
                            cells.clear();
                            println!("REPL state reset");
                            continue;
                        }
                        ":vars" => {
                            print_repl_vars(&vm);
                            continue;
                        }
                        ":functions" => {
                            print_repl_functions(&vm);
                            continue;
                        }
                        ":caps" => {
                            println!("network: {}", caps.network);
                            println!("environment: {}", caps.env);
                            println!("process: {}", caps.process);
                            println!("ffi: {}", caps.ffi);
                            println!("filesystem: {:?}", caps.fs);
                            continue;
                        }
                        ":history" => {
                            if cells.is_empty() {
                                println!("<no cells>");
                            } else {
                                for cell in &cells {
                                    let preview = cell.source.lines().next().unwrap_or("");
                                    let suffix = if cell.source.lines().count() > 1 {
                                        " …"
                                    } else {
                                        ""
                                    };
                                    println!("{}: {preview}{suffix}", cell.id);
                                }
                            }
                            continue;
                        }
                        _ => {}
                    }
                    if let Some(raw_id) = cmd.strip_prefix(":show ") {
                        match raw_id
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|id| cells.iter().find(|cell| cell.id == id))
                        {
                            Some(cell) => println!("{}", cell.source),
                            None => color::eprint_error("unknown REPL cell"),
                        }
                        continue;
                    }
                    if cmd == ":check" || cmd.starts_with(":check ") {
                        let extra = cmd.strip_prefix(":check").unwrap_or("").trim();
                        let mut source = session_source.clone();
                        if !extra.is_empty() {
                            append_repl_cell_source(&mut source, extra);
                        }
                        print_repl_check(&source);
                        continue;
                    }
                    if cmd.starts_with(':') {
                        color::eprint_error(format!("unknown REPL command: {cmd}"));
                        continue;
                    }
                }
                if cmd.is_empty() {
                    if !accumulator.is_empty() {
                        accumulator.push('\n');
                    }
                    continue;
                }

                if !accumulator.is_empty() {
                    accumulator.push('\n');
                }
                accumulator.push_str(trimmed);

                if repl_needs_continuation(&accumulator) {
                    continue;
                }

                let segment = accumulator.clone();
                accumulator.clear();
                let cell_id = cells.len() + 1;
                let cell_file = format!("<repl:{cell_id}>");
                if let Err(error) = rl.add_history_entry(&segment) {
                    color::eprint_error(format!("REPL history update failed: {error}"));
                }

                match run_source_in_vm(&mut vm, &segment, &cell_file) {
                    Ok(v) => {
                        append_repl_cell_source(&mut session_source, &segment);
                        cells.push(ReplCell {
                            id: cell_id,
                            source: segment,
                        });
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

    if let Some(parent) = hist.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            color::eprint_error(format!("REPL history directory failed: {error}"));
            return;
        }
    }
    if let Err(error) = rl.save_history(&hist) {
        color::eprint_error(format!("REPL history save failed: {error}"));
    }
}

#[cfg(test)]
mod repl_tests {
    use super::*;

    #[test]
    fn completion_uses_prior_repl_cells() {
        let (_, candidates) = repl_completions("func alpha() { return 1 }\n", "alp", "alp".len());
        assert!(candidates
            .iter()
            .any(|candidate| candidate.replacement == "alpha"));
    }

    #[test]
    fn completion_replaces_only_member_suffix() {
        let (start, candidates) = repl_completions("", "std.ma", "std.ma".len());
        assert_eq!(start, "std.".len());
        assert!(candidates
            .iter()
            .any(|candidate| candidate.replacement == "math"));
    }

    #[test]
    fn completion_position_never_splits_utf8() {
        assert_eq!(floor_char_boundary("名x", 1), 0);
        assert_eq!(floor_char_boundary("名x", 3), 3);
    }

    #[test]
    fn cell_source_is_separated_for_analysis() {
        let mut source = String::new();
        append_repl_cell_source(&mut source, "let x = 1");
        append_repl_cell_source(&mut source, "x + 1");
        assert_eq!(source, "let x = 1\nx + 1\n");
    }
}
