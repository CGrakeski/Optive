mod common;

use common::{assert_bool, assert_num, assert_text};
use optive::value::Value;

#[test]
fn struct_name_is_type_ref_not_text() {
    let v = common::value(
        r#"
struct A {}
A
"#,
    );
    assert!(
        matches!(v, Value::TypeRef(ref n) if n == "A"),
        "expected TypeRef(A), got {}",
        v.display_string()
    );
}

#[test]
fn struct_type_object_is_metatype() {
    // A is the type object; type(A) is the metatype `type`. A() is an instance of A.
    assert_text(
        r#"
struct A {}
type(A)
"#,
        "type",
    );
    assert_bool(
        r#"
struct A {}
type(A) == type
"#,
        true,
    );
    assert_text(
        r#"
struct A {}
type(A())
"#,
        "A",
    );
    assert_bool(
        r#"
struct A {}
is_a(A, type)
"#,
        true,
    );
}

#[test]
fn struct_construct_and_field_x() {
    assert_num(
        r#"
struct Point { let x let y }
Point(3, 4).x
"#,
        "3",
    );
}

#[test]
fn struct_second_field_y() {
    assert_num(
        r#"
struct Point { let x let y }
Point(3, 4).y
"#,
        "4",
    );
}

#[test]
fn struct_method_call() {
    assert_num(
        r#"
struct Counter {
    var n
    func sum(self) { return self.n + 10 }
}
Counter(5).sum()
"#,
        "15",
    );
}

#[test]
fn struct_var_field_mutable() {
    assert_num(
        r#"
struct Box { var value }
let b = Box(1)
b.value = 42
b.value
"#,
        "42",
    );
}

#[test]
fn struct_two_instances() {
    assert_num(
        r#"
struct Pair { let a let b }
Pair(1, 2).a + Pair(3, 4).b
"#,
        "5",
    );
}
