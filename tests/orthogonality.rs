#![allow(clippy::expect_used, clippy::panic)]

mod common;
use common::{assert_num, assert_text};

#[test]
fn pure_functions_share_tail_returns_and_argument_binding() {
    assert_num("const func a() { 42 }\nconst X = a()\nX", "42");
    assert_num(
        "const func a(x = 3, y = 4) { x * 10 + y }\nconst X = a(y = 8)\nX",
        "38",
    );
    assert_num("const func a(x, *rest, **kw) { x + len(rest) + kw[\"extra\"] }\nconst X = a(*(1, 2, 3), **{\"extra\": 4})\nX", "7");
    assert_num(
        "const func a(x) { if (x > 0) { 9 } else { 3 } }\nconst X = a(1)\nX",
        "9",
    );
    assert_num("const func a(x:: num) => num { x }\nconst X = a(3)\nX", "3");
    assert!(
        optive::run_source("const func a(x:: num) => num { x }\nconst X = a(\"bad\")\nX").is_err()
    );
    assert!(optive::run_source("const func a() => num { \"bad\" }\nconst X = a()\nX").is_err());
}

#[test]
fn special_method_contracts_are_strict_and_propagate_errors() {
    assert_text(
        "struct A { func __repr__(self) { return \"custom\" } }\nrepr(A())",
        "custom",
    );
    for source in [
        "struct A { func __repr__(self) { return 1 } }\nrepr(A())",
        "struct A { func __str__(self) { return 1 } }\nstr(A())",
        "struct A { func __bool__(self) { return 1 } }\nif (A()) {}",
        "struct A { func __len__(self) { return -1 } }\nlen(A())",
        "struct A { func __len__(self) { return 1.5 } }\nlen(A())",
    ] {
        assert!(optive::run_source(source).is_err(), "{source}");
    }
    assert_num("struct A { func __bool__(self) { throw ValueError(\"truth\") } }\nvar caught = 0\ntry { if (A()) { caught = 1 } } catch (e: ValueError) { caught = 2 }\ncaught", "2");
}

#[test]
fn unpack_is_a_real_method_and_custom_methods_override_it() {
    assert_num(
        "struct P { let x }\nlet p = P(3)\nlet { x = a } = p\np.__unpack__()[\"x\"] + a",
        "6",
    );
    assert_num("struct P { let x\nfunc __unpack__(self) { return {\"x\": self.x + 1} } }\nlet p = P(3)\nlet { x = a } = p\np.__unpack__()[\"x\"] + a", "8");
}

#[test]
fn handle_binds_before_or_and_defaults_survive_formatting() {
    assert_num("handle missing or 9", "9");
    let source = "let fallback = 9\nlet { retries = (fallback) } = {}\nretries";
    assert_num(source, "9");
    let formatted = optive::fmt::format_source(source).expect("format");
    assert_num(&formatted, "9");
    assert_num("let { retries = handle missing or 9 } = {}\nretries", "9");
}

#[test]
fn destructuring_works_in_loops_comprehensions_and_parameters() {
    assert_num(
        "var sum = 0\nfor ((a, b) in [(1, 2), (3, 4)]) { sum = sum + a + b }\nsum",
        "10",
    );
    assert_num(
        "let xs = [a + b for ((a, b) in [(1, 2), (3, 4)])]\nxs[1]",
        "7",
    );
    assert_num("func add((a, b)) { a + b }\nadd((2, 3))", "5");
    assert_num(
        "const func add((a, b)) { a + b }\nconst X = add((2, 3))\nX",
        "5",
    );
    assert_num("let xs = (a + b for ((a, b) in [(1, 2), (3, 4)]))\nuse std.iter.{ to_list }\nto_list(xs)[1]", "7");
    assert_num(
        "var sum = 0\nfor ({ x = a, y = b } in [{\"x\": 2, \"y\": 3}]) { sum = a + b }\nsum",
        "5",
    );
    assert_num(
        "func add({ x = a, y = b }) { a + b }\nadd({\"x\": 2, \"y\": 3})",
        "5",
    );
    assert_num(
        "var value = 0\nmatch ({\"x\": 7}) { case { x = a } { value = a } }\nvalue",
        "7",
    );
    assert_num(
        "var value = 0\nmatch ([1, 2, 3]) { case [head, *tail] { value = head + len(tail) } }\nvalue",
        "3",
    );
    assert_num(
        "func outer() { let fallback = 7\nreturn do({ x = (fallback) }) { x } }\nouter()({})",
        "7",
    );
    assert_num(
        "func sums() { let xs = [a + b for ((a, b) in [(1, 2), (3, 4)])]\nxs[1] }\nsums()",
        "7",
    );
    assert_num("struct Box { let data }\nmatch (Box([1, 2, 3])) { case Box([head, *tail]) { head + len(tail) } } else { 0 }", "3");
    assert_num(
        "match ({\"y\": 9}) { case { x = a } { a }\ncase { y = b } { b } } else { 0 }",
        "9",
    );
}

#[test]
fn outside_counts_functions_and_is_independent_of_control_flow() {
    assert_num("func outer() { if (false) { func helper() outside(1) { 42 } }\nreturn helper() }\nhelper() + outer()", "84");
    assert_num("const DEPTH = 1\nfunc outer() { if (false) { if (false) { func helper() outside(DEPTH) { 42 } } } }\nhelper()", "42");
    assert_num("func outer() { func middle() { if (false) { func helper() outside(2) { 42 } } } }\nhelper()", "42");
}

#[test]
fn protocol_strong_contracts_are_preserved_and_checked() {
    assert_num("protocol P { let x:: num\nfunc f(self, value:: num) => num {} }\nstruct Good { let x:: num\nfunc f(self, value:: num) => num { value } }\nfunc use_p(p:: P) { p.f(p.x) }\nuse_p(Good(7))", "7");
    assert!(optive::run_source("protocol P { let x:: num }\nstruct Bad { let x:: text }\nfunc f(p:: P) { 1 }\nf(Bad(\"x\"))").is_err());
    assert!(optive::run_source("protocol P { func f(self, value:: num) => num {} }\nstruct Bad { func f(self, value:: num) => text { \"x\" } }\nfunc use_p(p:: P) { 1 }\nuse_p(Bad())").is_err());
}
