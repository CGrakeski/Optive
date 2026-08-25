#![allow(clippy::unwrap_used, clippy::expect_used)]

#[test]
fn embed_engine_eval() {
    let mut eng = optive::embed::Engine::new();
    let v = eng.eval("40 + 2").unwrap();
    assert_eq!(v.display_string(), "42");
}

#[test]
fn embed_host_function() {
    let mut eng = optive::embed::Engine::new();
    eng.register_host("ping", |_vm, _args| Ok(optive::value::Value::text("pong")));
    let v = eng.eval("ping()").unwrap();
    assert_eq!(v.display_string(), "\"pong\"");
}

#[test]
fn reset_script_bindings_allows_const_rerun() {
    let mut vm = optive::vm::Vm::new();
    let src = "const let FROM = 2\nFROM + 40\n";
    let first = optive::run_source_in_vm(&mut vm, src, "<rerun>").unwrap();
    assert_eq!(first.display_string(), "42");
    vm.reset_script_bindings();
    let second = vm.run().unwrap();
    assert_eq!(second.display_string(), "42");
}
