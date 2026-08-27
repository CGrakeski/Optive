#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! 运行时能力隔离（沙箱）集成测试。

mod common;

use std::path::PathBuf;

use common::{assert_caps_err, run_with_caps};
use optive::caps::{Capabilities, DepGrant, FsAccess, FsPolicy};
use optive::value::Value;

fn cwd_root() -> Vec<PathBuf> {
    vec![std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))]
}

#[test]
fn full_caps_allow_fs_roundtrip() {
    let caps = Capabilities::full();
    let v = run_with_caps(
        r#"
use std.fs.{ write_text, read_text, remove }
let p = "__ol_test_sandbox_full.txt"
write_text(p, "ok")
let t = read_text(p)
remove(p)
t
"#,
        caps,
    )
    .expect("full caps should allow fs");
    match v {
        Value::Text(s) => assert_eq!(s.as_str(), "ok"),
        other => panic!("expected text, got {}", other.display_string()),
    }
}

#[test]
fn no_network_blocks_http() {
    let caps = Capabilities {
        network: false,
        fs: FsPolicy::Unrestricted,
        env: true,
        ffi: true,
        process: true,
        dep_grant: DepGrant::none(),
    };
    // caps 检查在真正发请求之前触发，故无需真实网络。
    assert_caps_err(
        r#"
import std.http as http
http.get("https://example.com")
"#,
        caps,
        "network access disabled",
    );
}

#[test]
fn sandbox_blocks_fs_escape() {
    let caps = Capabilities::sandbox(cwd_root());
    // `..` 逃逸到沙箱根之外，应在触盘前被拦下。
    assert_caps_err(
        r#"
use std.fs.{ read_text }
read_text("../__ol_test_sandbox_escape.txt")
"#,
        caps,
        "outside sandbox",
    );
}

#[test]
fn sandbox_allows_under_root() {
    let caps = Capabilities::sandbox(cwd_root());
    let v = run_with_caps(
        r#"
use std.fs.{ write_text, read_text, remove }
let p = "__ol_test_sandbox_inside.txt"
write_text(p, "inside-ok")
let t = read_text(p)
remove(p)
t
"#,
        caps,
    )
    .expect("sandbox should allow under-root file");
    match v {
        Value::Text(s) => assert_eq!(s.as_str(), "inside-ok"),
        other => panic!("expected text, got {}", other.display_string()),
    }
}

#[test]
fn sandbox_blocks_env_mutation() {
    let caps = Capabilities::sandbox(cwd_root());
    assert_caps_err(
        r#"
use std.os.{ setenv }
setenv("OPTIVE_SANDBOX_TEST", "1")
"#,
        caps,
        "environment mutation disabled",
    );
}

#[test]
fn sandbox_blocks_chdir() {
    let caps = Capabilities::sandbox(cwd_root());
    assert_caps_err(
        r#"
use std.os.{ chdir }
chdir("..")
"#,
        caps,
        "environment mutation disabled",
    );
}

#[test]
fn empty_roots_blocks_all_fs() {
    let caps = Capabilities::sandbox(vec![]);
    assert_caps_err(
        r#"
use std.fs.{ exists }
exists("anything.txt")
"#,
        caps,
        "filesystem access disabled",
    );
}

#[test]
fn allow_path_alone_keeps_network() {
    // --allow-path 不应顺带禁网。
    let caps = Capabilities {
        network: true,
        fs: FsPolicy::Allow(cwd_root()),
        env: true,
        ffi: true,
        process: true,
        dep_grant: DepGrant::none(),
    };
    // 文件在根下应可访问。
    let v = run_with_caps(
        r#"
use std.fs.{ write_text, read_text, remove }
let p = "__ol_test_sandbox_allowpath.txt"
write_text(p, "x")
let t = read_text(p)
remove(p)
t
"#,
        caps,
    )
    .expect("allow-path should permit under-root fs");
    match v {
        Value::Text(s) => assert_eq!(s.as_str(), "x"),
        other => panic!("expected text, got {}", other.display_string()),
    }
}

#[test]
fn allow_path_blocks_escape() {
    let caps = Capabilities {
        network: true,
        fs: FsPolicy::Allow(cwd_root()),
        env: true,
        ffi: true,
        process: true,
        dep_grant: DepGrant::none(),
    };
    assert_caps_err(
        r#"
use std.fs.{ write_text }
write_text("../__ol_test_sandbox_escape2.txt", "x")
"#,
        caps,
        "outside sandbox",
    );
}

#[test]
fn sandbox_blocks_remove_dir_escape() {
    let caps = Capabilities::sandbox(cwd_root());
    assert_caps_err(
        r#"
use std.fs.{ remove_dir }
remove_dir("../__ol_test_sandbox_escape_dir")
"#,
        caps,
        "outside sandbox",
    );
}

#[test]
fn sandbox_blocks_rename_escape() {
    let caps = Capabilities::sandbox(cwd_root());
    assert_caps_err(
        r#"
use std.fs.{ rename }
rename("../__ol_a.txt", "../__ol_b.txt")
"#,
        caps,
        "outside sandbox",
    );
}

#[test]
fn full_caps_value_is_none_when_script_returns_none() {
    // 回归：full caps 下普通脚本仍正常返回。
    let v = run_with_caps("none", Capabilities::full()).expect("run");
    assert!(matches!(v, Value::None));
}

fn fresh_sandbox_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "optive_sandbox_{label}_{}_{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn scoped_dependency_root_is_read_only() {
    let project = fresh_sandbox_dir("project_rw");
    let deps_parent = project.join("deps");
    let dep = deps_parent.join("dep_ro");
    std::fs::create_dir_all(&dep).unwrap();
    let caps = Capabilities {
        network: false,
        fs: FsPolicy::Scoped {
            read_write: vec![project.clone()],
            read_only: vec![dep.clone()],
        },
        env: false,
        ffi: false,
        process: false,
        dep_grant: DepGrant::none(),
    };
    assert!(caps
        .resolve_fs_path("read", dep.join("module.tive"), FsAccess::Read)
        .is_ok());
    assert!(caps
        .resolve_fs_path("write", dep.join("created.txt"), FsAccess::Write)
        .is_err());
    assert!(caps
        .resolve_fs_path("write", project.join("created.txt"), FsAccess::Write)
        .is_ok());
    assert!(caps
        .resolve_fs_path("rename", &deps_parent, FsAccess::Write)
        .is_err());
    let _ = std::fs::remove_dir_all(project);
}

#[test]
fn json_sqlite_and_abspath_use_path_gate() {
    let root = fresh_sandbox_dir("stdlib_gate");
    let caps = Capabilities::sandbox(vec![root.clone()]);
    for source in [
        r#"std.json.parse_file("../outside.json")"#,
        r#"std.json.dump("../outside.json", {"x": 1})"#,
        r#"std.path.abspath("../outside.txt")"#,
    ] {
        assert_caps_err(source, caps.clone(), "parent traversal");
    }
    assert_caps_err(
        r#"std.sqlite.open("inside.db")"#,
        caps,
        "file databases are disabled",
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn filesystem_sandbox_rejects_path_based_ffi_even_when_ffi_enabled() {
    let root = fresh_sandbox_dir("ffi_handle");
    let mut caps = Capabilities::sandbox(vec![root.clone()]);
    caps.ffi = true;
    assert_caps_err(
        "use std.language.{ C }\nC.frompath(\"missing.dll\")",
        caps,
        "platform loader",
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn sandbox_tmp_dir_is_inside_writable_root() {
    let root = fresh_sandbox_dir("tmp");
    let v = run_with_caps(
        "std.test.tmp_dir()",
        Capabilities::sandbox(vec![root.clone()]),
    )
    .expect("tmp dir");
    let Value::Text(path) = v else {
        panic!("expected text path");
    };
    let created = PathBuf::from(path);
    let created_canon = created.canonicalize().unwrap();
    let root_canon = root.canonicalize().unwrap();
    assert!(
        created_canon.starts_with(&root_canon),
        "{}",
        created.display()
    );
    assert!(created.is_dir());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn sandbox_string_import_resolves_under_import_base_not_cwd() {
    // 进程 cwd 在沙箱外时，`import "foo.tive"` 必须相对 import_base 解析。
    // 若先用裸相对路径做沙箱检查，会按 cwd 判定成 outside roots。
    let root = fresh_sandbox_dir("import_base_not_cwd");
    std::fs::write(root.join("hello.tive"), "export let value = 42\n").unwrap();
    let caps = Capabilities::sandbox(vec![root.clone()]);
    let mut vm = optive::vm::Vm::new();
    vm.install_caps(caps);
    vm.import_base = root.clone();
    let v = optive::run_source_in_vm(
        &mut vm,
        "import \"hello.tive\" as hello\nhello.value",
        "<test>",
    )
    .expect("module under import_base must be found even when cwd is outside the sandbox");
    match v {
        Value::Num(n) => assert_eq!(n.to_string(), "42"),
        other => panic!("expected 42, got {}", other.display_string()),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn sandbox_rejects_symlink_file_and_module_import() {
    use std::os::unix::fs::symlink;

    let root = fresh_sandbox_dir("symlink_root");
    let outside = fresh_sandbox_dir("symlink_outside");
    std::fs::write(outside.join("secret.txt"), "secret").unwrap();
    std::fs::write(outside.join("evil.tive"), "export let value = 7\n").unwrap();
    symlink(outside.join("secret.txt"), root.join("secret.txt")).unwrap();
    symlink(outside.join("evil.tive"), root.join("evil.tive")).unwrap();

    let caps = Capabilities::sandbox(vec![root.clone()]);
    let read = format!(
        "std.fs.read_text(\"{}\")",
        root.join("secret.txt").display()
    );
    assert_caps_err(&read, caps.clone(), "symbolic link");

    let mut vm = optive::vm::Vm::new();
    vm.install_caps(caps);
    vm.import_base = root.clone();
    let err = optive::run_source_in_vm(&mut vm, "import \"evil.tive\" as evil", "<test>")
        .expect_err("symlink import must fail");
    assert!(err.message().contains("symbolic link"), "{}", err.message());

    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(windows)]
#[test]
fn sandbox_rejects_windows_symlink_when_supported() {
    use std::os::windows::fs::symlink_file;

    let root = fresh_sandbox_dir("win_link_root");
    let outside = fresh_sandbox_dir("win_link_outside");
    let target = outside.join("secret.txt");
    std::fs::write(&target, "secret").unwrap();
    let link = root.join("secret.txt");
    if symlink_file(&target, &link).is_ok() {
        let caps = Capabilities::sandbox(vec![root.clone()]);
        assert!(caps
            .resolve_fs_path("read", &link, FsAccess::Read)
            .expect_err("link must fail")
            .message()
            .contains("reparse point"));
    }
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn handle_read_resists_concurrent_symlink_rename_swap() {
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let root = fresh_sandbox_dir("race_root");
    let outside = fresh_sandbox_dir("race_outside");
    let slot = root.join("slot.txt");
    let safe_hold = root.join("safe.hold");
    let evil_hold = root.join("evil.hold");
    let secret = outside.join("secret.txt");
    std::fs::write(&slot, "safe").unwrap();
    std::fs::write(&secret, "outside-secret").unwrap();
    symlink(&secret, &evil_hold).unwrap();

    let running = Arc::new(AtomicBool::new(true));
    let worker_running = running.clone();
    let worker = std::thread::spawn(move || {
        while worker_running.load(Ordering::Relaxed) {
            let _ = std::fs::rename(&slot, &safe_hold);
            let _ = std::fs::rename(&evil_hold, &slot);
            std::thread::yield_now();
            let _ = std::fs::rename(&slot, &evil_hold);
            let _ = std::fs::rename(&safe_hold, &slot);
        }
    });

    let caps = Capabilities::sandbox(vec![root.clone()]);
    let target = root.join("slot.txt");
    for _ in 0..2_000 {
        if let Ok(text) = caps.read_to_string("race read", &target) {
            assert_eq!(text, "safe", "capability read escaped root");
        }
    }
    running.store(false, Ordering::Relaxed);
    worker.join().unwrap();
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(windows)]
#[test]
fn handle_read_resists_windows_symlink_rename_swap_when_supported() {
    use std::os::windows::fs::symlink_file;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let root = fresh_sandbox_dir("win_race_root");
    let outside = fresh_sandbox_dir("win_race_outside");
    let slot = root.join("slot.txt");
    let safe_hold = root.join("safe.hold");
    let evil_hold = root.join("evil.hold");
    let secret = outside.join("secret.txt");
    std::fs::write(&slot, "safe").unwrap();
    std::fs::write(&secret, "outside-secret").unwrap();
    if symlink_file(&secret, &evil_hold).is_err() {
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
        return;
    }

    let running = Arc::new(AtomicBool::new(true));
    let worker_running = running.clone();
    let worker = std::thread::spawn(move || {
        while worker_running.load(Ordering::Relaxed) {
            let _ = std::fs::rename(&slot, &safe_hold);
            let _ = std::fs::rename(&evil_hold, &slot);
            std::thread::yield_now();
            let _ = std::fs::rename(&slot, &evil_hold);
            let _ = std::fs::rename(&safe_hold, &slot);
        }
    });

    let caps = Capabilities::sandbox(vec![root.clone()]);
    let target = root.join("slot.txt");
    for _ in 0..2_000 {
        if let Ok(text) = caps.read_to_string("race read", &target) {
            assert_eq!(text, "safe", "capability read escaped root");
        }
    }
    running.store(false, Ordering::Relaxed);
    worker.join().unwrap();
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn dependency_default_denies_network_env_ffi_and_root_is_read_only() {
    // 宿主 full()：依赖默认禁网 / 禁 env / 禁 FFI；包根只读可读、不可写。
    let dep_root = fresh_sandbox_dir("dep_default");
    let host = Capabilities::full();
    let dep = host.restrict_for_dependency(&dep_root);
    assert!(dep.check_network("get").is_err());
    assert!(dep.check_env("setenv").is_err());
    assert!(dep.check_process("os.run").is_err());
    assert!(dep.check_ffi("frompath").is_err());
    assert!(dep
        .resolve_fs_path("read", dep_root.join("mod.tive"), FsAccess::Read)
        .is_ok());
    assert!(dep
        .resolve_fs_path("write", dep_root.join("out.txt"), FsAccess::Write)
        .is_err());
    let _ = std::fs::remove_dir_all(dep_root);
}

#[test]
fn dependency_trust_all_inherits_host_network_and_ffi() {
    // dep_grant.trust_all = true：依赖继承宿主网络 / FFI（及宿主其余能力）。
    let dep_root = fresh_sandbox_dir("dep_trust_all");
    let mut host = Capabilities::full();
    host.dep_grant.trust_all = true;
    let dep = host.restrict_for_dependency(&dep_root);
    assert!(dep.check_network("get").is_ok());
    assert!(dep.check_ffi("frompath").is_ok());
    assert!(dep.check_env("setenv").is_ok());
    // 宿主 fs 为 Unrestricted，信任后依赖同样不受限。
    assert!(!dep.fs_restricted());
    let _ = std::fs::remove_dir_all(dep_root);
}

#[test]
fn dependency_network_grant_alone_keeps_ffi_and_env_blocked() {
    // 只开 dep_grant.network：有网、无 FFI、无 env；包根仍只读。
    let dep_root = fresh_sandbox_dir("dep_net_only");
    let mut host = Capabilities::full();
    host.dep_grant.network = true;
    let dep = host.restrict_for_dependency(&dep_root);
    assert!(dep.check_network("get").is_ok());
    assert!(dep.check_ffi("frompath").is_err());
    assert!(dep.check_env("setenv").is_err());
    assert!(dep
        .resolve_fs_path("read", dep_root.join("mod.tive"), FsAccess::Read)
        .is_ok());
    assert!(dep
        .resolve_fs_path("write", dep_root.join("out.txt"), FsAccess::Write)
        .is_err());
    let _ = std::fs::remove_dir_all(dep_root);
}

#[test]
fn transitive_dependency_keeps_denied_grants_and_sees_only_own_root() {
    // 传递依赖：A 再 restrict 到 B 后，B 仍无网；可读 B 根、不可读 A 根。
    let dep_a = fresh_sandbox_dir("dep_transitive_a");
    let dep_b = fresh_sandbox_dir("dep_transitive_b");
    let host = Capabilities::full();
    let caps_a = host.restrict_for_dependency(&dep_a);
    assert!(caps_a.check_network("get").is_err());
    let caps_b = caps_a.restrict_for_dependency(&dep_b);
    assert!(caps_b.check_network("get").is_err());
    assert!(caps_b.check_ffi("frompath").is_err());
    assert!(caps_b.check_env("setenv").is_err());
    // 可读 B 根。
    assert!(caps_b
        .resolve_fs_path("read", dep_b.join("mod.tive"), FsAccess::Read)
        .is_ok());
    // 不可读 A 根：B 的只读根只含 B 自己。
    assert!(caps_b
        .resolve_fs_path("read", dep_a.join("mod.tive"), FsAccess::Read)
        .is_err());
    // B 根仍只读。
    assert!(caps_b
        .resolve_fs_path("write", dep_b.join("out.txt"), FsAccess::Write)
        .is_err());
    let _ = std::fs::remove_dir_all(dep_a);
    let _ = std::fs::remove_dir_all(dep_b);
}
