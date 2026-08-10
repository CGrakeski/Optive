#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_bool, assert_num, assert_text};
use optive::fmt::format_source;

#[test]
fn snap_returns_value() {
    assert_num("snap 42", "42");
    assert_text(r#"snap "hi""#, "hi");
    assert_bool("snap true", true);
}

#[test]
fn snap_of_none_throws() {
    let err = optive::run_source("snap none").unwrap_err().to_string();
    assert!(
        err.contains("ValueError") && err.contains("snap of none"),
        "got: {err}"
    );
}

#[test]
fn snap_of_expr_result() {
    assert_num(
        r"
func maybe(x) {
    if (x > 0) { return x } else { return none }
}
snap maybe(3)
",
        "3",
    );
    let err = optive::run_source(
        r"
func maybe(x) {
    if (x > 0) { return x } else { return none }
}
snap maybe(0)
",
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("snap of none"), "got: {err}");
}

#[test]
fn empty_set_literal_comma() {
    assert_num("{,}.len()", "0");
    assert_bool("1 in {,}", false);
    assert_num(
        r"
let s = {,}
s.add(1)
s.add(2)
s.len()
",
        "2",
    );
}

#[test]
fn empty_brace_still_dict() {
    assert_num(
        r#"
let d = {}
d["a"] = 1
d.len()
"#,
        "1",
    );
}

#[test]
fn fmt_empty_set_uses_comma() {
    let formatted = format_source("{,}").expect("fmt");
    assert!(
        formatted.contains("{,}"),
        "expected {{,}} in formatted output, got: {formatted}"
    );
}
