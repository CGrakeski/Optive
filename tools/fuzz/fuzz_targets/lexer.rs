//noinspection RsDetachedFile
//! cargo-fuzz crate root (`[[bin]]` in `tools/fuzz/Cargo.toml`), not an optive module.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let _ = optive::tokenize(&s);
});
