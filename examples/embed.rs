//! Host embedding example. Prefer `optive::embed` over `Vm` fields.

fn main() {
    let mut engine = optive::embed::Engine::builder().workers(1).build();
    engine.register_host("host_id", |_vm, args| {
        Ok(args.first().cloned().unwrap_or(optive::value::Value::None))
    });
    match engine.eval("print(1 + 2)") {
        Ok(v) => println!("{}", v.display_string()),
        Err(e) => eprintln!("{e}"),
    }
}
