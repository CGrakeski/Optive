#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! lexer/parser fuzz 冒烟：固定种子 + 时间预算，不引入 libfuzzer。

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

fn feed(data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    let _ = optive::tokenize(&s);
    let _ = optive::parse_program(&s);
}

#[test]
fn fuzz_lexer_parser_budget() {
    let corpus: &[&[u8]] = &[
        b"",
        b"let x = 1\n",
        b"func f() {",
        b"/* unterminated",
        b"\"abc",
        b"import foo.bar\n",
        &[0xff, 0xfe, 0x00, 0x01],
        b"((((((((",
        b"struct P { let x }\n",
    ];
    for c in corpus {
        let r = catch_unwind(AssertUnwindSafe(|| feed(c)));
        assert!(r.is_ok(), "panic on corpus {c:?}");
    }

    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let start = Instant::now();
    let budget = Duration::from_millis(1500);
    let mut n = 0u32;
    while n < 2000 && start.elapsed() < budget {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let len = (seed % 80) as usize;
        let mut buf = vec![0u8; len];
        for b in &mut buf {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            *b = (seed >> 24) as u8;
        }
        let r = catch_unwind(AssertUnwindSafe(|| feed(&buf)));
        assert!(r.is_ok(), "panic on random input seed={seed} n={n}");
        n += 1;
    }
    assert!(n > 10, "fuzz loop did not run");
}
