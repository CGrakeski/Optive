mod common;

use common::{assert_num, assert_text, num};

#[test]
fn list_slice_basic() {
    assert_num(
        r#"
let xs = [1, 2, 3, 4, 5]
len(xs[1:3])
"#,
        "2",
    );
}

#[test]
fn list_slice_values() {
    assert_num(
        r#"
let xs = [1, 2, 3, 4, 5]
xs[1:3][0]
"#,
        "2",
    );
}

#[test]
fn list_slice_step() {
    assert_num(
        r#"
let xs = [1, 2, 3, 4, 5]
xs[0:5:2][1]
"#,
        "3",
    );
}

#[test]
fn list_slice_from_start() {
    assert_num(
        r#"
let xs = [1, 2, 3]
len(xs[:2])
"#,
        "2",
    );
}

#[test]
fn list_slice_to_end() {
    assert_num(
        r#"
let xs = [1, 2, 3]
xs[1:][0]
"#,
        "2",
    );
}

#[test]
fn list_slice_negative_index() {
    assert_num(
        r#"
let xs = [1, 2, 3, 4]
xs[-2:][0]
"#,
        "3",
    );
}

#[test]
fn text_slice_basic() {
    assert_text(
        r#"
"hello"[1:4]
"#,
        "ell",
    );
}

#[test]
fn text_slice_unicode() {
    assert_text(
        r#"
"世界你好"[2:4]
"#,
        "你好",
    );
}

#[test]
fn list_index_assign() {
    assert_num(
        r#"
let xs = [1, 2, 3]
xs[1] = 99
xs[1]
"#,
        "99",
    );
}

#[test]
fn dict_index_assign() {
    assert_num(
        r#"
let d = {1: 10}
d[1] = 20
d[1]
"#,
        "20",
    );
}

#[test]
fn list_slice_assign() {
    assert_num(
        r#"
let xs = [1, 2, 3, 4]
xs[1:3] = [20, 30]
xs[1] + xs[2]
"#,
        "50",
    );
}

#[test]
fn nested_index_after_slice() {
    assert_num(
        r#"
[[1, 2], [3, 4]][0:1][0][1]
"#,
        "2",
    );
}

#[test]
fn slice_empty_range() {
    assert_num(
        r#"
len([1, 2, 3][5:10])
"#,
        "0",
    );
}

#[test]
fn list_index_assign_negative() {
    assert_num(
        r#"
let xs = [1, 2, 3]
xs[-1] = 7
xs[2]
"#,
        "7",
    );
}

#[test]
fn slice_display_via_len() {
    assert_eq!(num("[1,2,3,4,5][1:4:2] |> len(_)"), "2");
}
