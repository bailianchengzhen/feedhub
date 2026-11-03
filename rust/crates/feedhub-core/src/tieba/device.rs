//! 贴吧 App 的**设备标识**（`source-tieba/TiebaDevice.kt` / `TiebaDeviceIds.kt` 的 Rust 版）。
//!
//! Rust 只管**读**：三个"原件"（`tieba.android_id` / `tieba.uuid` / `tieba.client_id`）由宿主
//! （`TiebaDevice.ensure(ctx.store)`）在第一次用之前生成并写进 `ctx.store`，之后一直用同一组
//! （和真 App 装一次就不变一样）。Rust 不取随机数、不造身份（风控红线：设备码只用 store 里那一份）。
//!
//! 派生出来的 `cuid_galaxy2` / `c3_aid` 是百度 "helios" 哈希那套（aiotieba `tbcrypto/cuid.c`，
//! Unlicense；TiebaLite 的 `Hasher.java` 结果一样）。纯函数，定值测试用的是 aiotieba 单测里的那两组：
//! `6723280942424242` / `67232809-3407-3442-4207-672346917aaa`
//! → `06C7F37D41256F25FABA97B885DB6EFB|VAPUDW7TA` / `A00-OGBA33NRAQASXI6FDZ4YAJFTK75EF4Y5-YVOG764X`。

/// 16 位小写十六进制，等价于 Android 的 `Settings.Secure.ANDROID_ID`。
pub const KEY_ANDROID_ID: &str = "tieba.android_id";
pub const KEY_UUID: &str = "tieba.uuid";
/// `wappc_{毫秒时间戳}_{三位随机数}`（TiebaLite `RetrofitTiebaApi.randomClientId` 的格式）。
pub const KEY_CLIENT_ID: &str = "tieba.client_id";

/// 假装成的那台手机（和 Kotlin `TiebaDevice` 的常量一字不差）。
pub const BRAND: &str = "Xiaomi";
pub const MODEL: &str = "23127PN0CC";
pub const OS_VERSION: &str = "34";
pub const IMEI: &str = "000000000000000";
pub const SCREEN_W: i32 = 1080;
pub const SCREEN_H: i32 = 2400;
pub const SCREEN_DIP: f64 = 3.0;

/// 一组设备标识 + 派生值。`install_time_ms` 是"安装时间"，从 `client_id` 里的时间戳取（和 Kotlin 一样，
/// 免得再多存一个键）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub android_id: String,
    pub uuid: String,
    pub client_id: String,
    pub cuid_galaxy2: String,
    pub c3_aid: String,
    pub install_time_ms: i64,
}

impl Device {
    pub fn new(android_id: impl Into<String>, uuid: impl Into<String>, client_id: impl Into<String>) -> Self {
        let android_id = android_id.into();
        let uuid = uuid.into();
        let client_id = client_id.into();
        let cuid_galaxy2 = cuid_galaxy2(&android_id);
        let c3_aid = c3_aid(&android_id, &uuid);
        let install_time_ms = client_id
            .split('_')
            .nth(1)
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        Self {
            android_id,
            uuid,
            client_id,
            cuid_galaxy2,
            c3_aid,
            install_time_ms,
        }
    }
}

/// `MD5("com.baidu" + androidId)` 的大写十六进制 + `|V` + helios(前面那串) 的 base32。
pub fn cuid_galaxy2(android_id: &str) -> String {
    let digest = md5_hex_upper(format!("com.baidu{android_id}").as_bytes());
    let head = format!("{digest}|V");
    format!("{head}{}", base32(&helios(digest.as_bytes())))
}

/// `A00-` + base32(SHA1("com.helios" + androidId + uuid)) + `-` + helios(前面那串) 的 base32。
pub fn c3_aid(android_id: &str, uuid: &str) -> String {
    let sha1 = sha1(format!("com.helios{android_id}{uuid}").as_bytes());
    let head = format!("A00-{}-", base32(&sha1));
    format!("{head}{}", base32(&helios(head.as_bytes())))
}

/// 5 字节的 helios 哈希：CRC32 和 xxHash32 交替混进一个 40 位的状态。
fn helios(src: &[u8]) -> [u8; STEP] {
    let mut sec: u64 = (1u64 << 40) - 1;
    let mut buf = [0u8; STEP * 4];
    buf[..STEP].fill(0xff);

    let mut crc = Crc32::new();
    crc.update(src);
    crc.update(&buf[..STEP]);
    sec = mix(sec, crc.value() as u64, 8, false);
    buf[STEP..STEP * 2].copy_from_slice(&write(sec));

    let mut second = src.to_vec();
    second.extend_from_slice(&buf[..STEP * 2]);
    sec = mix(sec, xxh32(&second) as u64 & 0xffff_ffff, 0, true);
    buf[STEP * 2..STEP * 3].copy_from_slice(&write(sec));

    let mut third = src.to_vec();
    third.extend_from_slice(&buf[..STEP * 3]);
    sec = mix(sec, xxh32(&third) as u64 & 0xffff_ffff, 1, true);
    buf[STEP * 3..].copy_from_slice(&write(sec));

    crc.update(&buf[STEP..STEP * 4]);
    sec = mix(sec, crc.value() as u64, 7, true);

    write(sec)
}

const STEP: usize = 5;

/// 把 `sec` 的第 `start..start+31` 位换成（它和 `hash` 做 xor / and 的结果）。
fn mix(sec: u64, hash: u64, start: u32, xor: bool) -> u64 {
    let mask = (1u64 << (start + 32)) - 1;
    let cur = (mask & sec) >> start;
    let v = if xor { cur ^ hash } else { cur & hash };
    let mut out = sec;
    for i in 0..32 {
        let bit = 1u64 << (start + i);
        out = if v & (1u64 << i) != 0 { out | bit } else { out & !bit };
    }
    out
}

fn write(sec: u64) -> [u8; STEP] {
    let mut out = [0u8; STEP];
    for (i, b) in out.iter_mut().enumerate() {
        *b = (sec >> (8 * i)) as u8;
    }
    out
}

/// RFC 4648 base32，不补 `=`。
pub fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        buffer = (buffer << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(ALPHABET[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// `java.util.zip.CRC32`：反射多项式 `0xEDB88320`，初值 / 末值都是 `0xFFFFFFFF`。
struct Crc32(u32);

impl Crc32 {
    fn new() -> Self {
        Self(0xffff_ffff)
    }

    fn update(&mut self, data: &[u8]) {
        let mut crc = self.0;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        self.0 = crc;
    }

    fn value(&self) -> u32 {
        self.0 ^ 0xffff_ffff
    }
}

/// 标准 xxHash32（seed 0）。
pub fn xxh32(data: &[u8]) -> u32 {
    const P1: u32 = 0x9E37_79B1;
    const P2: u32 = 0x85EB_CA77;
    const P3: u32 = 0xC2B2_AE3D;
    const P4: u32 = 0x27D4_EB2F;
    const P5: u32 = 0x1656_67B1;

    let len = data.len();
    let mut i = 0usize;
    let mut h: u32;
    if len >= 16 {
        let mut v1 = P1.wrapping_add(P2);
        let mut v2 = P2;
        let mut v3 = 0u32;
        let mut v4 = 0u32.wrapping_sub(P1);
        while i <= len - 16 {
            v1 = round(v1, int_le(data, i));
            v2 = round(v2, int_le(data, i + 4));
            v3 = round(v3, int_le(data, i + 8));
            v4 = round(v4, int_le(data, i + 12));
            i += 16;
        }
        h = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
    } else {
        h = P5;
    }
    h = h.wrapping_add(len as u32);
    while i + 4 <= len {
        h = h
            .wrapping_add(int_le(data, i).wrapping_mul(P3))
            .rotate_left(17)
            .wrapping_mul(P4);
        i += 4;
    }
    while i < len {
        h = h
            .wrapping_add((data[i] as u32).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
        i += 1;
    }
    h ^= h >> 15;
    h = h.wrapping_mul(P2);
    h ^= h >> 13;
    h = h.wrapping_mul(P3);
    h ^ (h >> 16)
}

fn round(acc: u32, input: u32) -> u32 {
    acc.wrapping_add(input.wrapping_mul(0x85EB_CA77))
        .rotate_left(13)
        .wrapping_mul(0x9E37_79B1)
}

fn int_le(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn md5_hex_upper(data: &[u8]) -> String {
    // 小写十六进制在 `crate::hosted`，这里只是转大写（helios 的 `md5HexU` 口径）。
    crate::hosted::md5_hex_bytes(data).to_ascii_uppercase()
}

/// SHA-1 的 20 字节摘要。
pub fn sha1(data: &[u8]) -> [u8; 20] {
    use sha1::Digest;
    sha1::Sha1::digest(data).into()
}

/// 标准 base64（带 `=` 补位）。
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

