//! bcrypt，语义照 Java 的 **jBCrypt 0.4**（`org.mindrot.jbcrypt.BCrypt`，`source-coolapk` 用的就是它）。
//!
//! 原来手写的 Blowfish 和 pi 常量表删了；这里只剩 jBCrypt 的外壳（salt 解析、bcrypt 字母表的 base64、
//! 拼 60 字符结果）。`AppTokenTest` 那几组期望值来自 Python + libxcrypt 的独立实现，照旧逐字节对上。
//!
//! 三个容易写错、这里逐条照抄 jBCrypt 的点：
//! 1. `$2a$` 的密码字节后面**补一个 NUL**（`password + "\0"`，OpenBSD 的 2a/2b 也是这么办的），
//!    它参与 key schedule，少一个字节结果就完全不同；
//! 2. salt 的 22 个字符按 bcrypt 字母表 `./A-Za-z0-9` 解码成 16 字节，遇到字母表外的字符就**停下**
//!    （jBCrypt `decode_base64` 的 `c == -1 → break`），不足 16 字节 jBCrypt 抛 `Bad salt length`；
//! 3. 每轮 `key(password)` / `key(salt)` 里的 key 字节流是**连续读**的（不按轮重置偏移）。
//!
use blowfish::Blowfish;

use super::COOLAPK_BCRYPT_ALPHABET;

const BCRYPT_SALT_LEN: usize = 16;
const BCRYPT_BLOCK_SIZE: usize = 8;

/// `bf_crypt_ciphertext`：bcrypt 固定加密的那 24 个字节（"OrpheanBeholderScryDoubt"）。
const CIPHERTEXT: [u32; 6] = [
    0x4f727068, 0x65616e42, 0x65686f6c, 0x64657253, 0x63727944, 0x6f756274,
];

/// 表外字符的 char64 结果（jBCrypt 用 -1）。
const INVALID: i32 = -1;

fn char64(c: u8) -> i32 {
    match c {
        b'.' => 0,
        b'/' => 1,
        b'A'..=b'Z' => (c - b'A') as i32 + 2,
        b'a'..=b'z' => (c - b'a') as i32 + 28,
        b'0'..=b'9' => (c - b'0') as i32 + 54,
        _ => INVALID,
    }
}

/// jBCrypt `decode_base64(s, maxolen)`：返回**实际解出来的字节**（可能少于 16）。
fn decode_base64(s: &[u8], maxolen: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(maxolen);
    let mut off = 0usize;
    let len = s.len();
    while off < len.saturating_sub(1) && out.len() < maxolen {
        let c1 = char64(s[off]);
        off += 1;
        let c2 = char64(s[off]);
        off += 1;
        if c1 == INVALID || c2 == INVALID {
            break;
        }
        out.push((((c1 << 2) | ((c2 & 0x30) >> 4)) as u8) & 0xff);
        if out.len() >= maxolen || off >= len {
            break;
        }
        let c3 = char64(s[off]);
        off += 1;
        if c3 == INVALID {
            break;
        }
        out.push(((((c2 & 0x0f) << 4) | ((c3 & 0x3c) >> 2)) as u8) & 0xff);
        if out.len() >= maxolen || off >= len {
            break;
        }
        let c4 = char64(s[off]);
        off += 1;
        out.push(((((c3 & 0x03) << 6) | c4) as u8) & 0xff);
    }
    out
}

/// jBCrypt `encode_base64(d, len)`：bcrypt 自己的字母表，尾部按 1 / 2 字节特判。
fn encode_base64(d: &[u8], len: usize) -> String {
    let mut out = String::new();
    let mut off = 0usize;
    let alphabet = COOLAPK_BCRYPT_ALPHABET.as_bytes();
    while off < len {
        let c1 = d[off] as u32;
        off += 1;
        out.push(alphabet[((c1 >> 2) & 0x3f) as usize] as char);
        let mut c1 = (c1 & 0x03) << 4;
        if off >= len {
            out.push(alphabet[c1 as usize] as char);
            break;
        }
        let c2 = d[off] as u32;
        off += 1;
        c1 |= (c2 >> 4) & 0x0f;
        out.push(alphabet[c1 as usize] as char);
        let mut c2 = (c2 & 0x0f) << 2;
        if off >= len {
            out.push(alphabet[c2 as usize] as char);
            break;
        }
        let c3 = d[off] as u32;
        off += 1;
        c2 |= (c3 >> 6) & 0x03;
        out.push(alphabet[c2 as usize] as char);
        out.push(alphabet[(c3 & 0x3f) as usize] as char);
    }
    out
}

/// 算一个 **完整的 60 字符 bcrypt 哈希**（`$2a$NN$<22 位 salt><31 位哈希>`）。
///
/// `salt` 是 jBCrypt 那种整串（`$2a$10$` + 22 位，后面多余的字符被忽略）。返回 `Err` 的情况
/// 只有 jBCrypt 也会抛异常的那几种（版本不对 / 轮数越界 / salt 不足 16 字节）。
pub fn hash(password: &str, salt: &str) -> Result<String, String> {
    let salt_bytes = salt.as_bytes();
    if salt_bytes.len() < 29 {
        return Err("Invalid salt".to_string());
    }
    if salt_bytes[0] != b'$' || salt_bytes[1] != b'2' {
        return Err("Invalid salt version".to_string());
    }
    let (minor, off) = if salt_bytes[2] == b'$' {
        (0u8, 3usize)
    } else {
        let minor = salt_bytes[2];
        if minor != b'a' || salt_bytes[3] != b'$' {
            return Err("Invalid salt revision".to_string());
        }
        (minor, 4usize)
    };
    if salt_bytes[off + 2] > b'$' {
        return Err("Missing salt rounds".to_string());
    }
    let rounds: u32 = salt[off..off + 2]
        .parse()
        .map_err(|_| "Missing salt rounds".to_string())?;
    let real_salt = &salt_bytes[off + 3..off + 25];
    if !(4..=30).contains(&rounds) {
        return Err("Bad number of rounds".to_string());
    }
    let saltb = decode_base64(real_salt, BCRYPT_SALT_LEN);
    if saltb.len() != BCRYPT_SALT_LEN {
        return Err("Bad salt length".to_string());
    }
    // jBCrypt：`$2a$` 起（minor >= 'a'）密码后面补一个 NUL。
    let mut passwordb = password.as_bytes().to_vec();
    if minor >= b'a' {
        passwordb.push(0);
    }

    // Eksblowfish：RustCrypto `blowfish` 的 bcrypt 接口（`salted_expand_key` = jBCrypt `ekskey`，
    // `bc_expand_key` = `key`，key / salt 字节流都是连续读、循环回卷，和 jBCrypt 逐步一致）
    let mut bf = Blowfish::bc_init_state();
    bf.salted_expand_key(&saltb, &passwordb);
    for _ in 0..(1u32 << rounds) {
        bf.bc_expand_key(&passwordb);
        bf.bc_expand_key(&saltb);
    }
    let mut cdata = CIPHERTEXT;
    for _ in 0..64 {
        for j in 0..(cdata.len() >> 1) {
            let lr = bf.bc_encrypt([cdata[j << 1], cdata[(j << 1) + 1]]);
            cdata[j << 1] = lr[0];
            cdata[(j << 1) + 1] = lr[1];
        }
    }
    let mut hashed = Vec::with_capacity(cdata.len() * 4);
    for word in cdata {
        hashed.extend_from_slice(&word.to_be_bytes());
    }

    let mut out = String::new();
    out.push('$');
    out.push('2');
    if minor >= b'a' {
        out.push(minor as char);
    }
    out.push('$');
    if rounds < 10 {
        out.push('0');
    }
    out.push_str(&rounds.to_string());
    out.push('$');
    out.push_str(&encode_base64(&saltb, saltb.len()));
    out.push_str(&encode_base64(
        &hashed,
        hashed.len() / BCRYPT_BLOCK_SIZE * BCRYPT_BLOCK_SIZE - 1,
    ));
    Ok(out)
}
