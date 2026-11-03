//! 酷安 `X-App-Token`（`AppToken`）的 v2 / v3 计算。
//!
//! 纯算法：不联网、不取时间、不取随机数。**设备码和时间戳由 Kotlin 传进来**
//! （设备码在 `CoolapkDevice` / `ctx.store` 里，时间戳是 `System.currentTimeMillis() / 1000`），
//! v3 的 930 字节密码表（`source-coolapk/src/main/resources/.../auth_blob.b64` 解码后）也由 Kotlin 传，
//! 这样"表归插件所有"这条边界和表情表一样。
//!
//! - **v2**（信息流 / 关注流 / 详情：13.4.1 / API 13）：
//!   `token://com.coolapk.market/dcf01e569c1e3db93a3d0fcf191a622c?<md5(秒)>$<md5(设备码)>&com.coolapk.market`
//!   → `password = md5(b64(token))`，`salt = "$2a$10$" + b64(秒) + "/" + md5(token)` 截 31 位再加 `u`，
//!   bcrypt cost 10，`$2a` 换 `$2y`，整体 b64（去 `=`），前面加 `v2`。
//! - **v3**（评论接口：16.2.0 / API 16）：从密码表里取 `((ts + code) % 100) * 4 + 0x80` 开始的 128 字节，
//!   base64 解码成 `segment`，拼进 `plain`，`password = md5(b64_with_padding(plain))`，
//!   salt 的末位按标准字母表回退 5 位，bcrypt **cost 4**，其余同上，前面加 `v3`。
//!
//! v3 和 coolapk-desktop `auth.rs`、YuleBest/coolapk-cli `internal/auth`（都是 MIT）一致，密码表取自 coolapk-cli。
//!
//! 两处容易看漏的细节（和 Kotlin `AppToken` 一致）：
//! - v2 的 `password` 用**不带 padding** 的 base64，v3 的 `plain` 用**带 padding** 的（`Base64.getEncoder()`）；
//! - v3 的 salt 里只要出现 bcrypt 字母表之外的字符，就**往后试下一秒**（最多 15 秒），
//!   和参考实现一致；同一个 `timestampSec` 的结果因此是确定的。

use super::bcrypt;

const APP_LABEL: &str = "token://com.coolapk.market/dcf01e569c1e3db93a3d0fcf191a622c";
const APP_ID: &str = "com.coolapk.market";
const STD_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// `md5_hex` 从 `crate::hosted` 来（小写十六进制）；`pub use` 保留老路径。
pub use crate::hosted::md5_hex;

/// 标准 base64；`pad` 为 false 时去掉结尾的 `=`（`AppToken.base64`）。
pub fn base64(bytes: &[u8], pad: bool) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        out.push(STD_ALPHABET[((b0 >> 2) & 0x3f) as usize] as char);
        out.push(STD_ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(STD_ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else if pad {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(STD_ALPHABET[(b2 & 0x3f) as usize] as char);
        } else if pad {
            out.push('=');
        }
    }
    out
}

/// 标准 base64 解码（和 `java.util.Base64.getDecoder()` 一样要求长度是 4 的倍数、`=` 在末尾）。
pub fn base64_decode(s: &[u8]) -> Result<Vec<u8>, String> {
    if s.len() % 4 != 0 {
        return Err(format!("base64 长度不是 4 的倍数：{}", s.len()));
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    let mut padded = false;
    let mut pad_count = 0usize;
    for &c in s {
        if c == b'=' {
            padded = true;
            pad_count += 1;
            if pad_count > 2 {
                return Err("base64 的 '=' 太多".to_string());
            }
            continue;
        }
        if padded {
            return Err("base64 的 '=' 不在末尾".to_string());
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            other => return Err(format!("base64 里的非法字符 {other}")),
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

/// v2 token（信息流 / 关注流 / 详情的身份）。
pub fn v2(device_code: &str, timestamp_sec: i64) -> Result<String, String> {
    let time_stamp = timestamp_sec.to_string();
    let md5_time_stamp = md5_hex(&time_stamp);
    let md5_device_code = md5_hex(device_code);
    let token = format!("{APP_LABEL}?{md5_time_stamp}${md5_device_code}&{APP_ID}");
    let md5_base64_token = md5_hex(&base64(token.as_bytes(), false));
    // 文档里的 salt 前缀是 `$2y$10$`；这里的 bcrypt 照 jBCrypt 只认 `$2a$`，所以用 `$2a$` 算、结果再换回 `$2y`
    let mut bcrypt_salt: String =
        format!("$2a$10${}/{}", base64(time_stamp.as_bytes(), false), md5_hex(&token)).chars().take(31).collect();
    bcrypt_salt.push('u');
    let bcrypt_result = format!("$2y{}", &bcrypt::hash(&md5_base64_token, &bcrypt_salt)?[3..]);
    Ok(format!("v2{}", base64(bcrypt_result.as_bytes(), false)))
}

/// v3 token（评论接口的身份）。`app_code` 必须和请求头里的 `X-App-Code` 一致。
///
/// `blob` 是密码表解码后的 930 字节（Kotlin `AppToken` 的 `v3Blob`）。
pub fn v3(device_code: &str, timestamp_sec: i64, app_code: i64, blob: &[u8]) -> Result<String, String> {
    let mut last: Option<String> = None;
    for offset in 0..=15i64 {
        let t = timestamp_sec + offset;
        match v3_once(device_code, t, app_code, blob) {
            // salt 里出现 bcrypt 字母表之外的字符：换下一秒（和参考实现一样）
            Ok(None) => {}
            Ok(Some(token)) => return Ok(token),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| "无法生成酷安 v3 token（16 秒内没有合法的 bcrypt salt）".to_string()))
}

fn v3_once(
    device_code: &str,
    t: i64,
    app_code: i64,
    blob: &[u8],
) -> Result<Option<String>, String> {
    let index = ((((t + app_code) % 100) * 4) + 0x80) as usize;
    if index + 0x80 > blob.len() {
        return Err(format!(
            "auth_blob 越界（index={index}，len={}）",
            blob.len()
        ));
    }
    let window = &blob[index..index + 0x80];
    let segment_bytes = base64_decode(window)?;
    let segment = String::from_utf8_lossy(&segment_bytes);
    let plain = format!(
        "{APP_ID}&{segment}&{}&{t}&{app_code}",
        md5_hex(device_code)
    );
    let password = md5_hex(&base64(plain.as_bytes(), true));
    let source_src = format!("{:x}/{}", t, md5_hex(&plain));
    let source = base64(source_src.as_bytes(), false);
    let source22: Vec<char> = source.chars().take(22).collect();
    if source22.len() < 22 {
        return Err("v3 salt 太短".to_string());
    }
    let last_char = source22[21];
    let idx = STD_ALPHABET.iter().position(|&c| c as char == last_char);
    let Some(idx) = idx else {
        return Err(format!("v3 salt 末位不在标准字母表里：{last_char}"));
    };
    // Kotlin: (indexOf - 5).mod(64)，负数取非负余数
    let shifted = STD_ALPHABET[(idx as i64 - 5).rem_euclid(64) as usize] as char;
    let salt: String = source22[..21].iter().collect::<String>() + &shifted.to_string();
    if !salt.bytes().all(is_bcrypt_salt_byte) {
        return Ok(None);
    }
    let hash = bcrypt::hash(&password, &format!("$2a$04${salt}"))?;
    let hashed = format!("$2y{}", &hash[3..]);
    Ok(Some(format!("v3{}", base64(hashed.as_bytes(), false))))
}

/// bcrypt salt 的字母表：`[./A-Za-z0-9]`（Kotlin `BCRYPT_SALT` 正则）。
fn is_bcrypt_salt_byte(b: u8) -> bool {
    b == b'.' || b == b'/' || b.is_ascii_alphanumeric()
}

