#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! 验证 `file:///` 本地 Git URL 可作为依赖源（与官方 git 行为一致）。

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

fn run_optive(args: &[&str], cwd: &Path, optive_home: &Path) -> (i32, String, String) {
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

/// 把绝对路径编成 git 认可的 `file:///` URL。
fn path_to_file_url(path: &Path) -> String {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut s = canon.to_string_lossy().replace('\\', "/");
    // Windows: \\?\C:\... → C:/...
    if let Some(stripped) = s.strip_prefix("//?/") {
        s = stripped.to_string();
    }
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        // C:/Users/... → file:///C:/Users/...
        format!("file:///{s}")
    }
}

fn scratch(name: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("optive_file_git_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn file_url_add_and_run_imports_local_git_dep() {
    let root = scratch("playground");
    let home = root.join("optive_home");
    fs::create_dir_all(&home).unwrap();

    // --- 本地「远程」库 greeter ---
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
export let version = "0.1.0"
"#,
    )
    .unwrap();

    git(&greeter, &["init"]);
    git(&greeter, &["add", "Optive.toml", "src"]);
    git(&greeter, &["commit", "-m", "greeter init"]);

    let file_url = path_to_file_url(&greeter);
    assert!(
        file_url.starts_with("file://"),
        "expected file URL, got {file_url}"
    );

    // --- 应用 hello_app ---
    let app = root.join("hello_app");
    let (code, stdout, stderr) = run_optive(&["new", "hello_app"], &root, &home);
    assert_eq!(code, 0, "new failed: stderr={stderr}\nstdout={stdout}");
    assert!(app.join("Optive.toml").is_file());

    let (code, stdout, stderr) = run_optive(&["add", &file_url], &app, &home);
    assert_eq!(
        code, 0,
        "add file:/// failed (git/gix should accept local repos):\nurl={file_url}\nstderr={stderr}\nstdout={stdout}"
    );

    let toml = fs::read_to_string(app.join("Optive.toml")).unwrap();
    assert!(
        toml.contains("greeter") && toml.contains("rev"),
        "toml should pin greeter rev:\n{toml}"
    );

    fs::write(
        app.join("src/main.tive"),
        "import greeter\nprint(greeter.hi(\"小明\"))\nprint(greeter.version)\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_optive(&["run"], &app, &home);
    assert_eq!(code, 0, "run failed: stderr={stderr}\nstdout={stdout}");
    assert!(
        stdout.contains("你好，小明") || stdout.contains("小明"),
        "expected greeting, stdout={stdout}"
    );
    assert!(stdout.contains("0.1.0"), "stdout={stdout}");
}

#[test]
fn file_url_clone_into_works() {
    // 更底层：只测 gix clone file:/// → 目录
    let root = scratch("clone_only");
    let repo = root.join("lib");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README"), "x\n").unwrap();
    git(&repo, &["init"]);
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-m", "init"]);

    let url = path_to_file_url(&repo);
    let dest = root.join("cloned");
    // 通过 Optive add 间接覆盖 clone；这里直接调二进制 add 到临时项目更重。
    // 用 `git clone` 对照官方支持：
    let git_clone = Command::new("git")
        .args(["clone", &url, dest.to_str().unwrap()])
        .output()
        .expect("git clone");
    assert!(
        git_clone.status.success(),
        "official git clone file:/// failed: {}\nurl={url}",
        String::from_utf8_lossy(&git_clone.stderr)
    );
    assert!(dest.join("README").is_file());
}
