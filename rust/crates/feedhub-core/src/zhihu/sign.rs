//! 知乎网页接口的 `x-zse-96` 签名（配合 `x-zse-93: 101_3_3.0`）。
//!
//! 按 Hydrogen `app/src/main/assets/services/api/zse96.lua`（MIT，zhihulite）的 `XZSE96V3` 写：
//! 1. 原文 `101_3_3.0+路径?query+d_c0[+body]` 取小写 md5 hex；
//! 2. 明文 = `[seed, 0] + md5 hex`，PKCS#7 补到 16 的倍数；
//! 3. 首块和 `keyPad`（`059053f7d15e01d7`）、42 异或后过一次分组变换，结果当 IV；其余块用它做 CBC；
//! 4. 密文从尾到头每 3 字节编 4 个字符（每 4 个字节里的第 1 个先异或 58），字母表是自定义的；
//! 5. 加前缀 `2.0_`。
//! 分组变换是 32 轮的类 SM4 结构，轮密钥 `ZK` 和 S 盒 `ZB` 两张表取自网页 JS（和 Hydrogen 的 `mapping` 相同）。
//!
//! 纯函数：**种子（seed）由调用方传进来**，Rust 不生成随机数、不存状态。网页的取法是 `Math.random() * 127` 取整；
//! 服务端只校验 md5，不看 seed。测试里注入固定 seed 才能逐字节比。
//!
//! 签错在本地看不出来（服务端只是拒绝），所以**靠定值测试兜底**：定值是另外两份独立的 JS 实现
//! （RSSHub / MediaCrawler）在 Node 里跑出来的，见 `zhihu/tests.rs`。

pub use crate::hosted::md5_hex;

/// `x-zse-93`：网页接口的版本串，参与签名原文的第一段。
pub const ZSE93: &str = "101_3_3.0";

/// 32 项轮密钥（有符号 32 位写法，Hydrogen 的 `mapping.zk`）。
const ZK: [i32; 32] = [
    1170614578, 1024848638, 1413669199, -343334464, -766094290, -1373058082, -143119608, -297228157,
    1933479194, -971186181, -406453910, 460404854, -547427574, -1891326262, -1679095901, 2119585428,
    -2029270069, 2035090028, -1521520070, -5587175, -77751101, -2094365853, -1243052806, 1579901135,
    1321810770, 456816404, -1391643889, -229302305, 330002838, -788960546, 363569021, -1947871109,
];

/// 256 项 S 盒（Hydrogen 的 `mapping.zb`）。
const ZB: [u8; 256] = [
    20, 223, 245, 7, 248, 2, 194, 209, 87, 6, 227, 253, 240, 128, 222, 91, 237, 9, 125, 157, 230,
    93, 252, 205, 90, 79, 144, 199, 159, 197, 186, 167, 39, 37, 156, 198, 38, 42, 43, 168, 217,
    153, 15, 103, 80, 189, 71, 191, 97, 84, 247, 95, 36, 69, 14, 35, 12, 171, 28, 114, 178, 148,
    86, 182, 32, 83, 158, 109, 22, 255, 94, 238, 151, 85, 77, 124, 254, 18, 4, 26, 123, 176, 232,
    193, 131, 172, 143, 142, 150, 30, 10, 146, 162, 62, 224, 218, 196, 229, 1, 192, 213, 27, 110,
    56, 231, 180, 138, 107, 242, 187, 54, 120, 19, 44, 117, 228, 215, 203, 53, 239, 251, 127, 81,
    11, 133, 96, 204, 132, 41, 115, 73, 55, 249, 147, 102, 48, 122, 145, 106, 118, 74, 190, 29, 16,
    174, 5, 177, 129, 63, 113, 99, 31, 161, 76, 246, 34, 211, 13, 60, 68, 207, 160, 65, 111, 82,
    165, 67, 169, 225, 57, 112, 244, 155, 51, 236, 200, 233, 58, 61, 47, 100, 137, 185, 64, 17, 70,
    234, 163, 219, 108, 170, 166, 59, 149, 52, 105, 24, 212, 78, 173, 45, 0, 116, 226, 119, 136,
    206, 135, 175, 195, 25, 92, 121, 208, 126, 139, 3, 75, 141, 21, 130, 98, 241, 40, 154, 66, 184,
    49, 181, 46, 243, 88, 101, 183, 8, 23, 72, 188, 104, 179, 210, 134, 250, 201, 164, 89, 216,
    202, 220, 50, 221, 152, 140, 33, 235, 214,
];

/// 编码用的字母表。共 65 个字符，但下标只到 63，最后那个 `E` 永远取不到（网页 JS 就是这样）。
const BASE64_CHARS: &[u8] = b"6fpLRqJO8M/c3jnYxFkUVC4ZIG12SiH=5v0mXDazWBTsuw7QetbKdoPyAl+hN9rgE";

/// 首块的异或键（ASCII `059053f7d15e01d7`），另外再异或 42。
const KEY_PAD: &[u8; 16] = b"059053f7d15e01d7";

/// 签名前的原文：`101_3_3.0+路径?query+d_c0[+body]`。
pub fn sign_source(path_and_query: &str, dc0: &str, body: Option<&str>) -> String {
    let mut parts = vec![ZSE93, path_and_query, dc0];
    if let Some(body) = body {
        parts.push(body);
    }
    parts.join("+")
}

/// 完整的 `x-zse-96` 请求头值：`2.0_` + 64 个字符。
pub fn zse96(path_and_query: &str, dc0: &str, body: Option<&str>, seed: u8) -> String {
    let digest = md5_hex(&sign_source(path_and_query, dc0, body));
    format!("2.0_{}", encrypt(&digest, seed))
}

/// 加密 `input`（实际调用时是 32 位 md5 hex），返回不带 `2.0_` 前缀的密文（Hydrogen 的 `b64encode`）。
/// `seed` 是明文首字节。
pub fn encrypt(input: &str, seed: u8) -> String {
    let mut padded = vec![seed, 0];
    padded.extend_from_slice(input.as_bytes());
    // PKCS#7：正好整块时也补一整块
    let pad_len = 16 - padded.len() % 16;
    padded.resize(padded.len() + pad_len, pad_len as u8);

    let mut header = [0u8; 16];
    for (i, slot) in header.iter_mut().enumerate() {
        *slot = padded[i] ^ KEY_PAD[i] ^ 42;
    }
    let iv = transform_block(&header);

    let mut combined = iv.to_vec();
    combined.extend(process_blocks(&padded[16..], iv));
    b64encode(combined)
}

/// CBC：每块先和上一块的密文（第一块用 IV）异或，再过分组变换。
fn process_blocks(data: &[u8], iv: [u8; 16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut chain = iv;
    for chunk in data.chunks(16) {
        let mut xored = [0u8; 16];
        for i in 0..16 {
            xored[i] = chunk.get(i).copied().unwrap_or(0) ^ chain[i];
        }
        chain = transform_block(&xored);
        out.extend_from_slice(&chain);
    }
    out
}

/// 一个 16 字节分组的 32 轮变换：`w[r+4] = w[r] ^ T(w[r+1] ^ w[r+2] ^ w[r+3] ^ ZK[r])`，输出是最后四个字倒序。
fn transform_block(data: &[u8; 16]) -> [u8; 16] {
    let mut words = [0u32; 36];
    for (i, w) in words.iter_mut().take(4).enumerate() {
        *w = u32::from_be_bytes([data[4 * i], data[4 * i + 1], data[4 * i + 2], data[4 * i + 3]]);
    }
    for r in 0..32 {
        let temp = words[r + 1] ^ words[r + 2] ^ words[r + 3] ^ ZK[r] as u32;
        words[r + 4] = words[r] ^ transform_value(temp);
    }
    let mut out = [0u8; 16];
    for (i, w) in [words[35], words[34], words[33], words[32]].iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

/// 逐字节过 S 盒，再和自己循环左移 2 / 10 / 18 / 24 位的结果异或。
fn transform_value(e: u32) -> u32 {
    let b = e.to_be_bytes();
    let r = u32::from_be_bytes([ZB[b[0] as usize], ZB[b[1] as usize], ZB[b[2] as usize], ZB[b[3] as usize]]);
    r ^ r.rotate_left(2) ^ r.rotate_left(10) ^ r.rotate_left(18) ^ r.rotate_left(24)
}

/// 补零到 3 的倍数，从尾到头每 3 字节一组编 4 个字符；第 n 个字节（从 0 数）在 `n % 4 == 0` 时先异或 58。
fn b64encode(mut combined: Vec<u8>) -> String {
    let pad = (3 - combined.len() % 3) % 3;
    combined.resize(combined.len() + pad, 0);
    let mut out = String::with_capacity(combined.len() / 3 * 4);
    let mut counter = 0usize;
    for group in combined.rchunks(3) {
        let mut num = 0u32;
        for (k, &byte) in group.iter().rev().enumerate() {
            let mask = if counter % 4 == 0 { 58 } else { 0 };
            num |= u32::from(byte ^ mask) << (8 * k);
            counter += 1;
        }
        for shift in [0, 6, 12, 18] {
            out.push(BASE64_CHARS[((num >> shift) & 63) as usize] as char);
        }
    }
    out
}
