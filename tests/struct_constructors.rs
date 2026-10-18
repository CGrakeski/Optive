#![allow(clippy::expect_used, clippy::panic)]

mod common;
use common::{assert_num, assert_text};

#[test]
fn overloaded_init_binds_keywords_before_type_selection() {
    assert_num(
        r#"
struct A {
    let value
    func __init__(self, x: num, offset = 2) overload { self.value = x + offset }
    func __init__(self, x: text, offset = 2) overload { self.value = len(x) + offset }
}
A(offset = 2, x = 40).value + A(x = "abc").value
"#,
        "47",
    );
}

#[test]
fn overloaded_init_expansions_and_binding_errors() {
    assert_num(
        r#"
struct A {
    let value
    func __init__(self, x: num) overload { self.value = x }
    func __init__(self, x: text) overload { self.value = len(x) }
}
A(**{"x": 42}).value
"#,
        "42",
    );
    let prefix = "struct A { let value\nfunc __init__(self, x: num) overload { self.value = x }\nfunc __init__(self, x: text) overload { self.value = len(x) } }\n";
    for bad in ["A(1, x = 2)", "A(bad = 2)", "A()", "A(x = true)"] {
        assert!(
            optive::run_source(&format!("{prefix}{bad}")).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn named_method_and_callable_instance_use_same_binder() {
    assert_num(
        r#"
struct A {
    func work(self, x: num, offset = 2) overload { return x + offset }
    func work(self, x: text, offset = 2) overload { return len(x) + offset }
    func __call__(self, x = 40, y = 2) { return x + y }
}
let a = A()
a.work(offset = 2, x = 40) + a.work(x = "abc") + a(y = 2)
"#,
        "89",
    );
}

#[test]
fn cycle_uses_init_signature_and_field_default() {
    assert_text(
        r#"
struct Cycle {
    var txt: text
    var current: int = 0
    func __init__(self, txt: text) { self.txt = txt }
    func __next__(self) -> text {
        let result = self.txt[self.current]
        self.current = (self.current + 1) % len(self.txt)
        return result
    }
}
let c = Cycle("/|\\-")
c.__next__() + c.__next__()
"#,
        "/|",
    );
}

#[test]
fn init_arguments_are_not_preassigned_or_limited_by_field_count() {
    assert_num(
        r#"
struct A {
    var x
    var y = 8
    func __init__(self, value) { self.x = value * 2 }
}
A(3).x + A(3).y
"#,
        "14",
    );
    assert_num(
        r#"
var calls = 0
struct A {
    func __init__(self, x, y, z) { calls = x + y + z }
}
A(1, 2, 3)
calls
"#,
        "6",
    );
}

#[test]
fn init_supports_defaults_named_arguments_and_variadics() {
    assert_num(
        r#"
struct A {
    var x
    func __init__(self, value = 7) { self.x = value }
}
A().x + A(value = 3).x
"#,
        "10",
    );
    assert_num(
        r#"
struct A {
    var x
    func __init__(self, *values, **options) {
        self.x = len(values) + options["extra"]
    }
}
A(1, 2, extra = 5).x
"#,
        "7",
    );
}

#[test]
fn field_constructor_fills_defaults_and_supports_keywords() {
    assert_num(
        r#"
struct Point { let x let y = 4 }
Point(3).x + Point(3).y + Point(y = 2, x = 1).y
"#,
        "9",
    );
    assert_num(
        "struct A { let x = 3 let y }\nA(y = 8).x + A(y = 8).y",
        "11",
    );
    assert!(optive::run_source("struct A { let x let y = 4 }\nA()").is_err());
    assert!(optive::run_source("struct A { let x }\nA(1, x = 2)").is_err());
    assert!(optive::run_source("struct A { let x = 0 }\nA(wrong = 2)").is_err());
}

#[test]
fn defaults_are_lazy_fresh_and_evaluated_in_field_order() {
    assert_num(
        r#"
var calls = 0
func fresh() { calls = calls + 1
return calls }
struct A { var x = fresh() }
A(99)
let a = A()
let b = A()
a.x * 10 + b.x + calls
"#,
        "14",
    );
    assert_num(
        r#"
struct A { var items = [0] }
let a = A()
let b = A()
a.items[0] = 9
b.items[0]
"#,
        "0",
    );
    assert_num("struct A { let x = 2 let y = self.x + 3 }\nA().y", "5");
    assert_num("var base = 7\nstruct A { let x = base }\nA().x", "7");
}

#[test]
fn immutable_fields_can_be_initialized_once_but_remain_immutable() {
    assert_num(
        "struct A { let x\nfunc __init__(self, value) { self.x = value } }\nA(7).x",
        "7",
    );
    assert!(optive::run_source(
        "struct A { let x\nfunc __init__(self, value) { self.x = value } }\nlet a = A(7)\na.x = 8"
    )
    .is_err());
    assert!(optive::run_source(
        "struct A { let x\nfunc __init__(self) { self.x = 1\nself.x = 2 } }\nA()"
    )
    .is_err());
}

#[test]
fn defaults_and_finished_instances_obey_strong_field_contracts() {
    assert_num(
        "struct A { let x:: num\nfunc __init__(self, value:: num) { self.x = value } }\nA(7).x",
        "7",
    );
    assert_num("struct A { let x:: num = 8 }\nA().x", "8");
    assert!(optive::run_source("struct A { let x:: num = \"wrong\" }\nA()").is_err());
    assert!(optive::run_source(
        "struct A { var x:: num\nfunc __init__(self) { self.x = \"wrong\" } }\nA()"
    )
    .is_err());
    assert!(optive::run_source("struct A { var x\nfunc __init__(self) {} }\nA()").is_err());
}

#[test]
fn default_and_init_exceptions_propagate_without_poisoning_later_constructions() {
    assert_num(
        r#"
func fail() { throw ValueError("default") }
struct Bad { let x = fail() }
struct Good { let x = 9 }
var caught = 0
try { Bad() } catch (e: ValueError) { caught = 1 }
caught + Good().x
"#,
        "10",
    );
    assert_num(
        r#"
struct Bad { var x
func __init__(self) { throw ValueError("init") } }
struct Good { let x = 9 }
var caught = 0
try { Bad() } catch (e: ValueError) { caught = 1 }
caught + Good().x
"#,
        "10",
    );
}

#[test]
fn constructor_initialization_privilege_is_revoked_after_failure() {
    assert_num(
        r#"
var leaked = none
struct A {
    let x
    func __init__(self) {
        leaked = self
        throw ValueError("fail")
    }
}
try { A() } catch (e: ValueError) {}
var rejected = 0
try { leaked.x = 3 } catch (e: AttributeError) { rejected = 1 }
rejected
"#,
        "1",
    );
}

#[test]
fn generic_init_inference_follows_parameter_types_not_field_positions() {
    assert_num(
        r#"
struct Box[T] {
    let value:: T
    func __init__(self, prefix: text, value:: T) { self.value = value }
}
Box("not a field", 7).value + Box[num]("explicit", 8).value
"#,
        "15",
    );
}

#[test]
fn construction_also_works_in_a_background_task() {
    assert_num(
        r#"
struct A { var x = 0
func __init__(self, value) { self.x = value } }
func build_one() { A(7).x }
let result = go build_one()
await result
"#,
        "7",
    );
}

#[test]
fn imported_defaults_and_init_use_the_defining_module_environment() {
    let path = format!(
        "{}/tests/fixtures/constructor_defaults_module.tive",
        env!("CARGO_MANIFEST_DIR")
    )
    .replace('\\', "/");
    let source = format!("import \"{path}\" as maker\nvar seed = 100\nmaker.Default().number + maker.Counter(\"ignored\").value");
    assert_num(&source, "15");
}

#[test]
fn checker_and_signature_help_follow_the_constructor_signature() {
    let source = "struct Cycle {\nvar txt\nvar current = 0\nfunc __init__(self, txt: text) { self.txt = txt }\n}\nCycle(\"abc\")";
    let diagnostics = optive::lsp::diagnostics(source, "<constructor-check>");
    assert!(
        !diagnostics
            .iter()
            .any(|(_, _, message)| message.contains("missing required argument")),
        "{diagnostics:?}"
    );
    let call_source = format!("{source}\nCycle(");
    let signature = optive::lsp::signature_help(&call_source, 6, 6);
    assert_eq!(signature["signatures"][0]["label"], "Cycle(txt: text)");
    let source = "struct A { let x = 2 let y = self.x + 3 }\nA()";
    let diagnostics = optive::lsp::diagnostics(source, "<constructor-check>");
    assert!(
        !diagnostics
            .iter()
            .any(|(_, _, message)| message.contains("undefined name")),
        "{diagnostics:?}"
    );
}
