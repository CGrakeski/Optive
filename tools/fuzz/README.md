# lexer / parser fuzz

默认 workspace **不**包含本目录：`libfuzzer-sys` 在 Windows 上难编，也不该进日常 `cargo test`。

## CI 冒烟（已接入）

根目录 `cargo test --test fuzz_frontend` 用固定种子 + 时间预算喂 lexer/parser，只要求不 panic / 不卡死。

## 完整 `cargo-fuzz`（Linux / nightly）

```bash
cargo install cargo-fuzz
rustup toolchain install nightly
cd tools/fuzz
cargo +nightly fuzz run lexer
cargo +nightly fuzz run parser
```

目标读任意字节，转成 lossy UTF-8 后调用 `tokenize` / `parse_program`。发现的崩溃放进 `artifacts/`（已 gitignore）。
