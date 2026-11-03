//! SHA-256（小写十六进制）：只给抖音扫码登录的 `p_no` 用。
//!
//! `p_no` 是 `p_*` 那几项拼起来再 SHA-256（Kotlin `MessageDigest.getInstance("SHA-256")`）。
//! 这里只留两个薄包装，FIPS 180-4 的算例（`abc` / 空串 / 448 位边界）照旧在测试里钉死。
//!
//! 口径：UTF-8 字节 → 标准 SHA-256 → 小写十六进制（和 `MessageDigest` + `toHex()` 一样）。

use sha2::Digest;

/// `text` 的 UTF-8 字节的 SHA-256，小写十六进制。
pub fn hex(text: &str) -> String {
    let digest = sha256(text.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 原始 32 字节摘要。
pub fn sha256(data: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(data).into()
}

