#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! Custom Pack：合并、渲染与身份不变量。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use optive::custom::{
    self, build_active_from_ids, load_pack_dir, load_pack_staging, set_active_pack, write_global_use,
    write_project_use, ActivePack, CliMsg, CustomPack, Diag, ErrorKindMsg, ParseMsg,
    PROJECT_CUSTOM_FILE,
};
use optive::error::{ExceptionKind, RuntimeError};
use optive::{diagnostics, run_source};

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn catgirl_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/custom/catgirl")
}

fn with_temp_home<T>(f: impl FnOnce(&PathBuf) -> T) -> T {
    let _guard = HOME_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile_dir();
    std::env::set_var("OPTIVE_HOME", &dir);
    std::env::remove_var("OPTIVE_CUSTOM");
    let out = f(&dir);
    std::env::remove_var("OPTIVE_HOME");
    std::env::remove_var("OPTIVE_CUSTOM");
    let _ = fs::remove_dir_all(&dir);
    out
}

fn tempfile_dir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "optive_custom_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn install_catgirl(home: &Path) {
    let dest = home.join("custom/catgirl");
    fs::create_dir_all(&dest).unwrap();
    fs::copy(
        catgirl_src().join("Custom.toml"),
        dest.join("Custom.toml"),
    )
    .unwrap();
}

#[test]
fn merge_field_level_layout_and_messages() {
    let base = CustomPack::builtin_en_us();
    let mut overlay = CustomPack::builtin_en_us();
    overlay.id = "mini".into();
    overlay.messages.insert(
        "runtime.zero_division".into(),
        optive::custom::MessageSpec {
            text: Some("boom".into()),
            suffix: Some("!".into()),
            style: None,
        },
    );
    overlay.layout.repl.prompt = "?> ".into();
    overlay.layout_set.repl_prompt = true;
    // 不设 continuation → 应保留 base
    let merged = base.merged_with(&overlay);
    assert_eq!(
        merged.render_message("runtime.zero_division", "x"),
        "boom!"
    );
    assert_eq!(merged.layout.repl.prompt, "?> ");
    assert_eq!(merged.layout.repl.continuation, "... ");
}

#[test]
fn catgirl_render_parse_and_zero_div() {
    with_temp_home(|home| {
        install_catgirl(home);
        let active = build_active_from_ids(&["catgirl".into()]).unwrap();
        set_active_pack(active);

        let pack = custom::active_pack();
        assert_eq!(pack.repl_prompt(), "喵>>> ");
        assert!(pack.parse_label_error().contains("错误"));
        assert_eq!(
            pack.render_diag(&Diag::Parse(ParseMsg::ExpectedExpression)),
            "期望表达式喵~"
        );
        assert_eq!(
            pack.render_diag(&Diag::Runtime(ErrorKindMsg::ZeroDivision)),
            "除以零喵~"
        );

        let err = optive::parse_program("1/").unwrap_err();
        let msg = diagnostics::format_parse_error("1/", "<t>", &err);
        assert!(msg.contains("错误："), "{msg}");
        assert!(msg.contains("期望表达式喵~"), "{msg}");

        let err = run_source("1/0").unwrap_err();
        let s = err.to_string();
        assert!(s.contains("ZeroDivisionError"), "{s}");
        assert!(s.contains("零除错误") || s.contains("除以零"), "{s}");
    });
}

#[test]
fn identity_type_name_stable_under_custom() {
    with_temp_home(|home| {
        install_catgirl(home);
        set_active_pack(build_active_from_ids(&["catgirl".into()]).unwrap());
        assert_eq!(
            ExceptionKind::ZeroDivision.type_name(),
            "ZeroDivisionError"
        );
        assert_eq!(
            RuntimeError::zero_div_diag().kind(),
            ExceptionKind::ZeroDivision
        );
        // catch 身份：源码仍用英文类型名
        let v = run_source(
            r#"
try {
  1/0
} catch (e: ZeroDivisionError) {
  "ok"
}
"#,
        )
        .unwrap();
        match v {
            optive::value::Value::Text(s) => assert_eq!(s, "ok"),
            other => panic!("expected Text, got {other:?}"),
        }
    });
}

#[test]
fn write_project_and_global_use() {
    with_temp_home(|home| {
        install_catgirl(home);
        let proj = home.join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("Optive.toml"),
            "[package]\nname = \"t\"\n",
        )
        .unwrap();
        let custom_toml = proj.join(PROJECT_CUSTOM_FILE);
        write_project_use(&custom_toml, &["catgirl".into()]).unwrap();
        let text = fs::read_to_string(&custom_toml).unwrap();
        assert!(text.contains("catgirl"));

        write_global_use(&["catgirl".into()]).unwrap();
        let g = custom::global_config_path();
        assert!(g.is_file(), "expected Config.toml at {}", g.display());
        let text = fs::read_to_string(&g).unwrap();
        assert!(text.contains("catgirl"));
    });
}

#[test]
fn staging_load_allows_tmp_dir_name() {
    with_temp_home(|home| {
        let tmp = home.join("custom/.tmp-add");
        fs::create_dir_all(&tmp).unwrap();
        fs::copy(catgirl_src().join("Custom.toml"), tmp.join("Custom.toml")).unwrap();
        let pack = load_pack_staging(&tmp).expect("staging should ignore dir name");
        assert_eq!(pack.id, "catgirl");
        assert!(load_pack_dir(&tmp).is_err(), "installed path must match id");
    });
}

#[test]
fn load_rejects_bad_placeholder() {
    with_temp_home(|home| {
        let dir = home.join("custom/bad");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Custom.toml"),
            r#"
id = "bad"
format_version = 1
[layout.exception]
line = "{name}: {hack}"
"#,
        )
        .unwrap();
        assert!(load_pack_dir(&dir).is_err());
    });
}

#[test]
fn cli_help_keys_render() {
    let s = custom::render(&Diag::Cli(CliMsg::HelpCustom));
    assert!(s.contains("custom"));
}

#[test]
fn active_chain_display() {
    let a = ActivePack {
        pack: CustomPack::builtin_en_us(),
        chain: vec!["en-US".into(), "catgirl".into()],
    };
    assert_eq!(a.chain_display(), "en-US → catgirl");
}
