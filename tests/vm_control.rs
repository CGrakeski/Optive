#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
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
        r"
var sum = 0
var i = 0
while (i < 5) {
    sum = sum + i
    i = i + 1
}
sum
",
        "10",
    );
}

#[test]
fn while_zero_iterations() {
    assert_num(
        r"
let x = 42
while (false) { x = 0 }
x
",
        "42",
    );
}

#[test]
fn loop_counted() {
    assert_num(
        r"
var n = 0
loop (5) { n = n + 1 }
n
",
        "5",
    );
}

#[test]
fn loop_zero() {
    assert_num(
        r"
var n = 0
loop (0) { n = n + 1 }
n
",
        "0",
    );
}

#[test]
fn loop_break() {
    assert_num(
        r"
var n = 0
loop {
    n = n + 1
    if (n == 3) { break }
}
n
",
        "3",
    );
}

#[test]
fn loop_counted_break() {
    assert_num(
        r"
var n = 0
loop (10) {
    n = n + 1
    if (n == 3) { break }
}
n
",
        "3",
    );
}

#[test]
fn loop_continue() {
    assert_num(
        r"
var sum = 0
var i = 0
loop (5) {
    i = i + 1
    if (i == 3) { continue }
    sum = sum + i
}
sum
",
        "12",
    );
}

#[test]
fn labeled_break_exits_outer_for() {
    assert_num(
        r"
var seen = 0
outer for (row in [[1, 2], [3, 4]]) {
    for (cell in row) {
        seen = seen + 1
        if (cell == 2) { break outer }
    }
}
seen
",
        "2",
    );
}

#[test]
fn labeled_continue_advances_outer_for() {
    assert_num(
        r"
var total = 0
outer for (row in [[1, 2], [3, 4]]) {
    for (cell in row) {
        if (cell == 1 or cell == 3) { continue outer }
        total = total + cell
    }
}
total
",
        "0",
    );
}

#[test]
fn labeled_break_cleans_nested_counted_loop() {
    assert_num(
        r"
var n = 0
outer loop (4) {
    loop (3) {
        n = n + 1
        break outer
    }
}
n
",
        "1",
    );
}

#[test]
fn loop_return_inside_counted() {
    assert_num(
        r"
func f() {
    loop (10) {
        return 7
    }
    return 0
}
f()
",
        "7",
    );
}

#[test]
fn nested_counted_loops() {
    assert_num(
        r"
var n = 0
loop (3) {
    loop (4) {
        n = n + 1
    }
}
n
",
        "12",
    );
}

#[test]
fn for_in_list() {
    assert_num(
        r"
var sum = 0
for (x in [1, 2, 3, 4]) { sum = sum + x }
sum
",
        "10",
    );
}

#[test]
fn for_in_text() {
    assert_num(
        r#"
var n = 0
for (c in "abc") { n = n + 1 }
n
"#,
        "3",
    );
}

#[test]
fn for_in_empty_list() {
    assert_num(
        r"
let n = 99
for (x in []) { n = 0 }
n
",
        "99",
    );
}

#[test]
fn for_body_let_is_fresh_on_every_iteration() {
    assert_num(
        r#"
var total = 0
for (x in [1, 2, 3]) {
    let key = text.(x)
    total = total + len(key)
}
total
"#,
        "3",
    );
}

#[test]
fn for_multiple_const_locals_keep_their_own_slots() {
    assert_num(
        r#"
var total = 0
for (x in [1, 2, 3]) {
    let a = x + 1
    let b = a + 1
    total = total + b
}
total
"#,
        "12",
    );
}

#[test]
fn for_bindings_do_not_leak_or_overwrite_outer_bindings() {
    assert_num(
        r"
let x = 40
for (x in [1, 2]) {
    let local = x + 10
}
x
",
        "40",
    );
}

#[test]
fn for_iteration_scope_is_left_on_continue_and_break() {
    assert_num(
        r"
var total = 0
for (x in [1, 2, 3, 4]) {
    let local = x
    if (x == 1) { continue }
    total = total + local
    if (x == 3) { break }
}
total
",
        "5",
    );
}

#[test]
fn block_scope() {
    assert_num(
        r"
let x = 1
{
    let x = 2
}
x
",
        "1",
    );
}
