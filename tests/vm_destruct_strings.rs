#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_bool, assert_list, assert_num, assert_text, run_err};

#[test]
fn multiline_string() {
    assert_text(
        r#"
let s = """
hello
world
"""
s
"#,
        "\nhello\nworld\n",
    );
}

#[test]
fn multiline_string_escapes() {
    assert_text(
        r#"
"""a\tb\nc"""
"#,
        "a\tb\nc",
    );
}

#[test]
fn string_hex_escape() {
    assert_text(r#""\x41\x42""#, "AB");
    assert_text(r#""hi\x20there""#, "hi there");
    assert_num(r#"len("\x00")"#, "1");
}

#[test]
fn fstring_hex_escape() {
    assert_text(r#"f"\x41={1+1}""#, "A=2");
}

#[test]
fn raw_string_no_escape() {
    assert_text(r#"r"\n\t\\""#, r"\n\t\\");
}

#[test]
fn raw_triple_string() {
    assert_text(
        r#"
r"""C:\Users\test
line2"""
"#,
        "C:\\Users\\test\nline2",
    );
}

#[test]
fn multiline_fstring() {
    assert_text(
        r#"
let n = 3
f"""x={n}
y"""
"#,
        "x=3\ny",
    );
}

#[test]
fn destruct_tuple_let() {
    assert_num(
        r"
let (x, y) = (10, 20)
x + y
",
        "30",
    );
}

#[test]
fn destruct_list_let() {
    assert_num(
        r"
let [a, b] = [1, 2]
a * 10 + b
",
        "12",
    );
}

#[test]
fn destruct_nested() {
    assert_num(
        r"
let ((a, b), [c, d]) = ((1, 2), [3, 4])
a + b + c + d
",
        "10",
    );
}

#[test]
fn destruct_deep_list() {
    assert_num(
        r"
let [a, [b, [c, d]]] = [1, [2, [3, 4]]]
a * 1000 + b * 100 + c * 10 + d
",
        "1234",
    );
}

#[test]
fn destruct_rest() {
    assert_list(
        r"
let [first, *rest] = [1, 2, 3, 4]
rest
",
        "[2, 3, 4]",
    );
}

#[test]
fn destruct_rest_middle() {
    assert_list(
        r"
let [a, *mid, b] = [1, 2, 3, 4, 5]
mid
",
        "[2, 3, 4]",
    );
}

#[test]
fn destruct_rest_discard() {
    assert_num(
        r"
let [a, *_, b] = [9, 0, 0, 7]
a * 10 + b
",
        "97",
    );
}

#[test]
fn destruct_from_tuple_with_list_pattern() {
    assert_num(
        r"
let [x, y] = (3, 4)
x + y
",
        "7",
    );
}

#[test]
fn destruct_assign_stmt() {
    assert_num(
        r"
x = 0
y = 0
(x, y) = (5, 6)
x + y
",
        "11",
    );
}

#[test]
fn destruct_assignment_swaps_from_one_rhs_snapshot() {
    assert_num(
        r"
var a = 2
var b = 7
(a, b) = (b, a)
a * 10 + b
",
        "72",
    );
}

#[test]
fn nested_destruct_assignment_with_rest() {
    assert_num(
        r"
var head = 0
var middle = []
var tail = 0
[head, *middle, tail] = [1, 2, 3, 4]
head * 100 + middle[0] * 10 + tail
",
        "124",
    );
}

#[test]
fn destruct_discard() {
    assert_num(
        r"
let (a, _, c) = (1, 99, 3)
a + c
",
        "4",
    );
}

#[test]
fn destruct_length_mismatch() {
    run_err(
        r"
let (a, b) = [1]
a
",
    );
}

#[test]
fn object_destruct_maps_keys_to_explicit_bindings_and_rest() {
    assert_num(
        r#"
let { x = left, y = top, **rest } = {"x": 2, "y": 3, "z": 4}
left * 100 + top * 10 + rest["z"]
"#,
        "234",
    );
}

#[test]
fn object_destruct_assignment_updates_existing_bindings() {
    assert_num(
        r#"
var left = 0
var top = 0
{ x = left, y = top } = {"x": 5, "y": 8}
left * 10 + top
"#,
        "58",
    );
}

#[test]
fn object_destruct_uses_default_only_for_a_missing_key() {
    assert_num(
        r#"
let { x = left, retries = 3 } = {"x": 7}
left * 10 + retries
"#,
        "73",
    );
}

#[test]
fn object_destruct_default_does_not_replace_present_none() {
    assert_bool(
        r#"
let { x = 3 } = {"x": none}
x is none
"#,
        true,
    );
}

#[test]
fn object_destruct_accepts_a_compile_time_frozen_dict() {
    assert_num(
        r#"
const OPTIONS = {"x": 6, "y": 2}
let { x = left, y = top } = OPTIONS
left * 10 + top
"#,
        "62",
    );
}

#[test]
fn object_destruct_uses_default_struct_unpacking() {
    assert_num(
        r#"
struct Point { let x let y }
let { x = left, y = top } = Point(4, 9)
left * 10 + top
"#,
        "49",
    );
}

#[test]
fn object_destruct_calls_custom_unpack_once() {
    assert_num(
        r#"
var calls = 0
struct Point {
    let x
    let y
    func __unpack__(self) {
        calls = calls + 1
        return {"x": self.y, "y": self.x}
    }
}
let { x = left, y = top } = Point(4, 9)
calls * 100 + left * 10 + top
"#,
        "194",
    );
}

#[test]
fn object_destruct_rejects_non_dict_unpack_result() {
    run_err(
        r#"
struct Bad {
    func __unpack__(self) { return 1 }
}
let { x = value } = Bad()
value
"#,
    );
}

#[test]
fn walrus_in_expression() {
    assert_num(
        r"
x = 0
if ((n := 4) > 0) { x = n }
x
",
        "4",
    );
}

#[test]
fn walrus_chained_and_paren() {
    assert_num(
        r"
(a := (b := 7))
a + b
",
        "14",
    );
}

#[test]
fn walrus_bare_statement() {
    assert_num(
        r"
p := 11
p
",
        "11",
    );
}

#[test]
fn walrus_still_works_in_if() {
    assert_num(
        r"
x = 0
if (p := 3) { x = p }
x
",
        "3",
    );
}
