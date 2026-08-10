#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
use optive::vm::Vm;

const fn assert_send<T: Send>() {}

#[test]
fn vm_is_send() {
    assert_send::<Vm>();
}
