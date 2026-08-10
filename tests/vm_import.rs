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
