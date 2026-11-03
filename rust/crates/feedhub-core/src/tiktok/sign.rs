//! TikTok web 的 HTTP 签名：`X-Dynosaur` + `X-Gnarly`（`X-Bogus` 固定字符串 `1`）。
//!
//! **逐行移植** `source-tiktok/.../TiktokSign.kt`（那一份又是 f2（Apache-2.0）
//! `xgnarly.py` 的逐行移植，常量、编码口径、
//! 自定义 ChaCha 的那几处"笔误"（对角轮第三组是 `(2,7,12,13)`）全部**刻意保持一致**：
//! 一个字不一样，签名就整串不同，平台回的是 HTTP 200 + 空 body + `tt_orcas_res: 1`。
//!
//! 三个要知道的点：
//! - `X-Dynosaur` 的 `0x2E` 是**业务查询串**（只有 query、不含路径）的 FNV 变体 hash_state，
//!   `0x30` 是 User-Agent 的；**签名用的 UA 必须和请求头里的 UA 逐字节相同**；
//! - `X-Gnarly` 封的是**追加了 `X-Dynosaur` 与 `msToken` 之后**的查询串，所以 `X-Bogus=1`
//!   夹在它们之间、不在封印范围内；
//! - `msToken` **原样使用，空字符串也一样，绝不伪造**。
//!
//! 时间戳 / nonce / 随机密钥一律由调用方注入（见 [`sign`]），这里不读时钟、不取随机数。

use std::collections::BTreeMap;

/// 32 位掩码（Kotlin 的 `MASK32`）。
pub const MASK32: u64 = 0xFFFF_FFFF;

/// 四个签名参数在查询串里的顺序（平台固定：业务参数 → Dynosaur → msToken → Bogus → Gnarly）。
pub const DYNOSAUR_PARAM: &str = "X-Dynosaur";
pub const MS_TOKEN_PARAM: &str = "msToken";
pub const BOGUS_PARAM: &str = "X-Bogus";
pub const GNARLY_PARAM: &str = "X-Gnarly";

/// HTTP 请求里 `X-Bogus` 固定是字符串 `"1"`（完整算出来的那套只用在 WebSocket 握手里）。
pub const BOGUS_VALUE: &str = "1";

/// 自定义 base64 字母表（64 字符，标准字母表按位置一一对应）。
pub const ALPHABET: &str = "u09tbS3UvgDEe6r-ZVMXzLpsAohTn7mdINQlW412GqBjfYiyk8JORCF5/xKHwacP";
const STANDARD: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 信封的第一个字节，位于插入了密钥的密文之前。
pub const ENVELOPE_TAG: u8 = 0x4B;

/// ChaCha 状态的第 0～3 个字：不是标准的 `expand 32-byte k`，是 SDK 里的值。
pub const CHACHA_INIT: [u32; 4] = [1196819126, 600974999, 3863347763, 1451689750];

/// FNV-1a 32 位变体：偏移基数非标准，且每个字节额外乘 33。
pub const FNV_OFFSET: u64 = 2166136260;
pub const FNV_PRIME: u64 = 16777619;

/// 载荷里携带的版本号（SDK 2.0.0.561）。**SDK 升级后这两个值和字段表都可能变**。
pub const SDK_VERSION: &str = "5.3.2";
pub const SCM_VERSION: &str = "2.0.0.561";

/// 环境指纹：只有这两个值会被服务器校验（写错没有任何提示）。
pub const ENV_CODE: u16 = 65;
pub const UB_CODE: u16 = 8;

/// `X-Dynosaur` 的 `0x38`：SDK 内部某个 md5 的 hash_state，与环境无关，直接用观测值。
pub const VM_STATE_HASH: u32 = 0xC46CE353;

/// canvas 指纹：`"-1"` = 没有 2d 上下文的页面上报的值。
pub const CANVAS_HASH: &str = "-1";

/// 另外三个模拟环境生成不了的字段（WebGL hash / 组件版本 / device hash），暂时固定 `"0"`。
pub const WEBGL_HASH: &str = "0";
pub const COMPONENT_VERSION: &str = "0";
pub const DEVICE_HASH: &str = "0";

/// SDK 认为自己所在页面的 `location.host + location.pathname`。
pub const PAGE: &str = "www.tiktok.com/";

/// SDK 从 1 开始统计自己的签名次数。
pub const CALL_SEQUENCE_START: u32 = 1;

/// 签名信封里插入的密钥字数。
pub const KEY_WORDS: usize = 12;

/// SDK 的两种字节编码参数 `(xor_base, add_base, pre_xor, rot, post_add)`。
struct Encoder {
    xor_base: u64,
    add_base: u64,
    pre_xor: i64,
    rotate: u32,
    post_add: u64,
}

/// A：除校验和以外的字段。
const ENCODER_A: Encoder = Encoder {
    xor_base: 103,
    add_base: 1,
    pre_xor: -1,
    rotate: 2,
    post_add: 1,
};

/// B：校验和与三个标志位。
const ENCODER_B: Encoder = Encoder {
    xor_base: 102,
    add_base: 0,
    pre_xor: 165,
    rotate: 1,
    post_add: 0,
};

/// 一次签名结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    /// 要原样发送的查询串（业务参数 → Dynosaur → msToken → Bogus → Gnarly）。
    pub query: String,
    pub dynosaur: String,
    pub ms_token: String,
    pub gnarly: String,
}

impl Signed {
    /// `X-Bogus` 永远是 [`BOGUS_VALUE`]。
    pub fn bogus(&self) -> &'static str {
        BOGUS_VALUE
    }

    /// 业务查询串（不含四个签名参数）。
    pub fn business_query(&self) -> &str {
        self.query
            .split_once(&format!("&{DYNOSAUR_PARAM}="))
            .map(|(head, _)| head)
            .unwrap_or(&self.query)
    }
}

// ---------- 查询串编码 ----------

/// 生产路径用的编码：只保留 `A-Za-z0-9-_.~`，其余一律 `%XX`（RFC 3986）。
///
/// 出处：f2 的 `quote(value, safe="")` 与 TiktokApis 的 `quote(value, safe="-_.~")`
/// （见 Kotlin `TiktokSign.rfc3986` 的类说明）。
pub fn rfc3986(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in value.as_bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0xF) as usize] as char);
        }
    }
    out
}

/// f2 的 `_escape`：**只转义浏览器必须转义的字符**，其余（括号、斜杠、冒号、逗号）保持原样。
///
/// 生产路径不用它（用 [`rfc3986`]）；这里留着是为了逐字节复现 f2 的 3 组对拍向量
/// （那份向量里的 query 是 f2 的 `encodeQuery` 出来的）。
pub fn escape(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let mapped = match ch {
            ' ' => Some("%20"),
            '"' => Some("%22"),
            '<' => Some("%3C"),
            '>' => Some("%3E"),
            '`' => Some("%60"),
            '#' => Some("%23"),
            _ => None,
        };
        if let Some(mapped) = mapped {
            out.push_str(mapped);
        } else if ch > ' ' && ch <= '~' {
            out.push(ch);
        } else {
            // Kotlin 是按**单个 char** 转 UTF-8 再逐字节转义的（f2 的低层实现）
            for b in ch.to_string().as_bytes() {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xF) as usize] as char);
            }
        }
    }
    out
}

/// f2 的 `encode_query`：低层编码 + `&` 拼接。**只用于复现它的对拍向量**。
pub fn encode_query(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", escape(k), escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// 生产路径的查询串：业务参数按 [`rfc3986`] 编码，**顺序照调用方给的**（顺序是签名输入的一部分）。
pub fn rfc3986_query(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", rfc3986(v)))
        .collect::<Vec<_>>()
        .join("&")
}

// ---------- 原语 ----------

/// SDK 的 FNV-1a 变体：标准的一轮之后再乘 33。
pub fn hash_state(text: &str) -> u32 {
    let mut value = FNV_OFFSET;
    for &b in text.as_bytes() {
        let step = ((value ^ (b as u64)) * FNV_PRIME) & MASK32;
        value = (step + ((step * 32) & MASK32)) & MASK32;
    }
    value as u32
}

/// 编码一个载荷字段：逐位置打乱字节、填充并带上长度（至少 6 字节，以 `0x00` + 原始长度结尾）。
fn encode_bytes(text: &str, config: &Encoder) -> Vec<u8> {
    // Kotlin 按 UTF-16 码元遍历；这里所有载荷字段都是 ASCII（数字 / 常量），`chars()` 与之一致。
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let size = std::cmp::max(len + 2, 6);
    let mut out = vec![0u8; size];
    for (index, ch) in chars.iter().enumerate() {
        let mut v = ((*ch as u32) as u64 ^ (config.xor_base + index as u64)) & MASK32;
        v = (v + config.add_base + (170 & index as u64)) % 256;
        if config.pre_xor >= 0 {
            v ^= config.pre_xor as u64;
        }
        v = ((v << config.rotate) | (v >> (8 - config.rotate))) & 0xFF;
        out[index] = (((v ^ 187) + config.post_add) % 256) as u8;
    }
    for index in len..size - 2 {
        out[index] = ((221 + index) & 0xFF) as u8;
    }
    out[size - 2] = 0;
    out[size - 1] = (len & 0xFF) as u8;
    out
}

/// 把字段排成平台解析的 TLV 条目（`[key][0][len][value…]`）；`lead_count` 时前面加一字节字段数。
fn pack_payload(fields: &BTreeMap<u8, Vec<u8>>, lead_count: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if lead_count {
        out.push((fields.len() & 0xFF) as u8);
    }
    for (key, value) in fields {
        out.push(*key);
        out.push(0);
        out.push((value.len() & 0xFF) as u8);
        out.extend_from_slice(value);
    }
    out
}

fn be(value: u32, size: usize) -> Vec<u8> {
    (0..size)
        .map(|i| ((value >> (8 * (size - 1 - i))) & 0xFF) as u8)
        .collect()
}

/// 把两个 32 位值折叠为 16 位，再在高位写入环境码。
fn mix(timestamp: u32, nonce: u32, env_code: u16) -> u32 {
    let folded = ((timestamp >> 16) ^ (nonce >> 16) ^ timestamp ^ nonce) & 0xFFFF;
    folded | ((env_code as u32) << 16)
}

/// 校验和的一项：数字按原值异或，字符串按前 4 个 UTF-8 字节（大端）或 0（`mode == 1`）。
#[derive(Clone)]
enum Value<'a> {
    Str(&'a str),
    Num(u64),
}

/// 对字段值做异或折叠，初值为全 1（输出的是普通折叠结果的补码）。
fn checksum(values: &[Value<'_>], mode: u8) -> u32 {
    let mut accumulator = MASK32;
    for value in values {
        let number = match value {
            Value::Num(n) => *n & MASK32,
            Value::Str(s) => {
                if mode == 1 {
                    0
                } else {
                    let bytes = s.as_bytes();
                    let mut n: u64 = 0;
                    for &b in bytes.iter().take(4) {
                        n = (n << 8) | (b as u64);
                    }
                    n
                }
            }
        };
        accumulator ^= number & MASK32;
    }
    (accumulator & MASK32) as u32
}

fn rotl(value: u32, count: u32) -> u32 {
    value.rotate_left(count)
}

fn quarter_round(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = rotl(state[d] ^ state[a], 16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = rotl(state[b] ^ state[c], 12);
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = rotl(state[d] ^ state[a], 8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = rotl(state[b] ^ state[c], 7);
}

/// 生成一个 64 字节的密钥流块。**这不是 ChaCha20**，差异见模块说明。
fn keystream(state: &[u32; 16], rounds: u32) -> [u32; 16] {
    let mut working = *state;
    let mut done = 0;
    loop {
        quarter_round(&mut working, 0, 4, 8, 12);
        quarter_round(&mut working, 1, 5, 9, 13);
        quarter_round(&mut working, 2, 6, 10, 14);
        quarter_round(&mut working, 3, 7, 11, 15);
        done += 1;
        if done >= rounds {
            break;
        }
        quarter_round(&mut working, 0, 5, 10, 15);
        quarter_round(&mut working, 1, 6, 11, 12);
        // 原实现的笔误：第三组是 (2,7,12,13)，标准 ChaCha 是 (2,7,8,13)。**要刻意保持一致**。
        quarter_round(&mut working, 2, 7, 12, 13);
        quarter_round(&mut working, 3, 4, 13, 14);
        done += 1;
        if done >= rounds {
            break;
        }
    }
    let mut out = [0u32; 16];
    for i in 0..16 {
        out[i] = working[i].wrapping_add(state[i]);
    }
    out
}

/// 把载荷按小端 uint32 读出来，与密钥流异或。
fn crypt(key: &[u32], rounds: u32, payload: &[u8]) -> Vec<u8> {
    let mut state = [0u32; 16];
    state[..4].copy_from_slice(&CHACHA_INIT);
    let mut key_words = [0u32; KEY_WORDS];
    for (i, w) in key.iter().take(KEY_WORDS).enumerate() {
        key_words[i] = *w;
    }
    state[4..].copy_from_slice(&key_words);

    let length = payload.len();
    let word_count = (length + 3) / 4;
    let mut words = vec![0u32; word_count];
    for i in 0..word_count {
        let mut w: u32 = 0;
        for j in (0..4).rev() {
            let index = 4 * i + j;
            w = (w << 8) | if index < length { payload[index] as u32 } else { 0 };
        }
        words[i] = w;
    }

    let mut offset = 0usize;
    while offset + 16 < word_count {
        let block = keystream(&state, rounds);
        state[12] = state[12].wrapping_add(1);
        for i in 0..16 {
            words[offset + i] ^= block[i];
        }
        offset += 16;
    }
    let block = keystream(&state, rounds);
    for i in 0..word_count - offset {
        words[offset + i] ^= block[i];
    }

    let mut out = vec![0u8; length];
    for i in 0..word_count {
        for j in 0..4 {
            let index = 4 * i + j;
            if index < length {
                out[index] = ((words[i] >> (8 * j)) & 0xFF) as u8;
            }
        }
    }
    out
}

/// 加密、把密钥插回密文并编码成自定义字母表的 base64。
pub fn seal(payload: &[u8], key: &[u32]) -> String {
    let mut nibbles = 0u64;
    for w in key.iter().take(KEY_WORDS) {
        nibbles += (w & 0xF) as u64;
    }
    let rounds = ((nibbles & 0xF) as u32) + 5;
    let ciphertext = crypt(key, rounds, payload);

    let mut key_bytes = Vec::with_capacity(KEY_WORDS * 4);
    for w in key.iter().take(KEY_WORDS) {
        for j in 0..4 {
            key_bytes.push(((w >> (8 * j)) & 0xFF) as u8);
        }
    }
    let sum: u64 = key_bytes.iter().map(|b| *b as u64).sum::<u64>()
        + ciphertext.iter().map(|b| *b as u64).sum::<u64>();
    let position = (sum % (ciphertext.len() as u64 + 1)) as usize;

    let mut raw = Vec::with_capacity(1 + ciphertext.len() + key_bytes.len());
    raw.push(ENVELOPE_TAG);
    raw.extend_from_slice(&ciphertext[..position]);
    raw.extend_from_slice(&key_bytes);
    raw.extend_from_slice(&ciphertext[position..]);
    to_custom_base64(&raw)
}

/// 标准字母表 base64（带 `=` 补位），再按 [`ALPHABET`] 换字。
fn to_custom_base64(raw: &[u8]) -> String {
    let standard = base64(raw);
    standard
        .chars()
        .map(|ch| match STANDARD.find(ch) {
            Some(i) => ALPHABET.as_bytes()[i] as char,
            None => ch,
        })
        .collect()
}

/// 标准字母表 + `=` 补位的 base64（和 `java.util.Base64.getEncoder()` 一样）。
fn base64(bytes: &[u8]) -> String {
    let a = STANDARD.as_bytes();
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i] as u32;
        let b1 = if i + 1 < bytes.len() { bytes[i + 1] as i32 } else { -1 };
        let b2 = if i + 2 < bytes.len() { bytes[i + 2] as i32 } else { -1 };
        out.push(a[(b0 >> 2) as usize] as char);
        out.push(a[(((b0 & 3) << 4) | (if b1 < 0 { 0 } else { (b1 as u32) >> 4 })) as usize] as char);
        if b1 < 0 {
            out.push_str("==");
        } else {
            let b1 = b1 as u32;
            out.push(a[(((b1 & 15) << 2) | (if b2 < 0 { 0 } else { (b2 as u32) >> 6 })) as usize] as char);
            if b2 < 0 {
                out.push('=');
            } else {
                out.push(a[(b2 as u32 & 63) as usize] as char);
            }
        }
        i += 3;
    }
    out
}

/// 小写十六进制 MD5。
// `md5_hex` / `md5_hex_bytes` 从 `crate::hosted` 来（小写十六进制）；`pub use` 保留老路径。
pub use crate::hosted::{md5_hex, md5_hex_bytes};

// ---------- 载荷 ----------

/// 环境报告，共 25 个 TLV 字段（键 `0x20`～`0x38`）。
fn dynosaur_payload(
    query: &str,
    user_agent: &str,
    timestamp: i64,
    nonce: u32,
    sequence: u32,
    env_code: u16,
    ub_code: u16,
) -> Vec<u8> {
    let a = |text: String| encode_bytes(&text, &ENCODER_A);
    let mut fields: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    fields.insert(0x21, encode_bytes("1", &ENCODER_B));
    fields.insert(0x22, encode_bytes("1", &ENCODER_B));
    fields.insert(0x23, a("0".to_string()));
    fields.insert(0x24, a(mix(timestamp as u32, nonce, env_code).to_string()));
    fields.insert(0x25, a(sequence.to_string()));
    fields.insert(0x26, a(env_code.to_string()));
    fields.insert(0x27, a(timestamp.to_string()));
    fields.insert(0x28, a(WEBGL_HASH.to_string()));
    fields.insert(0x29, a("0".to_string()));
    fields.insert(0x2A, a(SDK_VERSION.to_string()));
    fields.insert(0x2B, be(hash_state(""), 4));
    fields.insert(0x2C, a(CANVAS_HASH.to_string()));
    fields.insert(0x2D, a("0".to_string()));
    fields.insert(0x2E, be(hash_state(query), 4));
    fields.insert(0x2F, a(sequence.to_string()));
    fields.insert(0x30, be(hash_state(user_agent), 4));
    fields.insert(0x31, a(SCM_VERSION.to_string()));
    fields.insert(0x32, a(COMPONENT_VERSION.to_string()));
    fields.insert(0x33, a(DEVICE_HASH.to_string()));
    fields.insert(0x34, a(nonce.to_string()));
    fields.insert(0x35, a(PAGE.to_string()));
    fields.insert(0x36, a(ub_code.to_string()));
    fields.insert(0x37, a("0".to_string()));
    fields.insert(0x38, be(VM_STATE_HASH, 4));
    // 先占位：任何单字符字符串都行（单字符字段的第 1 个字节是固定填充字节），SDK 用的是 "0"。
    fields.insert(0x20, a("0".to_string()));
    let mut checksum = 0u8;
    for value in fields.values() {
        checksum ^= value[1];
    }
    fields.insert(0x20, encode_bytes(&checksum.to_string(), &ENCODER_B));
    pack_payload(&fields, false)
}

/// 请求封印的字段表，共 16 项（**没有 `0x07`**）。
#[allow(clippy::too_many_arguments)]
fn gnarly_payload(
    signed_query: &str,
    user_agent: &str,
    body: &[u8],
    timestamp: i64,
    nonce: u32,
    nonce2: u32,
    sequence: u32,
    env_code: u16,
    ub_code: u16,
) -> Vec<u8> {
    let query_md5 = md5_hex(signed_query);
    let body_md5 = md5_hex_bytes(body);
    let agent_md5 = md5_hex(user_agent);
    let mixed = mix(timestamp as u32, nonce, env_code);

    let covered = [
        Value::Num(0),
        Value::Num(env_code as u64),
        Value::Num(ub_code as u64),
        Value::Str(&query_md5),
        Value::Str(&body_md5),
        Value::Str(&agent_md5),
        Value::Num(timestamp as u64),
        Value::Num(0),
        Value::Num(nonce as u64),
        Value::Str(SDK_VERSION),
        Value::Str(SCM_VERSION),
        Value::Num(CALL_SEQUENCE_START as u64),
        Value::Num(sequence as u64),
        Value::Num(sequence as u64),
        Value::Num(mixed as u64),
        Value::Num(nonce2 as u64),
    ];
    let first = checksum(&covered, 2);
    let mut with_first: Vec<Value<'_>> = covered.to_vec();
    with_first.push(Value::Num(first as u64));
    let second = checksum(&with_first, 1);

    let mut fields: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    fields.insert(0x00, be(second, 4));
    fields.insert(0x01, be(env_code as u32, 2));
    fields.insert(0x02, be(ub_code as u32, 2));
    fields.insert(0x03, query_md5.into_bytes());
    fields.insert(0x04, body_md5.into_bytes());
    fields.insert(0x05, agent_md5.into_bytes());
    fields.insert(0x06, be(timestamp as u32, 4));
    fields.insert(0x08, be(nonce, 4));
    fields.insert(0x09, SDK_VERSION.as_bytes().to_vec());
    fields.insert(0x0A, SCM_VERSION.as_bytes().to_vec());
    fields.insert(0x0B, be(CALL_SEQUENCE_START, 2));
    fields.insert(0x0C, be(sequence, 2));
    fields.insert(0x0D, be(sequence, 2));
    fields.insert(0x0E, be(mixed, 4));
    fields.insert(0x0F, be(nonce2, 4));
    fields.insert(0x10, be(first, 4));
    pack_payload(&fields, true)
}

/// 为一个请求签名。[`query`] 是**已经编码好**的业务查询串（不含四个签名参数）——
/// 两个签名覆盖的就是这串字节，**签名之后不能再重新编码**。
///
/// 时间戳 / nonce / 两组密钥都由调用方注入（生产从 `TiktokSource` 的时钟与 `TiktokRandom` 来，
/// 测试注入固定值），这里不读时钟、不取随机数。
#[allow(clippy::too_many_arguments)]
pub fn sign(
    query: &str,
    user_agent: &str,
    ms_token: &str,
    body: &[u8],
    timestamp: i64,
    nonce: u32,
    nonce2: u32,
    sequence: u32,
    key: &[u32],
    key2: &[u32],
) -> Signed {
    sign_with_env(
        query,
        user_agent,
        ms_token,
        body,
        timestamp,
        nonce,
        nonce2,
        sequence,
        key,
        key2,
        ENV_CODE,
        UB_CODE,
    )
}

/// [`sign`] 的完整版（`envCode` / `ubCode` 可换；生产固定 65 / 8，测试用来复现对拍向量）。
#[allow(clippy::too_many_arguments)]
pub fn sign_with_env(
    query: &str,
    user_agent: &str,
    ms_token: &str,
    body: &[u8],
    timestamp: i64,
    nonce: u32,
    nonce2: u32,
    sequence: u32,
    key: &[u32],
    key2: &[u32],
    env_code: u16,
    ub_code: u16,
) -> Signed {
    let dynosaur = seal(
        &dynosaur_payload(query, user_agent, timestamp, nonce, sequence, env_code, ub_code),
        key,
    );
    // 封印覆盖追加了 X-Dynosaur 与 msToken 之后的查询串。
    let sealed_query = format!("{query}&{DYNOSAUR_PARAM}={dynosaur}&{MS_TOKEN_PARAM}={ms_token}");
    let gnarly = seal(
        &gnarly_payload(
            &sealed_query,
            user_agent,
            body,
            timestamp,
            nonce,
            nonce2,
            sequence,
            env_code,
            ub_code,
        ),
        key2,
    );
    let signed = format!(
        "{sealed_query}&{BOGUS_PARAM}={BOGUS_VALUE}&{GNARLY_PARAM}={gnarly}"
    );
    Signed {
        query: signed,
        dynosaur,
        ms_token: ms_token.to_string(),
        gnarly,
    }
}

/// f2 的 `_clock_nonce`：由微秒时钟得到的 32 位值（这里是毫秒时钟 ×1000）。
pub fn clock_nonce(now_ms: i64) -> u32 {
    ((now_ms as i128 * 1000) & (MASK32 as i128)) as u32
}

