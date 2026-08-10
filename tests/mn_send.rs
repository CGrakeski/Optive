#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
use optive::value::Value;
use optive::shared::Shared;
use optive::value::TaskInner;

const fn assert_send<T: Send>() {}
const fn assert_sync<T: Sync>() {}

#[test]
fn value_is_send_sync() {
    assert_send::<Value>();
    assert_sync::<Value>();
    assert_send::<Shared<TaskInner>>();
}
