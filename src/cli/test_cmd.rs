//! `Optive test`：发现并运行项目 `tests/**/*.tive`。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use optive::caps::Capabilities;
use optive::coverage::CoverageState;
use optive::run_source_in_vm;
use optive::shared::Shared;
use optive::vm::Vm;

use super::color;
use super::debug_cmd::inject_dep_map;
use super::deps;
use super::manifest;
use super::resolve::EnsureResult;

pub struct TestOptions {
    pub cover: bool,
    pub filter: Option<String>,
    pub jobs: usize,
    pub junit: Option<PathBuf>,
    pub lcov: Option<PathBuf>,
    pub cobertura: Option<PathBuf>,
    pub cover_min: Option<f64>,
}

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            cover: false,
            filter: None,
            jobs: 1,
            junit: None,
            lcov: None,
            cobertura: None,
            cover_min: None,
        }
    }
}

pub fn cmd_test(
    path: Option<&Path>,
    caps: Capabilities,
    script_args: &[String],
    opts: TestOptions,
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
    if let Some(filter) = &opts.filter {
        files.retain(|f| {
            f.to_string_lossy()
                .replace('\\', "/")
                .contains(filter.as_str())
        });
    }

    if files.is_empty() {
        println!("no tests found (looked for tests/**/*.tive)");
        return Ok(());
    }

    let cover_state = if opts.cover {
        Some(Shared::new(CoverageState::with_root(&project.root)))
    } else {
        None
    };

    let jobs = opts.jobs.max(1);
    let mut results: Vec<(String, Result<(), String>, Vec<String>)> = Vec::new();
    if jobs == 1 {
        for file in &files {
            results.push(run_listed_test(
                file,
                &project.root,
                &tests_root,
                &ensured,
                &caps,
                script_args,
                cover_state.clone(),
            ));
        }
    } else {
        let chunk = files.len().div_ceil(jobs).max(1);
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for part in files.chunks(chunk) {
                let ensured = &ensured;
                let caps = &caps;
                let cover_state = cover_state.clone();
                let root = &project.root;
                let tests_root = &tests_root;
                handles.push(scope.spawn(move || {
                    let mut local = Vec::new();
                    for file in part {
                        local.push(run_listed_test(
                            file,
                            root,
                            tests_root,
                            ensured,
                            caps,
                            script_args,
                            cover_state.clone(),
                        ));
                    }
                    local
                }));
            }
            for h in handles {
                if let Ok(part) = h.join() {
                    results.extend(part);
                } else {
                    results.push((
                        "<thread>".into(),
                        Err("test worker panicked".into()),
                        Vec::new(),
                    ));
                }
            }
        });
    }

    let mut passed = 0usize;
    let mut failed = 0usize;
    for (rel, result, log) in &results {
        print!("test {rel} ... ");
        match result {
            Ok(()) => {
                println!("ok");
                passed += 1;
            }
            Err(e) => {
                println!("FAILED");
                println!("  {e}");
                failed += 1;
            }
        }
        for line in log {
            println!("  {line}");
        }
    }

    if let Some(cov) = &cover_state {
        let report = project.root.join(".optive").join("cover.json");
        cov.borrow().write_report(&report)?;
        for (file, (hit, exec)) in cov.borrow().per_file() {
            let pct = if exec == 0 {
                "n/a".to_string()
            } else {
                format!("{:.0}%", (hit as f64) * 100.0 / (exec as f64))
            };
            println!("cover {file}  {hit}/{exec}  {pct}");
        }
        println!(
            "cover report: {}",
            report.display().to_string().replace('\\', "/")
        );
        if let Some(path) = &opts.lcov {
            write_lcov(&cov.borrow(), path)?;
            println!("lcov: {}", path.display().to_string().replace('\\', "/"));
        }
        if let Some(path) = &opts.cobertura {
            write_cobertura(&cov.borrow(), path)?;
            println!(
                "cobertura: {}",
                path.display().to_string().replace('\\', "/")
            );
        }
        if let Some(min) = opts.cover_min {
            let (hit, exec) = cov
                .borrow()
                .per_file()
                .values()
                .copied()
                .fold((0usize, 0usize), |acc, (h, e)| (acc.0 + h, acc.1 + e));
            let pct = if exec == 0 {
                100.0
            } else {
                (hit as f64) * 100.0 / (exec as f64)
            };
            if pct + f64::EPSILON < min {
                return Err(format!("coverage {pct:.1}% is below --cover-min {min}").into());
            }
        }
    }

    if let Some(path) = &opts.junit {
        write_junit(path, &results)?;
        println!("junit: {}", path.display().to_string().replace('\\', "/"));
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

fn run_listed_test(
    file: &Path,
    project_root: &Path,
    tests_root: &Path,
    ensured: &EnsureResult,
    caps: &Capabilities,
    script_args: &[String],
    cover_state: Option<Shared<CoverageState>>,
) -> (String, Result<(), String>, Vec<String>) {
    let rel = file
        .strip_prefix(project_root)
        .unwrap_or(file)
        .display()
        .to_string()
        .replace('\\', "/");
    let mut vm = Vm::new();
    vm.install_caps(caps.clone());
    vm.argv_override = Some(build_test_argv(&rel, script_args));
    inject_dep_map(&mut vm, ensured, project_root);
    if let Some(cov) = cover_state {
        optive::coverage::attach(&mut vm, cov);
    }
    let setup = nearest_fixture(file, tests_root, "_setup.tive");
    let teardown = nearest_fixture(file, tests_root, "_teardown.tive");
    let result = run_one_file(&mut vm, file, &rel, setup.as_deref(), teardown.as_deref());
    cleanup_tmp_dirs(&vm);
    let log = vm.test_case_log.clone();
    (rel, result, log)
}

fn run_one_file(
    vm: &mut Vm,
    file: &Path,
    rel: &str,
    setup: Option<&Path>,
    teardown: Option<&Path>,
) -> Result<(), String> {
    let setup_res = setup.map_or(Ok(()), |setup| {
        let src = fs::read_to_string(setup)
            .map_err(|e| format!("cannot read setup {}: {e}", display_rel(setup)))?;
        let srel = display_rel(setup);
        run_source_in_vm(vm, &src, &srel)
            .map(|_| ())
            .map_err(|e| format!("setup: {e}"))
    });
    let test_res = setup_res.and_then(|()| {
        let source =
            fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        run_source_in_vm(vm, &source, rel)
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    if let Some(td) = teardown {
        match fs::read_to_string(td) {
            Ok(src) => {
                let trel = display_rel(td);
                if let Err(e) = run_source_in_vm(vm, &src, &trel) {
                    eprintln!("  teardown WARN ({}): {e}", display_rel(td));
                }
            }
            Err(e) => {
                eprintln!("  teardown WARN ({}): cannot read: {e}", display_rel(td));
            }
        }
    }
    test_res
}

fn display_rel(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn nearest_fixture(test_file: &Path, tests_root: &Path, name: &str) -> Option<PathBuf> {
    let mut dir = test_file.parent()?.to_path_buf();
    loop {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
        if dir == tests_root {
            return None;
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn cleanup_tmp_dirs(vm: &Vm) {
    for dir in &vm.test_tmp_dirs {
        if let Err(e) = fs::remove_dir_all(dir) {
            eprintln!("  cleanup WARN ({}): {e}", display_rel(dir));
        }
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
            if name == "_setup.tive" || name == "_teardown.tive" {
                continue;
            }
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

/// 从 `--` 之前抽出测试标志。
pub fn take_test_flags(args: &[String]) -> Result<(TestOptions, Vec<String>), String> {
    let mut opts = TestOptions::default();
    let mut out = Vec::new();
    let mut seen_dd = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !seen_dd && a == "--" {
            seen_dd = true;
            out.push(a.clone());
            i += 1;
            continue;
        }
        if seen_dd {
            out.push(a.clone());
            i += 1;
            continue;
        }
        match a.as_str() {
            "--cover" | "-cover" => opts.cover = true,
            "--filter" => {
                i += 1;
                opts.filter = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| "--filter requires a pattern".to_string())?,
                );
            }
            "--jobs" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| "--jobs requires a number".to_string())?;
                opts.jobs = raw.parse().map_err(|_| format!("invalid --jobs {raw}"))?;
                if opts.jobs == 0 {
                    return Err("--jobs must be >= 1".into());
                }
            }
            "--junit" => {
                i += 1;
                opts.junit = Some(
                    args.get(i)
                        .map(PathBuf::from)
                        .ok_or_else(|| "--junit requires a path".to_string())?,
                );
            }
            "--lcov" => {
                i += 1;
                opts.cover = true;
                opts.lcov = Some(
                    args.get(i)
                        .map(PathBuf::from)
                        .ok_or_else(|| "--lcov requires a path".to_string())?,
                );
            }
            "--cobertura" => {
                i += 1;
                opts.cover = true;
                opts.cobertura = Some(
                    args.get(i)
                        .map(PathBuf::from)
                        .ok_or_else(|| "--cobertura requires a path".to_string())?,
                );
            }
            "--cover-min" => {
                i += 1;
                opts.cover = true;
                let raw = args
                    .get(i)
                    .ok_or_else(|| "--cover-min requires a number".to_string())?;
                let value: f64 = raw
                    .parse()
                    .map_err(|_| format!("invalid --cover-min {raw}"))?;
                if !value.is_finite() {
                    return Err(format!("invalid --cover-min {raw}"));
                }
                opts.cover_min = Some(value);
            }
            _ => out.push(a.clone()),
        }
        i += 1;
    }
    Ok((opts, out))
}

fn write_junit(
    path: &Path,
    results: &[(String, Result<(), String>, Vec<String>)],
) -> io::Result<()> {
    let failed = results.iter().filter(|(_, r, _)| r.is_err()).count();
    let mut xml = String::new();
    xml.push_str(&format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><testsuite name="optive" tests="{}" failures="{failed}">"#,
        results.len()
    ));
    for (name, result, _) in results {
        match result {
            Ok(()) => xml.push_str(&format!(
                r#"<testcase name="{}" classname="optive"/>"#,
                xml_escape(name)
            )),
            Err(e) => xml.push_str(&format!(
                r#"<testcase name="{}" classname="optive"><failure message="{}"/></testcase>"#,
                xml_escape(name),
                xml_escape(e)
            )),
        }
    }
    xml.push_str("</testsuite>");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, xml)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn write_lcov(cov: &CoverageState, path: &Path) -> io::Result<()> {
    let mut out = String::new();
    for (file, (hit, exec)) in cov.per_file() {
        let _ = (hit, exec);
        out.push_str("TN:\n");
        out.push_str(&format!("SF:{file}\n"));
        let mut lines: Vec<usize> = cov
            .executable
            .iter()
            .filter(|(f, _)| *f == file)
            .map(|(_, l)| *l)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        for line in lines {
            let hits = usize::from(cov.hits.contains(&(file.clone(), line)));
            out.push_str(&format!("DA:{line},{hits}\n"));
        }
        out.push_str("end_of_record\n");
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, out)
}

fn write_cobertura(cov: &CoverageState, path: &Path) -> io::Result<()> {
    let mut xml = String::from(
        r#"<?xml version="1.0"?><coverage><packages><package name="optive"><classes>"#,
    );
    for (file, (hit, exec)) in cov.per_file() {
        let rate = if exec == 0 {
            1.0
        } else {
            hit as f64 / exec as f64
        };
        xml.push_str(&format!(
            r#"<class filename="{}" line-rate="{rate:.4}"><lines>"#,
            xml_escape(&file)
        ));
        let mut lines: Vec<usize> = cov
            .executable
            .iter()
            .filter(|(f, _)| *f == file)
            .map(|(_, l)| *l)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        for line in lines {
            let hits = u32::from(cov.hits.contains(&(file.clone(), line)));
            xml.push_str(&format!(r#"<line number="{line}" hits="{hits}"/>"#));
        }
        xml.push_str("</lines></class>");
    }
    xml.push_str("</classes></package></packages></coverage>");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, xml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_test_flags_filter_jobs_and_reports() {
        let args = vec![
            "--filter".into(),
            "smoke".into(),
            "--jobs".into(),
            "4".into(),
            "--junit".into(),
            "out.xml".into(),
            "tests".into(),
        ];
        let (opts, rest) = take_test_flags(&args).unwrap();
        assert_eq!(opts.filter.as_deref(), Some("smoke"));
        assert_eq!(opts.jobs, 4);
        assert_eq!(opts.junit.as_deref(), Some(Path::new("out.xml")));
        assert_eq!(rest, vec!["tests"]);
    }

    #[test]
    fn take_test_flags_rejects_cover_min_nan() {
        let args = vec!["--cover-min".into(), "NaN".into()];
        match take_test_flags(&args) {
            Err(err) => assert!(
                err.contains("invalid --cover-min"),
                "unexpected error: {err}"
            ),
            Ok(_) => panic!("NaN must be rejected as invalid --cover-min"),
        }
    }
}
