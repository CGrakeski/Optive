//! `Optive test`：发现并运行项目 `tests/**/*.tive`。

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use optive::caps::Capabilities;
use optive::run_source_in_vm;
use optive::vm::Vm;

use super::color;
use super::debug_cmd::inject_dep_map;
use super::deps;
use super::manifest;

pub fn cmd_test(
    path: Option<&Path>,
    caps: Capabilities,
    script_args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let project = manifest::find_project(path)?;
    color::status_line(&format!("Test project {}", project.root.display()));
    let ensured = deps::ensure_for_run(&project)?;
    std::env::set_current_dir(&project.root)?;

    let tests_root = project.root.join("tests");
    let mut files = Vec::new();
    if tests_root.is_dir() {
        collect_tive_files(&tests_root, &mut files);
    }
    files.sort();

    if files.is_empty() {
        println!("no tests found (looked for tests/**/*.tive)");
        return Ok(());
    }

    let mut passed = 0usize;
    let mut failed = 0usize;
    for file in &files {
        let rel = file
            .strip_prefix(&project.root)
            .unwrap_or(file)
            .display()
            .to_string()
            .replace('\\', "/");
        print!("test {rel} ... ");
        let _ = io::stdout().flush();
        let source = match fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                println!("FAILED");
                println!("  cannot read {}: {e}", file.display());
                failed += 1;
                continue;
            }
        };
        let mut vm = Vm::new();
        vm.caps = caps.clone();
        vm.argv_override = Some(build_test_argv(&rel, script_args));
        inject_dep_map(&mut vm, &ensured, &project.root);
        match run_source_in_vm(&mut vm, &source, &rel) {
            Ok(_) => {
                println!("ok");
                passed += 1;
            }
            Err(e) => {
                println!("FAILED");
                println!("  {e}");
                failed += 1;
            }
        }
    }

    let total = passed + failed;
    if failed == 0 {
        println!("\ntest result: ok. {passed} passed; 0 failed; {total} total");
        Ok(())
    } else {
        println!("\ntest result: FAILED. {passed} passed; {failed} failed; {total} total");
        Err(format!("{failed} test(s) failed").into())
    }
}

fn collect_tive_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let mut ents: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            collect_tive_files(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("tive") {
            out.push(p);
        }
    }
}

fn build_test_argv(entry_display: &str, script_args: &[String]) -> Vec<String> {
    let exe = std::env::args()
        .next()
        .unwrap_or_else(|| "Optive".to_string());
    let mut argv = Vec::with_capacity(2 + script_args.len());
    argv.push(exe);
    argv.push(entry_display.to_string());
    argv.extend(script_args.iter().cloned());
    argv
}
