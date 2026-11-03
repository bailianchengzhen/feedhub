//! UniFFI 的绑定生成器。用 `--library` 模式直接从编好的 `.so` 里读元数据，
//! 所以**不需要 `cargo install uniffi-bindgen`**（那要再编一套完整依赖）。
//!
//! 用法（Gradle 的 `:rust-bridge:generateRustBindings` 就是这一行）：
//! `cargo run --release -p uniffi-bindgen -- generate --library <lib.so> --language kotlin --out-dir <dir> --no-format`
//!
//! 注意：`--library` 模式读 `.so` 里的 `UNIFFI_META_*` 符号，所以 **release profile 不能 `strip = "symbols"`**
//! （否则它 exit 0 但一个文件都不生成）。现在是 `strip = "debuginfo"`，见 `rust/Cargo.toml`。
fn main() {
    uniffi::uniffi_bindgen_main()
}
