mod common;

use common::{assert_num, assert_text, num, text};

#[test]
fn list_literal_len() {
    assert_num("len([1, 2, 3])", "3");
}

#[test]
fn empty_list_len() {
    assert_num("len([])", "0");
}

#[test]
fn list_index_first() {
    assert_num("[10, 20, 30][0]", "10");
}

#[test]
fn list_index_last() {
    assert_num("[10, 20, 30][2]", "30");
}

#[test]
fn list_nested() {
    assert_num("[[1, 2], [3]][1][0]", "3");
}

#[test]
fn string_concat() {
    assert_text(r#""hello" + " world""#, "hello world");
}

#[test]
fn string_index_unicode() {
    assert_text(r#""世界"[0]"#, "世");
}

#[test]
fn string_len_unicode() {
    assert_num(r#"len("世界")"#, "2");
}

#[test]
fn dict_literal_get() {
    assert_num(r#"{1: 10, 2: 20}[1]"#, "10");
}

#[test]
fn dict_len() {
    assert_num("len({1: 2, 3: 4})", "2");
}

#[test]
fn list_of_strings_index() {
    assert_text(r#"["a", "b"][1]"#, "b");
}

#[test]
fn mixed_list_arithmetic() {
    assert_num("[1, 2, 3][0] + [4, 5][0]", "5");
}

#[test]
fn text_in_list() {
    assert_eq!(text(r#"["x"][0]"#), "x");
}

#[test]
fn num_in_dict() {
    assert_eq!(num(r#"{0: 99}[0]"#), "99");
}

#[test]
fn list_concat_with_plus() {
    assert_num("len([1, 2] + [3, 4])", "4");
    assert_num("([1, 2] + [3])[2]", "3");
}

#[test]
fn empty_dict_len() {
    assert_num("len({})", "0");
}
