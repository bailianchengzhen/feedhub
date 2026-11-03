//! 小黑盒网页接口的路径签名（`hkey` / `_time` / `nonce`）。
//!
//! 逐条对照 `source-heybox/HkeySigner.kt`（上游是 xiaoheihe-cli `xhh_operator/signing.py`，MIT License，
//! Copyright (c) 2025 Li Fangyu）。只签路径（不含 query），每个请求重新算。
//!
//! 和 Kotlin 实现只有一处刻意的差别：**`newNonce()` 留在 Kotlin**（要 `SecureRandom`），Rust 侧只收 nonce；
//! 一次调用里的多个请求由 [`NonceLedger`] 从这个 nonce 派生出各自的一个。
//! 签名本身是纯函数，时间戳和 nonce 都由调用方给，所以能用定值回归。

/// 32 个字符的替换表（`signing.py` 的 `alphabet`）。
const ALPHABET: &str = "AB45STUVWZEFGJ6CH01D237IXYPQRKLMN89";

/// `"/bbs//app/feeds"` → `"/bbs/app/feeds/"`（签名用的形式，末尾总带 `"/"`）。
///
/// 和 Kotlin 一样：去掉空段、前面补一个 `/`、末尾补一个 `/`。所以 `"/"` → `"//"`。
pub fn normalize_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    format!("/{}/", parts.join("/"))
}

/// 纯函数：给定路径、时间戳（秒）和 nonce 算 hkey。
pub fn hkey(path: &str, timestamp: i64, nonce: &str) -> String {
    let short = &ALPHABET[..ALPHABET.len() - 2]; // Kotlin 的 dropLast(2)
    let parts = [
        map_chars(&(timestamp.wrapping_add(1)).to_string(), short),
        map_chars(&normalize_path(path), ALPHABET),
        map_chars(nonce, ALPHABET),
    ];
    let max = parts.iter().map(|p| p.len()).max().unwrap_or(0);
    let mut seed = Vec::with_capacity(max * 3);
    for i in 0..max {
        for p in &parts {
            if let Some(c) = p.get(i) {
                seed.push(*c);
            }
        }
    }
    seed.truncate(20);
    let digest = md5_hex(&seed);
    let bytes = digest.as_bytes();
    let tail = &bytes[digest.len() - 6..digest.len() - 2];
    let (a, b, c, d) = (tail[0], tail[1], tail[2], tail[3]);
    let mixed = [
        x13(a) ^ x11(b) ^ x9(c) ^ x3(d),
        x3(a) ^ x13(b) ^ x11(c) ^ x9(d),
        x9(a) ^ x3(b) ^ x13(c) ^ x11(d),
        x11(a) ^ x9(b) ^ x3(c) ^ x13(d),
    ];
    // 原 JS 实现把摘要最后两个字节也算进校验和（Kotlin 一样）
    let checksum =
        (mixed.iter().map(|v| *v as i64).sum::<i64>() + bytes[digest.len() - 2..].iter().map(|b| *b as i64).sum::<i64>())
            % 100;
    let head = map_chars(&digest[..5], &ALPHABET[..ALPHABET.len() - 4]);
    format!("{}{:02}", String::from_utf8_lossy(&head), checksum)
}

/// 要追加到 query 里的三个签名参数，顺序同 `signing.py`。
pub fn signed_params(path: &str, timestamp: i64, nonce: &str) -> Vec<(String, String)> {
    vec![
        ("hkey".to_string(), hkey(path, timestamp, nonce)),
        ("_time".to_string(), timestamp.to_string()),
        ("nonce".to_string(), nonce.to_string()),
    ]
}

/// Kotlin 的 `mapChars`：**按 UTF-16 码元**遍历（`for (ch in value)` 在 Kotlin 里就是码元），
/// 每个码元 `code % alphabet.length` 取一个字母。用 `encode_utf16` 才能和 Kotlin 逐字节一致
/// （emoji 在 Kotlin 里是两个码元、在 Rust `chars()` 里是一个）。
fn map_chars(value: &str, alphabet: &str) -> Vec<u8> {
    let table = alphabet.as_bytes();
    value
        .encode_utf16()
        .map(|unit| table[(unit as usize) % table.len()])
        .collect()
}

// 对应 signing.py 的 _double / _three / _nine / _eleven / _thirteen。组合方式原样照抄：
// 名字像 GF(2^8) 乘法，但 _nine 等其实不是真正的 ×9，**不要"修正"它**。
fn x2(v: u8) -> u8 {
    ((v << 1) ^ if v & 0x80 != 0 { 0x1b } else { 0 }) & 0xff
}
fn x3(v: u8) -> u8 {
    x2(v) ^ v
}
fn x9(v: u8) -> u8 {
    x3(x2(v))
}
fn x11(v: u8) -> u8 {
    x9(x3(x2(v)))
}
fn x13(v: u8) -> u8 {
    x11(v) ^ x9(v) ^ x3(v)
}

// `md5_hex_bytes` 从 `crate::hosted` 来（小写十六进制）。
use crate::hosted::md5_hex_bytes as md5_hex;

/// **每个请求一个 nonce**。宿主每次调用只给一个随机 nonce（`SecureRandom` 在 Kotlin），
/// 而一次调用可能发好几个请求（详情 = `link/tree` + 表情表 + 最多 8 批游戏卡片）：同一个 nonce 在一秒里发
/// 好几次是很明显的非浏览器特征，服务端做 nonce 去重时后面的请求还会被拒。
///
/// 做法：宿主给的 nonce **第一次**用就是它本身；同一个 nonce 再要发请求时回头问宿主要一个新的
/// 没接宿主的（纯 Rust 测试）退回 `MD5(nonce + ":" + n)` 的 32 位大写十六进制（格式和宿主生成的一样）。
/// 只记最近 [`NonceLedger::KEEP`] 个 nonce，旧的自然忘掉。
#[derive(Default)]
pub struct NonceLedger {
    seeds: std::sync::Mutex<std::collections::VecDeque<(String, u32)>>,
    host: std::sync::Mutex<Option<std::sync::Arc<dyn NonceSource>>>,
}

/// 宿主的 nonce 来源（`SecureRandom` 在宿主）：`timestamp` 是这次签名用的秒级时间戳（生成时要拼进去）。
pub trait NonceSource: Send + Sync {
    fn fresh(&self, timestamp: i64) -> String;
}

impl NonceLedger {
    const KEEP: usize = 32;

    pub fn set_host(&self, host: std::sync::Arc<dyn NonceSource>) {
        *self.host.lock().unwrap_or_else(|e| e.into_inner()) = Some(host);
    }

    /// 用 `seed` 在 `timestamp` 发下一个请求时该带的 nonce。
    pub fn next(&self, seed: &str, timestamp: i64) -> String {
        let mut seeds = self.seeds.lock().unwrap_or_else(|e| e.into_inner());
        let used = match seeds.iter_mut().find(|(s, _)| s == seed) {
            Some((_, n)) => {
                *n += 1;
                *n
            }
            None => {
                if seeds.len() >= Self::KEEP {
                    seeds.pop_front();
                }
                seeds.push_back((seed.to_string(), 0));
                0
            }
        };
        drop(seeds);
        if used == 0 {
            return seed.to_string();
        }
        match self.host.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            Some(host) => host.fresh(timestamp),
            None => md5_hex(format!("{seed}:{used}").as_bytes()).to_ascii_uppercase(),
        }
    }
}

