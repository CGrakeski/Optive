mod common;

use common::{assert_bool, assert_num, assert_text};

#[test]
fn enum_default_numbering() {
    assert_num(
        r#"
enum Color {
    Red
    Green
    Blue
}
Color.Red.__value__
"#,
        "0",
    );
    assert_num(
        r#"
enum Color { Red Green Blue }
Color.Green.__value__
"#,
        "1",
    );
    assert_num(
        r#"
enum Color { Red Green Blue }
Color.Blue.__value__
"#,
        "2",
    );
}

#[test]
fn enum_explicit_value() {
    assert_num(
        r#"
enum Http {
    Ok = 200
    NotFound = 404
}
Http.NotFound.__value__
"#,
        "404",
    );
}

#[test]
fn enum_members_and_name_of() {
    assert_num(
        r#"
enum Color { Red Green Blue }
len(Color.members())
"#,
        "3",
    );
    assert_text(
        r#"
enum Color { Red Green Blue }
Color.name_of(1)
"#,
        "Green",
    );
}

#[test]
fn enum_cross_type_eq_false() {
    assert_bool(
        r#"
enum A { x }
enum B { x }
A.x == B.x
"#,
        false,
    );
}

#[test]
fn enum_eq_num_via_value() {
    assert_bool(
        r#"
enum Color { Red Green Blue }
Color.Red.__value__ == 0
"#,
        true,
    );
}

#[test]
fn enum_match_value_pattern() {
    assert_text(
        r#"
enum Color { Red Green Blue }
match (Color.Green) {
    case (Color.Green) { "matched Green" }
} else { "no match" }
"#,
        "matched Green",
    );
}

#[test]
fn enum_generate_custom_numbering() {
    assert_num(
        r#"
enum C {
    a = 1
    b
    c
    d
    func __generate__(all) {
        ret = {}
        next = 1
        for (name in all.keys()) {
            v = all.get(name)
            if (v != none) {
                ret.set(name, v)
                next = v + 1
            } else {
                ret.set(name, next)
                next = next + 1
            }
        }
        return ret
    }
}
C.d.__value__
"#,
        "4",
    );
}
