#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
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
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(41 + 1)\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["run"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("42"), "expected print 42, got: {stdout}");
    assert!(root.join("Optive.lock").is_file(), "should write lock");
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

    let (code, stdout, stderr) = run_optive_env(&["run"], &root, &[("OPTIVE_USE_LOCAL_DEPS", "1")]);
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
        root.join("Optive.lock"),
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

    let (code, stdout, stderr) = run_optive_env(&["run"], &root, &[("OPTIVE_USE_LOCAL_DEPS", "1")]);
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(
        stderr.contains("expected 2")
            || stderr.contains("delete")
            || stderr.contains("regenerate")
            || stderr.contains("out of date")
            || stderr.contains("update")
            || stderr.contains("invalid"),
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
    assert!(stdout.contains("Hello from HelloApp"), "stdout={stdout}");
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

    let (code, stdout, stderr) = run_optive_env(&["run"], &root, &[("OPTIVE_USE_LOCAL_DEPS", "1")]);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("hello"), "stdout={stdout}");
}

#[test]
fn sandbox_reads_src_main_dependency_but_keeps_it_read_only() {
    let root = tempfile_project("sandbox_dep_ro");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "sandbox_dep_ro"
entry = "src/main.tive"

[dependencies]
greeter = { git = "https://github.com/example/greeter.git", rev = "abababababababababababababababababababab" }
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.tive"),
        "import greeter\nprint(greeter.hi)\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("deps/greeter/src")).unwrap();
    fs::write(
        root.join("deps/greeter/src/main.tive"),
        "export let hi = \"hello from src\"\n",
    )
    .unwrap();

    let env = [("OPTIVE_USE_LOCAL_DEPS", "1")];
    let (code, stdout, stderr) = run_optive_env(&["run", "--sandbox"], &root, &env);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("hello from src"), "stdout={stdout}");

    fs::write(
        root.join("src/main.tive"),
        "std.fs.write_text(\"deps/greeter/hack.txt\", \"no\")\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run_optive_env(&["run", "--sandbox"], &root, &env);
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(stderr.contains("read-only dependency root"), "{stderr}");
    assert!(!root.join("deps/greeter/hack.txt").exists());
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

    let (code, stdout, stderr) = run_optive_env(&["run"], &root, &[("OPTIVE_USE_LOCAL_DEPS", "1")]);
    // LOCAL_DEPS 同名冲突：greeter 会装 logging 到 deps/logging，根也会…
    // 根 import logging 未声明 → 应失败
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(
        stderr.contains("undeclared") || stderr.contains("logging") || stderr.contains("Error"),
        "stderr={stderr}"
    );
}

#[test]
fn run_inline_code_flag() {
    let root = tempfile_project("inline_c");
    let (code, stdout, stderr) = run_optive(&["-c", "print(40 + 2)"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("42"), "expected 42, got: {stdout}");
}

#[test]
fn run_inline_code_multiline() {
    let root = tempfile_project("inline_c_ml");
    let src = "let x = 1\nprint(x + 1)\n";
    let (code, stdout, stderr) = run_optive(&["-c", src], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains('2'), "expected 2, got: {stdout}");
}

#[test]
fn run_inline_code_hex_escape() {
    let root = tempfile_project("inline_c_hex");
    let (code, stdout, stderr) = run_optive(&["-c", r#"print("\x41\x42")"#], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("AB"), "expected AB, got: {stdout}");
}

#[test]
fn run_inline_code_missing_arg() {
    let root = tempfile_project("inline_c_miss");
    let (code, _stdout, stderr) = run_optive(&["-c"], &root);
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(
        stderr.contains("usage") || stderr.contains("-c"),
        "stderr={stderr}"
    );
}

#[test]
fn run_dashdash_uses_cwd_and_passes_script_args() {
    let root = tempfile_project("run_dashdash");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "run_dashdash"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.tive"),
        r"
let a = std.os.args()
print(a[len(a) - 2])
print(a[len(a) - 1])
",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["run", "--", "tests/data", "out.json"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("tests/data") && stdout.contains("out.json"),
        "expected script args in stdout, got: {stdout}"
    );
}

#[test]
fn run_path_then_dashdash_script_args() {
    let root = tempfile_project("run_path_dash");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "run_path_dash"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.tive"),
        "print(std.os.args()[len(std.os.args()) - 1])\n",
    )
    .unwrap();

    // 在独立 cwd 下用绝对项目路径调用，避免依赖隐式 cwd 发现。
    let cwd = tempfile_project("run_path_dash_cwd");
    let root_abs = fs::canonicalize(&root).unwrap_or(root);
    let (code, stdout, stderr) = run_optive(
        &[
            "run",
            root_abs.to_str().expect("utf8 path"),
            "--",
            "only_arg",
        ],
        &cwd,
    );
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("only_arg"), "got: {stdout}");
}

#[test]
fn run_sandbox_before_dashdash() {
    let root = tempfile_project("run_sandbox_dash");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "run_sandbox_dash"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.tive"),
        "print(std.os.args()[len(std.os.args()) - 1])\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["run", "--sandbox", "--", "sand_arg"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("sand_arg"), "got: {stdout}");
}

#[test]
fn run_rejects_multiple_args_before_dashdash() {
    let root = tempfile_project("run_multi_pre_dash");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "run_multi_pre_dash"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(1)\n").unwrap();

    let (code, _stdout, stderr) = run_optive(&["run", "a", "b", "--", "x"], &root);
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(
        stderr.contains("too many arguments before '--'") || stderr.contains("Error:"),
        "stderr={stderr}"
    );
}

#[test]
fn help_lists_test_and_index_sync() {
    let root = tempfile_project("help_cmds");
    let (code, stdout, stderr) = run_optive(&["--help"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("index sync"),
        "help should mention index sync:\n{stdout}"
    );
    assert!(
        stdout.contains("Optive test"),
        "help should mention test:\n{stdout}"
    );
    assert!(
        stdout.contains("Optive dap"),
        "help should mention dap:\n{stdout}"
    );
    assert!(
        stdout.contains("--trust-deps"),
        "help should mention --trust-deps:\n{stdout}"
    );
}

#[test]
fn test_command_runs_tive_files() {
    let root = tempfile_project("tive_tests");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "tive_tests"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(1)\n").unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("tests/ok.tive"),
        "use std.test.{ assert_eq }\nassert_eq(1 + 1, 2)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["test"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("ok.tive"), "stdout={stdout}");
    assert!(stdout.contains("test result: ok"), "stdout={stdout}");
}

#[test]
fn test_command_setup_and_each() {
    let root = tempfile_project("tive_tests_setup");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "tive_tests_setup"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(1)\n").unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(root.join("tests/_setup.tive"), "let setup_flag = 7\n").unwrap();
    fs::write(
        root.join("tests/uses_setup.tive"),
        r#"
use std.test.{ assert_eq, each }
assert_eq(setup_flag, 7)
each("add", [[1, 2, 3], [2, 2, 4]], do(row) {
    assert_eq(row[0] + row[1], row[2])
})
"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["test"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("add[0] ok") || stdout.contains("test result: ok"),
        "stdout={stdout}"
    );
}

#[test]
fn test_command_uses_nearest_nested_fixtures() {
    let root = tempfile_project("tive_tests_nested_fixture");
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"nested_fixture\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "").unwrap();
    fs::create_dir_all(root.join("tests/nested")).unwrap();
    fs::write(root.join("tests/_setup.tive"), "let fixture_value = 1\n").unwrap();
    fs::write(
        root.join("tests/nested/_setup.tive"),
        "let fixture_value = 2\n",
    )
    .unwrap();
    fs::write(
        root.join("tests/nested/_teardown.tive"),
        "std.fs.write_text(\"nested-teardown.txt\", \"done\")\n",
    )
    .unwrap();
    fs::write(
        root.join("tests/nested/case.tive"),
        "use std.test.{ assert_eq }\nassert_eq(fixture_value, 2)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["test"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(root.join("nested-teardown.txt").is_file());
}

#[test]
fn test_command_runs_teardown_after_body_failure() {
    let root = tempfile_project("tive_tests_failed_body_teardown");
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"failed_body_teardown\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "").unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("tests/_teardown.tive"),
        "std.fs.write_text(\"body-teardown.txt\", \"done\")\n",
    )
    .unwrap();
    fs::write(
        root.join("tests/fails.tive"),
        "use std.test.{ assert_eq }\nassert_eq(1, 2)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["test"], &root);
    assert_ne!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(root.join("body-teardown.txt").is_file());
}

#[test]
fn test_command_runs_teardown_after_setup_failure() {
    let root = tempfile_project("tive_tests_failed_setup_teardown");
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"failed_setup_teardown\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "").unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("tests/_setup.tive"),
        "throw AssertionError(\"setup failed\")\n",
    )
    .unwrap();
    fs::write(
        root.join("tests/_teardown.tive"),
        "std.fs.write_text(\"setup-teardown.txt\", \"done\")\n",
    )
    .unwrap();
    fs::write(root.join("tests/case.tive"), "none\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["test"], &root);
    assert_ne!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(root.join("setup-teardown.txt").is_file());
}

#[test]
fn test_each_logs_every_assertion_failure_detail() {
    let root = tempfile_project("tive_tests_each_failures");
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"each_failures\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "").unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("tests/each.tive"),
        r#"
use std.test.{ assert_eq, each }
each("rows", [1, 2, 3], do(row) {
    assert_eq(row, 0)
})
"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["test"], &root);
    assert_ne!(code, 0, "stderr={stderr}\nstdout={stdout}");
    for index in 0..3 {
        assert!(
            stdout.contains(&format!("rows[{index}] FAILED:")),
            "stdout={stdout}"
        );
    }
    assert!(stdout.contains("1 != 0"), "stdout={stdout}");
    assert!(stdout.contains("3 != 0"), "stdout={stdout}");
}

#[test]
fn test_command_cover_writes_json() {
    let root = tempfile_project("tive_tests_cover");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "tive_tests_cover"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.tive"),
        "export func never_called() {\n    let untouched = 41\n    return untouched + 1\n}\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("tests/ok.tive"),
        "use std.test.{ assert_eq }\nimport \"../src/main.tive\" as app\nassert_eq(1, 1)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["test", "--cover"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("cover") || root.join(".optive/cover.json").is_file(),
        "stdout={stdout}"
    );
    assert!(
        root.join(".optive/cover.json").is_file(),
        "missing cover.json"
    );
    let report: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join(".optive/cover.json")).expect("read cover report"),
    )
    .expect("parse cover report");
    let module = &report["files"]["src/main.tive"];
    assert!(
        module.is_object(),
        "report should use a stable project-relative module path: {report}"
    );
    let hit = module["hit"].as_u64().expect("module hit count");
    let exec = module["exec"].as_u64().expect("module executable count");
    assert!(
        exec > hit,
        "uncalled imported function lines must remain in the denominator: {module}"
    );
}

#[test]
fn test_command_fails_on_assertion() {
    let root = tempfile_project("tive_tests_fail");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "tive_tests_fail"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(1)\n").unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("tests/bad.tive"),
        "use std.test.{ assert_eq }\nassert_eq(1, 2)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["test"], &root);
    assert_ne!(code, 0, "expected failure, stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("FAILED") || stderr.contains("failed"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn test_command_passes_script_args_after_dashdash() {
    let root = tempfile_project("tive_tests_args");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "tive_tests_args"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(1)\n").unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("tests/args.tive"),
        r#"
use std.test.{ assert_eq }
let a = std.os.args()
assert_eq(a[len(a) - 1], "from-test")
"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["test", "--", "from-test"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("test result: ok"), "stdout={stdout}");
}

#[test]
fn test_command_rejects_extra_positionals_without_dashdash() {
    let root = tempfile_project("tive_tests_extra");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "tive_tests_extra"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(1)\n").unwrap();

    let (code, _stdout, stderr) = run_optive(&["test", "a", "b"], &root);
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(
        stderr.contains("too many arguments") || stderr.contains("Error:"),
        "stderr={stderr}"
    );
}

#[test]
fn check_ok_project_and_file() {
    let root = tempfile_project("check_ok");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "check_ok"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "let x = 1\nprint(x)\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["check"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("check ok") || stdout.contains("ok "),
        "stdout={stdout}"
    );

    let file = root.join("src/main.tive");
    let (code, stdout, stderr) = run_optive(&["check", file.to_str().unwrap()], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
}

#[test]
fn check_undefined_name_exits_1() {
    let root = tempfile_project("check_undef");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "check_undef"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(no_such_name)\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["check"], &root);
    assert_eq!(code, 1, "stdout={stdout}\nstderr={stderr}");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("undefined name") || combined.contains("FAILED"),
        "out={combined}"
    );
}

#[test]
fn check_reports_parse_error() {
    let root = tempfile_project("check_bad");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "check_bad"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "let x =\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["check"], &root);
    assert_ne!(code, 0, "stdout={stdout}");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("error") || combined.contains("FAILED") || combined.contains("lex"),
        "out={combined}"
    );
}

#[test]
fn fmt_check_fails_when_dirty_and_passes_when_clean() {
    let root = tempfile_project("fmt_check");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "fmt_check"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    let dirty = "func add(a,b){\nreturn a+b\n}\n";
    fs::write(root.join("src/main.tive"), dirty).unwrap();

    let (code, stdout, stderr) = run_optive(&["fmt", "--check"], &root);
    assert_ne!(code, 0, "stdout={stdout}\nstderr={stderr}");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("would reformat") || combined.contains("need formatting"),
        "out={combined}"
    );

    let (code, stdout, stderr) = run_optive(&["fmt"], &root);
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    let (code, stdout, stderr) = run_optive(&["fmt", "--check"], &root);
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
}

fn tempfile_project(name: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("optive_test_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}
