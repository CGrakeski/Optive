#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! `Optive lsp` 库函数：补全 / 悬停 / 诊断 / 签名 / 大纲。

use std::collections::HashMap;

use optive::lsp::{
    completion, completion_in, definition, definition_in, diagnostics, document_symbols, hover,
    hover_in, inlay_hints, references_in, signature_help,
};

fn labels(items: &serde_json::Value) -> Vec<&str> {
    items
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["label"].as_str())
        .collect()
}

#[test]
fn completion_offers_keywords() {
    let items = completion("", 0, 0);
    let labels = labels(&items);
    assert!(labels.contains(&"func"));
    assert!(labels.contains(&"let"));
    assert!(labels.contains(&"std"));
    assert!(labels.contains(&"print"));
}

#[test]
fn completion_std_http_members() {
    let src = "std.http.";
    let items = completion(src, 0, src.chars().count());
    let labels = labels(&items);
    assert!(labels.contains(&"get"));
    assert!(labels.contains(&"serve"));
    assert!(labels.contains(&"request"));
}

#[test]
fn completion_params_inside_func() {
    let src = "func greet(name) {\n  na\n}\n";
    let items = completion(src, 1, 4);
    assert!(labels(&items).contains(&"name"));
}

#[test]
fn completion_works_on_incomplete_source() {
    let src = "func add(a, b) {\n  pr\n";
    let items = completion(src, 1, 4);
    let got = labels(&items);
    assert!(got.contains(&"print"), "{got:?}");

    let items = completion("func add(a, b) {\n  \n", 1, 2);
    let got = labels(&items);
    assert!(got.contains(&"a"), "{got:?}");
    assert!(got.contains(&"b"), "{got:?}");
}

#[test]
fn hover_user_func() {
    let src = "func greet(name) { name }\ngreet(\"x\")\n";
    let hv = hover(src, 1, 0);
    assert_eq!(hv["contents"]["kind"], "plaintext");
    assert_eq!(hv["contents"]["value"], "func greet(name)");
}

#[test]
fn hover_blank_is_null() {
    assert!(hover("   \n", 0, 0).is_null());
}

#[test]
fn diagnostics_parse_error() {
    let d = diagnostics("let x =", "t.tive");
    assert_eq!(d.len(), 1);
}

#[test]
fn signature_help_active_arg() {
    let src = "func add(a, b) { a + b }\nadd(1, \n";
    let sh = signature_help(src, 1, 7);
    assert_eq!(sh["signatures"][0]["label"], "func add(a, b)");
    assert_eq!(sh["activeParameter"], 1);
}

#[test]
fn signature_help_highlights_second_param() {
    let src = "func add(a, b) { a + b }\nadd(1, \n";
    let sh = signature_help(src, 1, 7);
    let label = sh["signatures"][0]["label"].as_str().unwrap();
    let start = sh["signatures"][0]["parameters"][1]["label"][0]
        .as_u64()
        .unwrap() as usize;
    let end = sh["signatures"][0]["parameters"][1]["label"][1]
        .as_u64()
        .unwrap() as usize;
    let chars: Vec<char> = label.chars().collect();
    assert_eq!(chars[start..end].iter().collect::<String>(), "b");
}

#[test]
fn signature_help_std_and_inlay() {
    let sh = signature_help("std.http.get(\n", 0, 13);
    assert_eq!(sh["signatures"][0]["label"], "std.http.get(url, opts?)");
    let hints = inlay_hints(
        "func greet(name) { name }\ngreet(\"hi\")\n",
        "file:///x.tive",
        &HashMap::new(),
    );
    let labels: Vec<&str> = hints
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["label"].as_str())
        .collect();
    assert!(labels.contains(&"name ="), "{hints}");
}

#[test]
fn document_symbols_has_func() {
    let src = "func add(a, b) { a + b }\nlet x = 1\n";
    let syms = document_symbols(src, "file:///x.tive");
    let names: Vec<&str> = syms
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["name"].as_str())
        .collect();
    assert!(names.contains(&"add"));
    assert!(names.contains(&"x"));
}

#[test]
fn hover_infers_let_num() {
    let src = "let count = 1\ncount + 2\n";
    let hv = hover(src, 1, 0);
    assert_eq!(hv["contents"]["value"], "let count: num");
}

#[test]
fn definition_jumps_to_let() {
    let src = "let count = 1\ncount + 2\n";
    let loc = definition(src, "file:///tmp/x.tive", 1, 0);
    assert_eq!(loc["range"]["start"]["line"], 0);
}

#[test]
fn completion_local_var_prefix() {
    let src = "let count = 1\ncou\n";
    let items = completion(src, 1, 3);
    assert!(labels(&items).contains(&"count"));
}

#[test]
fn completion_struct_fields_from_inferred_type() {
    let src = "struct Point { let x let y }\nlet p = Point(1, 2)\np.\n";
    let items = completion(src, 2, 2);
    let got = labels(&items);
    assert!(got.contains(&"x"), "{got:?}");
    assert!(got.contains(&"y"), "{got:?}");
}

#[test]
fn use_jumps_across_open_docs() {
    let lib_uri = "file:///tmp/ws/lib.tive";
    let main_uri = "file:///tmp/ws/main.tive";
    let mut docs = HashMap::new();
    docs.insert(lib_uri.to_string(), "func greet(name) { name }\n".into());
    let main = "use \"lib.tive\".{ greet }\ngreet(\"hi\")\n";
    docs.insert(main_uri.to_string(), main.into());
    let loc = definition_in(main, main_uri, 1, 0, &docs);
    assert_eq!(loc["uri"], lib_uri);
    assert_eq!(loc["range"]["start"]["line"], 0);
    let hv = hover_in(main, main_uri, 1, 0, &docs);
    assert!(
        hv["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("func greet(name)"),
        "{hv:?}"
    );
}

#[test]
fn import_dot_completes_exports_not_intern() {
    let lib_uri = "file:///tmp/ws/lib.tive";
    let main_uri = "file:///tmp/ws/main.tive";
    let mut docs = HashMap::new();
    docs.insert(
        lib_uri.to_string(),
        "func greet(name) { name }\nintern func hidden() { 1 }\n".into(),
    );
    let main = "import \"lib.tive\" as lib\nlib.\n";
    docs.insert(main_uri.to_string(), main.into());
    let items = completion_in(main, main_uri, 1, 4, &docs);
    let got = labels(&items);
    assert!(got.contains(&"greet"), "{got:?}");
    assert!(!got.contains(&"hidden"), "{got:?}");
}

#[test]
fn signature_std_names_match_runtime() {
    let sh = signature_help("std.fs.read_text(\n", 0, 17);
    assert_eq!(sh["signatures"][0]["label"], "std.fs.read_text(path)");
    let sh = signature_help("std.os.getenv(\n", 0, 14);
    assert_eq!(sh["signatures"][0]["label"], "std.os.getenv(name)");
    let sh = signature_help("std.http.serve(\n", 0, 15);
    assert_eq!(
        sh["signatures"][0]["label"],
        "std.http.serve(port, handler)"
    );
    let sh = signature_help("std.net.connect(\n", 0, 16);
    assert_eq!(sh["signatures"][0]["label"], "std.net.connect(host, port)");
    let sh = signature_help("std.encoding.gzip_encode(\n", 0, 25);
    assert_eq!(
        sh["signatures"][0]["label"],
        "std.encoding.gzip_encode(data)"
    );
    let sh = signature_help("std.toml.stringify(\n", 0, 19);
    assert_eq!(sh["signatures"][0]["label"], "std.toml.stringify(value)");
}

#[test]
fn inlay_hints_on_incomplete_call() {
    let hints = inlay_hints(
        "func greet(name) { name }\ngreet(\"hi\"\n",
        "file:///x.tive",
        &HashMap::new(),
    );
    let labels: Vec<&str> = hints
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["label"].as_str())
        .collect();
    assert!(labels.contains(&"name ="), "{hints}");
}

#[test]
fn references_follow_import_and_dotted_use() {
    let lib_uri = "file:///tmp/ws/lib.tive";
    let main_uri = "file:///tmp/ws/main.tive";
    let mut docs = HashMap::new();
    docs.insert(lib_uri.to_string(), "func greet(name) { name }\n".into());
    let main = "import \"lib.tive\" as lib\nlib.greet(\"hi\")\n";
    docs.insert(main_uri.to_string(), main.into());
    let refs = references_in(main, main_uri, 1, 4, &docs);
    let uris: Vec<&str> = refs
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["uri"].as_str())
        .collect();
    assert!(uris.contains(&lib_uri), "{refs}");
    assert!(uris.contains(&main_uri), "{refs}");
}

#[test]
fn completion_sqlite_and_net_handles() {
    let src = "let db = std.sqlite.open(\":memory:\")\ndb.\n";
    let items = completion(src, 1, 3);
    let got = labels(&items);
    assert!(got.contains(&"execute"), "{got:?}");
    assert!(got.contains(&"query"), "{got:?}");

    let src = "let ln = std.net.listen(0)\nln.\n";
    let items = completion(src, 1, 3);
    let got = labels(&items);
    assert!(got.contains(&"accept"), "{got:?}");

    let src = "let c = std.net.connect(\"127.0.0.1\", 80)\nc.\n";
    let items = completion(src, 1, 2);
    let got = labels(&items);
    assert!(got.contains(&"read"), "{got:?}");
    assert!(got.contains(&"write"), "{got:?}");
}
