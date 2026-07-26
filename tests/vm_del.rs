mod common;

use common::{assert_num, run_err};

#[test]
fn del_list_index() {
    assert_num(
        r#"
let xs = [1, 2, 3]
del xs[1]
len(xs)
"#,
        "2",
    );
}

#[test]
fn del_list_index_value() {
    assert_num(
        r#"
let xs = [1, 2, 3]
del xs[1]
xs[1]
"#,
        "3",
    );
}

#[test]
fn del_dict_key() {
    assert_num(
        r#"
let d = {1: 10, 2: 20}
del d[1]
len(d)
"#,
        "1",
    );
}

#[test]
fn del_binding() {
    run_err(
        r#"
let tmp = 42
del tmp
tmp
"#,
    );
}

#[test]
fn del_missing_binding() {
    run_err("del no_such_name");
}

#[test]
fn del_const_binding() {
    run_err(
        r#"
const let x = 1
del x
"#,
    );
}

#[test]
fn del_list_negative_index() {
    assert_num(
        r#"
let xs = [1, 2, 3]
del xs[-1]
xs[1]
"#,
        "2",
    );
}

#[test]
fn del_dict_missing_key() {
    run_err(
        r#"
let d = {1: 2}
del d[9]
"#,
    );
}

#[test]
fn del_function_local() {
    assert_num(
        r#"
func f() {
    let a = 1
    del a
    return 2
}
f()
"#,
        "2",
    );
}

#[test]
fn struct_delitem() {
    assert_num(
        r#"
struct Bag {
    var data
    func __delitem__(self, k) {
        del self.data[k]
        return 0
    }
}
let b = Bag([10, 20, 30])
del b[1]
len(b.data)
"#,
        "2",
    );
}
