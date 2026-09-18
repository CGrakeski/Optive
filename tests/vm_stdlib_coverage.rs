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
use optive::run_source;

fn assert_error_contains(source: &str, needle: &str) {
    let error = run_source(source).expect_err("expected runtime error");
    let message = error.to_string();
    assert!(
        message.contains(needle),
        "expected error containing {needle:?}, got {message:?} for {source:?}"
    );
}

#[test]
fn productivity_modules_are_usable() {
    assert_text(
        r#"
let opts = std.cli.parse(["--dry-run", "--name=Ada", "-vq", "file.tive"])
str(opts["dry_run"]) + "|" + opts["name"] + "|" + str(opts["v"]) + "|" + opts["_"][0]
"#,
        "true|Ada|true|file.tive",
    );
    assert_text(
        r#"std.uri.query_decode(std.uri.query_encode("雪 & sun"))"#,
        "雪 & sun",
    );
    assert_num(r#"std.console.width("A雪")"#, "3");
    assert_bool(r#"std.uuid.is_valid(std.uuid.v4())"#, true);
    assert_bool(
        r#"len(std.secrets.hex(16)) == 32 and len(std.secrets.bytes(7)) == 7"#,
        true,
    );
}

#[test]
fn collections_and_test_helpers_cover_common_workflows() {
    assert_text(
        r#"
let counts = std.collections.counter(["a", "b", "a"])
let (even, odd) = std.collections.partition([1, 2, 3, 4], do(x) { x % 2 == 0 })
let windows = std.collections.window([1, 2, 3, 4], 3)
std.test.assert_false(false)
std.test.assert_ne(1, 2)
std.test.assert_contains(even, 4)
std.test.assert_approx(1 / 3, 0.333, 0.001)
str(counts["a"]) + "|" + str(even) + "|" + str(odd) + "|" + str(windows)
"#,
        "2|[2, 4]|[1, 3]|[[1, 2, 3], [2, 3, 4]]",
    );
}

#[test]
fn time_duration_arithmetic() {
    assert_text(
        r#"
let d = std.time.duration(90061)
str(d["days"]) + ":" + str(d["hours"]) + ":" + str(d["minutes"]) + ":" + str(d["seconds"]) + "|" + str(std.time.diff(std.time.add(100, 25), 100))
"#,
        "1:1:1:1|25",
    );
}

#[test]
fn archive_gzip_roundtrip_and_output_limit() {
    assert_text(
        r#"std.text.from_bytes(std.archive.gunzip(std.archive.gzip("雪-data", 9)))"#,
        "雪-data",
    );
    assert_error_contains(
        r#"std.archive.gunzip(std.archive.gzip("123456"), 5)"#,
        "exceeds 5 bytes",
    );
}

#[test]
fn archive_zip_and_tar_roundtrip() {
    assert_text(
        r#"
let files = {"docs/readme.txt": "hello", "src/main.tive": "print(1)"}
let z = std.archive.zip(files)
let t = std.archive.tar(files)
std.text.from_bytes(std.archive.zip_read(z, "docs/readme.txt")) + "|" + str(std.archive.zip_list(z)) + "|" + std.text.from_bytes(std.archive.tar_read(t, "src/main.tive")) + "|" + str(std.archive.tar_list(t))
"#,
        "hello|[\"docs/readme.txt\", \"src/main.tive\"]|print(1)|[\"docs/readme.txt\", \"src/main.tive\"]",
    );
    assert_error_contains(r#"std.archive.zip({"../escape": "bad"})"#, "unsafe archive");
    assert_error_contains(r#"std.archive.tar({"C:/escape": "bad"})"#, "unsafe archive");
}

#[test]
fn iter_predicates_search_and_count_cover_both_outcomes() {
    assert_num(
        r"
use std.iter.{ count, find }
count([10, 20, 30]) + find([1, 3, 4], do(x) { x % 2 == 0 })
",
        "7",
    );
    assert_bool(
        r"
use std.iter.{ any, all }
any([1, 2, 3], do(x) { x > 2 }) and all([1, 2, 3], do(x) { x > 0 })
",
        true,
    );
    assert_bool(
        r"
use std.iter.{ any, all }
(not any([1, 2], do(x) { x > 9 })) and (not all([1, 0], do(x) { x > 0 }))
",
        true,
    );
    assert_bool(
        r"
use std.iter.{ find }
find([1, 3], do(x) { x % 2 == 0 }) == none
",
        true,
    );
}

#[test]
fn iter_materializers_filter_zip_and_next_default() {
    assert_text(
        r#"
use std.iter.{ filter, zip, to_list, to_set, iter, next }
let evens = to_list(filter(do(x) { x % 2 == 0 }, [1, 2, 3, 4]))
let pairs = to_list(zip([1, 2], ["a", "b", "c"]))
let unique = to_set([1, 1, 2])
let cursor = iter([])
str(evens) + "|" + str(pairs) + "|" + str(len(unique)) + "|" + str(next(cursor, 99))
"#,
        "[2, 4]|[[1, \"a\"], [2, \"b\"]]|2|99",
    );
}

#[test]
fn iter_argument_errors_are_reported_at_the_api_boundary() {
    for (source, needle) in [
        ("std.iter.chain()", "chain requires at least 1 argument"),
        ("std.iter.zip([1])", "zip requires at least 2 arguments"),
        ("std.iter.take([1])", "take requires 2 arguments"),
        ("std.iter.next()", "next requires an iterator"),
        ("std.iter.repeat()", "repeat requires 1 or 2 arguments"),
    ] {
        assert_error_contains(source, needle);
    }
}

#[test]
fn text_partition_padding_case_and_affix_families() {
    assert_text(
        r#"
use std.text.{ rsplit, split_once, rsplit_once, partition, rpartition }
str(rsplit("a-b-c", "-", 1)) + "|" + str(split_once("a:b", ":")) + "|" + str(rsplit_once("a:b:c", ":")) + "|" + str(partition("abc", ":")) + "|" + str(rpartition("abc", ":"))
"#,
        "[\"a-b\", \"c\"]|(\"a\", \"b\")|(\"a:b\", \"c\")|(\"abc\", \"\", \"\")|(\"\", \"\", \"abc\")",
    );
    assert_text(
        r#"
use std.text.{ ljust, rjust, center, zfill, title, swapcase, removeprefix, removesuffix }
ljust("x", 3, ".") + "|" + rjust("x", 3, ".") + "|" + center("x", 4, ".") + "|" + zfill("-7", 4) + "|" + title("hELLO world") + "|" + swapcase("Ab-1") + "|" + removeprefix("foobar", "foo") + "|" + removesuffix("foobar", "bar")
"#,
        "x..|..x|.x..|-007|Hello World|aB-1|bar|foo",
    );
}

#[test]
fn text_unicode_conversion_search_and_validation() {
    assert_text(
        r#"
use std.text.{ chars, codepoints, from_chars, from_codepoints, byte_len, rfind, replace_n, cmp }
from_chars(chars("A好")) + "|" + from_codepoints(codepoints("é")) + "|" + str(byte_len("é")) + "|" + str(rfind("a好a好", "好")) + "|" + replace_n("aaaa", "a", "x", 2) + "|" + str(cmp("a", "b"))
"#,
        "A好|é|2|3|xxaa|-1",
    );
    assert_error_contains("std.text.split(\"abc\", \"\")", "empty separator");
    assert_error_contains("std.text.ljust(\"x\", 3, \"\")", "fill must be non-empty");
    assert_error_contains("std.text.from_chars([\"ab\"])", "single character");
    assert_error_contains("std.text.from_codepoints([1114112])", "out of range");
    assert_error_contains(
        "std.text.substring(\"abc\", 9)",
        "string index out of range",
    );
}

#[test]
fn format_fields_escaping_defaults_and_errors() {
    assert_text(r#"std.format.format("{{{1}}} {}", "x", 42)"#, "{42} x");
    assert_text("std.format.pad(\"already\", 3)", "already");
    assert_text("std.format.indent(\"a\\n\\nb\\n\", 2)", "  a\n\n  b\n");
    assert_error_contains("std.format.format(\"{\")", "unmatched '{'");
    assert_error_contains("std.format.format(\"}\")", "unmatched '}'");
    assert_error_contains("std.format.format(\"{name}\", 1)", "invalid field");
    assert_error_contains("std.format.format(\"{2}\", 1)", "missing argument");
    assert_error_contains("std.format.pad(\"x\")", "pad requires 2 or 3 arguments");
}
