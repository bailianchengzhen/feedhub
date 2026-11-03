//! 小红书签名用到的小算法：自定义字母表的 Base64、MD5、xxHash32、MurmurHash3、RC4、CRC32 变体、
//! 自定义 4 路 ARX 哈希（网页里叫 `_dsf`），以及 JS `JSON.stringify` 的字符串转义。
//!
//! 逐条对照 `source-xiaohongshu/src/main/kotlin/feedhub/source/xiaohongshu/XhsCodec.kt`：
//! 全是纯函数，没有网络、不取时间、不取随机数。
//!
//! 参考：
//! - xhshow（MIT）`utils/encoder.py` `utils/hash.py` `core/crc32_encrypt.py` `core/crypto.py:_custom_hash_v2`；
//! - cv-cat/Spider_XHS（同一作者已把这些脚本以 MIT 收进 cv-cat/catbus 的 `static/xhs/`）：`xhs_core/js/mns.js:dsf`、`xs_common.js:gens9`、`b1.js:rc4BinaryString`、
//!   `xhs_pc/params.py:murmur_hash3_32`。
//! 字母表、哈希初值这类常量是协议本身的事实，两个项目逐字一致。

use md5::{Digest, Md5};

/// X-s 外壳、X-S-Common、b1 用的字母表。
pub const XS_ALPHABET: &str = "ZmserbBoHQtNP+wOcza/LpngG8yJq42KWYj0DSfdikx3VT16IlUAFM97hECvuRX5";

/// X-s 里层 x3（`mns0301_`）用的字母表。
pub const X3_ALPHABET: &str = "MfgqrsbcyzPQRStuvC7mn501HIJBo2DEFTKdeNOwxWXYZap89+/A4UVLhijkl63G";

/// 自定义字母表的 Base64（带 `=` 补位）。Kotlin 版本按 3 字节一组手写，这里逐条照抄。
pub fn base64(bytes: &[u8], alphabet: &str) -> String {
    let a = alphabet.as_bytes();
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

/// [`base64`] 的逆运算。字母表外的字符是错误（Kotlin 那边是 `require`）。
pub fn base64_decode(text: &str, alphabet: &str) -> Result<Vec<u8>, String> {
    let a = alphabet.as_bytes();
    let body = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for c in body.chars() {
        let v = a.iter().position(|&x| x as char == c).ok_or_else(|| format!("字母表里没有 '{c}'"))?;
        buffer = (buffer << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

// `json_string` 从 `crate::hosted` 来（kotlinx / `JSON.stringify` 的转义口径，注释在那里）；
// `pub use` 保留 `codec::json_string` 这个老路径。
pub use crate::hosted::json_string;

pub fn md5(bytes: &[u8]) -> [u8; 16] {
    let mut h = Md5::new();
    h.update(bytes);
    let out = h.finalize();
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&out);
    buf
}

// `md5_hex` 从 `crate::hosted` 来（小写十六进制）；`pub use` 保留 `codec::md5_hex` 这个老路径。
pub use crate::hosted::md5_hex;

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn unhex(text: &str) -> Result<Vec<u8>, String> {
    if text.len() % 2 != 0 {
        return Err("十六进制长度要是偶数".to_string());
    }
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    let mut i = 0;
    while i < b.len() {
        let hi = (b[i] as char).to_digit(16).ok_or_else(|| format!("十六进制里出现非法字符：{}", &text[i..i + 1]))?;
        let lo = (b[i + 1] as char).to_digit(16).ok_or_else(|| format!("十六进制里出现非法字符：{}", &text[i + 1..i + 2]))?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Ok(out)
}

/// 标准 CRC-32（和 `java.util.zip.CRC32` 一致）。
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    });
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// X-S-Common 里 `x9` 用的 CRC32 变体：标准 CRC32 再异或一次多项式，按有符号 32 位。
/// 网页写作 `(-1 ^ c ^ 0xEDB88320) | 0`，`c` 是还没取反的中间状态，所以等于 `crc32 ^ 0xEDB88320`。
/// 网页对字符串逐个 `charCodeAt`；b1 是纯 ASCII，按 ASCII 字节算（Kotlin 那边用 ISO-8859-1 转字节，
/// 这里照 Java 的语义：码点 > 0xFF 的字符在 ISO-8859-1 里是 `?`）。
pub fn crc32_variant(text: &str) -> i32 {
    let bytes: Vec<u8> = text
        .chars()
        .map(|c| if (c as u32) <= 0xFF { c as u8 } else { b'?' })
        .collect();
    (crc32(&bytes) as i32) ^ (0xEDB8_8320u32 as i32)
}

/// xxHash32，seed 默认 0。逐条照 Kotlin 的 Int 运算（全是回绕）。
pub fn xxh32(data: &[u8], seed: u32) -> u32 {
    const P1: u32 = 0x9E37_79B1;
    const P2: u32 = 0x85EB_CA77;
    const P3: u32 = 0xC2B2_AE3D;
    const P4: u32 = 0x27D4_EB2F;
    const P5: u32 = 0x1656_67B1;

    fn le32(data: &[u8], o: usize) -> u32 {
        (data[o] as u32) | ((data[o + 1] as u32) << 8) | ((data[o + 2] as u32) << 16) | ((data[o + 3] as u32) << 24)
    }

    let len = data.len();
    let mut p = 0;
    let mut h: u32;
    if len >= 16 {
        let mut v1 = seed.wrapping_add(P1).wrapping_add(P2);
        let mut v2 = seed.wrapping_add(P2);
        let mut v3 = seed;
        let mut v4 = seed.wrapping_sub(P1);
        while p + 16 <= len {
            v1 = (v1.wrapping_add(le32(data, p).wrapping_mul(P2))).rotate_left(13).wrapping_mul(P1);
            v2 = (v2.wrapping_add(le32(data, p + 4).wrapping_mul(P2))).rotate_left(13).wrapping_mul(P1);
            v3 = (v3.wrapping_add(le32(data, p + 8).wrapping_mul(P2))).rotate_left(13).wrapping_mul(P1);
            v4 = (v4.wrapping_add(le32(data, p + 12).wrapping_mul(P2))).rotate_left(13).wrapping_mul(P1);
            p += 16;
        }
        h = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
    } else {
        h = seed.wrapping_add(P5);
    }
    h = h.wrapping_add(len as u32);
    while p + 4 <= len {
        h = (h.wrapping_add(le32(data, p).wrapping_mul(P3))).rotate_left(17).wrapping_mul(P4);
        p += 4;
    }
    while p < len {
        h = (h.wrapping_add((data[p] as u32).wrapping_mul(P5))).rotate_left(11).wrapping_mul(P1);
        p += 1;
    }
    h ^= h >> 15;
    h = h.wrapping_mul(P2);
    h ^= h >> 13;
    h = h.wrapping_mul(P3);
    h ^= h >> 16;
    h
}

/// MurmurHash3 x86 32 位，输入按 UTF-8。`xy-direction` 用它。返回值和 Kotlin 的 `Int` 一样是有符号的。
pub fn murmur3(key: &str, seed: i32) -> i32 {
    const C1: u32 = 0xcc9e_2d51;
    const C2: u32 = 0x1b87_3593;
    let data = key.as_bytes();
    let mut h = seed as u32;
    let blocks = data.len() / 4;
    for i in 0..blocks {
        let mut k = (data[4 * i] as u32)
            | ((data[4 * i + 1] as u32) << 8)
            | ((data[4 * i + 2] as u32) << 16)
            | ((data[4 * i + 3] as u32) << 24);
        k = k.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
        h ^= k;
        h = h.rotate_left(13).wrapping_mul(5).wrapping_add(0xe654_6b64);
    }
    let mut k: u32 = 0;
    let tail = blocks * 4;
    let rem = data.len() & 3;
    if rem >= 3 {
        k ^= (data[tail + 2] as u32) << 16;
    }
    if rem >= 2 {
        k ^= (data[tail + 1] as u32) << 8;
    }
    if rem >= 1 {
        k ^= data[tail] as u32;
        k = k.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
        h ^= k;
    }
    h ^= data.len() as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h as i32
}

/// RC4。b1 的加密用它（密钥 `xhswebmplfbt`）。
pub fn rc4(data: &[u8], key: &[u8]) -> Vec<u8> {
    let mut s = [0u8; 256];
    for (i, slot) in s.iter_mut().enumerate() {
        *slot = i as u8;
    }
    let mut j = 0usize;
    for i in 0..256 {
        j = (j + s[i] as usize + key[i % key.len()] as usize) & 0xFF;
        s.swap(i, j);
    }
    let mut out = vec![0u8; data.len()];
    let mut i = 0usize;
    j = 0;
    for (n, &b) in data.iter().enumerate() {
        i = (i + 1) & 0xFF;
        j = (j + s[i] as usize) & 0xFF;
        s.swap(i, j);
        out[n] = b ^ s[(s[i] as usize + s[j] as usize) & 0xFF];
    }
    out
}

/// x3 末尾 16 字节设备签名用的自定义哈希（网页里叫 `_dsf`）：4 路 ARX，每 8 字节一轮，
/// 输入长度要是 8 的倍数（这里总是 8 字节时间戳 + 16 字节 MD5 = 24）。输出 16 字节，小端。
pub fn dsf(input: &[u8]) -> Result<[u8; 16], String> {
    if input.len() % 8 != 0 {
        return Err("dsf 输入要是 8 的倍数".to_string());
    }
    let len = input.len() as u32;
    fn le32(input: &[u8], o: usize) -> u32 {
        (input[o] as u32) | ((input[o + 1] as u32) << 8) | ((input[o + 2] as u32) << 16) | ((input[o + 3] as u32) << 24)
    }

    let mut s0: u32 = 0x6D2B_79F5 ^ len;
    let mut s1: u32 = 0x1B87_3593 ^ (len << 8);
    let mut s2: u32 = 0x85EB_CA6B ^ (len << 16);
    let mut s3: u32 = 0xC2B2_AE35 ^ (len << 24);
    let mut o = 0;
    while o + 8 <= input.len() {
        let v0 = le32(input, o);
        let v1 = le32(input, o + 4);
        s0 = (s0.wrapping_add(v0) ^ s2).rotate_left(7);
        s1 = (s1 ^ v0).wrapping_add(s3).rotate_left(11);
        s2 = (s2.wrapping_add(v1) ^ s0).rotate_left(13);
        s3 = (s3 ^ v1).wrapping_add(s1).rotate_left(17);
        o += 8;
    }
    let t0 = s0 ^ len;
    let t1 = s1 ^ t0;
    let t2 = s2.wrapping_add(t1);
    let t3 = s3 ^ t2;
    let r0 = t0.rotate_left(9);
    let r1 = t1.rotate_left(13);
    let r2 = t2.rotate_left(17);
    let r3 = t3.rotate_left(19);
    let f0 = r0.wrapping_add(r2);
    let f1 = r1 ^ r3;
    let f2 = r2.wrapping_add(f0);
    let f3 = r3 ^ f1;
    let mut out = [0u8; 16];
    for (i, w) in [f0, f1, f2, f3].iter().enumerate() {
        for b in 0..4 {
            out[i * 4 + b] = (w >> (8 * b)) as u8;
        }
    }
    Ok(out)
}
