//! SM3 杂凑算法（GB/T 32905-2016），字节进、32 字节摘要出。
//!
//! 对应 `source-douyin` 的 `Sm3.kt`。a_bogus 里 SM3 用了三次（query+salt 两次、UA 的 Base64 结果一次）。
//! 仓库规矩是不许自己实现密码学原语）。
//!
//! 正确性靠**独立来源**：`douyin::tests` 里用 GB/T 32905-2016 附录 A 的两条标准测试向量
//! （和 Kotlin `Sm3Test` 用的是同一对定值）。

use sm3::{Digest, Sm3};

/// 摘要（32 字节）。
pub fn digest(message: &[u8]) -> [u8; 32] {
    Sm3::digest(message).into()
}

/// 两次 SM3（a_bogus 对 query / body 用的就是两次）。
pub fn digest_twice(message: &[u8]) -> [u8; 32] {
    digest(&digest(message))
}

/// 十六进制小写（测试 / 排障用）。
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
