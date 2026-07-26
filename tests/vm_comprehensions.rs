mod common;

use common::{assert_list, assert_num, value};

#[test]
fn list_comp_basic() {
    assert_list("[x * 2 for (x in [1, 2, 3])]", "[2, 4, 6]");
}

#[test]
fn list_comp_guard_and_zip() {
    // parallel for is zip, not cartesian: (1,10) filtered, (2,20) kept
    assert_list(
        "[x + y for (x in [1, 2], y in [10, 20]) if (x > 1)]",
        "[22]",
    );
}

#[test]
fn set_comp_basic() {
    let v = value("{x * x for (x in [1, 2, 2, 3])}");
    assert_eq!(v.display_string(), "{1, 4, 9}");
}

#[test]
fn set_comp_with_guard() {
    let v = value("{x for (x in [1, 2, 3, 4]) if (x == 2 or x == 4)}");
    assert_eq!(v.display_string(), "{2, 4}");
}

#[test]
fn dict_comp_basic() {
    let v = value("{x: x * x for (x in [1, 2, 3])}");
    assert_eq!(v.display_string(), "{1: 1, 2: 4, 3: 9}");
}

#[test]
fn dict_comp_with_guard() {
    let v = value("{x: x + 1 for (x in [1, 2, 3]) if (x != 2)}");
    assert_eq!(v.display_string(), "{1: 2, 3: 4}");
}

#[test]
fn generator_exp_lazy_to_list() {
    assert_list(
        r#"
use std.iter.{ to_list }
to_list((x * x for (x in [1, 2, 3])))
"#,
        "[1, 4, 9]",
    );
}

#[test]
fn generator_exp_with_guard() {
    assert_list(
        r#"
use std.iter.{ to_list }
use std.math.{ range }
to_list((x for (x in range(1, 6)) if (x == 2 or x == 4)))
"#,
        "[2, 4]",
    );
}

#[test]
fn generator_exp_is_lazy() {
    assert_num(
        r#"
let hits = 0
func bump(x) {
    hits = hits + 1
    return x
}
let g = (bump(x) for (x in [1, 2, 3]))
hits
"#,
        "0",
    );
}

#[test]
fn generator_exp_zip() {
    assert_list(
        r#"
use std.iter.{ to_list }
to_list((x + y for (x in [1, 2], y in [10, 20])))
"#,
        "[11, 22]",
    );
}

#[test]
fn nested_list_comp() {
    assert_list(
        "[y for (y in [x * 2 for (x in [1, 2, 3])] ) if (y > 2)]",
        "[4, 6]",
    );
}

#[test]
fn comp_capture_outer() {
    assert_list(
        r#"
let k = 10
[x + k for (x in [1, 2])]
"#,
        "[11, 12]",
    );
}

#[test]
fn generator_capture_outer() {
    assert_list(
        r#"
use std.iter.{ to_list }
let k = 5
to_list((x + k for (x in [1, 2])))
"#,
        "[6, 7]",
    );
}
