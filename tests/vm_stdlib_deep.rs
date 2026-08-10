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

#[test]
fn std_math_extra() {
    assert_num(
        r"
use std.math.{ floor, ceil, clamp, pow }
floor(3.7)
",
        "3",
    );
    assert_num(
        r"
use std.math.{ ceil }
ceil(1.2)
",
        "2",
    );
    assert_num(
        r"
use std.math.{ clamp }
clamp(5, 0, 3)
",
        "3",
    );
    assert_num(
        r"
use std.math.{ pow }
pow(2, 3)
",
        "8",
    );
}

#[test]
fn std_text_extra() {
    assert_text(
        r#"
use std.text.{ replace, startswith, join, repeat }
replace("ababa", "a", "x") + (if startswith("hello", "he") then "Y" else "N") + join("-", ["a", "b"]) + repeat("z", 3)
"#,
        "xbxbxYa-bzzz",
    );
}

#[test]
fn std_json_object() {
    assert_num(
        r#"
use std.json.{ parse, stringify }
let d = parse("{\"a\": 1, \"b\": [2, 3]}")
d["a"] + d["b"][0]
"#,
        "3",
    );
}

#[test]
fn std_iter_take_skip() {
    assert_text(
        r"
use std.iter.{ take, skip, to_list }
str(take([1, 2, 3, 4, 5], 2)) + str(skip([1, 2, 3, 4], 2))
",
        "[1, 2][3, 4]",
    );
}

#[test]
fn std_collections_flatten_chunk() {
    assert_text(
        r"
use std.collections.{ flatten, chunk }
str(flatten([[1, 2], [3]])) + str(chunk([1, 2, 3, 4, 5], 2))
",
        "[1, 2, 3][[1, 2], [3, 4], [5]]",
    );
}

#[test]
fn std_path_extension() {
    assert_text(
        r#"
use std.path.{ extension, stem, splitext }
extension("a/b.c") + stem("a/b.c") + str(splitext("a/b.c"))
"#,
        "cb[\"a/b\", \"c\"]",
    );
}

#[test]
fn std_path_abspath_join_roundtrip() {
    // B13：abspath 不应带 `\\?\`；join 后 is_dir/exists 在 Windows 上可用。
    assert_bool(
        r#"
use std.path.{ abspath, join, is_absolute }
use std.fs.{ is_dir, exists }
use std.text.{ startswith }
let r = abspath("tests")
let full = join(r, "import_fixtures")
is_absolute(r) and is_dir(full) and exists(full) and not startswith(r, "\\\\?\\")
"#,
        true,
    );
}

#[test]
fn std_os_name() {
    assert_bool(
        r"
use std.os.{ name }
len(name()) > 0
",
        true,
    );
}

#[test]
fn std_fs_roundtrip() {
    assert_text(
        r#"
use std.fs.{ write_text, read_text, exists, remove }
let p = "__ol_test_tmp_fs__.txt"
write_text(p, "hello-fs")
let ok = exists(p)
let t = read_text(p)
remove(p)
(if ok then t else "missing")
"#,
        "hello-fs",
    );
}

#[test]
fn std_dict_from_items() {
    assert_num(
        r#"
use std.dict.{ from_items, get }
get(from_items([["a", 1], ["b", 2]]), "b")
"#,
        "2",
    );
}

#[test]
fn std_time_now_ms() {
    assert_bool(
        r"
use std.time.{ now_ms }
now_ms() > 0
",
        true,
    );
}

#[test]
fn std_math_gcd_mod_sign() {
    assert_num(
        r"
use std.math.{ gcd, lcm, mod, sign }
gcd(12, 18) + lcm(4, 6) + mod(10, 3) + sign(-5)
",
        "18",
    );
}

#[test]
fn std_iter_cycle_take_fold() {
    assert_text(
        r#"
use std.iter.{ cycle, take, fold, repeat, to_list }
str(take(cycle([1, 2]), 5)) + str(fold(do(a, b) { return a + b }, 0, [1, 2, 3])) + str(to_list(repeat("x", 3)))
"#,
        "[1, 2, 1, 2, 1]6[\"x\", \"x\", \"x\"]",
    );
}

#[test]
fn std_dict_merge_invert() {
    assert_num(
        r#"
use std.dict.{ merge, invert, get, from_list }
let d = merge({"a": 1}, {"b": 2}, {"a": 9})
get(d, "a") + len(invert({"x": 1}))
"#,
        "10",
    );
    assert_num(
        r#"
use std.dict.{ from_list, get }
get(from_list([["k", 7]]), "k")
"#,
        "7",
    );
}

#[test]
fn std_random_seed_repro() {
    assert_bool(
        r"
use std.random.{ seed, randint }
seed(42)
let a = randint(1, 1000)
seed(42)
let b = randint(1, 1000)
a == b
",
        true,
    );
}

#[test]
fn std_text_ord_chr() {
    assert_text(
        r#"
use std.text.{ ord, chr }
chr(ord("A")) + str(ord("A"))
"#,
        "A65",
    );
}

#[test]
fn std_json_dump_parse_file() {
    assert_num(
        r#"
use std.json.{ dump, parse_file }
use std.fs.{ remove }
let p = "__ol_test_tmp_json__.json"
dump(p, {"n": 3})
let d = parse_file(p)
remove(p)
d["n"]
"#,
        "3",
    );
}

#[test]
fn builtin_copy_id_deepcopy() {
    assert_bool(
        r"
let a = [1, [2]]
let b = copy(a)
let c = deepcopy(a)
b[1][0] = 9
c[1][0] = 8
(a[1][0] == 9) and (c[1][0] == 8) and (id(a) != id(b))
",
        true,
    );
}

#[test]
fn std_ast_unparse() {
    assert_text(
        r#"
use std.ast.{ parse, unparse }
unparse(parse("1 + 2"))
"#,
        "(1 + 2)",
    );
}

#[test]
fn std_format_pad_indent() {
    assert_text(
        r#"
use std.format.{ pad, indent, format_num }
pad("hi", 5, ".") + indent("a\nb", 2) + format_num(314159 / 100000, 2)
"#,
        "...hi  a\n  b3.14",
    );
}
