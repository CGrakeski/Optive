#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! `Optive add pack@version` / `Optive search` 集成测试。

use std::fs;
use std::path::{Path, PathBuf};
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

fn run_optive(
    args: &[&str],
    cwd: &Path,
    optive_home: &Path,
    index_dir: &Path,
) -> (i32, String, String) {
    let bin = optive_bin();
    assert!(
        bin.is_file(),
        "Optive binary missing at {}; run `cargo build --bin Optive` first",
        bin.display()
    );
    let out = Command::new(&bin)
        .args(args)
        .current_dir(cwd)
        .env("OPTIVE_HOME", optive_home)
        .env("OPTIVE_INDEX", index_dir)
        .output()
        .expect("spawn Optive");
    (
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "optive-test")
        .env("GIT_AUTHOR_EMAIL", "optive-test@example.com")
        .env("GIT_COMMITTER_NAME", "optive-test")
        .env("GIT_COMMITTER_EMAIL", "optive-test@example.com")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn path_to_file_url(path: &Path) -> String {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut s = canon.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = s.strip_prefix("//?/") {
        s = stripped.to_string();
    }
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

fn scratch(name: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "optive_add_search_{name}_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn setup_greeter_index(root: &Path) -> (PathBuf, PathBuf, String) {
    let home = root.join("optive_home");
    let index_dir = root.join("index");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&index_dir).unwrap();

    let greeter = root.join("greeter");
    fs::create_dir_all(greeter.join("src")).unwrap();
    fs::write(
        greeter.join("Optive.toml"),
        r#"[package]
name = "greeter"
entry = "src/main.tive"

[dependencies]
"#,
    )
    .unwrap();
    fs::write(
        greeter.join("src/main.tive"),
        "export let version = \"0.1.2\"\n",
    )
    .unwrap();
    git(&greeter, &["init"]);
    git(&greeter, &["add", "Optive.toml", "src"]);
    git(&greeter, &["commit", "-m", "greeter 0.1.2"]);
    git(&greeter, &["tag", "0.1.2"]);

    let file_url = path_to_file_url(&greeter);
    fs::write(
        index_dir.join("index.json"),
        format!(r#"{{"greeter":"{file_url}","otherpack":"https://example.com/other.git"}}"#),
    )
    .unwrap();
    (home, index_dir, file_url)
}

#[test]
fn search_filters_by_substring() {
    let root = scratch("search");
    let (home, index_dir, _) = setup_greeter_index(&root);

    let (code, stdout, stderr) = run_optive(&["search", "Greet"], &root, &home, &index_dir);
    assert_eq!(code, 0, "search failed: stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("greeter"), "stdout={stdout}");
    assert!(!stdout.contains("otherpack"), "stdout={stdout}");

    let (code, stdout, stderr) = run_optive(&["search"], &root, &home, &index_dir);
    assert_eq!(code, 0, "search all failed: stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("greeter") && stdout.contains("otherpack"), "stdout={stdout}");
}

#[test]
fn add_pack_at_version_writes_index_dep() {
    let root = scratch("add_ver");
    let (home, index_dir, _) = setup_greeter_index(&root);

    let app = root.join("hello_app");
    let (code, stdout, stderr) = run_optive(&["new", "hello_app"], &root, &home, &index_dir);
    assert_eq!(code, 0, "new failed: stderr={stderr}\nstdout={stdout}");

    let (code, stdout, stderr) =
        run_optive(&["add", "greeter@0.1.2"], &app, &home, &index_dir);
    assert_eq!(code, 0, "add failed: stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("added greeter") || stderr.contains("added greeter"),
        "expected status line, stdout={stdout} stderr={stderr}"
    );

    let toml = fs::read_to_string(app.join("Optive.toml")).unwrap();
    assert!(
        toml.contains("greeter") && toml.contains("0.1.2"),
        "toml should have index dep:\n{toml}"
    );
    assert!(
        !toml.contains("git ="),
        "index add should not write git table:\n{toml}"
    );

    let lock = fs::read_to_string(app.join("Optive.lock")).unwrap();
    assert!(
        lock.contains("0.1.2") || lock.contains("tag"),
        "lock should record resolved tag:\n{lock}"
    );
}

#[test]
fn add_pack_without_version_picks_latest_tag() {
    let root = scratch("add_latest");
    let (home, index_dir, _) = setup_greeter_index(&root);
    let greeter = root.join("greeter");
    git(&greeter, &["tag", "0.1.0"]);
    fs::write(
        greeter.join("src/main.tive"),
        "export let version = \"0.2.0\"\n",
    )
    .unwrap();
    git(&greeter, &["add", "src"]);
    git(&greeter, &["commit", "-m", "greeter 0.2.0"]);
    git(&greeter, &["tag", "0.2.0"]);

    let app = root.join("hello_app");
    let (code, stdout, stderr) = run_optive(&["new", "hello_app"], &root, &home, &index_dir);
    assert_eq!(code, 0, "new failed: stderr={stderr}\nstdout={stdout}");

    let (code, stdout, stderr) = run_optive(&["add", "greeter"], &app, &home, &index_dir);
    assert_eq!(code, 0, "add failed: stderr={stderr}\nstdout={stdout}");

    let toml = fs::read_to_string(app.join("Optive.toml")).unwrap();
    assert!(
        toml.contains("greeter") && toml.contains("0.2.0"),
        "expected exact latest 0.2.0:\n{toml}"
    );
    let lock = fs::read_to_string(app.join("Optive.lock")).unwrap();
    assert!(
        lock.contains("0.2.0"),
        "lock should record newest tag:\n{lock}"
    );
}
