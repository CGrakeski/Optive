//! `Optive fmt [path] [--check] [-o|--out]`：文件或整个项目。

use std::fs;
use std::path::{Path, PathBuf};

use optive::diagnostics;
use optive::fmt::format_source;

use super::check;
use super::manifest;

pub fn cmd_fmt(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut out_only = false;
    let mut check_only = false;
    let mut path: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "-o" | "--out" => out_only = true,
            "--check" => check_only = true,
            "-h" | "--help" => {
                println!("usage: Optive fmt [path] [--check] [-o|--out]");
                println!("  no path: format src/ and tests/ of the current project");
                println!("  default: write formatted source back");
                println!("  --check: exit non-zero if any file would change");
                println!("  -o, --out: print formatted source to stdout only (single file)");
                return Ok(());
            }
            s if s.starts_with('-') => return Err(format!("unknown fmt flag: {s}").into()),
            s => {
                if path.is_some() {
                    return Err("usage: Optive fmt [path] [--check] [-o|--out]".into());
                }
                path = Some(s);
            }
        }
    }
    if out_only && check_only {
        return Err("--check and --out cannot be combined".into());
    }
    let files = collect_fmt_targets(path)?;
    if files.is_empty() {
        return Err("no .tive files to format".into());
    }
    if out_only && files.len() != 1 {
        return Err("--out requires a single file".into());
    }
    let mut dirty = 0usize;
    for file in &files {
        let display = file.display().to_string().replace('\\', "/");
        let source = fs::read_to_string(file)?;
        let formatted = format_source(&source)
            .map_err(|e| diagnostics::format_parse_error(&source, &display, &e))?;
        if formatted == source {
            if out_only {
                print!("{formatted}");
            }
            continue;
        }
        dirty += 1;
        if check_only {
            eprintln!("would reformat {display}");
            continue;
        }
        if out_only {
            print!("{formatted}");
        } else {
            fs::write(file, formatted)?;
        }
    }
    if check_only && dirty > 0 {
        return Err(format!("{dirty} file(s) need formatting").into());
    }
    if !out_only && !check_only {
        println!("formatted {} file(s); {dirty} changed", files.len());
    }
    Ok(())
}

fn collect_fmt_targets(path: Option<&str>) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    match path {
        Some(p) => {
            let p = Path::new(p);
            if p.is_file() {
                Ok(vec![p.to_path_buf()])
            } else if p.is_dir() {
                let project = manifest::find_project(Some(p))?;
                Ok(check::project_tive_files(&project.root))
            } else {
                Err(format!("not a file or directory: {}", p.display()).into())
            }
        }
        None => {
            let project = manifest::find_project(None)?;
            Ok(check::project_tive_files(&project.root))
        }
    }
}
