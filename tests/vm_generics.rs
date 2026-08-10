#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
mod common;

use common::{assert_num, assert_text, parse_ok, run_err};

#[test]
fn parse_protocol_and_generic_func() {
    parse_ok(
        r"
protocol Multiplyable {
    func __mul__(self, other) ...
}

func a[T: Multiplyable](x: T) -> T {
    print(T.__name__)
    return x * x
}
",
    );
}

#[test]
fn generic_func_explicit_type_args() {
    assert_num(
        r"
protocol Multiplyable {
    func __mul__(self, other) { }
}

func a[T: Multiplyable](x: T) -> T {
    return x * x
}

a[num](5)
",
        "25",
    );
}

#[test]
fn generic_func_type_name_substitution() {
    assert_num(
        r#"
protocol Multiplyable {
    func __mul__(self, other) { }
}

func a[T: Multiplyable](x: T) -> num {
    if (T.__name__ == "num") {
        return x * x
    }
    return 0
}

a[num](5)
"#,
        "25",
    );
}

#[test]
fn generic_func_inferred_type_args() {
    assert_num(
        r"
protocol Multiplyable {
    func __mul__(self, other) { }
}

func a[T: Multiplyable](x: T) -> T {
    return x * x
}

a(5)
",
        "25",
    );
}

#[test]
fn main_package_is_main_for_script() {
    assert_num(
        r#"
if (__package__ == "__main__") {
    1
} else {
    0
}
"#,
        "1",
    );
}

#[test]
fn protocol_field_requirement() {
    assert_num(
        r"
protocol HasA {
    var a
}

struct S {
    var a
    func __mul__(self, other) { return self.a * other.a }
}

func f[T: HasA](x: T) -> num {
    return x.a * x.a
}

f(S(3))
",
        "9",
    );
}

#[test]
fn generic_bound_rejected_at_call_site() {
    run_err(
        r"
protocol Multiplyable {
    func __mul__(self, other) { }
}

struct Plain {
    let v
}

func a[T: Multiplyable](x: T) -> T {
    return x
}

a[Plain](Plain(1))
",
    );
}

#[test]
fn parse_protocol_field_var() {
    parse_ok(
        r"
protocol A {
    var a
}
",
    );
}

#[test]
fn generic_func_return_wrapper_substitutes_type_param() {
    assert_num(
        r"
func a[T](b: T) -> T : T.(_) {
    return b
}
a(1)
",
        "1",
    );
}

#[test]
fn generic_func_return_type_param_as_value() {
    assert_text(
        r"
func a[T](b: T) {
    return T
}
type(a(1))
",
        "type",
    );
    assert_text(
        r"
func a[T](b: T) {
    return T
}
text(a(1))
",
        "num",
    );
}

#[test]
fn generic_func_repl_return_type_param() {
    use optive::{run_source_in_vm, vm::Vm};
    let mut vm = Vm::new();
    run_source_in_vm(
        &mut vm,
        "func a[T](b: T) { return T }",
        "<repl>",
    )
    .expect("define");
    let v = run_source_in_vm(&mut vm, "a(1)", "<repl>").expect("infer");
    assert_eq!(v.display_string(), "num");
    let v = run_source_in_vm(&mut vm, "a[num](1)", "<repl>").expect("explicit");
    assert_eq!(v.display_string(), "num");
}

#[test]
fn generic_func_repl_print_type_param() {
    use optive::{run_source_in_vm, vm::Vm};
    let mut vm = Vm::new();
    run_source_in_vm(&mut vm, "func a[T](b: T) { print(T) }", "<repl>").expect("define");
    let v = run_source_in_vm(&mut vm, "a(1)", "<repl>").expect("infer call");
    assert!(matches!(v, optive::value::Value::None));
    let v = run_source_in_vm(&mut vm, "a[num](1)", "<repl>").expect("explicit call");
    assert!(matches!(v, optive::value::Value::None));
}
