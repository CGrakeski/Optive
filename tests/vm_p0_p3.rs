#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_bool, assert_list, assert_num, assert_text, run_err, value};
use optive::{repl_needs_continuation, run_source_in_vm, vm::Vm};

#[test]
fn p0_match_as_expression() {
    assert_text(
        r#"
let n = 0
let s = match (n) {
    case (0) { return "zero" }
} else { return "other" }
s
"#,
        "zero",
    );
}

#[test]
fn p0_range_returns_iterator() {
    let v = value("type(std.math.range(3))");
    assert_eq!(v.display_string(), "iterator");
}

#[test]
fn p0_for_in_iterator() {
    assert_num(
        r"
let total = 0
for (x in std.math.range(3)) { total = total + x }
total
",
        "3",
    );
}

#[test]
fn p0_global_iter_next() {
    assert_num(
        r"
let it = std.math.range(2)
next(it) + next(it)
",
        "1",
    );
}

#[test]
fn p0_is_a_subtype() {
    assert_bool(
        r#"
struct SubText : text { let value }
is_a(SubText("hi"), text)
"#,
        true,
    );
}

#[test]
fn p0_hash_text() {
    assert_num(
        r#"
hash("ab")
"#,
        "3105",
    );
}

#[test]
fn p0_traceback_on_throw() {
    let v = value(
        r#"
let tb = none
try {
    throw ValueError("boom")
} catch (e) {
    tb = e.traceback
}
type(tb)
"#,
    );
    assert_eq!(v.display_string(), "Traceback");
}

#[test]
fn p1_parallel_for() {
    assert_num(
        r"
let total = 0
for (x in [1, 2], y in [10, 20]) { total = total + x + y }
total
",
        "33",
    );
}

#[test]
fn p1_list_comp_zip() {
    assert_list("[x + y for (x in [1, 2], y in [10, 20])]", "[11, 22]");
}

#[test]
fn p1_struct_generics_parse() {
    optive::parse_program("struct Box[T: num] { let value }").expect("parse generics");
}

#[test]
fn p1_outside_method() {
    assert_num(
        r"
struct S { let n
    func bump(self) outside { return self.n + 1 }
}
S(4).bump()
",
        "5",
    );
}

#[test]
fn p2_std_typing_union() {
    assert_text(
        r"
use std.typing.{ Union }
Union(num, text)
",
        "Union[num, text]",
    );
}

#[test]
fn p2_std_functional_map() {
    assert_list(
        r"
use std.functional.{ map }
use std.iter.{ to_list }
to_list(map(do(x) { return x * 2 }, std.math.range(3)))
",
        "[0, 2, 4]",
    );
}

#[test]
fn p2_std_collections_sum() {
    assert_num(
        r"
use std.collections.{ sum }
sum([1, 2, 3])
",
        "6",
    );
}

#[test]
fn p2_std_collections_sum_rejects_non_integer() {
    run_err(
        r#"
use std.collections.{ sum }
sum([1, "x"])
"#,
    );
}

#[test]
fn p2_list_append_method() {
    assert_list(
        r"
let xs = [1]
xs.append(2)
xs
",
        "[1, 2]",
    );
}

#[test]
fn p2_dict_get_method() {
    assert_num(
        r#"
let d = {"a": 1}
d.get("a")
"#,
        "1",
    );
}

#[test]
fn p2_text_upper_method() {
    assert_text(r#""hi".upper()"#, "HI");
}

#[test]
fn p2_decos_timer_exists() {
    value(
        r"
use std.decos.{ timer }
timer func f() { return 1 }
",
    );
}

#[test]
fn p3_std_json_roundtrip() {
    assert_text(
        r#"
use std.json.{ parse, stringify }
stringify(parse("[1, 2]"))
"#,
        "[1,2]",
    );
}

#[test]
fn p3_std_path_join() {
    assert_text(
        r#"
use std.path.{ join }
join("a", "b")
"#,
        "a/b",
    );
}

#[test]
fn p3_std_test_assert_eq() {
    value(
        r"
use std.test.{ assert_eq }
assert_eq(1, 1)
",
    );
}

#[test]
fn p3_std_debug_traceback() {
    let v = value(
        r"
use std.debug.{ traceback }
type(traceback())
",
    );
    assert_eq!(v.display_string(), "Traceback");
}

#[test]
fn p3_exception_assertion_error() {
    run_err("throw AssertionError(\"fail\")");
}

#[test]
fn p3_global_repr() {
    assert_text(r"repr(42)", "42");
}

#[test]
fn p3_repl_continuation_detection() {
    assert!(repl_needs_continuation("let x = ("));
    assert!(repl_needs_continuation("/* block"));
    assert!(repl_needs_continuation(r#"r"raw"#));
    assert!(repl_needs_continuation(r#"f"value {x}"#));
    assert!(repl_needs_continuation(r#"b"bytes"#));
    assert!(repl_needs_continuation(r#""""triple"#));
    assert!(!repl_needs_continuation("/* block */"));
    assert!(!repl_needs_continuation("1 + 2"));
}

#[test]
fn p3_repl_persistent_globals() {
    let mut vm = Vm::new();
    run_source_in_vm(&mut vm, "let acc = 0", "<repl>").unwrap();
    run_source_in_vm(&mut vm, "acc = acc + 5", "<repl>").unwrap();
    let v = run_source_in_vm(&mut vm, "acc", "<repl>").unwrap();
    assert_eq!(v.display_string(), "5");
}

#[test]
fn complete_dict_alternating_kv() {
    let v = value(r#"dict("a", 1, "b", 2)"#);
    assert_eq!(v.display_string(), r#"{"a": 1, "b": 2}"#);
}

#[test]
fn complete_lazy_map_does_not_precompute() {
    assert_num(
        r"
use std.functional.{ map }
let it = map(do(x) { return x * 10 }, std.math.range(1000000))
next(it)
",
        "0",
    );
}

#[test]
fn complete_assert_raises_catches() {
    value(
        r#"
use std.test.{ assert_raises }
assert_raises(do() { throw ValueError("bad") }, ValueError)
"#,
    );
}

#[test]
fn complete_assert_raises_fails_without_exception() {
    run_err(
        r"
use std.test.{ assert_raises }
assert_raises(do() { return 1 }, ValueError)
",
    );
}

#[test]
fn complete_traceback_has_source_line() {
    let v = value(
        r#"
let tb = none
try {
    throw ValueError("bad")
} catch (e: ValueError) {
    tb = e.traceback
}
tb.frames[0].line
"#,
    );
    assert!(v.display_string().parse::<i64>().unwrap_or(0) > 0);
}

#[test]
fn complete_generic_struct_inference() {
    assert_num(
        r"
struct Box[T: num] { var value: T }
let b = Box(42)
b.value
",
        "42",
    );
}

#[test]
fn complete_generic_struct_explicit_type_args() {
    assert_num(
        r"
struct Box[T: num] { var value: T }
let b = Box[num](99)
b.value
",
        "99",
    );
}

#[test]
fn rebind_global_function_updates_call() {
    assert_num(
        r"
func f() { return 1 }
f = do() { return 2 }
f()
",
        "2",
    );
}

#[test]
fn overwrite_global_function_with_int_does_not_call_stale() {
    run_err(
        r"
func f() { return 1 }
f = 3
f()
",
    );
}

#[test]
fn reloading_program_does_not_call_stale_hot_function() {
    let mut vm = Vm::with_workers(1);
    let v1 = run_source_in_vm(
        &mut vm,
        "func a() { return 10 }\nfunc f() { return 1 }\nf()",
        "<a>",
    )
    .expect("first snippet");
    let v2 =
        run_source_in_vm(&mut vm, "func f() { return 2 }\nf()", "<b>").expect("second snippet");
    assert_eq!(v1.display_string(), "1");
    assert_eq!(v2.display_string(), "2");
}

#[test]
fn debug_list_globals_sees_hot_int_store() {
    let mut vm = Vm::with_workers(1);
    run_source_in_vm(&mut vm, "x = 0\nx = 42\n", "<g>").expect("run");
    let found = vm.debug_list_globals().into_iter().find(|(k, _)| k == "x");
    let (_, value) = found.expect("global x");
    assert_eq!(value.display_string(), "42");
}
