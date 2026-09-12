#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{assert_num, assert_text};

#[test]
fn defer_runs_lifo_on_return_exception_break_and_continue() {
    assert_text(
        r#"
var log = []
func finish() {
    defer { log.append(1) }
    defer { log.append(2) }
    return 9
}
let result = finish()
try {
    defer { log.append(3) }
    throw ValueError("boom")
} catch (...) {
    log.append(4)
}
for (i in [1, 2]) {
    defer { log.append(i + 4) }
    if (i == 1) { continue }
    break
}
str(log) + ":" + str(result)
"#,
        "[2, 1, 3, 4, 5, 6]:9",
    );
}

#[test]
fn defer_is_registered_only_after_control_reaches_it() {
    assert_num(
        r#"
var n = 0
func stop_early() {
    return 1
    defer { n = 99 }
}
stop_early()
n
"#,
        "0",
    );
}

#[test]
fn standard_variants_construct_and_match() {
    assert_num(
        r#"
use std.variants.{ Result, Option, Either }
let result = Result.Ok(40)
let option = Option.Some(1)
let absent = Option.None()
let side = Either.Right(1)
var total = 0
match (result) { case Result.Ok(v) { total = total + v } else {} }
match (option) { case Option.Some(v) { total = total + v } else {} }
match (absent) { case Option.None() { total = total + 1 } else {} }
match (side) { case Either.Right(v) { total = total + v } else {} }
total
"#,
        "43",
    );
}

#[test]
fn question_unwraps_success_and_propagates_failure_through_defer() {
    assert_text(
        r#"
use std.variants.{ Result, Option }
var log = []
func result(ok) -> Result {
    defer { log.append("result") }
    var value = Result.Err("bad")
    if (ok) { value = Result.Ok(40) }
    let n = value?
    return Result.Ok(n + 2)
}
func option(ok) -> Option {
    defer { log.append("option") }
    var value = Option.None()
    if (ok) { value = Option.Some(7) }
    let n = value?
    return Option.Some(n + 1)
}
let a = result(true)
let b = result(false)
let c = option(true)
let d = option(false)
var out = ""
match (a) { case Result.Ok(v) { out = out + str(v) } else {} }
match (b) { case Result.Err(v) { out = out + ":" + v } else {} }
match (c) { case Option.Some(v) { out = out + ":" + str(v) } else {} }
match (d) { case Option.None() { out = out + ":none" } else {} }
out + ":" + str(log)
"#,
        "42:bad:8:none:[\"result\", \"result\", \"option\", \"option\"]",
    );
}
