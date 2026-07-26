mod common;

use common::{assert_list, assert_num, assert_text, run_err};

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
fn raw_string_no_escape() {
    assert_text(r#"r"\n\t\\""#, r"\n\t\\");
}

#[test]
fn raw_triple_string() {
    assert_text(
        r##"
r"""C:\Users\test
line2"""
"##,
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
        r#"
let (x, y) = (10, 20)
x + y
"#,
        "30",
    );
}

#[test]
fn destruct_list_let() {
    assert_num(
        r#"
let [a, b] = [1, 2]
a * 10 + b
"#,
        "12",
    );
}

#[test]
fn destruct_nested() {
    assert_num(
        r#"
let ((a, b), [c, d]) = ((1, 2), [3, 4])
a + b + c + d
"#,
        "10",
    );
}

#[test]
fn destruct_deep_list() {
    assert_num(
        r#"
let [a, [b, [c, d]]] = [1, [2, [3, 4]]]
a * 1000 + b * 100 + c * 10 + d
"#,
        "1234",
    );
}

#[test]
fn destruct_rest() {
    assert_list(
        r#"
let [first, *rest] = [1, 2, 3, 4]
rest
"#,
        "[2, 3, 4]",
    );
}

#[test]
fn destruct_rest_middle() {
    assert_list(
        r#"
let [a, *mid, b] = [1, 2, 3, 4, 5]
mid
"#,
        "[2, 3, 4]",
    );
}

#[test]
fn destruct_rest_discard() {
    assert_num(
        r#"
let [a, *_, b] = [9, 0, 0, 7]
a * 10 + b
"#,
        "97",
    );
}

#[test]
fn destruct_from_tuple_with_list_pattern() {
    assert_num(
        r#"
let [x, y] = (3, 4)
x + y
"#,
        "7",
    );
}

#[test]
fn destruct_assign_stmt() {
    assert_num(
        r#"
x = 0
y = 0
(x, y) = (5, 6)
x + y
"#,
        "11",
    );
}

#[test]
fn destruct_discard() {
    assert_num(
        r#"
let (a, _, c) = (1, 99, 3)
a + c
"#,
        "4",
    );
}

#[test]
fn destruct_length_mismatch() {
    run_err(
        r#"
let (a, b) = [1]
a
"#,
    );
}

#[test]
fn walrus_in_expression() {
    assert_num(
        r#"
x = 0
if ((n := 4) > 0) { x = n }
x
"#,
        "4",
    );
}

#[test]
fn walrus_chained_and_paren() {
    assert_num(
        r#"
(a := (b := 7))
a + b
"#,
        "14",
    );
}

#[test]
fn walrus_bare_statement() {
    assert_num(
        r#"
p := 11
p
"#,
        "11",
    );
}

#[test]
fn walrus_still_works_in_if() {
    assert_num(
        r#"
x = 0
if (p := 3) { x = p }
x
"#,
        "3",
    );
}
