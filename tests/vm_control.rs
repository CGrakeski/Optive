mod common;

use common::assert_num;

#[test]
fn if_true_branch() {
    assert_num("if (true) { return 10 } else { return 20 }", "10");
}

#[test]
fn if_false_else() {
    assert_num("if (false) { return 10 } else { return 20 }", "20");
}

#[test]
fn elif_taken() {
    assert_num(
        "if (false) { return 1 } elif (true) { return 2 } else { return 3 }",
        "2",
    );
}

#[test]
fn nested_if() {
    assert_num(
        "if (true) { if (false) { return 1 } else { return 2 } } else { return 3 }",
        "2",
    );
}

#[test]
fn while_sum() {
    assert_num(
        r#"
let sum = 0
let i = 0
while (i < 5) {
    sum = sum + i
    i = i + 1
}
sum
"#,
        "10",
    );
}

#[test]
fn while_zero_iterations() {
    assert_num(
        r#"
let x = 42
while (false) { x = 0 }
x
"#,
        "42",
    );
}

#[test]
fn loop_counted() {
    assert_num(
        r#"
let n = 0
loop (5) { n = n + 1 }
n
"#,
        "5",
    );
}

#[test]
fn loop_zero() {
    assert_num(
        r#"
let n = 0
loop (0) { n = n + 1 }
n
"#,
        "0",
    );
}

#[test]
fn loop_break() {
    assert_num(
        r#"
let n = 0
loop {
    n = n + 1
    if (n == 3) { break }
}
n
"#,
        "3",
    );
}

#[test]
fn loop_counted_break() {
    assert_num(
        r#"
let n = 0
loop (10) {
    n = n + 1
    if (n == 3) { break }
}
n
"#,
        "3",
    );
}

#[test]
fn loop_continue() {
    assert_num(
        r#"
let sum = 0
let i = 0
loop (5) {
    i = i + 1
    if (i == 3) { continue }
    sum = sum + i
}
sum
"#,
        "12",
    );
}

#[test]
fn for_in_list() {
    assert_num(
        r#"
let sum = 0
for (x in [1, 2, 3, 4]) { sum = sum + x }
sum
"#,
        "10",
    );
}

#[test]
fn for_in_text() {
    assert_num(
        r#"
let n = 0
for (c in "abc") { n = n + 1 }
n
"#,
        "3",
    );
}

#[test]
fn for_in_empty_list() {
    assert_num(
        r#"
let n = 99
for (x in []) { n = 0 }
n
"#,
        "99",
    );
}

#[test]
fn block_scope() {
    assert_num(
        r#"
let x = 1
{
    let x = 2
}
x
"#,
        "1",
    );
}
