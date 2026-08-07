//! C FFI / sized types / extern / implicit.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use common::{assert_bool, assert_num, assert_text, value};
use optive::run_source;
use optive::value::Value;

fn build_adding_dll() -> PathBuf {
    static DLL: OnceLock<PathBuf> = OnceLock::new();
    DLL.get_or_init(|| {
        let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ffi_test_dll_v2");
        fs::create_dir_all(&out_dir).expect("create dll dir");
        let src = out_dir.join("adding.rs");
        fs::write(
            &src,
            r#"
use std::ffi::{c_char, CStr};

#[no_mangle]
pub extern "C" fn add(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

#[no_mangle]
pub extern "C" fn mul_f64(a: f64, b: f64) -> f64 {
    a * b
}

#[no_mangle]
pub unsafe extern "C" fn c_strlen(s: *const c_char) -> i32 {
    if s.is_null() { return 0; }
    CStr::from_ptr(s).to_bytes().len() as i32
}

#[repr(C)]
pub struct Point { pub x: i32, pub y: i32 }

#[no_mangle]
pub unsafe extern "C" fn point_sum(p: *const Point) -> i32 {
    (*p).x + (*p).y
}

#[no_mangle]
pub extern "C" fn apply_binop(
    a: i32,
    b: i32,
    cb: Option<extern "C" fn(i32, i32) -> i32>,
) -> i32 {
    match cb {
        Some(f) => f(a, b),
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn apply_unary_f64(
    x: f64,
    cb: Option<extern "C" fn(f64) -> f64>,
) -> f64 {
    match cb {
        Some(f) => f(x),
        None => 0.0,
    }
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
fn sized_zero_int_literal() {
    // B1：`0i32` 不得被当成非法 `0i` 数字前缀。
    let v = value("0i32");
    assert!(matches!(v, Value::Sized(optive::sized::SizedNum::I32(0))));
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

#[test]
fn c_alloc_write_read_free() {
    assert_num(
        r#"
use std.language.{ C }
let p = C.alloc(16)
C.write_i32(p, 0, 42i32)
let v = C.read_i32(p, 0)
C.free(p, 16)
num.(v)
"#,
        "42",
    );
}

#[test]
fn c_cstring_roundtrip() {
    assert_text(
        r#"
use std.language.{ C }
let pair = C.cstring("hello")
let t = C.cstring_to_text(pair[0])
C.free(pair[0], pair[1])
t
"#,
        "hello",
    );
}

#[test]
fn extern_char_ptr_strlen() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h, "c_strlen") func c_strlen(
    implicit s: C.types.char_ptr
) -> C.types.int : num.(i32.(_)) ...
c_strlen("abcd")
"#
    );
    assert_num(&src, "4");
}

#[test]
fn c_struct_point_sum() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let Point = C.Struct([["x", C.types.int], ["y", C.types.int]])
let pair = Point.alloc()
let p = pair[0]
Point.write(p, "x", 3i32)
Point.write(p, "y", 4i32)
let h = C.frompath({path})
extern(h, "point_sum") func point_sum(
    implicit p: C.types.void_ptr
) -> C.types.int : num.(i32.(_)) ...
let s = point_sum(p)
C.free(p, pair[1])
s
"#
    );
    assert_num(&src, "7");
}

#[test]
fn extern_abi_stdcall_accepted() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h, "add", "stdcall") func add(
    implicit a: C.types.int,
    implicit b: C.types.int
) -> C.types.int : num.(i32.(_)) ...
add(1, 2)
"#
    );
    assert_num(&src, "3");
}

#[test]
fn sandbox_blocks_ffi() {
    use optive::caps::Capabilities;
    let caps = Capabilities::sandbox(vec![std::env::current_dir().unwrap()]);
    let err = common::run_with_caps(
        r#"
use std.language.{ C }
C.frompath("x.dll")
"#,
        caps,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("FFI disabled") || err.contains("native FFI"),
        "unexpected: {err}"
    );
}

#[test]
fn c_callback_binop() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
func add_cb(a, b) {{
  return a + b
}}
let pair = C.callback(add_cb, [C.types.int, C.types.int], C.types.int)
let h = C.frompath({path})
extern(h, "apply_binop") func apply_binop(
    implicit a: C.types.int,
    implicit b: C.types.int,
    implicit cb: C.types.void_ptr
) -> C.types.int : num.(i32.(_)) ...
let r = apply_binop(10, 20, pair[0])
C.callback_free(pair[1])
r
"#
    );
    assert_num(&src, "30");
}

#[test]
fn c_callback_unary_f64() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
func square_cb(x) {{
  // x 为 f64 sized；转 num 再算
  let n = num.(x)
  return f64.(n * n)
}}
let pair = C.callback(square_cb, [C.types.double], C.types.double)
let h = C.frompath({path})
extern(h, "apply_unary_f64") func apply_unary_f64(
    implicit x: C.types.double,
    implicit cb: C.types.void_ptr
) -> C.types.double : num.(f64.(_)) ...
let r = apply_unary_f64(3.0, pair[0])
C.callback_free(pair[1])
r
"#
    );
    let v = value(&src);
    let f = match v {
        Value::Num(n) => n.to_f64_checked().expect("f64"),
        Value::Sized(optive::sized::SizedNum::F64(x)) => x,
        other => panic!("expected 9.0, got {other:?}"),
    };
    assert!((f - 9.0).abs() < 1e-9, "got {f}");
}

#[test]
fn mul_f64_via_extern() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h) func mul_f64(
    implicit a: C.types.double,
    implicit b: C.types.double
) -> C.types.double : num.(f64.(_)) ...
mul_f64(2.5, 4.0)
"#
    );
    let v = value(&src);
    match v {
        Value::Num(n) => {
            let f = n.to_f64_checked().expect("f64");
            assert!((f - 10.0).abs() < 1e-9, "got {f}");
        }
        other => panic!("expected num, got {other:?}"),
    }
}

#[test]
fn c_errno_is_number_after_call() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h) func add(
    implicit a: C.types.int,
    implicit b: C.types.int
) -> C.types.int : num.(i32.(_)) ...
add(1, 2)
C.errno()
"#
    );
    let v = value(&src);
    assert!(
        matches!(v, Value::Num(_)),
        "expected num errno, got {v:?}"
    );
}

#[test]
fn c_types_uchar_resolves() {
    let v = value(
        r#"
use std.language.{ C }
C.types.uchar
"#,
    );
    match v {
        Value::TypeRef(n) => assert!(n.contains("uchar") || n.contains("unsigned"), "got {n}"),
        other => panic!("expected TypeRef, got {}", other.type_name()),
    }
}

#[test]
fn mn_ffi_parallel_add_stress() {
    use optive::vm::Vm;
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h) func add(
    implicit a: C.types.int,
    implicit b: C.types.int
) -> C.types.int : num.(i32.(_)) ...
let m = Mutex(0)
let wg = WaitGroup(40)
loop (40) {{
  go do {{
    let s = add(1, 2)
    let g = m.lock()
    g.set(g.get() + s)
    g.unlock()
    wg.done()
  }}
}}
wg.wait()
let g = m.lock()
let total = g.get()
g.unlock()
total
"#
    );
    let mut vm = Vm::with_workers(4);
    let v = optive::run_source_in_vm(&mut vm, &src, "<ffi-mn>").expect("run");
    match v {
        Value::Num(n) => assert_eq!(n.to_string(), "120"),
        other => panic!("expected 120, got {other:?}"),
    }
}

#[test]
fn alloc_array_index_roundtrip() {
    assert_num(
        r#"
use std.language.{ C }
let p = C.alloc_array(i32, 4)
p[0] = 10i32
p[1] = 20i32
p[2] = 30i32
let s = num.(p[0]) + num.(p[1]) + num.(p[2])
C.free(p)
s
"#,
        "60",
    );
}

#[test]
fn ptr_live_false_after_free() {
    assert_bool(
        r#"
use std.language.{ C }
let p = C.alloc(8)
C.write_i32(p, 0, 1i32)
C.free(p)
C.ptr_live(p)
"#,
        false,
    );
}

#[test]
fn ptr_live_false_for_unsafe_foreign() {
    // 外来登记可 peek，但不是 Optive Owned → ptr_live false
    use optive::ptr_registry::{self, PtrEntry, PtrKind};
    let addr = 0x_0000_0000_0F00_ba5eusize;
    ptr_registry::unregister(addr);
    ptr_registry::register(PtrEntry {
        addr,
        nbytes: 64,
        align: 1,
        elem: None,
        kind: PtrKind::ForeignUnsafe,
    });
    assert!(!ptr_registry::is_live(addr));
    assert!(ptr_registry::is_registered(addr));
    ptr_registry::unregister(addr);
}

#[test]
fn use_after_free_index_errors() {
    common::run_err(
        r#"
use std.language.{ C }
let p = C.alloc_array(i32, 2)
C.free(p)
p[0] = 1i32
"#,
    );
}

#[test]
fn implicit_narrowing_out_of_range_errors() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
let h = C.frompath({path})
extern(h) func add(
    implicit a: C.types.int,
    implicit b: C.types.int
) -> C.types.int : num.(i32.(_)) ...
add(3000000000, 1)
"#
    );
    common::run_err(&src);
}

#[test]
fn cstring_to_text_is_copy_survives_free() {
    assert_text(
        r#"
use std.language.{ C }
let pair = C.cstring("keep")
let t = C.cstring_to_text(pair[0])
C.free(pair[0])
t
"#,
        "keep",
    );
}

#[test]
fn c_layout_struct_as_extern_param_rejected() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
typed struct Point {{
  let x: i32
  let y: i32
}} : C.layout
let h = C.frompath({path})
extern(h, "point_sum") func point_sum(
    implicit p: Point
) -> C.types.int : num.(i32.(_)) ...
point_sum
"#
    );
    common::run_err(&src);
}

#[test]
fn unregistered_peek_fails_unless_unsafe() {
    // 使用不太可能被其它用例登记的地址；切勿用小整数假地址做成功路径 peek。
    common::run_err(
        r#"
use std.language.{ C }
C.read_i32(987654321, 0)
"#,
    );
    assert_num(
        r#"
use std.language.{ C }
let p = C.alloc(8)
C.write_i32(p, 0, 7i32)
let q = C.unsafe_ptr(p)
let v = C.read_i32(q, 0)
C.free(p)
num.(v)
"#,
        "7",
    );
}

#[test]
fn typed_struct_c_layout_load_store_point_sum() {
    let path = dll_path_literal();
    let src = format!(
        r#"
use std.language.{{ C }}
typed struct Point {{
  let x: i32
  let y: i32
}} : C.layout
let raw = C.alloc(Point)
C.store(raw, Point(3i32, 4i32))
let h = C.frompath({path})
extern(h, "point_sum") func point_sum(
    implicit p: C.types.void_ptr
) -> C.types.int : num.(i32.(_)) ...
let s = point_sum(raw)
let back = C.load(Point, raw)
C.free(raw)
s + num.(back.x) + num.(back.y)
"#
    );
    assert_num(&src, "14");
}

#[test]
fn ptr_type_form_resolves() {
    let v = value(
        r#"
use std.language.{ C }
C.types.ptr[i32]
"#,
    );
    match v {
        Value::TypeSpec(spec) => {
            assert!(
                spec.name.contains("ptr"),
                "expected ptr TypeSpec, got {}",
                spec.name
            );
            assert_eq!(spec.args.len(), 1);
        }
        other => panic!("expected TypeSpec, got {other:?}"),
    }
}
