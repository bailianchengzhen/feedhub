//! 贴吧 App 的签名（`source-tieba/TiebaSign.kt`）。
//!
//! 参数按 key 排序（同 key 再按 value），拼成无分隔的 `k=v` 串，末尾加盐，取 md5（小写十六进制）。
//! 值用原文（不做 URL 编码）。移植自 aiotieba `helper/crypto/sign.py` 的 `compute_sign`（Unlicense）；
//! TiebaLite 的 `SortAndSignInterceptor`（GPL-3.0）是同一个算法。
//!
//! **排序是 UTF-16 码元序**（Kotlin 的 `String.compareTo`），不是 Rust 默认的 UTF-8 字节序：
//! 两者只在"代理对字符（emoji）和 U+E000..U+FFFF 的字符同时出现"时不同（UTF-16 里代理对排在它们前面）。
//! 签名值可能是用户输入的搜索词，所以这里照 Kotlin 的口径来。

use std::cmp::Ordering;

use md5::{Digest, Md5};

/// App 侧（表单接口 / protobuf 请求里的普通字段）的盐。
pub const APP_SALT: &str = "tiebaclient!!!";

/// 网页接口（全吧搜索 `/mo/q/search/thread`）的 PC salt，aiotieba `helper/crypto/const.py`。
pub const PC_SALT: &str = "36770b1f34c9bbf2e7d1a99d2b82fa9e";

/// 算签名：`md5(排序后的 "k=v" 拼接 + salt)`，小写十六进制。
pub fn compute(params: &[(String, String)], salt: &str) -> String {
    let mut sorted: Vec<&(String, String)> = params.iter().collect();
    sorted.sort_by(|a, b| compare_utf16(&a.0, &b.0).then_with(|| compare_utf16(&a.1, &b.1)));
    let mut md5 = Md5::new();
    for (key, value) in sorted {
        md5.update(key.as_bytes());
        md5.update(b"=");
        md5.update(value.as_bytes());
    }
    md5.update(salt.as_bytes());
    hex(&md5.finalize())
}

/// 去掉已有的 `sign`，按 key 排好序，末尾追加 `sign`（返回值就是请求里参数的顺序）。
pub fn signed(params: &[(String, String)], salt: &str) -> Vec<(String, String)> {
    let mut rest: Vec<(String, String)> = params
        .iter()
        .filter(|(key, _)| key != "sign")
        .cloned()
        .collect();
    rest.sort_by(|a, b| compare_utf16(&a.0, &b.0).then_with(|| compare_utf16(&a.1, &b.1)));
    let signature = compute(&rest, salt);
    rest.push(("sign".to_string(), signature));
    rest
}

/// Kotlin `String.compareTo`：按 UTF-16 码元逐个比，前缀短的小。
fn compare_utf16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

