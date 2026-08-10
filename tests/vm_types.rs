#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_num, run_err};

fn caught_type_error(source: &str) -> bool {
    let src = format!(
        r"
try {{
    {source}
    0
}} catch (e: TypeError) {{
    1
}}
"
    );
    common::num(&src) == "1"
}

fn caught_type_or_name_error(source: &str) -> bool {
    let src = format!(
        r"
try {{
    {source}
    0
}} catch (e: TypeError) {{
    1
}} catch (e: NameError) {{
    1
}}
"
    );
    common::num(&src) == "1"
}

#[test]
fn soft_var_annotation_allows_mismatch() {
    assert_num(
        r#"
let a: num = "oops"
1
"#,
        "1",
    );
}

#[test]
fn strong_var_init_rejects_mismatch() {
    assert!(caught_type_error(r#"let b:: num = "oops""#));
}

#[test]
fn strong_var_reassign_rejects_mismatch() {
    assert!(caught_type_error(
        r#"
let b:: num = 1
b = "oops"
"#
    ));
}

#[test]
fn soft_reassign_no_check() {
    assert_num(
        r#"
let a: num = 1
a = "oops"
1
"#,
        "1",
    );
}

#[test]
fn strong_param_rejects_at_call() {
    assert!(caught_type_error(
        r#"
func hard(x:: num) { return x }
hard("a")
"#
    ));
}

#[test]
fn soft_param_allows_mismatch() {
    assert_num(
        r#"
func soft(x: num) { return 1 }
soft("a")
"#,
        "1",
    );
}

#[test]
fn strong_list_rejects_bad_append() {
    assert!(caught_type_error(
        r#"
let xs:: list[num] = [1]
xs.append("a")
"#
    ));
}

#[test]
fn strong_list_alias_shares_contract() {
    assert!(caught_type_error(
        r#"
let xs:: list[num] = [1]
let ys = xs
ys.append("a")
"#
    ));
}

#[test]
fn strong_list_index_set_rejects() {
    assert!(caught_type_error(
        r#"
let xs:: list[num] = [1]
xs[0] = "a"
"#
    ));
}

#[test]
fn soft_list_allows_append() {
    assert_num(
        r#"
let xs: list[num] = [1]
xs.append("a")
1
"#,
        "1",
    );
}

#[test]
fn never_rejects_all() {
    assert!(caught_type_error(
        r"
let x:: Never = 1
"
    ));
}

#[test]
fn literal_via_is_a() {
    assert_num(
        r#"
use std.typing.{ Literal }
if is_a("hi", Literal("hi")) then 1 else 0
"#,
        "1",
    );
}

#[test]
fn literal_via_is_a_rejects() {
    assert_num(
        r#"
use std.typing.{ Literal }
if is_a("bye", Literal("hi")) then 1 else 0
"#,
        "0",
    );
}

#[test]
fn strong_dict_rejects_bad_value() {
    assert!(caught_type_error(
        r#"
let d:: dict[text, num] = {"a": 1}
d["b"] = "oops"
"#
    ));
}

#[test]
fn is_a_accepts_typespec_union() {
    assert_num(
        r"
use std.typing.{ Union }
if is_a(1, Union(num, text)) then 1 else 0
",
        "1",
    );
}

#[test]
fn strong_default_arg_rejects_mismatch() {
    assert!(caught_type_error(
        r#"
func f(x:: num = "a") { return x }
f()
"#
    ));
}

#[test]
fn strong_default_explicit_arg_ok() {
    assert_num(
        r#"
func f(x:: num = "a") { return x }
f(3)
"#,
        "3",
    );
}

#[test]
fn strong_return_rejects_mismatch() {
    assert!(caught_type_error(
        r"
func tag() => text { return 1 }
tag()
"
    ));
}

#[test]
fn soft_return_allows_mismatch() {
    assert_num(
        r"
func retSoft(x) -> text { return x }
retSoft(1)
",
        "1",
    );
}

#[test]
fn typed_struct_arrow_return_is_soft() {
    // typed struct 不再把 `->` 升格为强；要强返回须写 `=>`。
    assert_num(
        r"
typed struct Box { var v: num
    func tag(self) -> text { return self.v }
}
Box(1).tag()
",
        "1",
    );
}

#[test]
fn strong_return_with_wrapper_checks_outer() {
    assert!(caught_type_error(
        r"
variant Result {
    typed Ok(value: num)
    Err = typed struct { value: text }
}
func bad() => num : Result(_) { return 1 }
bad()
"
    ));
}

#[test]
fn strong_return_wrapper_ok_when_outer_matches() {
    assert_num(
        r"
variant Result {
    typed Ok(value: num)
    Err = typed struct { value: text }
}
func good() => Result : Result(_) { return Result.Ok(1) }
r = good()
1
",
        "1",
    );
}

#[test]
fn typed_struct_field_colon_checked_on_construct() {
    assert!(caught_type_error(
        r#"
typed struct Point { let x: num let y: num }
Point("a", 2)
"#
    ));
}

#[test]
fn typed_struct_accepts_valid() {
    assert_num(
        r"
typed struct Point { let x: num let y: num }
Point(3, 4).x
",
        "3",
    );
}

#[test]
fn struct_strong_field_on_construct() {
    assert!(caught_type_error(
        r#"
struct Box { var value:: num }
Box("x")
"#
    ));
}

#[test]
fn struct_soft_field_allows_mismatch() {
    assert_num(
        r#"
struct Box { var value: num }
Box("x").value = 1
1
"#,
        "1",
    );
}

#[test]
fn struct_strong_field_assign() {
    assert!(caught_type_error(
        r#"
struct Box { var value:: num }
let b = Box(1)
b.value = "x"
"#
    ));
}

#[test]
fn subtype_text_base_accepts_substruct() {
    assert_num(
        r#"
struct SubText : text { var value: text }
func take(x:: text) { return 1 }
take(SubText("hi"))
"#,
        "1",
    );
}

#[test]
fn list_element_type_check() {
    assert!(caught_type_error(
        r#"
let xs:: list[num] = [1, "two"]
"#
    ));
}

#[test]
fn list_element_type_accepts_valid() {
    assert_num(
        r"
let xs:: list[num] = [1, 2, 3]
xs[0]
",
        "1",
    );
}

#[test]
fn union_type_accepts_member() {
    assert_num(
        r"
func f(x:: Union[num, text]) { return 1 }
f(42)
",
        "1",
    );
}

#[test]
fn union_type_rejects_non_member() {
    assert!(caught_type_error(
        r"
func f(x:: Union[num, text]) { return 1 }
f(true)
"
    ));
}

#[test]
fn maybe_accepts_none() {
    assert_num(
        r"
let m:: Maybe[num] = none
1
",
        "1",
    );
}

#[test]
fn maybe_accepts_inner() {
    assert_num(
        r"
let m:: Maybe[num] = 5
m
",
        "5",
    );
}

#[test]
fn strong_param_passes_valid() {
    assert_num(
        r"
func hard(x:: num) { return x + 1 }
hard(41)
",
        "42",
    );
}

#[test]
fn uncaught_type_error_propagates() {
    run_err(
        r#"
let x:: num = "bad"
"#,
    );
}

#[test]
fn strong_set_rejects_bad_add() {
    assert!(caught_type_error(
        r#"
let s:: set[num] = {1, 2}
s.add("a")
"#
    ));
}

#[test]
fn strong_set_init_rejects_bad_element() {
    assert!(caught_type_error(
        r#"
let s:: set[num] = {1, "two"}
"#
    ));
}

#[test]
fn strong_dict_init_rejects_bad_value() {
    assert!(caught_type_error(
        r#"
let d:: dict[text, num] = {"a": 1, "b": "x"}
"#
    ));
}

#[test]
fn protocol_strong_binding_rejects() {
    assert!(caught_type_error(
        r"
protocol HasMul {
    func __mul__(self, other) { }
}

struct Plain { let v }

let x:: HasMul = Plain(1)
"
    ));
}

#[test]
fn protocol_strong_binding_accepts() {
    assert_num(
        r"
protocol HasMul {
    func __mul__(self, other) { }
}

struct MulNum {
    let v
    func __mul__(self, other) { return self.v * other }
}

let x:: HasMul = MulNum(3)
1
",
        "1",
    );
}

#[test]
fn is_a_protocol_runtime() {
    assert_num(
        r"
protocol HasMul {
    func __mul__(self, other) { }
}

struct MulNum {
    let v
    func __mul__(self, other) { return self.v * other }
}

if is_a(MulNum(2), HasMul) then 1 else 0
",
        "1",
    );
}

#[test]
fn type_error_path_for_list_element() {
    let msg = common::text(
        r#"
try {
    let xs:: list[num] = [1, "bad"]
    "ok"
} catch (e: TypeError) {
    e.message
}
"#,
    );
    assert!(
        msg.contains("expected num") && msg.contains("got text") && msg.contains("[1]"),
        "unexpected message: {msg}"
    );
}

#[test]
fn help_documents_typing_sigils() {
    assert_num(
        r"
help()
1
",
        "1",
    );
}

#[test]
fn strong_param_type_alias_resolves_at_def() {
    assert_num(
        r"
let T = num
func f(x:: T) { return x }
f(3)
",
        "3",
    );
}

#[test]
fn strong_param_unbound_type_name_errors_at_def() {
    assert!(caught_type_or_name_error(
        r"
func f(x:: NotAType) { return x }
"
    ));
}

#[test]
fn annotation_non_type_errors_at_def() {
    // `str` 是内建函数，不是类型 → 定义时失败。
    assert!(caught_type_or_name_error(
        r#"
func greet(name: str, greeting:: str) { greeting + " " + name }
"#
    ));
}

#[test]
fn strong_param_accepts_type_of_function() {
    assert_num(
        r"
func f(x:: type(do() {})) { return 1 }
f(do() { return 9 })
",
        "1",
    );
}

#[test]
fn strong_param_rejects_non_function_for_type_of_function() {
    assert!(caught_type_error(
        r"
func f(x:: type(do() {})) { return 1 }
f(3)
"
    ));
}
