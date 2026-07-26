//! C FFI / sized types / extern / implicit.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use common::{assert_num, assert_text, value};
use optive::run_source;
use optive::value::Value;

fn build_adding_dll() -> PathBuf {
    static DLL: OnceLock<PathBuf> = OnceLock::new();
    DLL.get_or_init(|| {
        let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ffi_test_dll");
        fs::create_dir_all(&out_dir).expect("create dll dir");
        let src = out_dir.join("adding.rs");
        fs::write(
            &src,
            r#"
#[no_mangle]
pub extern "C" fn add(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

#[no_mangle]
pub extern "C" fn mul_f64(a: f64, b: f64) -> f64 {
    a * b
}
"#,
        )
        .expect("write adding.rs");

        #[cfg(windows)]
        let lib_name = "adding.dll";
        #[cfg(target_os = "macos")]
        let lib_name = "libadding.dylib";
        #[cfg(all(unix, not(target_os = "macos")))]
        let lib_name = "libadding.so";

        let lib_path = out_dir.join(lib_name);
        let status = Command::new("rustc")
            .args(["--crate-type", "cdylib", "-O", "-o"])
            .arg(&lib_path)
            .arg(&src)
            .status()
            .expect("spawn rustc");
        assert!(status.success(), "rustc failed to build test DLL");
        lib_path
    })
    .clone()
}

fn dll_path_literal() -> String {
    let p = build_adding_dll();
    let s = p.to_string_lossy().replace('\\', "/");
    format!("\"{s}\"")
}

#[test]
fn sized_int_literal() {
    let v = value("1i32");
    assert!(matches!(v, Value::Sized(optive::sized::SizedNum::I32(1))));
}

#[test]
fn sized_float_literal() {
    let v = value("3.5f64");
    match v {
        Value::Sized(optive::sized::SizedNum::F64(x)) => assert!((x - 3.5).abs() < 1e-12),
        other => panic!("expected f64, got {other:?}"),
    }
}

#[test]
fn sized_type_name() {
    assert_text("type(1i32)", "i32");
}

#[test]
fn convert_num_to_i32() {
    let v = value("i32.(7)");
    assert!(matches!(v, Value::Sized(optive::sized::SizedNum::I32(7))));
}

#[test]
fn convert_i32_to_c_types_int() {
    let v = value(
        r#"
use std.language.{ C }
C.types.int.(1i32)
"#,
    );
    assert!(matches!(v, Value::Sized(optive::sized::SizedNum::I32(1))));
}

#[test]
fn c_types_getattr_chain() {
    let v = value(
        r#"
use std.language.{ C }
C.types.int
"#,
    );
    match v {
        Value::TypeRef(n) => assert_eq!(n, "C.types.int"),
        other => panic!("expected TypeRef, got {}", other.type_name()),
    }
}

#[test]
fn implicit_converts_on_normal_func() {
    let v = value(
        r#"
func f(implicit a: i32) {
    return type(a) == "i32"
}
f(1)
"#,
    );
    assert!(matches!(v, Value::Bool(true)));
}

#[test]
fn frompath_missing_library_errors() {
    let err = run_source(
        r#"
use std.language.{ C }
C.frompath("definitely_missing_library_xyz.dll")
"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("failed to load") || err.contains("definitely_missing"),
        "unexpected error: {err}"
    );
}

#[test]
fn extern_missing_symbol_errors() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h, "no_such_symbol") func missing(a: C.types.int) -> C.types.int ...
"#
    );
    let err = run_source(&src).unwrap_err().to_string();
    assert!(
        err.contains("not found") || err.contains("no_such_symbol"),
        "unexpected error: {err}"
    );
}

#[test]
fn extern_add_happy_path() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h) func add(
    implicit a: C.types.int,
    implicit b: C.types.int
) -> C.types.int : num.(i32.(_)) ...
add(1i32, 2i32)
"#
    );
    assert_num(&src, "3");
}

#[test]
fn extern_add_implicit_from_num() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h) func add(
    implicit a: C.types.int,
    implicit b: C.types.int
) -> C.types.int : num.(i32.(_)) ...
add(10, 20)
"#
    );
    assert_num(&src, "30");
}

#[test]
fn extern_without_implicit_rejects_num() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h) func add(
    a: C.types.int,
    b: C.types.int
) -> C.types.int : num.(i32.(_)) ...
try {{
    add(1, 2)
    0
}} catch (e: TypeError) {{
    1
}}
"#
    );
    assert_num(&src, "1");
}

#[test]
fn extern_hard_param_rejects() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h) func add(
    a:: C.types.int,
    b:: C.types.int
) -> C.types.int : num.(i32.(_)) ...
try {{
    add("x", 1i32)
    0XCE
}} catch (e: TypeError) {{
    1
}}
"#
    );
    assert_num(&src, "1");
}

#[test]
fn extern_survives_handle_drop() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h) func add(
    implicit a: C.types.int,
    implicit b: C.types.int
) -> C.types.int : num.(i32.(_)) ...
h = none
add(3i32, 4i32)
"#
    );
    assert_num(&src, "7");
}

#[test]
fn extern_unsupported_abi_type_errors() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h) func add(a: list) -> C.types.int ...
"#
    );
    let err = run_source(&src).unwrap_err().to_string();
    assert!(
        err.contains("unsupported C ABI") || err.contains("list"),
        "unexpected error: {err}"
    );
}

#[test]
fn void_ptr_alias_resolves() {
    let v = value(
        r#"
use std.language.{ C }
C.types.void_ptr
"#,
    );
    match v {
        Value::TypeRef(n) => assert_eq!(n, "C.types.void*"),
        other => panic!("expected TypeRef, got {}", other.type_name()),
    }
}
