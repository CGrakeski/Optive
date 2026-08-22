//! Optive LSP：诊断、补全、悬停、定义、引用、大纲、签名、格式化。
//!
//! 传输：stdio JSON-RPC（`Content-Length`）。不实现 DAP / 重命名 / 语义高亮。

mod catalog;
mod symbols;
mod workspace;

use std::collections::HashMap;
use std::io::{self, BufRead, Write};

use serde_json::{json, Value as Json};

use crate::error::ParseError;
use crate::fmt::format_source;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::token::{Token, TokenKind};

use catalog::{
    handle_member_sig, handle_members, std_export_doc, std_export_sig, std_module_doc, BUILTINS,
    KEYWORDS, SNIPPETS, STD_EXPORTS, STD_MODULES,
};
use symbols::{
    import_path_for_name, index_program, index_source, infer_receiver_from_index, parse_for_lsp,
    FileIndex, KIND_CLASS, KIND_FIELD, KIND_FUNC, KIND_KEYWORD, KIND_METHOD, KIND_MODULE,
    KIND_SNIPPET,
};
use workspace::{
    find_export, is_std_spec, load_index, load_module, path_to_uri, resolve_doc,
    resolve_import_file,
};

pub fn run_stdio() -> io::Result<()> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = io::stdout();
    let mut docs: HashMap<String, String> = HashMap::new();
    while let Some(msg) = read_rpc(&mut reader)? {
        let method = msg.get("method").and_then(Json::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        match method {
            "initialize" => {
                write_rpc(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "capabilities": {
                                "textDocumentSync": 1,
                                "definitionProvider": true,
                                "referencesProvider": true,
                                "documentSymbolProvider": true,
                                "hoverProvider": true,
                                "completionProvider": {
                                    "triggerCharacters": [".", "("]
                                },
                                "signatureHelpProvider": {
                                    "triggerCharacters": ["(", ","],
                                    "retriggerCharacters": [","]
                                },
                                "inlayHintProvider": true,
                                "documentFormattingProvider": true
                            },
                            "serverInfo": { "name": "Optive", "version": env!("CARGO_PKG_VERSION") }
                        }
                    }),
                )?;
            }
            "initialized" | "textDocument/didSave" => {}
            "shutdown" => {
                write_rpc(
                    &mut stdout,
                    json!({"jsonrpc":"2.0","id": id, "result": null}),
                )?;
            }
            "exit" => break,
            "textDocument/didOpen" => {
                if let Some(td) = msg.pointer("/params/textDocument") {
                    let uri = td
                        .get("uri")
                        .and_then(Json::as_str)
                        .unwrap_or("")
                        .to_string();
                    let text = td
                        .get("text")
                        .and_then(Json::as_str)
                        .unwrap_or("")
                        .to_string();
                    docs.insert(uri.clone(), text.clone());
                    publish_diags(&mut stdout, &uri, &text)?;
                }
            }
            "textDocument/didChange" => {
                if let Some(uri) = msg
                    .pointer("/params/textDocument/uri")
                    .and_then(Json::as_str)
                {
                    let uri = uri.to_string();
                    if let Some(text) = msg
                        .pointer("/params/contentChanges/0/text")
                        .and_then(Json::as_str)
                    {
                        docs.insert(uri.clone(), text.to_string());
                        publish_diags(&mut stdout, &uri, text)?;
                    }
                }
            }
            "textDocument/didClose" => {
                if let Some(uri) = msg
                    .pointer("/params/textDocument/uri")
                    .and_then(Json::as_str)
                {
                    docs.remove(uri);
                }
            }
            "textDocument/definition" => {
                let (uri, line, character, text) = doc_pos(&docs, &msg);
                write_rpc(
                    &mut stdout,
                    json!({"jsonrpc":"2.0","id": id, "result": definition_in(&text, &uri, line, character, &docs)}),
                )?;
            }
            "textDocument/references" => {
                let (uri, line, character, text) = doc_pos(&docs, &msg);
                write_rpc(
                    &mut stdout,
                    json!({"jsonrpc":"2.0","id": id, "result": references_in(&text, &uri, line, character, &docs)}),
                )?;
            }
            "textDocument/documentSymbol" => {
                let (uri, _, _, text) = doc_pos(&docs, &msg);
                write_rpc(
                    &mut stdout,
                    json!({"jsonrpc":"2.0","id": id, "result": document_symbols(&text, &uri)}),
                )?;
            }
            "textDocument/completion" => {
                let (uri, line, character, text) = doc_pos(&docs, &msg);
                write_rpc(
                    &mut stdout,
                    json!({"jsonrpc":"2.0","id": id, "result": completion_in(&text, &uri, line, character, &docs)}),
                )?;
            }
            "textDocument/hover" => {
                let (uri, line, character, text) = doc_pos(&docs, &msg);
                write_rpc(
                    &mut stdout,
                    json!({"jsonrpc":"2.0","id": id, "result": hover_in(&text, &uri, line, character, &docs)}),
                )?;
            }
            "textDocument/signatureHelp" => {
                let (uri, line, character, text) = doc_pos(&docs, &msg);
                write_rpc(
                    &mut stdout,
                    json!({"jsonrpc":"2.0","id": id, "result": signature_help_in(&text, &uri, line, character, &docs)}),
                )?;
            }
            "textDocument/inlayHint" => {
                let (uri, _, _, text) = doc_pos(&docs, &msg);
                write_rpc(
                    &mut stdout,
                    json!({"jsonrpc":"2.0","id": id, "result": inlay_hints(&text, &uri, &docs)}),
                )?;
            }
            "textDocument/formatting" => {
                let (_, _, _, text) = doc_pos(&docs, &msg);
                write_rpc(
                    &mut stdout,
                    json!({"jsonrpc":"2.0","id": id, "result": formatting(&text)}),
                )?;
            }
            _ => {
                if id.is_some() {
                    write_rpc(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": format!("method not found: {method}") }
                        }),
                    )?;
                }
            }
        }
    }
    Ok(())
}

pub fn diagnostics(source: &str, _file: &str) -> Vec<(usize, usize, String)> {
    match Parser::parse(source) {
        Ok(_) => Vec::new(),
        Err(ParseError::Message {
            line,
            column,
            message,
        }) => vec![(line, column, message)],
    }
}

fn doc_pos(docs: &HashMap<String, String>, msg: &Json) -> (String, usize, usize, String) {
    let raw = msg
        .pointer("/params/textDocument/uri")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    let line = msg
        .pointer("/params/position/line")
        .and_then(Json::as_u64)
        .unwrap_or(0) as usize;
    let character = msg
        .pointer("/params/position/character")
        .and_then(Json::as_u64)
        .unwrap_or(0) as usize;
    if let Some((uri, text)) = resolve_doc(docs, &raw) {
        return (uri, line, character, text);
    }
    let text = docs.get(&raw).cloned().unwrap_or_default();
    (raw, line, character, text)
}

/// LSP `textDocument/completion`。
#[must_use]
pub fn completion(source: &str, lsp_line: usize, lsp_col: usize) -> Json {
    completion_in(source, "", lsp_line, lsp_col, &HashMap::new())
}

#[must_use]
pub fn completion_in(
    source: &str,
    uri: &str,
    lsp_line: usize,
    lsp_col: usize,
    docs: &HashMap<String, String>,
) -> Json {
    let typed = dotted_prefix(source, lsp_line, lsp_col);
    let (parent, partial) = split_dotted(&typed);
    let partial_l = partial.to_ascii_lowercase();
    let mut items: Vec<Json> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let idx = index_source(source);
    let line_1 = lsp_line.saturating_add(1);

    let mut push = |label: &str, kind: u8, detail: &str, sort: &str, snippet: Option<&str>| {
        if !partial_l.is_empty() && !label.to_ascii_lowercase().starts_with(&partial_l) {
            return;
        }
        if !seen.insert(label.to_string()) {
            return;
        }
        let mut it = json!({
            "label": label,
            "kind": kind,
            "detail": detail,
            "sortText": sort,
            "insertText": snippet.unwrap_or(label)
        });
        if snippet.is_some() {
            it["insertTextFormat"] = json!(2);
        }
        items.push(it);
    };

    if parent == "std" {
        for m in STD_MODULES {
            push(m, KIND_MODULE, "std submodule", "0", None);
        }
        return Json::Array(items);
    }
    if let Some(mod_name) = parent.strip_prefix("std.") {
        for (m, exp) in STD_EXPORTS {
            if *m == mod_name {
                push(exp, KIND_FUNC, &std_export_doc(m, exp), "0", None);
            }
        }
        return Json::Array(items);
    }
    if !parent.is_empty() {
        if let Some(mod_sym) = idx.def_of(&parent, line_1).or_else(|| idx.any_def(&parent)) {
            if let Some(spec) = &mod_sym.module_spec {
                if let Some((_, tidx)) = load_index(uri, spec, docs) {
                    for s in tidx.exports() {
                        push(&s.name, s.kind, &s.detail, "0", None);
                    }
                    return Json::Array(items);
                }
            }
        }
        if let Some(ty) = infer_receiver_from_index(&idx, &parent) {
            for s in idx.members_of(&ty) {
                push(&s.name, s.kind, &s.detail, "0", None);
            }
            for (name, detail) in handle_members(&ty) {
                push(name, KIND_METHOD, detail, "0", None);
            }
        }
        for s in idx.members_of(&parent) {
            push(&s.name, s.kind, &s.detail, "0", None);
        }
        return Json::Array(items);
    }

    for s in idx.in_scope(line_1) {
        let sort = if s.kind == KIND_FUNC {
            "0a"
        } else if s.kind == KIND_MODULE {
            "0c"
        } else {
            "0b"
        };
        push(&s.name, s.kind, &s.detail, sort, None);
    }
    for (name, doc) in BUILTINS {
        push(name, KIND_FUNC, doc, "1", None);
    }
    for kw in KEYWORDS {
        push(kw, KIND_KEYWORD, "keyword", "2", None);
    }
    for sn in SNIPPETS {
        if !seen.contains(sn.label) {
            seen.insert(sn.label.to_string());
            items.push(json!({
                "label": sn.label,
                "kind": KIND_SNIPPET,
                "detail": sn.detail,
                "sortText": "3",
                "insertText": sn.insert,
                "insertTextFormat": 2,
                "filterText": sn.label
            }));
        }
    }
    Json::Array(items)
}

/// LSP `textDocument/hover`。
#[must_use]
pub fn hover(source: &str, lsp_line: usize, lsp_col: usize) -> Json {
    hover_in(source, "", lsp_line, lsp_col, &HashMap::new())
}

#[must_use]
pub fn hover_in(
    source: &str,
    uri: &str,
    lsp_line: usize,
    lsp_col: usize,
    docs: &HashMap<String, String>,
) -> Json {
    let typed = dotted_name_at(source, lsp_line, lsp_col);
    if typed.is_empty() {
        return Json::Null;
    }
    if typed == "std" {
        return hover_md("std — standard library module");
    }
    if let Some(rest) = typed.strip_prefix("std.") {
        if rest.split('.').nth(1).is_none() && STD_MODULES.contains(&rest) {
            return hover_md(&std_module_doc(rest));
        }
        if let Some((m, exp)) = rest.split_once('.') {
            if STD_EXPORTS.iter().any(|(mm, e)| *mm == m && *e == exp) {
                return hover_md(&std_export_doc(m, exp));
            }
        }
    }
    if let Some((_, doc)) = BUILTINS.iter().find(|(n, _)| *n == typed) {
        return hover_md(doc);
    }
    let idx = index_source(source);
    let line_1 = lsp_line.saturating_add(1);
    if let Some((parent, name)) = typed.rsplit_once('.') {
        if let Some(ty) = infer_receiver_from_index(&idx, parent) {
            if let Some(m) = idx.members_of(&ty).into_iter().find(|s| s.name == name) {
                return hover_md(&m.detail);
            }
            if let Some(sig) = handle_member_sig(&ty, name) {
                return hover_md(sig);
            }
        }
        if let Some(mod_sym) = idx.any_def(parent) {
            if let Some(spec) = &mod_sym.module_spec {
                if let Some((_, tidx)) = load_index(uri, spec, docs) {
                    if let Some(e) = find_export(&tidx, name) {
                        return hover_md(&format!("{}  ({spec})", e.detail));
                    }
                }
            }
        }
    }
    if let Some(s) = idx.def_of(&typed, line_1).or_else(|| idx.any_def(&typed)) {
        if let Some((spec, exp)) = &s.imported_from {
            if is_std_spec(spec) {
                if let Some(m) = spec.strip_prefix("std.") {
                    return hover_md(&std_export_doc(m, exp));
                }
            }
            if let Some((_, tidx)) = load_index(uri, spec, docs) {
                if let Some(e) = find_export(&tidx, exp) {
                    return hover_md(&format!("{}  ({spec})", e.detail));
                }
            }
        }
        return hover_md(&s.detail);
    }
    if KEYWORDS.contains(&typed.as_str()) {
        return hover_md(&format!("keyword `{typed}`"));
    }
    Json::Null
}

fn hover_md(text: &str) -> Json {
    json!({ "contents": { "kind": "plaintext", "value": text } })
}

pub fn definition(source: &str, uri: &str, lsp_line: usize, lsp_col: usize) -> Json {
    definition_in(source, uri, lsp_line, lsp_col, &HashMap::new())
}

#[must_use]
pub fn definition_in(
    source: &str,
    uri: &str,
    lsp_line: usize,
    lsp_col: usize,
    docs: &HashMap<String, String>,
) -> Json {
    let line_1 = lsp_line.saturating_add(1);
    let col_1 = lsp_col.saturating_add(1);
    let tokens = Lexer::new(source).tokenize().unwrap_or_default();
    let Some(tok) = token_at(&tokens, line_1, col_1) else {
        let typed = dotted_name_at(source, lsp_line, lsp_col);
        if typed.is_empty() {
            return Json::Null;
        }
        return definition_name(source, uri, lsp_line, &typed, docs);
    };
    if tok.kind == TokenKind::StringLiteral {
        let path = tok.value.trim_matches('"');
        if looks_like_import_path(path) {
            if let Some((turi, _)) = load_module(uri, path, docs) {
                return location_json(&turi, 0, 0);
            }
            if let Some(resolved) = resolve_import_file(uri, path) {
                return location_json(&path_to_uri(&resolved), 0, 0);
            }
        }
        return Json::Null;
    }
    if tok.kind != TokenKind::Identifier {
        let typed = dotted_name_at(source, lsp_line, lsp_col);
        if typed.is_empty() {
            return Json::Null;
        }
        return definition_name(source, uri, lsp_line, &typed, docs);
    }
    let typed = {
        let dotted = dotted_name_at(source, lsp_line, lsp_col);
        if dotted.is_empty() {
            tok.value.clone()
        } else {
            dotted
        }
    };
    definition_name(source, uri, lsp_line, &typed, docs)
}

fn definition_name(
    source: &str,
    uri: &str,
    lsp_line: usize,
    typed: &str,
    docs: &HashMap<String, String>,
) -> Json {
    let line_1 = lsp_line.saturating_add(1);
    let name = typed.rsplit('.').next().unwrap_or(typed);
    let idx = index_source(source);

    if let Some((parent, exp)) = typed.rsplit_once('.') {
        if let Some(ty) = infer_receiver_from_index(&idx, parent) {
            if let Some(m) = idx.members_of(&ty).into_iter().find(|s| s.name == exp) {
                return location_json(uri, m.line.saturating_sub(1), m.col.saturating_sub(1));
            }
        }
        if !parent.starts_with("std") {
            if let Some(mod_sym) = idx.any_def(parent) {
                if let Some(spec) = &mod_sym.module_spec {
                    if let Some((turi, tidx)) = load_index(uri, spec, docs) {
                        if let Some(e) = find_export(&tidx, exp) {
                            return location_json(
                                &turi,
                                e.line.saturating_sub(1),
                                e.col.saturating_sub(1),
                            );
                        }
                        return location_json(&turi, 0, 0);
                    }
                }
            }
        }
    }

    if let Some(program) = parse_for_lsp(source) {
        if let Some(spec) = import_path_for_name(&program, name) {
            if let Some((turi, _)) = load_module(uri, spec, docs) {
                return location_json(&turi, 0, 0);
            }
            if let Some(resolved) = resolve_import_file(uri, spec) {
                return location_json(&path_to_uri(&resolved), 0, 0);
            }
        }
    }
    if let Some(s) = idx.def_of(name, line_1).or_else(|| idx.any_def(name)) {
        if let Some((spec, exp)) = &s.imported_from {
            if !is_std_spec(spec) {
                if let Some((turi, tidx)) = load_index(uri, spec, docs) {
                    if let Some(e) = find_export(&tidx, exp) {
                        return location_json(
                            &turi,
                            e.line.saturating_sub(1),
                            e.col.saturating_sub(1),
                        );
                    }
                    return location_json(&turi, 0, 0);
                }
            }
        }
        if let Some(spec) = &s.module_spec {
            if let Some((turi, _)) = load_module(uri, spec, docs) {
                return location_json(&turi, 0, 0);
            }
        }
        return location_json(uri, s.line.saturating_sub(1), s.col.saturating_sub(1));
    }
    Json::Null
}

#[must_use]
pub fn references(source: &str, uri: &str, lsp_line: usize, lsp_col: usize) -> Json {
    references_in(source, uri, lsp_line, lsp_col, &HashMap::new())
}

#[must_use]
pub fn references_in(
    source: &str,
    uri: &str,
    lsp_line: usize,
    lsp_col: usize,
    docs: &HashMap<String, String>,
) -> Json {
    let typed = dotted_name_at(source, lsp_line, lsp_col);
    if typed.is_empty() {
        return json!([]);
    }
    let idx = index_source(source);
    let last = typed.rsplit('.').next().unwrap_or(&typed);
    let mut locs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push_loc = |turi: &str, line: usize, col: usize| {
        let key = (turi.to_string(), line, col);
        if seen.insert(key) {
            locs.push(location_json(
                turi,
                line.saturating_sub(1),
                col.saturating_sub(1),
            ));
        }
    };

    collect_file_refs(source, uri, &typed, last, &mut push_loc);

    if let Some((parent, exp)) = typed.rsplit_once('.') {
        if let Some(mod_sym) = idx.any_def(parent) {
            if let Some(spec) = &mod_sym.module_spec {
                if let Some((turi, tsrc)) = load_module(uri, spec, docs) {
                    collect_file_refs(&tsrc, &turi, exp, exp, &mut push_loc);
                }
            }
        }
    }
    if let Some(s) = idx.any_def(last) {
        if let Some((spec, exp)) = &s.imported_from {
            if !is_std_spec(spec) {
                if let Some((turi, tsrc)) = load_module(uri, spec, docs) {
                    collect_file_refs(&tsrc, &turi, exp, exp, &mut push_loc);
                }
            }
        }
    }
    for (duri, dsrc) in docs {
        if duri == uri {
            continue;
        }
        collect_file_refs(dsrc, duri, &typed, last, &mut push_loc);
    }
    Json::Array(locs)
}

fn collect_file_refs(
    source: &str,
    uri: &str,
    typed: &str,
    last: &str,
    push: &mut impl FnMut(&str, usize, usize),
) {
    let idx = index_source(source);
    if let Some(s) = idx.any_def(last).or_else(|| idx.any_def(typed)) {
        push(uri, s.line, s.col);
    }
    for (name, line, col) in &idx.uses {
        if use_refers(name, typed, last) {
            push(uri, *line, *col);
        }
    }
}

fn use_refers(use_name: &str, typed: &str, last: &str) -> bool {
    if use_name == typed || use_name == last {
        return true;
    }
    if typed.contains('.') {
        return use_name.rsplit('.').next() == Some(last);
    }
    use_name.rsplit('.').next() == Some(last) && use_name.contains('.')
}

#[must_use]
pub fn document_symbols(source: &str, _uri: &str) -> Json {
    let idx = index_source(source);
    let items: Vec<Json> = idx
        .symbols
        .iter()
        .filter(|s| s.container.is_none())
        .map(|s| {
            let line = s.line.saturating_sub(1);
            let col = s.col.saturating_sub(1);
            json!({
                "name": s.name,
                "kind": s.kind,
                "detail": s.detail,
                "range": {
                    "start": { "line": line, "character": col },
                    "end": { "line": line, "character": col + s.name.chars().count() }
                },
                "selectionRange": {
                    "start": { "line": line, "character": col },
                    "end": { "line": line, "character": col + s.name.chars().count() }
                }
            })
        })
        .collect();
    Json::Array(items)
}

#[must_use]
pub fn signature_help(source: &str, lsp_line: usize, lsp_col: usize) -> Json {
    signature_help_in(source, "", lsp_line, lsp_col, &HashMap::new())
}

#[must_use]
pub fn signature_help_in(
    source: &str,
    uri: &str,
    lsp_line: usize,
    lsp_col: usize,
    docs: &HashMap<String, String>,
) -> Json {
    let Some((name, active)) = call_context(source, lsp_line, lsp_col) else {
        return Json::Null;
    };
    let idx = index_source(source);
    let label = resolve_signature(uri, docs, &idx, &name);
    let params = params_from_label(&label);
    json!({
        "signatures": [{
            "label": label,
            "parameters": params
        }],
        "activeSignature": 0,
        "activeParameter": active
    })
}

fn resolve_signature(
    uri: &str,
    docs: &HashMap<String, String>,
    idx: &FileIndex,
    name: &str,
) -> String {
    if let Some(rest) = name.strip_prefix("std.") {
        if let Some((m, exp)) = rest.split_once('.') {
            if let Some(sig) = std_export_sig(m, exp) {
                return sig;
            }
            return format!("std.{m}.{exp}(...)");
        }
    }
    if let Some((parent, exp)) = name.rsplit_once('.') {
        if let Some(ty) = infer_receiver_from_index(idx, parent) {
            if let Some(m) = idx.members_of(&ty).into_iter().find(|s| s.name == exp) {
                return m.detail.clone();
            }
            if let Some(sig) = handle_member_sig(&ty, exp) {
                return sig.to_string();
            }
        }
        if let Some(mod_sym) = idx.any_def(parent) {
            if let Some(spec) = &mod_sym.module_spec {
                if let Some((_, tidx)) = load_index(uri, spec, docs) {
                    if let Some(e) = find_export(&tidx, exp) {
                        return e.detail.clone();
                    }
                }
            }
        }
    }
    if let Some(s) = idx.any_def(name) {
        if let Some((spec, exp)) = &s.imported_from {
            if is_std_spec(spec) {
                if let Some(m) = spec.strip_prefix("std.") {
                    if let Some(sig) = std_export_sig(m, exp) {
                        return sig;
                    }
                    return std_export_doc(m, exp);
                }
            }
            if let Some((_, tidx)) = load_index(uri, spec, docs) {
                if let Some(e) = find_export(&tidx, exp) {
                    return e.detail.clone();
                }
            }
        }
        if s.kind == KIND_CLASS {
            let fields: Vec<&str> = idx
                .members_of(name)
                .into_iter()
                .filter(|m| m.kind == KIND_FIELD)
                .map(|m| m.name.as_str())
                .collect();
            if !fields.is_empty() {
                return format!("{name}({})", fields.join(", "));
            }
        }
        return s.detail.clone();
    }
    if let Some((_, doc)) = BUILTINS.iter().find(|(n, _)| *n == name) {
        return (*doc).to_string();
    }
    format!("{name}(...)")
}

fn params_from_label(label: &str) -> Vec<Json> {
    let Some(byte_start) = label.find('(') else {
        return Vec::new();
    };
    let Some(byte_end) = label.rfind(')') else {
        return Vec::new();
    };
    if byte_end <= byte_start + 1 {
        return Vec::new();
    }
    let mut params = Vec::new();
    let mut offset = byte_start + 1;
    for part in label[byte_start + 1..byte_end].split(',') {
        let trimmed = part.trim();
        if !trimmed.is_empty() && trimmed != "..." {
            let rel = part.find(trimmed).unwrap_or(0);
            let a = utf16_len(&label[..offset + rel]);
            let b = a + utf16_len(trimmed);
            params.push(json!({ "label": [a, b] }));
        }
        offset += part.len() + 1;
    }
    params
}

fn param_names(label: &str) -> Vec<String> {
    let Some(start) = label.find('(') else {
        return Vec::new();
    };
    let Some(end) = label.rfind(')') else {
        return Vec::new();
    };
    if end <= start + 1 {
        return Vec::new();
    }
    label[start + 1..end]
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty() && *p != "...")
        .map(|p| {
            p.trim_start_matches('*')
                .trim_end_matches('?')
                .split(':')
                .next()
                .unwrap_or(p)
                .trim()
                .to_string()
        })
        .filter(|p| !p.is_empty())
        .collect()
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn call_context(source: &str, lsp_line: usize, lsp_col: usize) -> Option<(String, u32)> {
    let tokens = Lexer::new(source).tokenize().ok()?;
    let line_1 = lsp_line.saturating_add(1);
    let col_1 = lsp_col.saturating_add(1);
    let mut depth = 0i32;
    let mut commas = 0u32;
    let mut i = tokens.len();
    while i > 0 {
        i -= 1;
        let t = &tokens[i];
        if t.line > line_1 || (t.line == line_1 && t.column > col_1) {
            continue;
        }
        match t.kind {
            TokenKind::RParen => depth += 1,
            TokenKind::LParen => {
                if depth == 0 {
                    return callee_before_paren(&tokens, i).map(|n| (n, commas));
                }
                depth -= 1;
            }
            TokenKind::Comma if depth == 0 => commas += 1,
            _ => {}
        }
    }
    None
}

fn callee_before_paren(tokens: &[Token], paren_i: usize) -> Option<String> {
    if paren_i == 0 || tokens[paren_i - 1].kind != TokenKind::Identifier {
        return None;
    }
    let mut parts = vec![tokens[paren_i - 1].value.clone()];
    let mut j = paren_i - 1;
    while j >= 2
        && tokens[j - 1].kind == TokenKind::Dot
        && tokens[j - 2].kind == TokenKind::Identifier
    {
        parts.push(tokens[j - 2].value.clone());
        j -= 2;
    }
    parts.reverse();
    Some(parts.join("."))
}

/// 调用处参数名内嵌提示：`greet(name = "hi")`。关键字参数是 `a = b`，不是 `a: b`。
#[must_use]
pub fn inlay_hints(source: &str, uri: &str, docs: &HashMap<String, String>) -> Json {
    let Some(program) = parse_for_lsp(source) else {
        return json!([]);
    };
    let hi = source.lines().count().max(1).saturating_add(1);
    let idx = index_program(&program, hi);
    let mut hints = Vec::new();
    collect_call_hints(&program.stmts, uri, docs, &idx, &mut hints);
    Json::Array(hints)
}

fn collect_call_hints(
    stmts: &[crate::ast::LocatedStmt],
    uri: &str,
    docs: &HashMap<String, String>,
    idx: &FileIndex,
    out: &mut Vec<Json>,
) {
    for st in stmts {
        walk_stmt_calls(&st.stmt, uri, docs, idx, out);
    }
}

fn walk_stmt_calls(
    stmt: &crate::ast::Stmt,
    uri: &str,
    docs: &HashMap<String, String>,
    idx: &FileIndex,
    out: &mut Vec<Json>,
) {
    use crate::ast::Stmt;
    match stmt {
        Stmt::VarDecl { init, .. } => {
            if let Some(e) = init {
                walk_expr_calls(e, uri, docs, idx, out);
            }
        }
        Stmt::DestructDecl { init, .. } => walk_expr_calls(init, uri, docs, idx, out),
        Stmt::Assign { value, .. } | Stmt::DestructAssign { value, .. } => {
            walk_expr_calls(value, uri, docs, idx, out)
        }
        Stmt::FuncDecl {
            body, decorators, ..
        } => {
            for d in decorators {
                walk_expr_calls(d, uri, docs, idx, out);
            }
            collect_call_hints(body, uri, docs, idx, out);
        }
        Stmt::MacroDecl { body, .. } | Stmt::Block(body) => {
            collect_call_hints(body, uri, docs, idx, out)
        }
        Stmt::FriendFuncDecl { body, .. } => {
            if let Some(body) = body {
                collect_call_hints(body, uri, docs, idx, out);
            }
        }
        Stmt::StructDecl {
            fields,
            methods,
            layout,
            ..
        } => {
            for f in fields {
                if let Some(e) = &f.default_expr {
                    walk_expr_calls(e, uri, docs, idx, out);
                }
            }
            if let Some(e) = layout {
                walk_expr_calls(e, uri, docs, idx, out);
            }
            for m in methods {
                collect_call_hints(&m.body, uri, docs, idx, out);
            }
        }
        Stmt::Return(Some(e))
        | Stmt::Yield(Some(e))
        | Stmt::YieldFrom(e)
        | Stmt::Throw(e)
        | Stmt::Expr(e) => walk_expr_calls(e, uri, docs, idx, out),
        Stmt::If {
            cond,
            then_block,
            elifs,
            else_block,
        } => {
            walk_expr_calls(cond, uri, docs, idx, out);
            collect_call_hints(then_block, uri, docs, idx, out);
            for (c, b) in elifs {
                walk_expr_calls(c, uri, docs, idx, out);
                collect_call_hints(b, uri, docs, idx, out);
            }
            if let Some(b) = else_block {
                collect_call_hints(b, uri, docs, idx, out);
            }
        }
        Stmt::While { cond, body } => {
            walk_expr_calls(cond, uri, docs, idx, out);
            collect_call_hints(body, uri, docs, idx, out);
        }
        Stmt::Loop { count, body } => {
            if let Some(c) = count {
                walk_expr_calls(c, uri, docs, idx, out);
            }
            collect_call_hints(body, uri, docs, idx, out);
        }
        Stmt::For { items, body } => {
            for it in items {
                walk_expr_calls(&it.iterable, uri, docs, idx, out);
            }
            collect_call_hints(body, uri, docs, idx, out);
        }
        Stmt::Try {
            body,
            catches,
            else_block,
        } => {
            collect_call_hints(body, uri, docs, idx, out);
            for c in catches {
                collect_call_hints(&c.body, uri, docs, idx, out);
            }
            if let Some(b) = else_block {
                collect_call_hints(b, uri, docs, idx, out);
            }
        }
        Stmt::Match {
            subject,
            cases,
            else_block,
        } => {
            walk_expr_calls(subject, uri, docs, idx, out);
            for c in cases {
                collect_call_hints(&c.body, uri, docs, idx, out);
            }
            if let Some(b) = else_block {
                collect_call_hints(b, uri, docs, idx, out);
            }
        }
        Stmt::With { context, body, .. } => {
            walk_expr_calls(context, uri, docs, idx, out);
            collect_call_hints(body, uri, docs, idx, out);
        }
        Stmt::EnumDecl { methods, .. } => {
            for m in methods {
                collect_call_hints(&m.body, uri, docs, idx, out);
            }
        }
        Stmt::Del(t) => match t {
            crate::ast::DelTarget::Name(_) => {}
            crate::ast::DelTarget::Member { object, .. } => {
                walk_expr_calls(object, uri, docs, idx, out)
            }
            crate::ast::DelTarget::Index { object, index } => {
                walk_expr_calls(object, uri, docs, idx, out);
                walk_expr_calls(index, uri, docs, idx, out);
            }
        },
        Stmt::Return(None)
        | Stmt::Yield(None)
        | Stmt::Import { .. }
        | Stmt::Use { .. }
        | Stmt::ProtocolDecl { .. }
        | Stmt::VariantDecl { .. }
        | Stmt::Break
        | Stmt::Continue
        | Stmt::Comment { .. } => {}
    }
}

fn walk_expr_calls(
    expr: &crate::ast::Expr,
    uri: &str,
    docs: &HashMap<String, String>,
    idx: &FileIndex,
    out: &mut Vec<Json>,
) {
    use crate::ast::ExprKind;
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if let Some(name) = callee_path(callee) {
                let label = resolve_signature(uri, docs, idx, &name);
                let names = param_names(&label);
                let mut pi = 0usize;
                for a in args {
                    walk_expr_calls(&a.value, uri, docs, idx, out);
                    if a.name.is_some() || a.is_splat || a.is_kwsplat {
                        if a.name.is_none() {
                            pi += 1;
                        }
                        continue;
                    }
                    if let Some(pname) = names.get(pi) {
                        if pname != "..." && !pname.is_empty() {
                            out.push(json!({
                                "position": {
                                    "line": a.value.loc.line.saturating_sub(1),
                                    "character": a.value.loc.column.saturating_sub(1)
                                },
                                "label": format!("{pname} ="),
                                "kind": 2,
                                "paddingRight": true
                            }));
                        }
                    }
                    pi += 1;
                }
            } else {
                for a in args {
                    walk_expr_calls(&a.value, uri, docs, idx, out);
                }
            }
            walk_expr_calls(callee, uri, docs, idx, out);
        }
        ExprKind::Unary { operand, .. }
        | ExprKind::Handle { operand }
        | ExprKind::Go { operand }
        | ExprKind::Snap { operand }
        | ExprKind::Await { operand } => walk_expr_calls(operand, uri, docs, idx, out),
        ExprKind::Binary { left, right, .. } => {
            walk_expr_calls(left, uri, docs, idx, out);
            walk_expr_calls(right, uri, docs, idx, out);
        }
        ExprKind::Member { object, .. } => walk_expr_calls(object, uri, docs, idx, out),
        ExprKind::Index { object, index } => {
            walk_expr_calls(object, uri, docs, idx, out);
            walk_expr_calls(index, uri, docs, idx, out);
        }
        ExprKind::List(xs) | ExprKind::Set(xs) | ExprKind::Tuple(xs) => {
            for e in xs {
                walk_expr_calls(e, uri, docs, idx, out);
            }
        }
        ExprKind::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            walk_expr_calls(cond, uri, docs, idx, out);
            walk_expr_calls(then_expr, uri, docs, idx, out);
            walk_expr_calls(else_expr, uri, docs, idx, out);
        }
        ExprKind::Dict(ents) => {
            for (k, v) in ents {
                walk_expr_calls(k, uri, docs, idx, out);
                walk_expr_calls(v, uri, docs, idx, out);
            }
        }
        ExprKind::FString(parts) => {
            for p in parts {
                if let crate::ast::FStringPart::Expr(e) = p {
                    walk_expr_calls(e, uri, docs, idx, out);
                }
            }
        }
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => {
            walk_expr_calls(object, uri, docs, idx, out);
            if let Some(e) = start {
                walk_expr_calls(e, uri, docs, idx, out);
            }
            if let Some(e) = end {
                walk_expr_calls(e, uri, docs, idx, out);
            }
            if let Some(e) = step {
                walk_expr_calls(e, uri, docs, idx, out);
            }
        }
        ExprKind::TypeConvert { type_expr, value } => {
            walk_expr_calls(type_expr, uri, docs, idx, out);
            walk_expr_calls(value, uri, docs, idx, out);
        }
        ExprKind::ListComp {
            elem,
            items,
            guards,
        }
        | ExprKind::SetComp {
            elem,
            items,
            guards,
        }
        | ExprKind::GeneratorExp {
            elem,
            items,
            guards,
        } => {
            walk_expr_calls(elem, uri, docs, idx, out);
            for it in items {
                walk_expr_calls(&it.iterable, uri, docs, idx, out);
            }
            for g in guards {
                walk_expr_calls(g, uri, docs, idx, out);
            }
        }
        ExprKind::DictComp {
            key,
            value,
            items,
            guards,
        } => {
            walk_expr_calls(key, uri, docs, idx, out);
            walk_expr_calls(value, uri, docs, idx, out);
            for it in items {
                walk_expr_calls(&it.iterable, uri, docs, idx, out);
            }
            for g in guards {
                walk_expr_calls(g, uri, docs, idx, out);
            }
        }
        ExprKind::NamedAssign { value, .. } => walk_expr_calls(value, uri, docs, idx, out),
        ExprKind::Pipeline { left, right, .. } => {
            walk_expr_calls(left, uri, docs, idx, out);
            walk_expr_calls(right, uri, docs, idx, out);
        }
        ExprKind::DoFunc { body, .. } => collect_call_hints(body, uri, docs, idx, out),
        ExprKind::Match {
            subject,
            cases,
            else_block,
        } => {
            walk_expr_calls(subject, uri, docs, idx, out);
            for c in cases {
                collect_call_hints(&c.body, uri, docs, idx, out);
            }
            if let Some(b) = else_block {
                collect_call_hints(b, uri, docs, idx, out);
            }
        }
        ExprKind::MacroCall { callee, .. } => walk_expr_calls(callee, uri, docs, idx, out),
        ExprKind::ParFor { items, body } => {
            for it in items {
                walk_expr_calls(&it.iterable, uri, docs, idx, out);
            }
            collect_call_hints(body, uri, docs, idx, out);
        }
        ExprKind::ParBlock { exprs } => {
            for e in exprs {
                walk_expr_calls(e, uri, docs, idx, out);
            }
        }
        ExprKind::Select { cases, else_block } => {
            for c in cases {
                walk_expr_calls(&c.event, uri, docs, idx, out);
                collect_call_hints(&c.body, uri, docs, idx, out);
            }
            if let Some(b) = else_block {
                collect_call_hints(b, uri, docs, idx, out);
            }
        }
        ExprKind::Quote { bindings, body, .. } => {
            for e in bindings {
                walk_expr_calls(e, uri, docs, idx, out);
            }
            collect_call_hints(body, uri, docs, idx, out);
        }
        ExprKind::Placeholder
        | ExprKind::Suspend
        | ExprKind::Number(_)
        | ExprKind::String(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Bytes(_)
        | ExprKind::Var(_) => {}
    }
}

fn callee_path(expr: &crate::ast::Expr) -> Option<String> {
    match &expr.kind {
        crate::ast::ExprKind::Var(n) => Some(n.clone()),
        crate::ast::ExprKind::Member { object, field } => {
            Some(format!("{}.{}", callee_path(object)?, field))
        }
        _ => None,
    }
}

#[must_use]
pub fn formatting(source: &str) -> Json {
    let Ok(formatted) = format_source(source) else {
        return json!([]);
    };
    if formatted == source {
        return json!([]);
    }
    let line_count = source.lines().count();
    let last_line = line_count.saturating_sub(1);
    let last_col = source
        .lines()
        .last()
        .map(|l| l.chars().count())
        .unwrap_or(0);
    let end = if source.ends_with('\n') {
        json!({ "line": line_count, "character": 0 })
    } else {
        json!({ "line": last_line, "character": last_col })
    };
    json!([{
        "range": {
            "start": { "line": 0, "character": 0 },
            "end": end
        },
        "newText": formatted
    }])
}

fn dotted_prefix(source: &str, lsp_line: usize, lsp_col: usize) -> String {
    let line = source.lines().nth(lsp_line).unwrap_or("");
    let chars: Vec<char> = line.chars().collect();
    let col = lsp_col.min(chars.len());
    let mut i = col;
    while i > 0 {
        let c = chars[i - 1];
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            i -= 1;
        } else {
            break;
        }
    }
    chars[i..col].iter().collect()
}

fn dotted_name_at(source: &str, lsp_line: usize, lsp_col: usize) -> String {
    let line = source.lines().nth(lsp_line).unwrap_or("");
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let mut col = lsp_col.min(chars.len());
    if col < chars.len() && (chars[col].is_ascii_alphanumeric() || chars[col] == '_') {
        while col < chars.len() && (chars[col].is_ascii_alphanumeric() || chars[col] == '_') {
            col += 1;
        }
    }
    dotted_prefix(source, lsp_line, col)
}

fn split_dotted(s: &str) -> (String, String) {
    if let Some(i) = s.rfind('.') {
        (s[..i].to_string(), s[i + 1..].to_string())
    } else {
        (String::new(), s.to_string())
    }
}

fn looks_like_import_path(path: &str) -> bool {
    path.ends_with(".tive") || path.contains('/') || path.contains('\\') || path.contains('.')
}

fn token_span(t: &Token) -> (usize, usize) {
    let start = t.column;
    let len = t.value.chars().count().max(1);
    (start, start + len)
}

fn token_at(tokens: &[Token], line: usize, col: usize) -> Option<&Token> {
    let covers: Vec<&Token> = tokens
        .iter()
        .filter(|t| {
            if t.line != line {
                return false;
            }
            let (start, end) = token_span(t);
            col >= start && col < end
        })
        .collect();
    covers
        .iter()
        .copied()
        .find(|t| matches!(t.kind, TokenKind::Identifier | TokenKind::StringLiteral))
        .or_else(|| covers.into_iter().next())
        .or_else(|| {
            tokens.iter().rev().find(|t| {
                if t.line != line {
                    return false;
                }
                let (_start, end) = token_span(t);
                col == end && matches!(t.kind, TokenKind::Identifier | TokenKind::StringLiteral)
            })
        })
}

fn location_json(uri: &str, line: usize, character: usize) -> Json {
    json!({
        "uri": uri,
        "range": {
            "start": { "line": line, "character": character },
            "end": { "line": line, "character": character }
        }
    })
}

fn publish_diags(out: &mut impl Write, uri: &str, text: &str) -> io::Result<()> {
    let diags: Vec<Json> = diagnostics(text, uri)
        .into_iter()
        .map(|(line, col, message)| {
            let sl = line.saturating_sub(1);
            let sc = col.saturating_sub(1);
            json!({
                "range": {
                    "start": { "line": sl, "character": sc },
                    "end": { "line": sl, "character": sc + 1 }
                },
                "severity": 1,
                "source": "Optive",
                "message": message
            })
        })
        .collect();
    write_rpc(
        out,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diags }
        }),
    )
}

fn read_rpc(reader: &mut impl BufRead) -> io::Result<Option<Json>> {
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        headers.push_str(&line);
    }
    let mut len = 0usize;
    for h in headers.lines() {
        let h = h.trim_end_matches('\r');
        if let Some(rest) = h.strip_prefix("Content-Length:") {
            len = rest.trim().parse().unwrap_or(0);
        }
    }
    if len == 0 {
        return Ok(None);
    }
    let mut buf = vec![0u8; len];
    std::io::Read::read_exact(reader, &mut buf)?;
    Ok(serde_json::from_slice(&buf).ok())
}

fn write_rpc(out: &mut impl Write, v: Json) -> io::Result<()> {
    let body = serde_json::to_vec(&v)?;
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_reports_parse_error() {
        let diags = diagnostics("let x =", "x.tive");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn diagnostics_clean_source() {
        assert!(diagnostics("let x = 1\n", "x.tive").is_empty());
    }

    #[test]
    fn definition_same_file_func() {
        let src = "func add(a, b) { a + b }\nadd(1, 2)\n";
        let loc = definition(src, "file:///tmp/x.tive", 1, 0);
        assert_eq!(loc["uri"], "file:///tmp/x.tive");
        assert_eq!(loc["range"]["start"]["line"], 0);
    }

    #[test]
    fn completion_keywords_and_func() {
        let src = "func add(a, b) { a + b }\n";
        let items = completion(src, 1, 0);
        let labels: Vec<&str> = items
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["label"].as_str())
            .collect();
        assert!(labels.contains(&"func"));
        assert!(labels.contains(&"add"));
        assert!(labels.contains(&"print"));
    }

    #[test]
    fn completion_local_param() {
        let src = "func add(left, right) {\n  le\n}\n";
        let items = completion(src, 1, 4);
        let labels: Vec<&str> = items
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["label"].as_str())
            .collect();
        assert!(labels.contains(&"left"), "{labels:?}");
    }

    #[test]
    fn completion_std_http_dot() {
        let src = "std.http.";
        let items = completion(src, 0, src.len());
        let labels: Vec<&str> = items
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["label"].as_str())
            .collect();
        assert!(labels.contains(&"get"));
        assert!(labels.contains(&"serve"));
    }

    #[test]
    fn hover_func_signature() {
        let src = "func add(a, b) { a + b }\nadd(1, 2)\n";
        let hv = hover(src, 1, 0);
        assert_eq!(hv["contents"]["value"], "func add(a, b)");
    }

    #[test]
    fn hover_unknown_is_null() {
        let hv = hover("1 + 2\n", 0, 0);
        assert!(hv.is_null());
    }

    #[test]
    fn signature_help_user_func() {
        let src = "func add(a, b) { a + b }\nadd(1, \n";
        let sh = signature_help(src, 1, 7);
        assert_eq!(sh["signatures"][0]["label"], "func add(a, b)");
        assert_eq!(sh["activeParameter"], 1);
        let label = sh["signatures"][0]["label"].as_str().unwrap();
        let a0 = sh["signatures"][0]["parameters"][0]["label"][0]
            .as_u64()
            .unwrap() as usize;
        let a1 = sh["signatures"][0]["parameters"][0]["label"][1]
            .as_u64()
            .unwrap() as usize;
        let chars: Vec<char> = label.chars().collect();
        assert_eq!(chars[a0..a1].iter().collect::<String>(), "a");
    }

    #[test]
    fn signature_help_std_http() {
        let src = "std.http.get(\n";
        let sh = signature_help(src, 0, 13);
        assert_eq!(sh["signatures"][0]["label"], "std.http.get(url, opts?)");
        assert_eq!(sh["activeParameter"], 0);
    }

    #[test]
    fn signature_help_struct_ctor() {
        let src = "struct Point { let x let y }\nPoint(1, \n";
        let sh = signature_help(src, 1, 8);
        assert_eq!(sh["signatures"][0]["label"], "Point(x, y)");
        assert_eq!(sh["activeParameter"], 1);
    }

    #[test]
    fn inlay_hints_param_names() {
        let src = "func greet(name) { name }\ngreet(\"hi\")\n";
        let hints = inlay_hints(src, "file:///x.tive", &HashMap::new());
        let labels: Vec<&str> = hints
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h["label"].as_str())
            .collect();
        assert!(labels.contains(&"name ="), "{hints}");
    }

    #[test]
    fn document_symbols_lists_func() {
        let src = "func add(a, b) { a + b }\n";
        let syms = document_symbols(src, "file:///x.tive");
        let names: Vec<&str> = syms
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["name"].as_str())
            .collect();
        assert!(names.contains(&"add"));
    }

    #[test]
    fn formatting_indents() {
        let edits = formatting("func add(a,b){return a+b}\n");
        let text = edits[0]["newText"].as_str().unwrap();
        assert!(text.contains("func add(a, b)"));
    }

    #[test]
    fn hover_and_def_local_var() {
        let src = "let count = 1\ncount + 2\n";
        let hv = hover(src, 1, 0);
        assert_eq!(hv["contents"]["value"], "let count: num");
        let loc = definition(src, "file:///tmp/x.tive", 1, 0);
        assert_eq!(loc["range"]["start"]["line"], 0);
    }

    #[test]
    fn definition_struct_field() {
        let src = "struct Point { let x let y }\nlet p = Point(1, 2)\np.x\n";
        let loc = definition(src, "file:///tmp/x.tive", 2, 2);
        assert_eq!(loc["range"]["start"]["line"], 0);
        let hv = hover(src, 2, 2);
        assert_eq!(hv["contents"]["value"], "Point.x");
    }

    #[test]
    fn completion_inferred_struct_fields() {
        let src = "struct Point { let x let y }\nlet p = Point(1, 2)\np.\n";
        let items = completion(src, 2, 2);
        let labels: Vec<&str> = items
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["label"].as_str())
            .collect();
        assert!(labels.contains(&"x"), "{labels:?}");
        assert!(labels.contains(&"y"), "{labels:?}");
    }

    fn ws_docs() -> (HashMap<String, String>, String, String) {
        let lib_uri = "file:///tmp/ws/lib.tive".to_string();
        let main_uri = "file:///tmp/ws/main.tive".to_string();
        let mut docs = HashMap::new();
        docs.insert(
            lib_uri.clone(),
            "func greet(name) { name }\nintern func hidden() { 1 }\n".into(),
        );
        docs.insert(
            main_uri.clone(),
            "use \"lib.tive\".{ greet }\ngreet(\"hi\")\n".into(),
        );
        (docs, lib_uri, main_uri)
    }

    #[test]
    fn definition_use_jumps_to_other_file() {
        let (docs, lib_uri, main_uri) = ws_docs();
        let main = docs.get(&main_uri).unwrap();
        let loc = definition_in(main, &main_uri, 1, 0, &docs);
        assert_eq!(loc["uri"], lib_uri);
        assert_eq!(loc["range"]["start"]["line"], 0);
    }

    #[test]
    fn completion_import_module_exports() {
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
        let labels: Vec<&str> = items
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["label"].as_str())
            .collect();
        assert!(labels.contains(&"greet"), "{labels:?}");
        assert!(!labels.contains(&"hidden"), "{labels:?}");
    }

    #[test]
    fn definition_import_member() {
        let lib_uri = "file:///tmp/ws/lib.tive";
        let main_uri = "file:///tmp/ws/main.tive";
        let mut docs = HashMap::new();
        docs.insert(lib_uri.to_string(), "func greet(name) { name }\n".into());
        let main = "import \"lib.tive\" as lib\nlib.greet(\"hi\")\n";
        docs.insert(main_uri.to_string(), main.into());
        let loc = definition_in(main, main_uri, 1, 4, &docs);
        assert_eq!(loc["uri"], lib_uri, "{loc}");
        assert_eq!(loc["range"]["start"]["line"], 0);
    }
}
