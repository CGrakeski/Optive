use optive::vm::Vm;

fn assert_send<T: Send>() {}

#[test]
fn vm_is_send() {
    assert_send::<Vm>();
}
