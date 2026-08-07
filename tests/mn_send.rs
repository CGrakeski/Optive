use optive::value::Value;
use optive::shared::Shared;
use optive::value::TaskInner;

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

#[test]
fn value_is_send_sync() {
    assert_send::<Value>();
    assert_sync::<Value>();
    assert_send::<Shared<TaskInner>>();
}
