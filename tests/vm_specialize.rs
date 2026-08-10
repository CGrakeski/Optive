#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_num, assert_text};

#[test]
fn specialized_num_add_runs() {
    assert_num("1 + 2", "3");
}

#[test]
fn specialized_local_num_interval() {
    assert_num(
        r"
let a = 10
a + 5
",
        "15",
    );
}

#[test]
fn specialized_text_concat() {
    assert_text(r#""ab" + "cd""#, "abcd");
}

#[test]
fn specialized_list_concat() {
    assert_num("len([1, 2] + [3])", "3");
}

#[test]
fn specialized_cmp() {
    assert_num(
        r"
let a = 3
let b = 5
if (a < b) { 1 } else { 0 }
",
        "1",
    );
}

#[test]
fn specialized_strong_param_interval() {
    assert_num(
        r"
func add1(x:: num) {
    return x + 1
}
add1(41)
",
        "42",
    );
}

#[test]
fn soft_param_still_runs() {
    // 无强注解：编译期不播种；绑定后通过访问实参类型执行，语义正确。
    assert_num(
        r"
func add1(x: num) {
    return x + 1
}
add1(41)
",
        "42",
    );
}

#[test]
fn untyped_param_still_runs() {
    assert_num(
        r"
func add1(x) {
    return x + 1
}
add1(41)
",
        "42",
    );
}

#[test]
fn untyped_text_param_concat() {
    assert_text(
        r#"
func cat(a) {
    return a + "!"
}
cat("hi")
"#,
        "hi!",
    );
}
