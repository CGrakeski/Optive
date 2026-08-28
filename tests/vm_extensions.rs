#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_bool, assert_num, assert_text};
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn fstring_basic() {
    assert_text(
        r#"
x = 3
f"hi {x}!"
"#,
        "hi 3!",
    );
}

#[test]
fn fstring_expression() {
    assert_text(
        r#"
f"{1 + 2}-{true}"
"#,
        "3-true",
    );
}

#[test]
fn is_object_identity() {
    assert_bool(
        r"
a = [1, 2]
b = a
c = [1, 2]
a is b
",
        true,
    );
    assert_bool(
        r"
a = [1, 2]
c = [1, 2]
a is c
",
        false,
    );
    assert_bool(
        r"
none is none
",
        true,
    );
}

#[test]
fn exception_inheritance_chain() {
    assert_text(
        r#"
use std.exceptions.{ chain }
chain("KeyError")[1]
"#,
        "LookupError",
    );
    assert_text(
        r#"
use std.exceptions.{ chain }
chain("ValueError")[2]
"#,
        "BaseException",
    );
    assert_text(
        r#"
use std.exceptions.{ chain }
chain("ZeroDivisionError")[1]
"#,
        "ArithmeticError",
    );
}

#[test]
fn catch_base_exception_subclass() {
    assert_text(
        r#"
try {
    throw KeyError("missing")
} catch (e: Exception) {
    return "caught"
} else {
    return "no"
}
"#,
        "caught",
    );
}

#[test]
fn std_re_match_and_sub() {
    assert_bool(
        r#"
use std.re.{ match as re_match }
m = re_match("\\d+", "abc123")
m != none
"#,
        true,
    );
    assert_text(
        r##"
use std.re.{ sub }
sub("\\d+", "#", "a1b2")
"##,
        "a#b#",
    );
}

#[test]
fn std_hash_md5_sha256() {
    assert_text(
        r#"
use std.hash.{ md5, sha256 }
md5("")
"#,
        "d41d8cd98f00b204e9800998ecf8427e",
    );
    assert_num(
        r#"
use std.hash.{ sha256 }
len(sha256("hello"))
"#,
        "64",
    );
    assert_text(
        r#"
use std.hash.{ md5 }
md5(b"")
"#,
        "d41d8cd98f00b204e9800998ecf8427e",
    );
}

#[test]
fn import_relative_path() {
    let path = fixture("things.tive");
    let src = format!("import \"{path}\" as things\nthings.value\n");
    assert_num(&src, "42");
}

#[test]
fn use_relative_path_nested_exports() {
    let path = fixture("things.tive");
    let src = format!("use \"{path}\".sth0.{{ sth1, sth2 }}\nsth1() + sth2()\n");
    assert_num(&src, "303");
}

#[test]
fn dict_get_raises_key_error_catchable() {
    assert_text(
        r#"
use std.dict.{ get }
try {
    get({}, "x")
    return "no"
} catch (e: KeyError) {
    return "yes"
}
"#,
        "yes",
    );
}
