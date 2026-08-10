#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_bool, assert_num, assert_text, value};
use optive::run_source;

#[test]
fn set_literal_and_contains() {
    assert_bool("1 in {1, 2, 3}", true);
    assert_bool("4 in {1, 2, 3}", false);
    assert_num("{1, 2, 3}.len()", "3");
}

#[test]
fn nested_dict_in_set_errors_consistently() {
    // 语句级与赋值右侧都应把 `{{}}` 当作集合字面量，并因 dict 不可哈希而失败。
    common::run_err("{{}}");
    common::run_err("{{{}}}");
    common::run_err("a = {{}}");
    common::run_err("let a = {{}}");
    let err = optive::run_source("{{}}").unwrap_err().to_string();
    assert!(
        err.contains("unhashable"),
        "expected unhashable error, got: {err}"
    );
}

#[test]
fn single_element_set_at_statement_level() {
    assert_num("{1}.len()", "1");
    assert_bool("1 in {1}", true);
}

#[test]
fn set_empty_via_ctor() {
    assert_num("set().len()", "0");
}

#[test]
fn empty_set_displays_as_comma_literal() {
    assert_eq!(value("set()").display_string(), "{,}");
    assert_eq!(value("{,}").display_string(), "{,}");
}


#[test]
fn set_add_remove() {
    assert_num(
        r"
let s = {1, 2}
s.add(3)
s.remove(1)
s.len()
",
        "2",
    );
}

#[test]
fn set_eq_ignores_order() {
    assert_bool("{1, 2, 3} == {3, 1, 2}", true);
}

#[test]
fn tuple_literal_and_index() {
    assert_num("(10, 20, 30)[1]", "20");
    assert_num("(42,).len()", "1");
    assert_num("().len()", "0");
}

#[test]
fn tuple_eq() {
    assert_bool("(1, 2) == (1, 2)", true);
    assert_bool("(1, 2) == (2, 1)", false);
}

#[test]
fn tuple_from_list_convert() {
    assert_num("tuple.([1, 2, 3])[0]", "1");
}

#[test]
fn bytes_literal() {
    assert_num(r#"b"hi".len()"#, "2");
    assert_num(r#"b"hi"[0]"#, "104");
}

#[test]
fn bytes_hex_escape() {
    assert_num(r#"b"\x00\xff".len()"#, "2");
    assert_num(r#"b"\xff"[0]"#, "255");
}

#[test]
fn bytes_decode() {
    assert_text(r#"b"abc".decode()"#, "abc");
}

#[test]
fn bytes_ctor_from_list() {
    assert_num("bytes([65, 66]).len()", "2");
    assert_text(r"bytes([65, 66]).decode()", "AB");
}

#[test]
fn list_set_tuple_roundtrip_convert() {
    assert_bool("list.({1, 2}).len() == 2", true);
    assert_num("tuple.([7, 8])[1]", "8");
}

#[test]
fn gc_breaks_list_cycle() {
    let cleared = run_source(
        r"
func make_cycle() {
    let a = []
    a.append(a)
    return none
}
make_cycle()
gc()
",
    )
    .expect("gc");
    match cleared {
        optive::value::Value::Num(n) => {
            assert!(n.to_i64().unwrap_or(0) >= 1, "expected at least 1 cleared");
        }
        other => panic!("expected num, got {}", other.display_string()),
    }
    let again = value("gc()");
    assert_eq!(again.display_string(), "0");
}

#[test]
fn hash_small_num_works() {
    assert_num("hash(42)", "42");
}
