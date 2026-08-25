use std::collections::BTreeSet;

use optive::api_registry::{BUILTINS, STD_EXPORTS, STD_MODULES};
use optive::value::Value;
use optive::vm::Vm;

fn registry_std() -> BTreeSet<(String, String)> {
    STD_EXPORTS
        .iter()
        .map(|(module, export)| ((*module).to_string(), (*export).to_string()))
        .collect()
}

fn runtime_std() -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    let std = optive::std_modules::build_std_module();
    let std = std.borrow();
    let modules = std.children.keys().cloned().collect();
    let mut exports: BTreeSet<(String, String)> = std
        .exports
        .keys()
        .map(|name| (String::new(), name.clone()))
        .collect();
    for (module, child) in &std.children {
        exports.extend(
            child
                .borrow()
                .exports
                .keys()
                .map(|name| (module.clone(), name.clone())),
        );
    }
    (modules, exports)
}

#[test]
fn runtime_std_exports_and_registry_are_bidirectionally_equal() {
    let (runtime_modules, runtime_exports) = runtime_std();
    let registry_modules: BTreeSet<String> =
        STD_MODULES.iter().map(|name| (*name).to_string()).collect();
    let registry_exports = registry_std();

    let missing_modules: Vec<_> = runtime_modules
        .difference(&registry_modules)
        .cloned()
        .collect();
    let phantom_modules: Vec<_> = registry_modules
        .difference(&runtime_modules)
        .cloned()
        .collect();
    let missing_exports: Vec<_> = runtime_exports
        .difference(&registry_exports)
        .cloned()
        .collect();
    let phantom_exports: Vec<_> = registry_exports
        .difference(&runtime_exports)
        .cloned()
        .collect();
    assert!(
        missing_modules.is_empty()
            && phantom_modules.is_empty()
            && missing_exports.is_empty()
            && phantom_exports.is_empty(),
        "missing modules: {missing_modules:?}\nphantom modules: {phantom_modules:?}\n\
         missing exports: {missing_exports:?}\nphantom exports: {phantom_exports:?}"
    );
}

#[test]
fn public_runtime_globals_and_registry_are_bidirectionally_equal() {
    let vm = Vm::new();
    let runtime: BTreeSet<String> = vm
        .globals
        .keys()
        .into_iter()
        .filter_map(|name| {
            let value = vm.globals.get(&name).expect("global key has value");
            (!name.starts_with("__")
                && (matches!(
                    value,
                    Value::Builtin(_) | Value::Bool(_) | Value::None | Value::Module(_)
                ) || (name == "type" && matches!(value, Value::TypeRef(_)))))
            .then_some(name)
        })
        .collect();
    let registry: BTreeSet<String> = BUILTINS
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();

    let missing: Vec<_> = runtime.difference(&registry).cloned().collect();
    let phantom: Vec<_> = registry.difference(&runtime).cloned().collect();
    assert!(
        missing.is_empty() && phantom.is_empty(),
        "runtime globals missing from registry: {missing:?}\n\
         registry globals missing from runtime: {phantom:?}"
    );
}

#[test]
fn registry_entries_are_visible_to_lsp_completion() {
    fn labels(value: serde_json::Value) -> BTreeSet<String> {
        value
            .as_array()
            .expect("completion array")
            .iter()
            .filter_map(|item| item["label"].as_str().map(str::to_string))
            .collect()
    }

    let globals = labels(optive::lsp::completion("", 0, 0));
    for (name, _) in BUILTINS {
        assert!(globals.contains(*name), "LSP misses global `{name}`");
    }
    let modules = labels(optive::lsp::completion("std.", 0, 4));
    for module in STD_MODULES {
        assert!(
            modules.contains(*module),
            "LSP misses std module `{module}`"
        );
    }
    assert!(
        modules.contains("concat"),
        "LSP misses root export `std.concat`"
    );
    for module in STD_MODULES {
        let source = format!("std.{module}.");
        let got = labels(optive::lsp::completion(&source, 0, source.len()));
        for (_, export) in STD_EXPORTS.iter().filter(|(owner, _)| owner == module) {
            assert!(
                got.contains(*export),
                "LSP misses registry export `std.{module}.{export}`"
            );
        }
    }
}

#[test]
fn every_registry_export_has_lsp_signature_metadata() {
    for (module, export) in STD_EXPORTS {
        assert!(
            optive::api_registry::std_export_sig(module, export).is_some(),
            "missing signature metadata for std.{module}.{export}"
        );
    }
}

#[test]
fn runtime_global_types_are_the_static_diagnostic_source() {
    let vm = Vm::new();
    for name in optive::type_registry::global_type_names() {
        assert!(
            vm.globals.contains_key(name),
            "runtime misses global type `{name}`"
        );
        let source = format!("let value: {name}\n");
        let diags = optive::lsp::diagnostics(&source, "registry-types.tive");
        assert!(
            !diags
                .iter()
                .any(|(_, _, message)| message.contains("undefined name")),
            "static diagnostics miss global type `{name}`: {diags:?}"
        );
    }
}
