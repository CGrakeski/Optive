#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use optive::run_source;

fn assert_under(src: &str, ms: u128) {
    let t0 = Instant::now();
    run_source(src).expect(src);
    let elapsed = t0.elapsed().as_millis();
    assert!(
        elapsed < ms,
        "kernel `{src}` took {elapsed}ms, ceiling {ms}ms"
    );
}

#[test]
fn regression_empty_and_arith_loops() {
    assert_under("loop (20000) { }\n42\n", 5_000);
    assert_under("let sum = 0\nloop (20000) { sum = sum + 1 }\nsum\n", 5_000);
}
