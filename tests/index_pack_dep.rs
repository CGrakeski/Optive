#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! `pack_name = "0.1.2"` 从 index.json 查 git URL，再按版本 tag 安装。

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
        "optive_index_pack_{name}_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn index_version_string_clones_tagged_pack() {
    let root = scratch("vstr");
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
        r#"export func hi(name) {
    return f"你好，{name}！"
}
export let version = "0.1.2"
"#,
    )
    .unwrap();
    git(&greeter, &["init"]);
    git(&greeter, &["add", "Optive.toml", "src"]);
    git(&greeter, &["commit", "-m", "greeter 0.1.2"]);
    git(&greeter, &["tag", "0.1.2"]);

    let file_url = path_to_file_url(&greeter);
    fs::write(
        index_dir.join("index.json"),
        format!(r#"{{"greeter":"{file_url}"}}"#),
    )
    .unwrap();

    let app = root.join("hello_app");
    let (code, stdout, stderr) = run_optive(&["new", "hello_app"], &root, &home, &index_dir);
    assert_eq!(code, 0, "new failed: stderr={stderr}\nstdout={stdout}");

    let toml_path = app.join("Optive.toml");
    let mut toml = fs::read_to_string(&toml_path).unwrap();
    toml.push_str("greeter = \"0.1.2\"\n");
    fs::write(&toml_path, toml).unwrap();
    fs::write(
        app.join("src/main.tive"),
        "import greeter\nprint(greeter.hi(\"小明\"))\nprint(greeter.version)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["run"], &app, &home, &index_dir);
    assert_eq!(code, 0, "run failed: stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("你好，小明") || stdout.contains("小明"),
        "expected greeting, stdout={stdout}"
    );
    assert!(stdout.contains("0.1.2"), "stdout={stdout}");

    let lock = fs::read_to_string(app.join("Optive.lock")).unwrap();
    assert!(
        lock.contains("tag") && lock.contains("0.1.2"),
        "lock should record version tag:\n{lock}"
    );
}

#[test]
fn index_caret_range_picks_highest_matching_tag() {
    let root = scratch("caret");
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
        "export let version = \"0.1.0\"\n",
    )
    .unwrap();
    git(&greeter, &["init"]);
    git(&greeter, &["add", "Optive.toml", "src"]);
    git(&greeter, &["commit", "-m", "0.1.0"]);
    git(&greeter, &["tag", "0.1.0"]);

    fs::write(
        greeter.join("src/main.tive"),
        "export let version = \"0.1.2\"\n",
    )
    .unwrap();
    git(&greeter, &["add", "src"]);
    git(&greeter, &["commit", "-m", "0.1.2"]);
    git(&greeter, &["tag", "0.1.2"]);

    fs::write(
        greeter.join("src/main.tive"),
        "export let version = \"0.2.0\"\n",
    )
    .unwrap();
    git(&greeter, &["add", "src"]);
    git(&greeter, &["commit", "-m", "0.2.0"]);
    git(&greeter, &["tag", "0.2.0"]);

    let file_url = path_to_file_url(&greeter);
    fs::write(
        index_dir.join("index.json"),
        format!(r#"{{"greeter":"{file_url}"}}"#),
    )
    .unwrap();

    let app = root.join("hello_app");
    let (code, stdout, stderr) = run_optive(&["new", "hello_app"], &root, &home, &index_dir);
    assert_eq!(code, 0, "new failed: stderr={stderr}\nstdout={stdout}");

    let toml_path = app.join("Optive.toml");
    let mut toml = fs::read_to_string(&toml_path).unwrap();
    toml.push_str("greeter = \"^0.1.0\"\n");
    fs::write(&toml_path, toml).unwrap();
    fs::write(
        app.join("src/main.tive"),
        "import greeter\nprint(greeter.version)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["run"], &app, &home, &index_dir);
    assert_eq!(code, 0, "run failed: stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("0.1.2"),
        "caret should pick 0.1.2 not 0.2.0, stdout={stdout}"
    );
    assert!(
        !stdout.contains("0.2.0"),
        "must not select 0.2.0 for ^0.1.0, stdout={stdout}"
    );
}
