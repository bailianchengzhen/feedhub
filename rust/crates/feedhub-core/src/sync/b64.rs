//! base64url（无填充）——**口径同 `java.util.Base64.getUrlEncoder().withoutPadding()` /
//! `getUrlDecoder()`**。
//!
//! 复制一份（而不是直接用 `base64` crate 的 `URL_SAFE_NO_PAD`），是因为 JDK 的解码器有几条自己的规矩，
//! 而同步文件和二维码里的每一个字段都要和 Kotlin 那边**互认**：
//!
//! | 输入 | JDK url decoder | `base64` 的 `URL_SAFE_NO_PAD` |
//! |---|---|---|
//! | 带 `=` 填充（`AA==`、`AAA=`） | **接受** | 拒绝 |
//! | 长度 1（`A`） | 拒绝 | 拒绝 |
//! | 长度 2 / 3 | 接受（1 / 2 字节） | 接受 |
//! | `+` `/` | 拒绝（URL 字母表） | 拒绝 |
//! | 空白 / 换行 | 拒绝 | 拒绝 |
//!
//! 所以编码用 crate、解码自己写（就一张表，30 行）。**错误一律 `None`**（Kotlin 那边是
//! `runCatching { }.getOrNull()`），调用方不需要区分"填充不对"和"字符不对"。

use base64::Engine as _;

const URL_SAFE_NO_PAD: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    base64::engine::GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
);

/// 编码成 base64url，**不带**填充（`SyncCrypto.base64Url`）。
pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 解码 base64url，认 `=` 填充也认不带填充；任何一处不对返回 `None`
/// （`SyncCrypto.base64UrlDecode` 的 `IllegalArgumentException` → `None`）。
pub fn decode(text: &str) -> Option<Vec<u8>> {
    // JDK 的 URL 解码器不认 `+` `/`（那是 standard 字母表）；`base64` 的 URL_SAFE 也不认，
    // 但它的 `Indifferent` 填充模式只放宽 `=`，所以这里可以直接用。
    // 全是 `=` 的一串不是合法 base64（JDK 拒；`trim_end_matches` 会把它整段吃掉，所以先单独挡掉）
    if text.is_empty() {
        return Some(Vec::new());
    }
    if text.bytes().all(|b| b == b'=') {
        return None;
    }
    let trimmed = text.trim_end_matches('=');
    // JDK：长度 %4 == 1 直接拒（`A`、`AAAAA`）；其余按 4 的倍数补齐后解。
    if trimmed.len() % 4 == 1 {
        return None;
    }
    // 填充字符只允许在末尾（`AA=A` 这种中间有 `=` 的 JDK 也拒）；上面的 trim_end 已经保证
    // 剩下的部分里不能再有 `=`。
    if trimmed.contains('=') {
        return None;
    }
    URL_SAFE_NO_PAD.decode(trimmed).ok()
}

