mod common;

use common::{assert_num, assert_text};
use optive::value::Value;

#[test]
fn builtin_print_returns_none() {
    let v = common::value("print(1)");
    assert!(matches!(v, Value::None));
}

#[test]
fn len_list_five() {
    assert_num("len([1, 2, 3, 4, 5])", "5");
}

#[test]
fn len_text_hello() {
    assert_num(r#"len("hello")"#, "5");
}

#[test]
fn len_empty_text() {
    assert_num(r#"len("")"#, "0");
}

#[test]
fn str_number() {
    assert_text("str(42)", "42");
}

#[test]
fn str_bool_true() {
    assert_text("str(true)", "true");
}

#[test]
fn type_of_num() {
    assert_text("type(1)", "num");
}

#[test]
fn type_of_text() {
    assert_text(r#"type("x")"#, "text");
}

#[test]
fn type_of_list() {
    assert_text("type([])", "list");
}

#[test]
fn global_true_literal() {
    assert_num("if (true) { 1 } else { 0 }", "1");
}

#[test]
fn global_false_literal() {
    assert_num("if (false) { 0 } else { 1 }", "1");
}

#[test]
fn global_none_value() {
    assert_eq!(common::value("none").display_string(), "none");
}

#[test]
fn type_of_bool() {
    assert_text("type(true)", "bool");
}

#[test]
fn type_of_none() {
    assert_text("type(none)", "nonetype");
}
