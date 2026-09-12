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
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const LOCAL_DEPS_ENV: &[(&str, &str)] = &[
    ("OPTIVE_USE_LOCAL_DEPS", "1"),
    ("OPTIVE_ALLOW_UNVERIFIED_FIXTURE", "1"),
];

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

fn run_optive_with_stdin(
    args: &[&str],
    cwd: &std::path::Path,
    input: &str,
) -> (i32, String, String) {
    let home = cwd.join(".optive_home");
    fs::create_dir_all(&home).unwrap();
    let mut child = Command::new(optive_bin())
        .args(args)
        .current_dir(cwd)
        .env("OPTIVE_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn Optive REPL");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(input.as_bytes())
        .expect("write REPL input");
    let output = child.wait_with_output().expect("wait for Optive REPL");
    (
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn project_repl_runs_cells_lists_state_and_tracks_cell_sources() {
    let root = tempfile_project("project_repl");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"project_repl\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(root.join("src/main.tive"), "0\n").unwrap();

    let input = concat!(
        "let answer = 41\n",
        "answer + 1\n",
        ":vars\n",
        ":history\n",
        ":check print(1 / 0)\n",
        "func boom() { missing }\n",
        "boom()\n",
        ":quit\n",
    );
    let (code, stdout, stderr) =
        run_optive_with_stdin(&["--color=never", "repl", "."], &root, input);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("42"), "{stdout}");
    assert!(stdout.contains("answer = 41"), "{stdout}");
    assert!(stdout.contains("1: let answer = 41"), "{stdout}");
    assert!(stdout.contains("error[E7001]"), "{stdout}");
    assert!(stderr.contains("Project project_repl"), "{stderr}");
    assert!(stderr.contains("<repl:3>"), "{stderr}");
    assert!(stderr.contains("<repl:4>"), "{stderr}");
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
    assert!(
        !stdout.contains("Project"),
        "status lines must not pollute stdout: {stdout:?}"
    );
    assert!(
        stderr.contains("Project") || stderr.contains("Running"),
        "status lines belong on stderr, got: {stderr:?}"
    );
    assert!(root.join("Optive.lock").is_file(), "should write lock");
}

#[test]
fn build_emits_incremental_module_interfaces_and_bundle() {
    let root = tempfile_project("build_artifacts");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"build_artifacts\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.tive"),
        "export const ANSWER = 6 * 7\nexport const func twice(x) { return x * 2 }\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["build", "--bundle"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    let build = root.join(".optive/build");
    assert!(build.join("graph.json").is_file());
    assert!(build.join("app.tivb").is_file());
    assert_eq!(fs::read_dir(build.join("modules")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(build.join("interfaces")).unwrap().count(), 1);
    let interface_path = fs::read_dir(build.join("interfaces"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let interface: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(interface_path).unwrap()).unwrap();
    let answer = interface["exports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "ANSWER")
        .unwrap();
    assert_eq!(answer["value"]["value"], 42);

    let (code, stdout, stderr) = run_optive(&["build", "--explain"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("HIT "), "{stdout}");
    assert!(stdout.contains("reused 1"), "{stdout}");
}

#[test]
fn build_uses_topological_const_interfaces_and_precise_invalidation() {
    let root = tempfile_project("build_graph_ctfe");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"build_graph_ctfe\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/constants.tive"),
        "export const BASE = 21\nlet private_detail = 1\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.tive"),
        "use constants.{ BASE }\nexport const ANSWER = BASE * 2\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["build", "--explain"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("BUILD __root__:src/constants.tive"),
        "{stdout}"
    );
    assert!(stdout.contains("BUILD __root__:src/main.tive"), "{stdout}");

    let interfaces = root.join(".optive/build/interfaces");
    let main_interface = fs::read_dir(&interfaces)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| fs::read_to_string(entry.path()).unwrap())
        .find(|text| text.contains("src/main.tive"))
        .unwrap();
    let main: serde_json::Value = serde_json::from_str(&main_interface).unwrap();
    let answer = main["exports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "ANSWER")
        .unwrap();
    assert_eq!(answer["value"]["value"], 42);

    fs::write(
        root.join("src/constants.tive"),
        "export const BASE = 21\nlet private_detail = 999\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run_optive(&["build", "--explain"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("BUILD __root__:src/constants.tive"),
        "{stdout}"
    );
    assert!(stdout.contains("HIT  __root__:src/main.tive"), "{stdout}");

    fs::write(
        root.join("src/constants.tive"),
        "export const BASE = 22\nlet private_detail = 999\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run_optive(&["build", "--explain"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("BUILD __root__:src/constants.tive"),
        "{stdout}"
    );
    assert!(stdout.contains("BUILD __root__:src/main.tive"), "{stdout}");
}

#[test]
fn build_reports_module_cycles_with_the_cycle_path() {
    let root = tempfile_project("build_cycle");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"build_cycle\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(root.join("src/main.tive"), "import a\n").unwrap();
    fs::write(root.join("src/a.tive"), "import b\n").unwrap();
    fs::write(root.join("src/b.tive"), "import a\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["build"], &root);
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(stderr.contains("module dependency cycle"), "{stderr}");
    assert!(
        stderr.contains("src/a.tive") && stderr.contains("src/b.tive"),
        "{stderr}"
    );
}

#[test]
fn build_injects_imported_module_namespace_into_ctfe() {
    let root = tempfile_project("build_import_member");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"build_import_member\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(root.join("src/config.tive"), "export const VALUE = 21\n").unwrap();
    fs::write(
        root.join("src/main.tive"),
        "import config\nexport const ANSWER = config.VALUE * 2\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["build", "--explain"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    let interfaces = root.join(".optive/build/interfaces");
    let main = fs::read_dir(&interfaces)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| fs::read_to_string(entry.path()).unwrap())
        .find(|text| text.contains("src/main.tive"))
        .unwrap();
    let main: serde_json::Value = serde_json::from_str(&main).unwrap();
    let answer = main["exports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "ANSWER")
        .unwrap();
    assert_eq!(answer["value"]["value"], 42);
    assert_eq!(main["format"], 3);
}

#[test]
fn build_emit_and_reachability_and_stale_gc() {
    let root = tempfile_project("build_emit_reach");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"build_emit_reach\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(root.join("src/main.tive"), "export const ANSWER = 1\n").unwrap();
    fs::write(root.join("src/unused.tive"), "export const DEAD = 9\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["build", "--emit", "interface"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert_eq!(
        fs::read_dir(root.join(".optive/build/interfaces"))
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        fs::read_dir(root.join(".optive/build/modules"))
            .unwrap()
            .count(),
        0
    );

    let (code, stdout, stderr) = run_optive(&["build", "--all-modules", "--emit", "all"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert_eq!(
        fs::read_dir(root.join(".optive/build/interfaces"))
            .unwrap()
            .count(),
        2
    );
    assert_eq!(
        fs::read_dir(root.join(".optive/build/modules"))
            .unwrap()
            .count(),
        2
    );

    fs::remove_file(root.join("src/unused.tive")).unwrap();
    let (code, stdout, stderr) = run_optive(&["build"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert_eq!(
        fs::read_dir(root.join(".optive/build/interfaces"))
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        fs::read_dir(root.join(".optive/build/modules"))
            .unwrap()
            .count(),
        1
    );
}

fn snapshot_build_dir(root: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    use std::collections::BTreeMap;
    let build = root.join(".optive/build");
    let mut files = BTreeMap::new();
    for dir_name in ["modules", "interfaces"] {
        let dir = build.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            files.insert(
                format!("{dir_name}/{}", entry.file_name().to_string_lossy()),
                fs::read(entry.path()).unwrap(),
            );
        }
    }
    files.insert(
        "graph.json".into(),
        fs::read(build.join("graph.json")).unwrap(),
    );
    files
}

#[test]
fn build_parallel_and_serial_artifacts_match() {
    let root = tempfile_project("build_jobs_determinism");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"build_jobs_determinism\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(root.join("src/a.tive"), "export const A = 1\n").unwrap();
    fs::write(root.join("src/b.tive"), "export const B = 2\n").unwrap();
    fs::write(
        root.join("src/main.tive"),
        "import a\nimport b\nexport const ANSWER = a.A + b.B\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["build", "--jobs", "1", "--clean"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    let serial = snapshot_build_dir(&root);

    let (code, stdout, stderr) = run_optive(&["build", "--jobs", "8", "--clean"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    let parallel = snapshot_build_dir(&root);
    assert_eq!(serial, parallel);
}

#[test]
fn build_bundle_is_binary_and_runnable() {
    let root = tempfile_project("build_bundle_run");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"build_bundle_run\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(root.join("src/config.tive"), "export const VALUE = 21\n").unwrap();
    fs::write(
        root.join("src/main.tive"),
        "import config\nexport const ANSWER = config.VALUE * 2\nprint(ANSWER)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["build", "--bundle"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    let bundle = root.join(".optive/build/app.tivb");
    let bytes = fs::read(&bundle).unwrap();
    assert_eq!(&bytes[..4], b"TIVB");

    let (code, stdout, stderr) = run_optive(&["run", bundle.to_str().unwrap()], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("42"), "stdout={stdout}");

    let mut corrupt = bytes.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    let bad = root.join("bad.tivb");
    fs::write(&bad, corrupt).unwrap();
    let (code, stdout, stderr) = run_optive(&["run", bad.to_str().unwrap()], &root);
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(stderr.contains("checksum"), "{stderr}");

    let mut version = bytes;
    version[4..8].copy_from_slice(&99u32.to_le_bytes());
    let old = root.join("old.tivb");
    fs::write(&old, version).unwrap();
    let (code, stdout, stderr) = run_optive(&["run", old.to_str().unwrap()], &root);
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(stderr.contains("unsupported tivb version 99"), "{stderr}");
}

#[test]
fn build_exe_is_self_contained_and_runnable() {
    let root = tempfile_project("build_standalone_exe");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"build_standalone_exe\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.tive"),
        "print(40 + 2)\nif (len(std.os.args()) > 2) { print(std.os.args()[2]) }\n",
    )
    .unwrap();

    let copied = root.join("dist/copied-app.exe");
    let (code, stdout, stderr) = run_optive(
        &["build", "--exe", "--output", copied.to_str().unwrap()],
        &root,
    );
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    let name = if cfg!(windows) { "app.exe" } else { "app" };
    let executable = root.join(".optive/build").join(name);
    assert!(executable.is_file(), "missing {}", executable.display());
    assert!(
        copied.is_file(),
        "missing copied output {}",
        copied.display()
    );
    let output = Command::new(&executable)
        .current_dir(&root)
        .output()
        .expect("run standalone executable");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("42"));

    // Flag-looking arguments belong to the packaged application, not Optive.
    let output = Command::new(&executable)
        .arg("--quiet")
        .current_dir(&root)
        .output()
        .expect("run standalone executable with flag-like argument");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--quiet"));
}

#[test]
fn build_failed_rebuild_keeps_previous_artifacts() {
    let root = tempfile_project("build_atomic");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"build_atomic\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(root.join("src/config.tive"), "export const VALUE = 1\n").unwrap();
    fs::write(
        root.join("src/main.tive"),
        "import config\nexport const ANSWER = config.VALUE\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run_optive(&["build"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    let before = snapshot_build_dir(&root);

    fs::write(root.join("src/config.tive"), "export const VALUE = 2\n").unwrap();
    fs::write(
        root.join("src/main.tive"),
        "import config\nexport const ANSWER = (\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run_optive(&["build"], &root);
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(stderr.contains("Error") || !stderr.is_empty(), "{stderr}");
    let after = snapshot_build_dir(&root);
    assert_eq!(before, after);
}

#[test]
fn run_injects_imported_module_namespace_into_ctfe() {
    let root = tempfile_project("run_import_member");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"run_import_member\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(root.join("src/config.tive"), "export const VALUE = 21\n").unwrap();
    fs::write(
        root.join("src/main.tive"),
        "import config\nexport const ANSWER = config.VALUE * 2\nprint(ANSWER)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["run"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("42"), "stdout={stdout}");
}

#[test]
fn run_resolves_src_package_module_like_build() {
    let root = tempfile_project("run_src_module");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"run_src_module\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::write(root.join("src/helper.tive"), "export let n = 21\n").unwrap();
    fs::write(
        root.join("src/main.tive"),
        "import helper\nprint(helper.n * 2)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["run"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("42"), "stdout={stdout}");
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

    let (code, stdout, stderr) = run_optive_env(&["run"], &root, LOCAL_DEPS_ENV);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("ok"), "stdout={stdout}");
}

#[test]
fn local_deps_unmarked_dir_requires_fixture_flag() {
    let root = tempfile_project("demo_unmarked_dep");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "demo_unmarked_dep"
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
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(
        stderr.contains(".optive-id")
            || stderr.contains("UNVERIFIED")
            || stderr.contains("fixture"),
        "stderr={stderr}"
    );
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

    let (code, stdout, stderr) = run_optive_env(&["run"], &root, LOCAL_DEPS_ENV);
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(
        stderr.contains("expected 1")
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
        stderr.contains("\u{1b}[32m  Project demo_color"),
        "expected green indented Project line on stderr, got: {stderr:?}"
    );
    assert!(stderr.contains("Running src"), "stderr={stderr}");
    assert!(
        !stdout.contains("Project") && !stdout.contains("Running src"),
        "status lines must not be on stdout: {stdout:?}"
    );
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
        !stderr.contains('\u{1b}'),
        "expected no ANSI on stderr, got: {stderr:?}"
    );
    assert!(
        !stdout.contains('\u{1b}'),
        "expected no ANSI on stdout, got: {stdout:?}"
    );
    assert!(stderr.contains("  Project demo_nocolor"), "stderr={stderr}");
    assert!(
        !stdout.contains("Project"),
        "status lines must not be on stdout: {stdout:?}"
    );
}

#[test]
fn quiet_hides_status_lines() {
    let root = tempfile_project("demo_quiet");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "demo_quiet"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(9)\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["--quiet", "--no-color", "run"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains('9'), "expected print 9, got: {stdout}");
    assert!(
        !stderr.contains("Project") && !stderr.contains("Running"),
        "expected no status lines with --quiet, stderr={stderr:?}"
    );
}

#[test]
fn run_does_not_print_last_expression() {
    let root = tempfile_project("demo_no_last");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "demo_no_last"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "41 + 1\n").unwrap();

    let (code, stdout, stderr) = run_optive(&["--no-color", "run"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        !stdout.contains("42"),
        "Optive run must not print last expression, stdout={stdout:?}"
    );
}

#[test]
fn inline_code_prints_last_expression() {
    let root = tempfile_project("inline_last");
    let (code, stdout, stderr) = run_optive(&["-c", "41 + 1"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("42"),
        "Optive -c should print last value, got: {stdout}"
    );
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

    let (code, stdout, stderr) = run_optive_env(&["run"], &root, LOCAL_DEPS_ENV);
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

    let (code, stdout, stderr) = run_optive_env(&["run", "--sandbox"], &root, LOCAL_DEPS_ENV);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("hello from src"), "stdout={stdout}");

    fs::write(
        root.join("src/main.tive"),
        "std.fs.write_text(\"deps/greeter/hack.txt\", \"no\")\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run_optive_env(&["run", "--sandbox"], &root, LOCAL_DEPS_ENV);
    assert_ne!(code, 0, "stdout={stdout}");
    assert!(
        stderr.contains("installed package source is immutable"),
        "{stderr}"
    );
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

    let (code, stdout, stderr) = run_optive_env(&["run"], &root, LOCAL_DEPS_ENV);
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
    assert!(
        stdout.contains("--quiet"),
        "help should mention --quiet:\n{stdout}"
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
    fs::write(
        root.join("src/main.tive"),
        "use \"helper.tive\".{ answer }\nprint(answer)\n",
    )
    .unwrap();
    fs::write(root.join("src/helper.tive"), "export const answer = 1\n").unwrap();

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
fn check_validates_question_operator_statically() {
    let root = tempfile_project("check_question");
    fs::write(
        root.join("Optive.toml"),
        r#"
[package]
name = "check_question"
entry = "src/main.tive"
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.tive"),
        "func bad() -> num { return 1? }\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["check"], &root);
    assert_eq!(code, 1, "stdout={stdout}\nstderr={stderr}");
    assert!(
        format!("{stdout}{stderr}").contains("returning Result or Option"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn check_json_reports_codes_and_warning_policy() {
    let root = tempfile_project("check_json");
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"check_json\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.tive"),
        "use std.math.{ sin }\nprint(1)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["check", "--json"], &root);
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON diagnostics");
    assert_eq!(json[0]["severity"], "warning");
    assert_eq!(json[0]["code"], "W1001");

    let (code, stdout, stderr) = run_optive(&["check", "--deny-warnings", "--json"], &root);
    assert_eq!(code, 1, "stdout={stdout}\nstderr={stderr}");
}

#[test]
fn check_fails_for_missing_modules() {
    let root = tempfile_project("check_missing_module");
    fs::write(
        root.join("Optive.toml"),
        "[package]\nname = \"check_missing_module\"\nentry = \"src/main.tive\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.tive"),
        "import \"missing.tive\" as missing\nprint(missing)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["check"], &root);
    assert_eq!(code, 1, "stdout={stdout}\nstderr={stderr}");
    let combined = format!("{stdout}{stderr}");
    assert!(combined.contains("E2001"), "{combined}");
    assert!(combined.contains("cannot resolve module"), "{combined}");
    assert!(combined.contains("help:"), "{combined}");
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
