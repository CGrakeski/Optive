#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_num, assert_text, run_err};

#[test]
fn import_std_math_abs() {
    assert_num(
        r"
import std.math as math
math.abs(-5)
",
        "5",
    );
}

#[test]
fn use_std_math_range() {
    assert_num(
        r"
use std.math.{ range }
let n = 0
for (x in range(1, 4)) { n = n + x }
n
",
        "6",
    );
}

#[test]
fn use_std_math_abs_as_alias() {
    assert_num(
        r"
use std.math.{ abs as magnitude }
magnitude(-7)
",
        "7",
    );
}

#[test]
fn std_concat() {
    assert_text(
        r#"
import std as s
s.concat("a", "b", "c")
"#,
        "abc",
    );
}

#[test]
fn std_format_join() {
    assert_text(
        r#"
use std.format.{ join }
join("-", [1, 2, 3])
"#,
        "1-2-3",
    );
}

#[test]
fn std_dict_get() {
    assert_num(
        r#"
use std.dict.{ get }
let d = { "x": 9 }
get(d, "x")
"#,
        "9",
    );
}

#[test]
fn import_user_module() {
    assert_num(
        r#"
import "tests/import_fixtures/sample.tive" as sample
sample.double(21)
"#,
        "42",
    );
}

#[test]
fn use_user_module_export() {
    assert_num(
        r#"
use "tests/import_fixtures/sample.tive".{ answer }
answer
"#,
        "42",
    );
}

#[test]
fn intern_not_exported() {
    run_err(
        r#"
use "tests/import_fixtures/sample.tive".{ hidden }
hidden
"#,
    );
}

#[test]
fn import_string_path() {
    assert_num(
        r#"
import "tests/import_fixtures/helper.tive" as helper
helper.add(3, 4)
"#,
        "7",
    );
}

#[test]
fn module_cache_reimport() {
    assert_num(
        r"
import std.math as m1
import std.math as m2
m1.abs(-3) + m2.abs(-2)
",
        "5",
    );
}

#[test]
fn missing_module_errors() {
    run_err(
        r"
import no.such.module
",
    );
}

#[test]
fn import_module_function_uses_module_globals() {
    assert_num(
        r#"
import "tests/import_fixtures/widget_lib.tive" as lib
let w = lib.make_widget()
lib.bump_twice(w)
w.count
"#,
        "2",
    );
}

#[test]
fn import_library_module_load_sample_books() {
    assert_num(
        r#"
import "tests/import_fixtures/library.tive" as lib
let library = lib.create_library()
lib.load_sample_books(library)
len(library.books)
"#,
        "6",
    );
}

/// Regression: export func with struct ctor + `len(param)` must not export as `none`.
#[test]
fn export_func_struct_ctor_and_len_param() {
    assert_num(
        r#"
import "tests/import_fixtures/struct_len_export.tive" as m
m.make_tokens("abcde")
"#,
        "5",
    );
}

/// `friend` handler that uses module globals / builtins after import.
#[test]
fn export_friend_with_struct_and_len() {
    assert_num(
        r#"
import "tests/import_fixtures/struct_len_export.tive" as m
let t = m.Token("ab", "cd")
m.describe(t)
"#,
        "4",
    );
}

/// 导入后模块内非 export 函数仍须能读本模块级 `let`（`LoadGlobal` 走 `module_env`）。
#[test]
fn imported_module_top_export_is_ab() {
    assert_text(
        r#"
import "tests/import_fixtures/module_internal_global.tive" as m
m.TOP
"#,
        "ab",
    );
}

#[test]
fn imported_module_internal_func_sees_module_let() {
    assert_num(
        r#"
import "tests/import_fixtures/module_internal_global.tive" as m
let ok = if m.TOP == "ab" then 1 else 0
let r = if m.test_let() then 1 else 0
ok + r
"#,
        "2",
    );
}

#[test]
fn module_internal_global_as_script_defines_prefix() {
    let src = concat!(
        include_str!("import_fixtures/module_internal_global.tive"),
        "\nlet r = if test_let() then 1 else 0\nr\n",
    );
    let mut vm = optive::vm::Vm::new();
    let v = optive::run_source_in_vm(&mut vm, src, "<prefix_script>").expect("run");
    let names: Vec<String> = vm
        .debug_list_globals()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(
        names.iter().any(|n| n == "PREFIX"),
        "PREFIX missing from globals: {names:?}"
    );
    assert_eq!(v.display_string(), "1");
}

/// `use` 引入的函数必须保留定义模块的 globals，不能被调用方 `module_env` 换绑。
#[test]
fn use_imported_func_keeps_defining_module_globals() {
    assert_text(
        r#"
import "tests/import_fixtures/use_caller_with_struct.tive" as cu
cu.via_use("x")
"#,
        "Identifier",
    );
    assert_text(
        r#"
import "tests/import_fixtures/use_caller_with_struct.tive" as cu
cu.via_use("let")
"#,
        "KwLet",
    );
}

/// 导入后模块函数对模块全局的赋值必须留在 `module_env，不能污染调用方`。
#[test]
fn imported_module_mutates_own_global() {
    assert_num(
        r#"
import "tests/import_fixtures/mutable_counter.tive" as c
c.bump()
c.bump()
"#,
        "2",
    );
}

/// B10：被导入模块里 `use ….{ C }` 的绑定在跨模块调用时仍可见。
#[test]
fn imported_use_c_binding_visible() {
    assert_num(
        r#"
import "tests/import_fixtures/use_c_export.tive" as m
m.probe_c()
"#,
        "1",
    );
}

#[test]
fn imported_traceback_and_debug_stack_use_real_source_path() {
    use optive::debug::{self, DebugState, StopReason};
    use optive::shared::Shared;
    use optive::vm::Vm;

    let module_path = std::path::Path::new("tests/import_fixtures/module_metadata.tive");
    let source = r#"
import "tests/import_fixtures/module_metadata.tive" as m
m.explode()
"#;
    let mut vm = Vm::new();
    let err = optive::run_source_in_vm(&mut vm, source, "<test>")
        .expect_err("imported function should throw");
    let message = err.to_string();
    assert!(
        message.contains("module_metadata.tive")
            && message.contains("throw ValueError(\"module metadata boom\")"),
        "traceback did not use imported source metadata: {message}"
    );

    let debug_source = r#"
import "tests/import_fixtures/module_metadata.tive" as m
m.never_called()
"#;
    let mut vm = Vm::new();
    vm.source_file = "<test>".into();
    vm.current_source = Some(std::sync::Arc::from(debug_source));
    let state = Shared::new(DebugState {
        stop_on_entry: false,
        ..Default::default()
    });
    state
        .borrow_mut()
        .add_line_breakpoint(&module_path.to_string_lossy(), 6);
    debug::attach(&mut vm, state.clone());
    let compiled =
        optive::compile_with_context(&vm, debug_source, "<test>").expect("compile entry");
    vm.load_program(compiled).expect("load entry");
    assert!(vm
        .run_until_debug_break()
        .expect("run to imported breakpoint")
        .is_none());
    assert_eq!(state.borrow().stop_reason, Some(StopReason::Breakpoint));
    let (actual_file, _) = debug::current_location(&vm);
    assert_eq!(
        debug::normalize_path(&actual_file),
        debug::normalize_path(&module_path.to_string_lossy())
    );
    let stack = debug::stack_frames(&vm);
    assert!(
        stack.iter().any(|frame| debug::normalize_path(&frame.file)
            == debug::normalize_path(&module_path.to_string_lossy())),
        "stack did not contain imported module path: {stack:?}"
    );
}

#[test]
fn nested_import_failure_restores_entry_context() {
    use optive::vm::Vm;

    let source = r#"import "import_fixtures/context_outer.tive" as outer"#;
    let entry = "tests/context_entry.tive";
    let root = std::env::current_dir().expect("cwd");
    let mut vm = Vm::new();
    vm.current_package_id = "context-root".into();
    vm.package_root = Some(root.clone());

    optive::run_source_in_vm(&mut vm, source, entry).expect_err("nested import should fail");

    assert_eq!(vm.source_file, entry);
    assert_eq!(vm.current_source.as_deref(), Some(source));
    assert_eq!(vm.import_base, std::path::PathBuf::from("tests"));
    assert_eq!(vm.current_package_id, "context-root");
    assert_eq!(vm.package_root.as_deref(), Some(root.as_path()));
}

#[test]
fn string_import_undeclared_dep_is_explicit() {
    use optive::run_source_in_vm;
    use optive::vm::{DepPackage, Vm};

    let mut vm = Vm::new();
    vm.dep_map.insert(
        ("__root__".into(), "real-pack".into()),
        DepPackage {
            path: std::path::PathBuf::from("/nonexistent-pack"),
            id: "id".into(),
        },
    );
    let err = run_source_in_vm(&mut vm, r#"import "missing-pack""#, "<test>")
        .expect_err("should reject undeclared pack");
    let msg = err.to_string();
    assert!(
        msg.contains("undeclared dependency") && msg.contains("missing-pack"),
        "got: {msg}"
    );
}
