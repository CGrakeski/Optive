//! `Optive check [path]`：只做词法/语法，不启动 VM。

use std::fs;
use std::path::{Path, PathBuf};

use optive::diagnostics;
use optive::parser::Parser;

use super::color;
use super::manifest;

pub fn cmd_check(path: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let files = collect_targets(path)?;
    if files.is_empty() {
        return Err("no .tive files to check".into());
    }
    let mut errors = 0usize;
    for file in &files {
        let display = file.display().to_string().replace('\\', "/");
        match fs::read_to_string(file) {
            Ok(src) => match Parser::parse(&src) {
                Ok(_) => println!("ok {display}"),
                Err(e) => {
                    errors += 1;
                    eprintln!("{}", diagnostics::format_parse_error(&src, &display, &e));
                }
            },
            Err(e) => {
                errors += 1;
                eprintln!("cannot read {display}: {e}");
            }
        }
    }
    let n = files.len();
    if errors == 0 {
        color::status_line(&format!("check ok. {n} file(s)"));
        Ok(())
    } else {
        eprintln!("check FAILED. {errors} error(s) in {n} file(s)");
        Err(format!("{errors} file(s) failed check").into())
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

fn project_tive_files(root: &Path) -> Vec<PathBuf> {
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
