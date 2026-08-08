mod common;

use common::assert_num;

#[test]
fn modulo() {
    assert_num("10 % 3", "1");
}

#[test]
fn bitand() {
    assert_num("5 & 3", "1");
}

#[test]
fn bitor() {
    assert_num("5 | 2", "7");
}

#[test]
fn bitxor() {
    assert_num("5 ^ 1", "4");
}

#[test]
fn lshift() {
    assert_num("1 << 3", "8");
}

#[test]
fn rshift() {
    assert_num("16 >> 2", "4");
}

#[test]
fn invert() {
    assert_num("~0", "-1");
    assert_num("~5", "-6");
}

#[test]
fn struct_mod_magic() {
    assert_num(
        r#"
struct N {
    var v
    func __mod__(self, other) { return N(self.v % other.v) }
}
(N(10) % N(3)).v
"#,
        "1",
    );
}

#[test]
fn struct_rmod_magic() {
    assert_num(
        r#"
struct N {
    var v
    func __rmod__(self, other) { return N(other % self.v) }
}
(10 % N(3)).v
"#,
        "1",
    );
}

#[test]
fn struct_bitand_and_invert_magic() {
    assert_num(
        r#"
struct N {
    var v
    func __and__(self, other) { return N(self.v & other) }
    func __invert__(self) { return N(~self.v) }
}
(~(N(0) & 0)).v
"#,
        "-1",
    );
}

#[test]
fn precedence_mul_mod() {
    // * 与 % 同级、左结合：1 + ((2 * 3) % 4) = 1 + 2 = 3
    assert_num("1 + 2 * 3 % 4", "3");
}

#[test]
fn precedence_shift_vs_add() {
    // 移位低于加减：(1 + 2) << 3 = 24
    assert_num("1 + 2 << 3", "24");
}

#[test]
fn precedence_bitand_vs_bitor() {
    // & 高于 |：(5 & 3) | 2 = 1 | 2 = 3
    assert_num("5 & 3 | 2", "3");
}

#[test]
fn precedence_xor() {
    // ^ 介于 | 与 & 之间：5 | 1 ^ 3 & 1 → 5 | (1 ^ (3 & 1)) = 5 | (1 ^ 1) = 5
    assert_num("5 | 1 ^ 3 & 1", "5");
}

#[test]
fn chained_ops() {
    assert_num("(1 << 4) | (1 << 2) | 1", "21");
    assert_num("255 & 240 >> 4", "15");
}
