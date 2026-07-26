mod common;

use common::{assert_bool, assert_num, assert_text, run_err};

#[test]
fn struct_add_magic() {
    assert_num(
        r#"
struct Pair {
    let a
    let b
    func __add__(self, other) {
        return self.a + other.a + self.b + other.b
    }
}
Pair(1, 2) + Pair(3, 4)
"#,
        "10",
    );
}

#[test]
fn struct_radd_magic() {
    assert_num(
        r#"
struct Wrap {
    var v
    func __radd__(self, other) {
        return Wrap(other + self.v)
    }
}
(3 + Wrap(2)).v
"#,
        "5",
    );
}

#[test]
fn struct_sub_mul_div_magic() {
    assert_num(
        r#"
struct N {
    var v
    func __sub__(self, other) { return N(self.v - other.v) }
    func __mul__(self, other) { return N(self.v * other.v) }
    func __div__(self, other) { return N(self.v / other.v) }
}
let a = N(10)
let b = N(2)
(a - b).v * (a / b).v
"#,
        "40",
    );
}

#[test]
fn struct_pow_magic() {
    assert_num(
        r#"
struct N {
    var v
    func __pow__(self, other) { return N(self.v ** other.v) }
}
(N(2) ** N(10)).v
"#,
        "1024",
    );
}

#[test]
fn struct_rpow_magic() {
    assert_num(
        r#"
struct N {
    var v
    func __rpow__(self, other) { return N(other ** self.v) }
}
(2 ** N(8)).v
"#,
        "256",
    );
}

#[test]
fn num_pow_operator() {
    assert_num("2 ** 10", "1024");
    assert_num("2 ** 3 ** 2", "512");
    // 词法上行首 `-1` 是负数字面量，故 `-1 ** 2` ≡ `(-1) ** 2`
    assert_num("-1 ** 2", "1");
    assert_num("-(1 ** 2)", "-1");
    assert_num("2 ** -1", "1/2");
    assert_num("9 ** 0.5", "3");
}

#[test]
fn struct_neg_magic() {
    assert_num(
        r#"
struct N {
    var v
    func __neg__(self) { return N(-self.v) }
}
(-N(7)).v
"#,
        "-7",
    );
}

#[test]
fn struct_eq_magic() {
    assert_bool(
        r#"
struct N {
    var v
    func __eq__(self, other) { return self.v == other.v }
}
N(3) == N(3)
"#,
        true,
    );
}

#[test]
fn struct_ne_magic() {
    assert_bool(
        r#"
struct N {
    var v
    func __ne__(self, other) { return self.v != other.v }
}
N(1) != N(2)
"#,
        true,
    );
}

#[test]
fn struct_ne_fallback_via_eq() {
    assert_bool(
        r#"
struct N {
    var v
    func __eq__(self, other) { return self.v == other.v }
}
N(1) != N(2)
"#,
        true,
    );
}

#[test]
fn struct_lt_magic() {
    assert_bool(
        r#"
struct N {
    var v
    func __lt__(self, other) { return self.v < other.v }
}
N(1) < N(2)
"#,
        true,
    );
}

#[test]
fn struct_call_magic() {
    assert_num(
        r#"
struct Adder {
    var base
    func __call__(self, x) { return self.base + x }
}
Adder(10)(5)
"#,
        "15",
    );
}

#[test]
fn struct_repr_magic() {
    assert_text(
        r#"
struct N {
    var v
    func __repr__(self) { return "N(" + str(self.v) + ")" }
}
str(N(42))
"#,
        "N(42)",
    );
}

#[test]
fn struct_str_prefers_over_repr() {
    assert_text(
        r#"
struct N {
    var v
    func __str__(self) { return "s:" + str(self.v) }
    func __repr__(self) { return "r:" + str(self.v) }
}
str(N(1))
"#,
        "s:1",
    );
}

#[test]
fn struct_len_magic() {
    assert_num(
        r#"
struct Box {
    var data
    func __len__(self) { return len(self.data) }
}
len(Box([1, 2, 3]))
"#,
        "3",
    );
}

#[test]
fn struct_init_magic() {
    assert_num(
        r#"
struct Counter {
    var n
    func __init__(self, start) { self.n = start + 1 }
}
Counter(4).n
"#,
        "5",
    );
}

#[test]
fn builtin_arith_fallback_without_magic() {
    assert_num("1 + 2 * 3", "7");
}

#[test]
fn unsupported_struct_add_errors() {
    run_err(
        r#"
struct A { let x }
struct B { let y }
A(1) + B(2)
"#,
    );
}
