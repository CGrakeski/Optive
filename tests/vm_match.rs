mod common;

use common::{assert_num, assert_text};

#[test]
fn match_value_zero() {
    assert_text(
        r#"
match (0) {
    case (0) { "zero" }
} else { "other" }
"#,
        "zero",
    );
}

#[test]
fn match_value_or_pattern() {
    assert_text(
        r#"
match (2) {
    case (1) | (2) { "small" }
} else { "big" }
"#,
        "small",
    );
}

#[test]
fn match_list_destructure() {
    assert_num(
        r#"
match ([10, 20]) {
    case [a, b] { a + b }
} else { 0 }
"#,
        "30",
    );
}

#[test]
fn match_struct_destructure() {
    assert_num(
        r#"
struct Point { let x let y }
match (Point(3, 4)) {
    case Point { x, y } { x + y }
} else { 0 }
"#,
        "7",
    );
}

#[test]
fn match_struct_call_pattern_binds_fields() {
    assert_num(
        r#"
struct Point { let x let y }
match (Point(3, 4)) {
    case Point(a, b) { a + b }
} else { 0 }
"#,
        "7",
    );
}

#[test]
fn match_else_when_no_match() {
    assert_text(
        r#"
match (99) {
    case (0) { "zero" }
} else { "other" }
"#,
        "other",
    );
}

#[test]
fn match_no_else_returns_none_display() {
    let v = common::value(
        r#"
match (99) {
    case (0) { "zero" }
}
"#,
    );
    assert_eq!(v.display_string(), "none");
}

#[test]
fn match_tuple_literal_and_bind() {
    assert_num(
        r#"
let ev = ("done", 42)
match (ev) {
  case ("done", rep) { rep }
} else {
  0
}
"#,
        "42",
    );
}

#[test]
fn hex_and_binary_literals() {
    assert_num("0x10 + 0b1010", "26");
}
