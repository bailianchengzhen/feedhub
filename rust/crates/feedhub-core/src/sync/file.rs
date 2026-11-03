//! **同步文件容器**（`core/sync/LibrarySyncFile.kt` 的逐行移植）：格式 v2 的读写 + v1 旧文件的读取。
//!
//! 两种模式，同一套"正文 AES-256-GCM + AAD 覆盖整个文件头"：
//!
//! 1. **按设备加密**（默认）：随机生成 32 字节**文件密钥**，正文照旧 gzip + AES-256-GCM；
//!    文件头里给每个收件设备放一份"包好的文件密钥"（临时 X25519 → HKDF-SHA256 → AES-256-GCM，
//!    类似 age 的多收件人）。本机自己也算一个收件人。
//! 2. **密码**（备用 / 给没配对的设备）：PBKDF2-HMAC-SHA256（≥210,000 次）→ 32 字节密钥 → AES-256-GCM。
//!
//! 文件布局（大端）：
//! ```text
//! 0   magic "FHSYNCF2"(8)
//! 8   version = 2 (1)
//! 9   mode: 1 = 密码, 2 = 收件设备 (1)
//! 10  device (8)         导出方设备 id
//! 18  generatedAt (8)
//! 26  seq (8)
//! 34  nonce (12)         正文 GCM 的 nonce
//! 46  —— 按 mode 分叉：
//!      mode 1: iterations(4) | salt(16)                      → 头共 66 字节
//!      mode 2: epk(32) | count(2) | count × [device(8) | wrapNonce(12) | wrappedFileKey(48)]
//!                                                             → 头共 80 + count×68 字节
//! 之后: AES-256-GCM(gzip(快照 JSON))，tag 16 字节跟在密文后
//! ```
//! **AAD = 上面整段文件头**（含收件人列表）——改版本 / 设备 / 序号 / 收件人 / epk 都认证失败。
//! 每个收件人的"包好的文件密钥"另外用 `AAD = 头部到 epk 为止的前缀` 认证。
//! **旧文件（v1，"FHSYNCF1"）照旧能读**：v1 只有密码模式。
//!
//! ## gzip 的字节不要求相等
//!
//! 正文是 `gzip(快照 JSON)`。Rust 的 `flate2` 和 JDK 的 `GZIPOutputStream` **压出来的字节不同**
//! （压缩算法 / 头字段实现无关），所以不能拿"整份文件逐字节相同"当门。门是两条：
//! 1. **定值向量**：密钥 / nonce / salt / 迭代次数都固定时，中间那层 `gcm_seal(gzip(plain))` 的
//!    **密文之后**（`gcm_seal` 的输入是压缩后的字节）不比——只在解锁层面比"解开的明文逐字节相同"；
//! 2. **互通**：Kotlin 加密 → Rust 解、Rust 加密 → Kotlin 解，解出来的明文都必须逐字节相同
//!    （`RustSyncFileInteropTest` 两边各跑一遍）。
//!
//! 密码 / 私钥都不进日志、不落盘；[`Opened`] 里的明文由调用方负责及时丢掉。

use super::b64;
use super::crypto::{self, NONCE_BYTES, TAG_BYTES};
use super::snapshot::{LibSyncError, Result};
use super::text::is_blank;

/// 导出文件的扩展名。
pub const EXTENSION: &str = "feedhub-sync";
/// 最少迭代次数。打开时**低于这个值直接拒**。
pub const MIN_ITERATIONS: u32 = 210_000;
/// 导出时用的迭代次数。
pub const DEFAULT_ITERATIONS: u32 = 210_000;
/// 迭代次数上限。
pub const MAX_ITERATIONS: u32 = 10_000_000;
/// 打开时最多接受多大的文件（密文）。
pub const MAX_FILE_BYTES: usize = 96 * 1024 * 1024;
/// 解压后最多接受多大（防 zip bomb）。
pub const MAX_PLAIN_BYTES: usize = 64 * 1024 * 1024;
/// 一个文件最多发给多少台设备。
pub const MAX_RECIPIENTS: usize = 32;
/// 当前的文件格式版本。
pub const VERSION: u8 = 2;
/// 旧版（只有密码）的文件格式版本，仍然能读。
pub const VERSION_PASSWORD_ONLY: u8 = 1;

const MAGIC_V1: &[u8] = b"FHSYNCF1";
const MAGIC_V2: &[u8] = b"FHSYNCF2";
const KDF_PBKDF2_HMAC_SHA256: u8 = 1;
const MODE_PASSWORD: u8 = 1;
const MODE_RECIPIENTS: u8 = 2;
const SALT_BYTES: usize = 16;
const FILE_KEY_BYTES: usize = 32;
const EPK_BYTES: usize = crypto::X25519_BYTES;
const WRAP_NONCE_BYTES: usize = 12;
const WRAPPED_FILE_KEY_BYTES: usize = FILE_KEY_BYTES + TAG_BYTES;
/// 设备 id 长度（8 字节）。
const DEVICE_BYTES: usize = 8;
const RECIPIENT_SLOT_BYTES: usize = DEVICE_BYTES + WRAP_NONCE_BYTES + WRAPPED_FILE_KEY_BYTES;
/// 头部公共部分：magic + version + mode + device + generatedAt + seq + nonce。
const COMMON_HEADER_BYTES: usize = 8 + 1 + 1 + DEVICE_BYTES + 8 + 8 + NONCE_BYTES;
const PASSWORD_HEADER_BYTES: usize = COMMON_HEADER_BYTES + 4 + SALT_BYTES;
/// 收件人分叉的头前缀：公共部分 + epk（包密钥的 AAD 就到这儿）。
const PREFIX_TO_EPK_BYTES: usize = COMMON_HEADER_BYTES + EPK_BYTES;
/// v1（旧格式）头部。
const V1_HEADER_BYTES: usize = 8 + 1 + 1 + 4 + SALT_BYTES + DEVICE_BYTES + 8 + 8 + NONCE_BYTES;
/// HKDF 的 info 串（换版本就换这个）。
const WRAP_INFO: &[u8] = b"feedhub-sync/v2/file-key";

/// 文件是密码加密还是按设备。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Password,
    Recipients,
}

/// 这份文件是怎么解开的（界面显示"已用本机密钥解开"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unlock {
    Password,
    DeviceKey,
}

/// 一个收件设备：设备 id + X25519 公钥。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub device: String,
    pub public_key: Vec<u8>,
}

impl Recipient {
    /// 构造并校验（`LibrarySyncFile.Recipient` 的 `require`）：设备 id 必须是 8 字节 base64url、
    /// 公钥 32 字节且不是全 0。失败的消息和 Kotlin 一样。
    pub fn new(device: &str, public_key: &[u8]) -> Result<Self> {
        if b64::decode(device).map(|b| b.len()) != Some(DEVICE_BYTES) {
            return Err(LibSyncError::Format(
                "收件设备 id 不是 8 字节的 base64url".to_string(),
            ));
        }
        if public_key.len() != EPK_BYTES {
            return Err(LibSyncError::Format("收件设备公钥长度不对".to_string()));
        }
        if public_key.iter().all(|b| *b == 0) {
            return Err(LibSyncError::Format("收件设备公钥不合法".to_string()));
        }
        Ok(Recipient {
            device: device.to_string(),
            public_key: public_key.to_vec(),
        })
    }
}

/// 文件头（不含密文）。打开文件后可以用它显示"来自哪台设备 / 什么时候生成 / 发给谁"，不用先解密。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub version: u8,
    pub mode: Mode,
    /// 密码模式的 PBKDF2 迭代次数；按设备加密是 0。
    pub iterations: u32,
    pub device: String,
    pub generated_at: i64,
    pub seq: i64,
    /// 收件设备的 id 列表（按设备加密才有；包括导出方自己）。
    pub recipients: Vec<String>,
}

/// 解密成功的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub header: Header,
    /// 解压后的明文（快照 JSON 的字节）。
    pub plaintext: Vec<u8>,
    pub unlock: Unlock,
}

/// **导出 / 解包要用的随机源**：nonce / salt / 文件密钥 / 临时密钥对。
///
/// 和平台层一个规矩：这一层**不自己取随机数**。生产用 [`RandomSource::system`]（`getrandom`），
/// 测试用固定序列的 [`RandomSource::fixed`]，两边才能逐字节对拍。
pub struct RandomSource<'a> {
    source: Box<dyn FnMut(usize) -> Vec<u8> + 'a>,
}

impl<'a> RandomSource<'a> {
    /// 生产用：一次读这么多字节的系统随机数。
    pub fn system() -> Self {
        RandomSource {
            source: Box::new(|n: usize| {
                let mut buf = vec![0u8; n];
                getrandom::getrandom(&mut buf).expect("读系统随机数失败");
                buf
            }),
        }
    }

    /// 测试用：每次都从这一串字节里按顺序取（用完了就循环）。
    ///
    /// **注意**：长度不必是 2 的幂 / 不需要随机质量——它只用来让两边"拿到同一批字节"。
    pub fn fixed(bytes: &'a [u8]) -> Self {
        let mut offset = 0usize;
        RandomSource {
            source: Box::new(move |n: usize| {
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(bytes[offset % bytes.len()]);
                    offset += 1;
                }
                out
            }),
        }
    }

    /// 完全交给调用方：每次要 `n` 字节就调一次闭包（FFI 层用它在几条固定字节串之间逐个切）。
    pub fn from_fn(source: impl FnMut(usize) -> Vec<u8> + 'a) -> Self {
        RandomSource {
            source: Box::new(source),
        }
    }

    /// 一个"递增字节"的确定性随机源（`0,1,2,…` 循环）：方便手写定值向量。
    pub fn counting(seed: u8) -> Self {
        let mut value = seed;
        RandomSource {
            source: Box::new(move |n: usize| {
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(value);
                    value = value.wrapping_add(1);
                }
                out
            }),
        }
    }

    /// 取 `n` 字节随机量。
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (self.source)(n)
    }
}

// ===========================================================================
// 写
// ===========================================================================

/// 用密码加密（"改用密码" / 给没配对的设备）。
///
/// * `iterations` 只在测试里改小；生产用 [`DEFAULT_ITERATIONS`]。
/// * 密码为空 / 迭代次数越界 / 快照太大 / 设备 id 不合法都是 `LibSyncError::Format`。
pub fn seal(
    plaintext: &[u8],
    password: &str,
    device: &str,
    generated_at: i64,
    seq: i64,
    iterations: u32,
    random: &mut RandomSource<'_>,
) -> Result<Vec<u8>> {
    if is_blank(password) {
        return Err(LibSyncError::Format("同步密码不能为空".to_string()));
    }
    if iterations < MIN_ITERATIONS {
        return Err(LibSyncError::Format(format!(
            "PBKDF2 迭代次数不能少于 {MIN_ITERATIONS}"
        )));
    }
    if iterations > MAX_ITERATIONS {
        return Err(LibSyncError::Format("PBKDF2 迭代次数太多".to_string()));
    }
    if plaintext.len() > MAX_PLAIN_BYTES {
        return Err(LibSyncError::Format(format!(
            "快照太大（{} 字节）",
            plaintext.len()
        )));
    }
    let device_bytes = b64::decode(device)
        .filter(|b| b.len() == DEVICE_BYTES)
        .ok_or_else(|| LibSyncError::Format("设备标识不是 8 字节的 base64url".to_string()))?;

    let salt = random.bytes(SALT_BYTES);
    let nonce = random.bytes(NONCE_BYTES);
    let mut header = Vec::with_capacity(PASSWORD_HEADER_BYTES);
    header.extend_from_slice(MAGIC_V2);
    header.push(VERSION);
    header.push(MODE_PASSWORD);
    header.extend_from_slice(&device_bytes);
    header.extend_from_slice(&generated_at.to_be_bytes());
    header.extend_from_slice(&seq.to_be_bytes());
    header.extend_from_slice(&nonce);
    header.extend_from_slice(&iterations.to_be_bytes());
    header.extend_from_slice(&salt);

    let mut key = crypto::password_key(password, &salt, iterations);
    let ciphertext = seal_body(&key, &nonce, plaintext, &header);
    key.fill(0);
    let mut out = header;
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// 按设备加密：正文用随机文件密钥，文件头里给每个收件设备包一份文件密钥。
///
/// * `recipients` 由调用方给（**应该**把本机自己放进来）；按设备 id 去重，最多 [`MAX_RECIPIENTS`] 台。
pub fn seal_to_recipients(
    plaintext: &[u8],
    device: &str,
    generated_at: i64,
    seq: i64,
    recipients: &[Recipient],
    random: &mut RandomSource<'_>,
) -> Result<Vec<u8>> {
    if plaintext.len() > MAX_PLAIN_BYTES {
        return Err(LibSyncError::Format(format!(
            "快照太大（{} 字节）",
            plaintext.len()
        )));
    }
    let device_bytes = b64::decode(device)
        .filter(|b| b.len() == DEVICE_BYTES)
        .ok_or_else(|| LibSyncError::Format("设备标识不是 8 字节的 base64url".to_string()))?;
    // distinctBy { it.device }：保留第一次出现的
    let mut unique: Vec<&Recipient> = Vec::new();
    for r in recipients {
        if !unique.iter().any(|u| u.device == r.device) {
            unique.push(r);
        }
    }
    if unique.is_empty() {
        return Err(LibSyncError::Format(
            "没有收件设备（先配对，或者改用密码）".to_string(),
        ));
    }
    if unique.len() > MAX_RECIPIENTS {
        return Err(LibSyncError::Format(format!(
            "收件设备太多（最多 {MAX_RECIPIENTS} 台）"
        )));
    }

    let file_key = random.bytes(FILE_KEY_BYTES);
    // 临时密钥对：私钥 / 公钥都从随机源里拿（这样测试能定值）
    let ephemeral_secret: [u8; EPK_BYTES] = random
        .bytes(EPK_BYTES)
        .try_into()
        .expect("随机源给的长度不对");
    let ephemeral_public = crypto::x25519_public_key(&ephemeral_secret);
    let nonce = random.bytes(NONCE_BYTES);

    // 头前缀到 epk 为止：每个收件人包密钥的 AAD（也是正文 AAD 的前半段）
    let mut prefix = Vec::with_capacity(PREFIX_TO_EPK_BYTES);
    prefix.extend_from_slice(MAGIC_V2);
    prefix.push(VERSION);
    prefix.push(MODE_RECIPIENTS);
    prefix.extend_from_slice(&device_bytes);
    prefix.extend_from_slice(&generated_at.to_be_bytes());
    prefix.extend_from_slice(&seq.to_be_bytes());
    prefix.extend_from_slice(&nonce);
    prefix.extend_from_slice(&ephemeral_public);

    let mut slots = Vec::new();
    slots.extend_from_slice(&(unique.len() as u16).to_be_bytes());
    for recipient in &unique {
        let mut shared = crypto::x25519_agree(&ephemeral_secret, &recipient.public_key)
            .ok_or_else(|| LibSyncError::Format("收件设备公钥不合法".to_string()))?;
        let mut salt = Vec::with_capacity(EPK_BYTES * 2);
        salt.extend_from_slice(&ephemeral_public);
        salt.extend_from_slice(&recipient.public_key);
        let mut wrap_key: [u8; 32] = crypto::hkdf_sha256(&shared, &salt, WRAP_INFO, 32)
            .try_into()
            .expect("HKDF 输出长度是 32");
        shared.fill(0);
        let wrap_nonce = random.bytes(WRAP_NONCE_BYTES);
        let wrapped = crypto::gcm_seal(&wrap_key, &wrap_nonce, &file_key, &prefix);
        wrap_key.fill(0);
        slots.extend_from_slice(
            &b64::decode(&recipient.device)
                .filter(|b| b.len() == DEVICE_BYTES)
                .ok_or_else(|| LibSyncError::Format("收件设备 id 不是 8 字节的 base64url".to_string()))?,
        );
        slots.extend_from_slice(&wrap_nonce);
        slots.extend_from_slice(&wrapped);
    }
    let mut header = prefix;
    header.extend_from_slice(&slots);
    let mut key = file_key.clone();
    let ciphertext = seal_body(&key, &nonce, plaintext, &header);
    key.fill(0);
    let mut out = header;
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

// ===========================================================================
// 读
// ===========================================================================

/// 用密码打开（v2 密码文件 / v1 旧文件）。
pub fn open(bytes: &[u8], password: &str) -> Result<Opened> {
    check_size(bytes)?;
    let version = version_of(bytes)?;
    match version {
        VERSION_PASSWORD_ONLY => open_v1(bytes, password),
        VERSION => {
            let header = read_header_v2(bytes)?;
            if header.mode != Mode::Password {
                return Err(LibSyncError::Format(
                    "这个文件是按设备加密的，要用本机的设备密钥打开（或者让导出方改用密码）".to_string(),
                ));
            }
            open_password_v2(bytes, &header, password)
        }
        other => Err(LibSyncError::Format(format!(
            "同步文件的格式版本是 {other}，这个程序认 {VERSION_PASSWORD_ONLY} 和 {VERSION}"
        ))),
    }
}

/// 用本机设备密钥打开按设备加密的文件。
///
/// * `device_id` = 本机的 8 字节 base64url 设备 id（**不用**传私钥以外的任何东西）。
/// * `private_key` = 本机的 X25519 私钥（32 字节）。
pub fn open_with_device_key(bytes: &[u8], device_id: &str, private_key: &[u8]) -> Result<Opened> {
    check_size(bytes)?;
    let version = version_of(bytes)?;
    if version == VERSION_PASSWORD_ONLY {
        return Err(LibSyncError::Format(
            "这个文件是老版本、用密码加密的，需要输入同步密码".to_string(),
        ));
    }
    if version != VERSION {
        return Err(LibSyncError::Format(format!(
            "同步文件的格式版本是 {version}，这个程序认 {VERSION_PASSWORD_ONLY} 和 {VERSION}"
        )));
    }
    let header = read_header_v2(bytes)?;
    if header.mode != Mode::Recipients {
        return Err(LibSyncError::Format(
            "这个文件是用密码加密的，需要输入同步密码".to_string(),
        ));
    }
    if !header.recipients.iter().any(|d| d == device_id) {
        return Err(LibSyncError::Format(format!(
            "这个同步文件不是发给这台设备的（文件里 {} 台收件设备，没有本机）。让导出方把你加进已配对设备，或者改用密码导出",
            header.recipients.len()
        )));
    }
    if private_key.len() != EPK_BYTES {
        return Err(LibSyncError::Format("本机设备私钥长度不对".to_string()));
    }
    let secret: [u8; EPK_BYTES] = private_key.try_into().expect("长度已经检查过");
    let public_key = crypto::x25519_public_key(&secret);
    let body_start = header_bytes_of(bytes)?;
    let prefix = &bytes[..PREFIX_TO_EPK_BYTES];
    let ephemeral_public = &bytes[COMMON_HEADER_BYTES..PREFIX_TO_EPK_BYTES];
    let mut file_key = unwrap_file_key(bytes, body_start, ephemeral_public, prefix, device_id, &secret, &public_key)?;
    let header_bytes = &bytes[..body_start];
    let nonce = &bytes[COMMON_HEADER_BYTES - NONCE_BYTES..COMMON_HEADER_BYTES];
    let plaintext = open_body(&file_key, nonce, &bytes[body_start..], header_bytes);
    file_key.fill(0);
    let plaintext = plaintext?;
    Ok(Opened {
        header,
        plaintext,
        unlock: Unlock::DeviceKey,
    })
}

/// 文件头（不解密）。**只用于显示**：是否可信要等 `open` / `open_with_device_key` 的 AAD 验过。
pub fn read_header(bytes: &[u8]) -> Result<Header> {
    let version = version_of(bytes)?;
    match version {
        VERSION_PASSWORD_ONLY => read_header_v1(bytes),
        VERSION => read_header_v2(bytes),
        other => Err(LibSyncError::Format(format!(
            "同步文件的格式版本是 {other}，这个程序认 {VERSION_PASSWORD_ONLY} 和 {VERSION}"
        ))),
    }
}

/// 这个文件有没有本机的收件条目。读头部，不解密。
pub fn has_recipient(bytes: &[u8], device_id: &str) -> bool {
    match read_header(bytes) {
        Ok(header) => header.mode == Mode::Recipients && header.recipients.iter().any(|d| d == device_id),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------- v2 实现

fn open_password_v2(bytes: &[u8], header: &Header, password: &str) -> Result<Opened> {
    let mut p = COMMON_HEADER_BYTES;
    let iterations = get_u32(bytes, p);
    p += 4;
    if !(MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations) {
        return Err(LibSyncError::Format(format!(
            "文件的密码派生参数不被接受（{iterations} 次）"
        )));
    }
    let salt = &bytes[p..p + SALT_BYTES];
    let header_bytes = &bytes[..PASSWORD_HEADER_BYTES];
    let mut key = crypto::password_key(password, salt, iterations);
    let nonce = &bytes[COMMON_HEADER_BYTES - NONCE_BYTES..COMMON_HEADER_BYTES];
    let plaintext = open_body(&key, nonce, &bytes[PASSWORD_HEADER_BYTES..], header_bytes);
    key.fill(0);
    let mut header = header.clone();
    header.iterations = iterations;
    Ok(Opened {
        header,
        plaintext: plaintext?,
        unlock: Unlock::Password,
    })
}

fn read_header_v2(bytes: &[u8]) -> Result<Header> {
    if bytes.len() < COMMON_HEADER_BYTES {
        return Err(LibSyncError::Format("文件不完整".to_string()));
    }
    let version = bytes[8];
    if version != VERSION {
        return Err(LibSyncError::Format(format!(
            "同步文件的格式版本是 {version}，这个程序只认 {VERSION}"
        )));
    }
    let mode = match bytes[9] {
        MODE_PASSWORD => Mode::Password,
        MODE_RECIPIENTS => Mode::Recipients,
        other => {
            return Err(LibSyncError::Format(format!(
                "不认识的同步文件模式（{other}）"
            )))
        }
    };
    let device = b64::encode(&bytes[10..18]);
    let generated_at = get_i64(bytes, 18);
    let seq = get_i64(bytes, 26);
    if mode == Mode::Password {
        if bytes.len() < PASSWORD_HEADER_BYTES {
            return Err(LibSyncError::Format("文件不完整".to_string()));
        }
        let iterations = get_u32(bytes, COMMON_HEADER_BYTES);
        return Ok(Header {
            version: VERSION,
            mode,
            iterations,
            device,
            generated_at,
            seq,
            recipients: Vec::new(),
        });
    }
    if bytes.len() < PREFIX_TO_EPK_BYTES + 2 {
        return Err(LibSyncError::Format("文件不完整".to_string()));
    }
    let count = ((bytes[PREFIX_TO_EPK_BYTES] as usize) << 8) | bytes[PREFIX_TO_EPK_BYTES + 1] as usize;
    if !(1..=MAX_RECIPIENTS).contains(&count) {
        return Err(LibSyncError::Format(format!("收件设备数量不对（{count}）")));
    }
    let expected_header = PREFIX_TO_EPK_BYTES + 2 + count * RECIPIENT_SLOT_BYTES;
    if bytes.len() < expected_header {
        return Err(LibSyncError::Format("文件不完整".to_string()));
    }
    let recipients = (0..count)
        .map(|i| {
            let off = PREFIX_TO_EPK_BYTES + 2 + i * RECIPIENT_SLOT_BYTES;
            b64::encode(&bytes[off..off + DEVICE_BYTES])
        })
        .collect();
    Ok(Header {
        version: VERSION,
        mode,
        iterations: 0,
        device,
        generated_at,
        seq,
        recipients,
    })
}

/// 收件人模式下文件头到哪儿结束（密文从哪儿开始）。
fn header_bytes_of(bytes: &[u8]) -> Result<usize> {
    if bytes.len() < COMMON_HEADER_BYTES {
        return Err(LibSyncError::Format("文件不完整".to_string()));
    }
    if bytes[9] == MODE_PASSWORD {
        return Ok(PASSWORD_HEADER_BYTES);
    }
    if bytes.len() < PREFIX_TO_EPK_BYTES + 2 {
        return Err(LibSyncError::Format("文件不完整".to_string()));
    }
    let count = ((bytes[PREFIX_TO_EPK_BYTES] as usize) << 8) | bytes[PREFIX_TO_EPK_BYTES + 1] as usize;
    Ok(PREFIX_TO_EPK_BYTES + 2 + count * RECIPIENT_SLOT_BYTES)
}

#[allow(clippy::too_many_arguments)]
fn unwrap_file_key(
    bytes: &[u8],
    body_start: usize,
    ephemeral_public: &[u8],
    prefix: &[u8],
    device_id: &str,
    secret: &[u8; EPK_BYTES],
    public_key: &[u8; EPK_BYTES],
) -> Result<Vec<u8>> {
    let count = ((bytes[PREFIX_TO_EPK_BYTES] as usize) << 8) | bytes[PREFIX_TO_EPK_BYTES + 1] as usize;
    for i in 0..count {
        let off = PREFIX_TO_EPK_BYTES + 2 + i * RECIPIENT_SLOT_BYTES;
        let device = b64::encode(&bytes[off..off + DEVICE_BYTES]);
        if device != device_id {
            continue;
        }
        if body_start <= off {
            return Err(LibSyncError::Format("文件不完整".to_string()));
        }
        let wrap_nonce = &bytes[off + DEVICE_BYTES..off + DEVICE_BYTES + WRAP_NONCE_BYTES];
        let wrapped = &bytes[off + DEVICE_BYTES + WRAP_NONCE_BYTES..off + RECIPIENT_SLOT_BYTES];
        let mut shared = crypto::x25519_agree(secret, ephemeral_public)
            .ok_or_else(|| LibSyncError::Format("收件设备公钥不合法".to_string()))?;
        let mut salt = Vec::with_capacity(EPK_BYTES * 2);
        salt.extend_from_slice(ephemeral_public);
        salt.extend_from_slice(public_key);
        let mut wrap_key: [u8; 32] = crypto::hkdf_sha256(&shared, &salt, WRAP_INFO, 32)
            .try_into()
            .expect("HKDF 输出长度是 32");
        shared.fill(0);
        let opened = crypto::gcm_open(&wrap_key, wrap_nonce, wrapped, prefix);
        wrap_key.fill(0);
        return opened.ok_or_else(|| {
            LibSyncError::Format("文件被改过，或者本机的设备密钥和配对时不是同一把".to_string())
        });
    }
    Err(LibSyncError::Format(
        "这个同步文件不是发给这台设备的（没有本机的收件条目）".to_string(),
    ))
}

// ---------------------------------------------------------------- v1（旧格式，只读）

fn open_v1(bytes: &[u8], password: &str) -> Result<Opened> {
    if bytes.len() < V1_HEADER_BYTES + TAG_BYTES {
        return Err(LibSyncError::Format("文件不完整".to_string()));
    }
    let mut p = 8;
    let version = bytes[p];
    p += 1;
    if version != VERSION_PASSWORD_ONLY {
        return Err(LibSyncError::Format(format!(
            "同步文件的格式版本是 {version}，这个程序认 {VERSION_PASSWORD_ONLY} 和 {VERSION}"
        )));
    }
    let kdf = bytes[p];
    p += 1;
    if kdf != KDF_PBKDF2_HMAC_SHA256 {
        return Err(LibSyncError::Format(format!(
            "不支持的密钥派生算法（{kdf}）"
        )));
    }
    let iterations = get_u32(bytes, p);
    p += 4;
    if !(MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations) {
        return Err(LibSyncError::Format(format!(
            "文件的密码派生参数不被接受（{iterations} 次）"
        )));
    }
    let salt = &bytes[p..p + SALT_BYTES];
    p += SALT_BYTES;
    let device = b64::encode(&bytes[p..p + DEVICE_BYTES]);
    p += DEVICE_BYTES;
    let generated_at = get_i64(bytes, p);
    p += 8;
    let seq = get_i64(bytes, p);
    p += 8;
    let nonce = &bytes[p..p + NONCE_BYTES];
    let header_bytes = &bytes[..V1_HEADER_BYTES];
    let mut key = crypto::password_key(password, salt, iterations);
    let plaintext = open_body(&key, nonce, &bytes[V1_HEADER_BYTES..], header_bytes);
    key.fill(0);
    Ok(Opened {
        header: Header {
            version: VERSION_PASSWORD_ONLY,
            mode: Mode::Password,
            iterations,
            device,
            generated_at,
            seq,
            recipients: Vec::new(),
        },
        plaintext: plaintext?,
        unlock: Unlock::Password,
    })
}

fn read_header_v1(bytes: &[u8]) -> Result<Header> {
    if bytes.len() < V1_HEADER_BYTES {
        return Err(LibSyncError::Format("文件不完整".to_string()));
    }
    let mut p = 8;
    let version = bytes[p];
    p += 1;
    if version != VERSION_PASSWORD_ONLY {
        return Err(LibSyncError::Format(format!(
            "同步文件的格式版本是 {version}，这个程序只认 {VERSION_PASSWORD_ONLY}"
        )));
    }
    let kdf = bytes[p];
    p += 1;
    if kdf != KDF_PBKDF2_HMAC_SHA256 {
        return Err(LibSyncError::Format("不支持的密钥派生算法".to_string()));
    }
    let iterations = get_u32(bytes, p);
    p += 4;
    p += SALT_BYTES;
    let device = b64::encode(&bytes[p..p + DEVICE_BYTES]);
    p += DEVICE_BYTES;
    let generated_at = get_i64(bytes, p);
    p += 8;
    let seq = get_i64(bytes, p);
    Ok(Header {
        version: VERSION_PASSWORD_ONLY,
        mode: Mode::Password,
        iterations,
        device,
        generated_at,
        seq,
        recipients: Vec::new(),
    })
}

// ---------------------------------------------------------------- 公共小件

fn check_size(bytes: &[u8]) -> Result<()> {
    if bytes.len() < COMMON_HEADER_BYTES {
        return Err(LibSyncError::Format(format!(
            "文件不完整（只有 {} 字节）",
            bytes.len()
        )));
    }
    if bytes.len() > MAX_FILE_BYTES {
        return Err(LibSyncError::Format(format!(
            "文件太大（{} MB，最多 {} MB）",
            bytes.len() / 1024 / 1024,
            MAX_FILE_BYTES / 1024 / 1024
        )));
    }
    let magic = &bytes[..8];
    if magic != MAGIC_V1 && magic != MAGIC_V2 {
        return Err(LibSyncError::Format("不是 feedhub 同步文件".to_string()));
    }
    // magic 和版本必须配套
    let version = bytes[8];
    if magic == MAGIC_V2 && version == VERSION_PASSWORD_ONLY {
        return Err(LibSyncError::Format(
            "同步文件头不一致（新文件头配了旧版本号）".to_string(),
        ));
    }
    if magic == MAGIC_V1 && version != VERSION_PASSWORD_ONLY {
        return Err(LibSyncError::Format(
            "同步文件头不一致（旧文件头配了新版本号）".to_string(),
        ));
    }
    Ok(())
}

fn version_of(bytes: &[u8]) -> Result<u8> {
    if bytes.len() < 9 {
        return Err(LibSyncError::Format("文件不完整".to_string()));
    }
    let magic = &bytes[..8];
    if magic != MAGIC_V1 && magic != MAGIC_V2 {
        return Err(LibSyncError::Format("不是 feedhub 同步文件".to_string()));
    }
    Ok(bytes[8])
}

/// 正文：明文 → gzip → 密文（tag 在后面）。
fn seal_body(key: &[u8], nonce: &[u8], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    let compressed = gzip(plaintext);
    crypto::gcm_seal(key, nonce, &compressed, aad)
}

fn open_body(key: &[u8], nonce: &[u8], ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    if ciphertext.len() < TAG_BYTES {
        return Err(LibSyncError::Format("文件不完整".to_string()));
    }
    let compressed = crypto::gcm_open(key, nonce, ciphertext, aad)
        .ok_or_else(|| LibSyncError::Format("密码不对，或者文件被改过 / 不完整".to_string()))?;
    gunzip(&compressed)
}

/// gzip（`GZIPOutputStream` 等价：mtime = 0、OS 字节不写（255），所以**同一份输入每次压出来一样**）。
pub fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).expect("写进内存里的 gzip 不会失败");
    encoder.finish().expect("收尾内存里的 gzip 不会失败")
}

/// 解压；超过 [`MAX_PLAIN_BYTES`] 直接拒（防 zip bomb）。
pub fn gunzip(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut decoder = flate2::read::GzDecoder::new(data);
    let mut out = Vec::new();
    // 多读一个字节就能发现"解压后超上限"
    let limit = MAX_PLAIN_BYTES as u64 + 1;
    decoder
        .by_ref()
        .take(limit)
        .read_to_end(&mut out)
        .map_err(|_| LibSyncError::Format("文件里的数据解不开（不是 gzip）".to_string()))?;
    if out.len() > MAX_PLAIN_BYTES {
        return Err(LibSyncError::Format(format!(
            "解压后超过 {} MB，这个文件不对",
            MAX_PLAIN_BYTES / 1024 / 1024
        )));
    }
    Ok(out)
}

fn get_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3]])
}

fn get_i64(buf: &[u8], offset: usize) -> i64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[offset..offset + 8]);
    i64::from_be_bytes(bytes)
}

/// 给文件头 / 报文算 AAD 之外，**导出方**还要用的一个小工具：把 8 字节设备 id 编成 base64url。
pub fn device_id(bytes: &[u8]) -> Result<String> {
    if bytes.len() != DEVICE_BYTES {
        return Err(LibSyncError::Format("设备标识不是 8 字节".to_string()));
    }
    Ok(b64::encode(bytes))
}

