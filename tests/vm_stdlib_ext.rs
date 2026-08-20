#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! 新增/扩充标准库冒烟测试：encoding / csv / toml / yaml / xml / math / typing / % 格式化。
mod common;

use common::{assert_num, assert_text, bool_val, value};
use optive::value::Value;

#[test]
fn percent_format_positional() {
    assert_text(r#""%d + %d = %d" % [2, 3, 5]"#, "2 + 3 = 5");
}

#[test]
fn percent_format_named() {
    assert_text(
        r#""%(name)s=%(val)d" % {"name": "x", "val": 42}"#,
        "x=42",
    );
}

#[test]
fn encoding_base64_roundtrip() {
    let enc = value(r#"std.encoding.base64_encode("hi")"#);
    match enc {
        Value::Text(s) => assert_eq!(s, "aGk="),
        other => panic!("expected text, got {}", other.display_string()),
    }
    let dec = value(r#"std.encoding.base64_decode("aGk=")"#);
    match dec {
        Value::Bytes(b) => assert_eq!(b.as_slice(), b"hi"),
        other => panic!("expected bytes, got {}", other.display_string()),
    }
}

#[test]
fn encoding_hex_roundtrip() {
    let enc = value(r#"std.encoding.hex_encode("AB")"#);
    match enc {
        Value::Text(s) => assert_eq!(s, "4142"),
        other => panic!("{}", other.display_string()),
    }
}

#[test]
fn encoding_url() {
    assert_text(r#"std.encoding.url_encode("a b")"#, "a%20b");
    assert_text(r#"std.encoding.url_decode("a%20b")"#, "a b");
}

#[test]
fn encoding_gzip_roundtrip() {
    let v = value(
        r#"
let z = std.encoding.gzip_encode("hello")
let out = std.encoding.gzip_decode(z)
out
"#,
    );
    match v {
        Value::Bytes(b) => assert_eq!(b.as_slice(), b"hello"),
        other => panic!("{}", other.display_string()),
    }
}

#[test]
fn time_format_parse_roundtrip() {
    assert_text(
        r#"
let s = std.time.format(0, "%Y-%m-%d %H:%M:%S")
s
"#,
        "1970-01-01 00:00:00",
    );
    assert_num(
        r#"
std.time.parse("1970-01-01 00:00:00", "%Y-%m-%d %H:%M:%S")
"#,
        "0",
    );
}

#[test]
fn os_capture_echo() {
    // Windows: cmd /C echo；Unix: echo。
    let src = if cfg!(windows) {
        r#"
let r = std.os.capture(["cmd", "/C", "echo", "hi"])
r["ok"] and std.text.contains(r["stdout"], "hi")
"#
    } else {
        r#"
let r = std.os.capture(["echo", "hi"])
r["ok"] and std.text.contains(r["stdout"], "hi")
"#
    };
    assert!(bool_val(src), "{src}");
}

#[test]
fn csv_stringify_rows() {
    assert_text(
        r#"
std.csv.stringify([["a", "b"], ["1", "2"]])
"#,
        "a,b\n1,2\n",
    );
}

#[test]
fn csv_parse_with_header() {
    let v = value(
        r#"
std.csv.parse("a,b\n1,2\n3,4")
"#,
    );
    match v {
        Value::List(rows) => {
            let rows = rows.borrow();
            assert_eq!(rows.len(), 2);
            match &rows[0] {
                Value::Dict(d) => {
                    assert_eq!(
                        d.borrow()
                            .get(&optive::value::ValueKey::Text("a".into()))
                            .map(optive::value::Value::print_string),
                        Some("1".into())
                    );
                }
                other => panic!("{}", other.display_string()),
            }
        }
        other => panic!("{}", other.display_string()),
    }
}

#[test]
fn toml_parse() {
    let v = value(r#"std.toml.parse("x = 1\ny = \"hi\"")"#);
    match v {
        Value::Dict(d) => {
            let d = d.borrow();
            assert!(d.get(&optive::value::ValueKey::Text("x".into())).is_some());
        }
        other => panic!("{}", other.display_string()),
    }
}

#[test]
fn yaml_parse() {
    let v = value("std.yaml.parse(\"x: 1\")");
    match v {
        Value::Dict(d) => {
            assert!(d
                .borrow()
                .get(&optive::value::ValueKey::Text("x".into()))
                .is_some());
        }
        other => panic!("{}", other.display_string()),
    }
}

#[test]
fn xml_parse() {
    let v = value(r#"std.xml.parse("<root a=\"1\"><child>hi</child></root>")"#);
    match v {
        Value::Dict(d) => {
            let tag = d
                .borrow()
                .get(&optive::value::ValueKey::Text("tag".into()))
                .cloned();
            match tag {
                Some(Value::Text(s)) => assert_eq!(s, "root"),
                other => panic!("{other:?}"),
            }
        }
        other => panic!("{}", other.display_string()),
    }
}

#[test]
fn toml_yaml_xml_stringify_roundtrip() {
    let v = value(r#"std.toml.parse(std.toml.stringify({"x": 1, "y": "hi"}))"#);
    match v {
        Value::Dict(d) => {
            let d = d.borrow();
            match d.get(&optive::value::ValueKey::Text("x".into())) {
                Some(Value::Num(n)) => assert_eq!(n.to_i64(), Some(1)),
                other => panic!("expected x=1, got {other:?}"),
            }
            match d.get(&optive::value::ValueKey::Text("y".into())) {
                Some(Value::Text(s)) => assert_eq!(s, "hi"),
                other => panic!("expected y=\"hi\", got {other:?}"),
            }
        }
        other => panic!("{}", other.display_string()),
    }
    let v = value(r#"std.yaml.parse(std.yaml.stringify({"x": 1}))"#);
    match v {
        Value::Dict(d) => {
            match d.borrow().get(&optive::value::ValueKey::Text("x".into())) {
                Some(Value::Num(n)) => assert_eq!(n.to_i64(), Some(1)),
                other => panic!("expected x=1, got {other:?}"),
            }
        }
        other => panic!("{}", other.display_string()),
    }
    let xml = value(
        r#"std.xml.stringify({"tag": "root", "attrs": {"a": "1"}, "text": "hi", "children": []})"#,
    );
    match xml {
        Value::Text(s) => {
            assert!(s.contains("<root"), "{s}");
            assert!(s.contains("a=\"1\""), "{s}");
            assert!(s.contains("hi"), "{s}");
        }
        other => panic!("{}", other.display_string()),
    }
}

#[test]
fn xml_stringify_rejects_invalid_qname() {
    for src in [
        r#"std.xml.stringify({"tag": ":root", "attrs": {}, "text": "", "children": []})"#,
        r#"std.xml.stringify({"tag": "xml:", "attrs": {}, "text": "", "children": []})"#,
        r#"std.xml.stringify({"tag": "a:b:c", "attrs": {}, "text": "", "children": []})"#,
        r#"std.xml.stringify({"tag": "xml:1foo", "attrs": {}, "text": "", "children": []})"#,
    ] {
        assert!(
            optive::run_source(src).is_err(),
            "expected invalid tag error for: {src}"
        );
    }
    let ok = value(
        r#"std.xml.stringify({"tag": "xml:root", "attrs": {}, "text": "", "children": []})"#,
    );
    match ok {
        Value::Text(s) => assert!(s.contains("<xml:root"), "{s}"),
        other => panic!("{}", other.display_string()),
    }
}

#[test]
fn math_atan2_and_divmod() {
    let v = value("std.math.divmod(7, 3)");
    match v {
        Value::List(l) => {
            let l = l.borrow();
            assert_eq!(l.len(), 2);
        }
        other => panic!("{}", other.display_string()),
    }
    let _ = value("std.math.atan2(1, 1)");
    let _ = value("std.math.asin(0)");
    let _ = value("std.math.hypot(3, 4)");
}

#[test]
fn typing_isinstanceof_and_optional() {
    assert_eq!(
        value("isinstanceof(1, num)").display_string(),
        "true"
    );
    assert_eq!(
        value("std.typing.isinstanceof(none, std.typing.Optional(num))").display_string(),
        "true"
    );
}

#[test]
fn typing_tuple_and_callable() {
    assert_eq!(
        value("isinstanceof((1, \"a\"), std.typing.Tuple(num, text))").display_string(),
        "true"
    );
    assert_eq!(
        value("isinstanceof(print, std.typing.Callable())").display_string(),
        "true"
    );
}

#[test]
fn sync_module_exports() {
    let _ = value("std.sync.Channel");
    let _ = value("std.sync.RWMutex");
    let _ = value("std.sync.RwLock");
    let _ = value("std.sync.WaitGroup");
    let _ = value("std.sync.Semaphore");
    let _ = value("std.sync.Once");
    let _ = value("std.sync.Barrier");
    let _ = value("std.sync.Cond");
    let _ = value("std.sync.Atomic.num(0)");
}

#[test]
fn numeric_mod_still_works() {
    assert_num("10 % 3", "1");
}
