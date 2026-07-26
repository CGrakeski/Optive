mod common;

use common::{assert_num, assert_text};

#[test]
fn closure_captures_outer_let() {
    assert_num(
        r#"
let base = 10
let add = do(x) { return x + base }
add(5)
"#,
        "15",
    );
}

#[test]
fn closure_capture_is_shared_mutable() {
    assert_num(
        r#"
let n = 1
let get = do() { return n }
n = 5
get()
"#,
        "5",
    );
}

#[test]
fn closure_mutates_outer_binding() {
    assert_num(
        r#"
let n = 1
let inc = do() { n = n + 1 }
inc()
n
"#,
        "2",
    );
}

#[test]
fn closure_captures_function_local() {
    assert_num(
        r#"
func outer() {
    let x = 1
    let inner = do() { return x }
    x = 2
    return inner()
}
outer()
"#,
        "2",
    );
}

#[test]
fn nested_func_captures_outer_local() {
    assert_num(
        r#"
func outer() {
    let x = 1
    func inner() {
        return x
    }
    x = 2
    return inner()
}
outer()
"#,
        "2",
    );
}

#[test]
fn decorator_on_func() {
    assert_num(
        r#"
func double(f) {
    return do(x) { return f(x) * 2 }
}
double func inc(x) { return x + 1 }
inc(20)
"#,
        "42",
    );
}

#[test]
fn with_context_manager() {
    assert_num(
        r#"
struct Ctx {
    var n
    func __enter__(self) {
        self.n = self.n + 1
        return self.n
    }
    func __exit__(self, exc_type, exc_val, exc_tb) {
        self.n = self.n - 1
        return false
    }
}
let b = Ctx(0)
with (b as v) {
    v
}
"#,
        "1",
    );
}

#[test]
fn friend_func_dispatch_still_works() {
    assert_text(
        r#"
friend func add(x:: num) { return text(x + 1) }
add.__dispatch__.append(do(x:: text) { return x + "!" })
add(41)
"#,
        "42",
    );
}

#[test]
fn friend_func_best_match_subtype() {
    assert_num(
        r#"
struct Base { var n: num }
struct Sub : Base { var n: num }

friend func f(x:: Base) { return 1 }
f.__dispatch__.append(do(x:: Sub) { return 2 })

let s = Sub(42)
f(s)
"#,
        "2",
    );
}

#[test]
fn friend_func_best_match_base_type() {
    assert_num(
        r#"
struct Base { var n: num }
struct Sub : Base { var n: num }

friend func f(x:: Base) { return 1 }
f.__dispatch__.append(do(x:: Sub) { return 2 })

let b = Base(42)
f(b)
"#,
        "1",
    );
}
