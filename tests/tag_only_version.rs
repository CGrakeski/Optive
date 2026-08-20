//! Tag-only 模型：通过 Optive 二进制测 new / publish。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]

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
    let bin = optive_bin();
    assert!(
        bin.is_file(),
        "Optive binary missing at {}; run `cargo build --bin Optive` first",
        bin.display()
    );
    let home = cwd.join(".optive_home");
    let index = cwd.join(".optive_index");
    let _ = fs::create_dir_all(&home);
    let _ = fs::create_dir_all(&index);
    let out = Command::new(&bin)
        .args(args)
        .current_dir(cwd)
        .env("OPTIVE_HOME", &home)
        .env("OPTIVE_INDEX", &index)
        .output()
        .expect("spawn Optive");
    (
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tmp_dir(label: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "optive_{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn git(cwd: &std::path::Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git");
    assert!(st.success(), "git {args:?} failed");
}

fn init_git_repo(root: &std::path::Path) {
    git(root, &["init"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "tag.gpgsign", "false"]);
    git(root, &["config", "tag.forceSignAnnotated", "false"]);
    let _ = Command::new("git")
        .args(["checkout", "-b", "main"])
        .current_dir(root)
        .status();
}

#[test]
fn new_project_omits_package_version() {
    let parent = tmp_dir("new_no_ver");
    let (code, stdout, stderr) = run_optive(&["new", "DemoPkg"], &parent);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    let toml = fs::read_to_string(parent.join("DemoPkg/Optive.toml")).unwrap();
    assert!(
        !toml
            .lines()
            .any(|l| l.trim_start().starts_with("version")),
        "{toml}"
    );
    assert!(toml.contains("git tags"), "{toml}");
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn publish_creates_v_tag_rejects_dirty_and_duplicate() {
    let root = tmp_dir("publish_ok");
    init_git_repo(&root);
    fs::write(
        root.join("Optive.toml"),
        r#"[package]
name = "pubdemo"
entry = "src/main.tive"

[dependencies]
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.tive"), "print(1)\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "init"]);

    let (code, stdout, stderr) = run_optive(&["publish", "0.1.0"], &root);
    assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("v0.1.0"), "{stdout}");

    let tags = Command::new("git")
        .args(["tag", "-l"])
        .current_dir(&root)
        .output()
        .unwrap();
    let tags = String::from_utf8_lossy(&tags.stdout);
    assert!(tags.lines().any(|t| t.trim() == "v0.1.0"), "{tags}");

    fs::write(root.join("src/main.tive"), "print(2)\n").unwrap();
    let (code, _, stderr) = run_optive(&["publish", "0.2.0"], &root);
    assert_ne!(code, 0);
    assert!(stderr.contains("dirty") || stderr.contains("worktree"), "{stderr}");

    git(&root, &["checkout", "--", "."]);
    let (code, _, stderr) = run_optive(&["publish", "0.1.0"], &root);
    assert_ne!(code, 0);
    assert!(stderr.contains("already exists"), "{stderr}");

    let _ = fs::remove_dir_all(&root);
}
