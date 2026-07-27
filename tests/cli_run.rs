mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn optive_bin() -> PathBuf {
    let target = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .to_string_lossy()
            .into_owned()
    });
    let mut p = PathBuf::from(target);
    p.push("debug");
    p.push(if cfg!(windows) {
        "Optive.exe"
    } else {
        "Optive"
    });
    p
}

fn run_optive(args: &[&str], cwd: &std::path::Path) -> (i32, String, String) {
    run_optive_env(args, cwd, &[])
}

fn run_optive_env(
    args: &[&str],
    cwd: &std::path::Path,
    env: &[(&str, &str)],
) -> (i32, String, String) {
    let bin = optive_bin();
    assert!(
        bin.is_file(),
        "Optive binary missing at {}; run `cargo build --bin Optive` first",
        bin.display()
    );
    let mut cmd = Command::new(&bin);
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    // 隔离全局 home，避免污染开发者机器
    let home = cwd.join(".optive_home");
    let _ = fs::create_dir_all(&home);
    cmd.env("OPTIVE_HOME", &home);
    let out = cmd.output().expect("spawn Optive");
    let code = out.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (code, stdout, stderr)
}

#[test]
fn run_project_with_manifest_no_deps() {
    let root = tempfile_project("demo_no_deps");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "demo_no_deps"
version = "0.1.0"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(41 + 1)\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["run"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("42"), "expected print 42, got: {stdout}");
    assert!(root.join("optive.lock").is_file(), "should write lock");
}

#[test]
fn run_local_deps_reuses_existing_dir() {
    let root = tempfile_project("demo_cached_dep");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "demo_cached_dep"
entry = "main.tive"

[dependencies]
fake_lib = { git = "https://github.com/example/fake_lib.git", rev = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
"#,
    )
    .unwrap();
    fs::write(root.join("main.tive"), "print(\"ok\")\n").unwrap();
    fs::create_dir_all(root.join("deps/fake_lib")).unwrap();
    fs::write(root.join("deps/fake_lib/main.tive"), "export let x = 1\n").unwrap();

    let (code, stdout, stderr) = run_optive_env(
        &["run"],
        &root,
        &[("OPTIVE_USE_LOCAL_DEPS", "1")],
    );
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("ok"), "stdout={stdout}");
}

#[test]
fn run_fails_when_lock_stale() {
    let root = tempfile_project("demo_stale_lock");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "demo_stale_lock"
entry = "main.tive"

[dependencies]
helper = { git = "https://github.com/example/helper.git", rev = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }
"#,
    )
    .unwrap();
    fs::write(root.join("main.tive"), "print(1)\n").unwrap();
    fs::write(
        root.join("optive.lock"),
        r#"
version = 1

[[edges]]
parent = "__root__"
name = "old"
git = "https://github.com/example/old.git"
rev = "cccccccccccccccccccccccccccccccccccccccc"
id = "dead"
"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive_env(
        &["run"],
        &root,
        &[("OPTIVE_USE_LOCAL_DEPS", "1")],
    );
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(
        stderr.contains("out of date")
            || stderr.contains("update")
            || stderr.contains("up")
            || stderr.contains("invalid")
            || stderr.contains("pinned")
            || stderr.contains("missing"),
        "stderr={stderr}"
    );
}

#[test]
fn env_prints_home() {
    let root = tempfile_project("demo_env");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "demo_env"
entry = "main.tive"
"#,
    )
    .unwrap();
    fs::write(root.join("main.tive"), "none\n").unwrap();
    let (code, stdout, stderr) = run_optive(&["env"], &root);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("OPTIVE_HOME"), "stdout={stdout}");
    assert!(stdout.contains("index.db"), "stdout={stdout}");
}

#[test]
fn manifest_unit_parse_via_cli_module() {
    let src = r#"
[package]
name = "x"
[dependencies]
a = "https://github.com/a/b"
b = { git = "https://github.com/c/d", tag = "v1" }
"#;
    let v: toml::Table = src.parse().unwrap();
    assert_eq!(v["package"]["name"].as_str(), Some("x"));
}

#[test]
fn run_with_forced_color_emits_ansi() {
    let root = tempfile_project("demo_color");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "demo_color"
version = "0.0.1"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(1)\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["--color", "run"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("\u{1b}[32m  Project demo_color"),
        "expected green indented Project line, got: {stdout:?}"
    );
    assert!(stdout.contains("Running src"), "stdout={stdout}");
}

#[test]
fn run_with_no_color_has_no_ansi() {
    let root = tempfile_project("demo_nocolor");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "demo_nocolor"
entry = "main.tive"
"#,
    )
    .unwrap();
    fs::write(root.join("main.tive"), "none\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["--no-color", "run"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        !stdout.contains('\u{1b}'),
        "expected no ANSI, got: {stdout:?}"
    );
    assert!(stdout.contains("  Project demo_nocolor"), "stdout={stdout}");
}

#[test]
fn new_then_run_project() {
    let parent = tempfile_project("new_parent");
    let name = "HelloApp";
    let (code, stdout, stderr) = run_optive(&["new", name], &parent);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    let root = parent.join(name);
    assert!(root.join("Optive.toml").is_file());
    assert!(root.join("src/main.tive").is_file());
    assert!(root.join(".gitignore").is_file());

    let (code, stdout, stderr) = run_optive(&["run"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("Hello from HelloApp"),
        "stdout={stdout}"
    );
}

#[test]
fn new_rejects_existing_dir() {
    let parent = tempfile_project("new_exists");
    let name = "Dup";
    let (code, _, _) = run_optive(&["new", name], &parent);
    assert_eq!(code, 0);
    let (code, _, stderr) = run_optive(&["new", name], &parent);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("already exists") || stderr.contains("Error"),
        "stderr={stderr}"
    );
}

#[test]
fn import_declared_local_dep() {
    let root = tempfile_project("demo_import_dep");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "demo_import_dep"
entry = "main.tive"

[dependencies]
greeter = { git = "https://github.com/example/greeter.git", rev = "cccccccccccccccccccccccccccccccccccccccc" }
"#,
    )
    .unwrap();
    fs::write(
        root.join("main.tive"),
        "import greeter\nprint(greeter.hi)\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("deps/greeter")).unwrap();
    fs::write(
        root.join("deps/greeter/main.tive"),
        "export let hi = \"hello\"\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive_env(
        &["run"],
        &root,
        &[("OPTIVE_USE_LOCAL_DEPS", "1")],
    );
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("hello"), "stdout={stdout}");
}

#[test]
fn undeclared_transitive_import_fails() {
    let root = tempfile_project("demo_phantom");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "demo_phantom"
entry = "main.tive"

[dependencies]
greeter = { git = "https://github.com/example/greeter.git", rev = "dddddddddddddddddddddddddddddddddddddddd" }
"#,
    )
    .unwrap();
    // 根试图 import logging，但未声明
    fs::write(root.join("main.tive"), "import logging\nprint(1)\n").unwrap();
    fs::create_dir_all(root.join("deps/greeter")).unwrap();
    fs::write(
        root.join("deps/greeter/Optive.toml"),
        r#"
[package]
name = "greeter"
[dependencies]
logging = { git = "https://github.com/example/logging.git", rev = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee" }
"#,
    )
    .unwrap();
    fs::write(root.join("deps/greeter/main.tive"), "export let x = 1\n").unwrap();
    fs::create_dir_all(root.join("deps/logging")).unwrap();
    fs::write(root.join("deps/logging/main.tive"), "export let y = 2\n").unwrap();

    let (code, stdout, stderr) = run_optive_env(
        &["run"],
        &root,
        &[("OPTIVE_USE_LOCAL_DEPS", "1")],
    );
    // LOCAL_DEPS 同名冲突：greeter 会装 logging 到 deps/logging，根也会…
    // 根 import logging 未声明 → 应失败
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(
        stderr.contains("undeclared") || stderr.contains("logging") || stderr.contains("Error"),
        "stderr={stderr}"
    );
}

fn tempfile_project(name: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("optive_test_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}
