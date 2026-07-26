mod common;

use common::{assert_num, assert_text, run_err};

#[test]
fn import_std_math_abs() {
    assert_num(
        r#"
import std.math as math
math.abs(-5)
"#,
        "5",
    );
}

#[test]
fn use_std_math_range() {
    assert_num(
        r#"
use std.math.{ range }
let n = 0
for (x in range(1, 4)) { n = n + x }
n
"#,
        "6",
    );
}

#[test]
fn use_std_math_abs_as_alias() {
    assert_num(
        r#"
use std.math.{ abs as magnitude }
magnitude(-7)
"#,
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
        r#"
import std.math as m1
import std.math as m2
m1.abs(-3) + m2.abs(-2)
"#,
        "5",
    );
}

#[test]
fn missing_module_errors() {
    run_err(
        r#"
import no.such.module
"#,
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
