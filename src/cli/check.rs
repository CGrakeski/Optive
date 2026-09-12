//! `Optive check [path]`：词法/语法 + 与 LSP 共享的名字/`std.*`/arity，不启动 VM。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use optive::lsp;

use super::color;
use super::manifest;

#[derive(Debug, Clone, Copy, Default)]
pub struct CheckOptions {
    pub json: bool,
    pub deny_warnings: bool,
}

pub fn cmd_check_args(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut options = CheckOptions::default();
    let mut path = None;
    for arg in args {
        match arg.as_str() {
            "--json" => options.json = true,
            "--deny-warnings" => options.deny_warnings = true,
            "-h" | "--help" => {
                println!(
                    "usage: Optive check [path] [--json] [--deny-warnings]\n\
                     \n\
                     --json           emit a stable JSON diagnostic array\n\
                     --deny-warnings  make warnings fail the check"
                );
                return Ok(());
            }
            _ if arg.starts_with('-') => {
                return Err(format!("unknown check option: {arg}").into());
            }
            _ if path.is_none() => path = Some(PathBuf::from(arg)),
            _ => return Err("Optive check accepts at most one path".into()),
        }
    }
    cmd_check_with_options(path.as_deref(), options)
}

struct Report {
    file: String,
    source: String,
    line: usize,
    column: usize,
    severity: optive::semantic::Severity,
    code: &'static str,
    message: String,
    help: Option<&'static str>,
}

pub fn cmd_check_with_options(
    path: Option<&Path>,
    options: CheckOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let files = collect_targets(path)?;
    if files.is_empty() {
        return Err("no .tive files to check".into());
    }
    // Load the complete source set before analysis. This makes imports resolve
    // against one consistent project snapshot instead of checking isolated files.
    let mut docs = HashMap::new();
    let mut sources = Vec::with_capacity(files.len());
    for file in &files {
        let display = file.display().to_string().replace('\\', "/");
        match fs::read_to_string(file) {
            Ok(src) => {
                docs.insert(lsp::file_uri(file), src.clone());
                sources.push((file, display, src));
            }
            Err(e) => return Err(format!("cannot read {display}: {e}").into()),
        }
    }
    let mut reports = Vec::new();
    let mut clean_files = Vec::new();
    for (file, display, src) in sources {
        let uri = lsp::file_uri(file);
        let diags = lsp::diagnostics_in(&src, &uri, &docs);
        if diags.is_empty() {
            clean_files.push(display);
        } else {
            for (line, column, message) in diags {
                let metadata = optive::semantic::diagnostic_metadata(&message);
                reports.push(Report {
                    file: display.clone(),
                    source: src.clone(),
                    line,
                    column,
                    severity: metadata.severity,
                    code: metadata.code,
                    message,
                    help: metadata.help,
                });
            }
        }
    }
    reports.sort_by(|a, b| {
        (&a.file, a.line, a.column, a.code, &a.message)
            .cmp(&(&b.file, b.line, b.column, b.code, &b.message))
    });
    reports.dedup_by(|a, b| {
        a.file == b.file
            && a.line == b.line
            && a.column == b.column
            && a.code == b.code
            && a.message == b.message
    });

    if options.json {
        let values: Vec<_> = reports
            .iter()
            .map(|report| {
                serde_json::json!({
                    "file": report.file,
                    "line": report.line,
                    "column": report.column,
                    "severity": match report.severity {
                        optive::semantic::Severity::Error => "error",
                        optive::semantic::Severity::Warning => "warning",
                    },
                    "code": report.code,
                    "message": report.message,
                    "help": report.help,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&values)?);
    } else {
        for display in clean_files {
            println!("ok {display}");
        }
        for report in &reports {
            let label = match report.severity {
                optive::semantic::Severity::Error => {
                    color::red(&format!("error[{}]: ", report.code))
                }
                optive::semantic::Severity::Warning => {
                    color::purple(&format!("warning[{}]: ", report.code))
                }
            };
            eprintln!(
                "{}",
                optive::diagnostics::format_check_diagnostic(
                    &report.source,
                    &report.file,
                    report.line,
                    report.column,
                    &label,
                    &report.message,
                    report.help,
                )
            );
        }
    }

    let errors = reports
        .iter()
        .filter(|report| report.severity == optive::semantic::Severity::Error)
        .count();
    let warnings = reports.len() - errors;
    let failed_files = reports
        .iter()
        .filter(|report| {
            report.severity == optive::semantic::Severity::Error
                || (options.deny_warnings && report.severity == optive::semantic::Severity::Warning)
        })
        .map(|report| report.file.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let n = files.len();
    let failed = errors > 0 || (options.deny_warnings && warnings > 0);
    if !options.json {
        color::status_line(&format!(
            "checked {n} file(s): {errors} error(s), {warnings} warning(s)"
        ));
    }
    if !failed {
        Ok(())
    } else {
        Err(format!(
            "check failed: {errors} error(s), {warnings} warning(s) in {failed_files} file(s)"
        )
        .into())
    }
}

fn collect_targets(path: Option<&Path>) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    match path {
        Some(p) if p.is_file() => Ok(vec![p.to_path_buf()]),
        Some(p) if p.is_dir() => {
            let project = manifest::find_project(Some(p))?;
            Ok(project_tive_files(&project.root))
        }
        Some(p) => Err(format!("not a file or directory: {}", p.display()).into()),
        None => {
            let project = manifest::find_project(None)?;
            Ok(project_tive_files(&project.root))
        }
    }
}

pub(super) fn project_tive_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_tive(root.join("src"), &mut files);
    collect_tive(root.join("tests"), &mut files);
    files.sort();
    files
}

fn collect_tive(dir: PathBuf, out: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    let Ok(rd) = fs::read_dir(&dir) else {
        return;
    };
    let mut ents: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let name = e.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            collect_tive(p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("tive") {
            out.push(p);
        }
    }
}
