//! 资料库同步用到的密码学：**算法 / 参数 / 编码必须和 Kotlin 那边完全一致**（老版本 App 要能和新版本
//! 互相同步），所以每一样都配了逐字节对拍的单测。
//!
//! | 这一层 | Kotlin 那边 | 参数 |
//! |---|---|---|
//! | [`gcm_seal`] / [`gcm_open`] | `Cipher.getInstance("AES/GCM/NoPadding")` | AES-256、**12 字节 nonce**、**128 位 tag**、tag 跟在密文后 |
//! | [`password_key`] | `SecretKeyFactory("PBKDF2WithHmacSHA256")` | 32 字节输出；口令按 **UTF-8** 编（`PBEKeySpec` 的 `char[]` 在 SunJCE 里就是 UTF-8） |
//! | [`x25519_generate`] / [`x25519_agree`] | BouncyCastle `X25519Agreement` | 裸 32 字节标量 / u 坐标；低阶点（共享秘密全 0）算失败 |
//! | [`hkdf_sha256`] | BouncyCastle `HKDFBytesGenerator(SHA256Digest)` | RFC 5869，`salt` / `info` 原样用 |
//! | [`sha256`] | `MessageDigest("SHA-256")` | — |
//! | [`pairing_code`] | `PairingCode.of` | 域串 + 按设备 id 排序后拼两段（id + 公钥），取摘要前 4 字节 % 1,000,000，补 6 位 |
//!
//! ## 随机量从外面传
//!
//! 和平台层一样的规矩：**这一层不取随机数**（除了 [`x25519_generate`] 这种"就是要一把新密钥"的便利入口）。
//! 加密 / 配对报文里的 nonce、salt、临时密钥都由调用方给（生产是宿主的 `SecureRandom`，测试是固定序列），
//! 对拍才逐字节可比。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};

/// 同步密钥长度（AES-256）。
pub const KEY_BYTES: usize = 32;
/// GCM 的 nonce 长度（96 位，AES-GCM 的推荐值；JDK 的 `GCMParameterSpec` 也按这个长度给）。
pub const NONCE_BYTES: usize = 12;
/// GCM 的认证标签长度（128 位）。
pub const TAG_BYTES: usize = 16;
/// X25519 公钥 / 私钥长度。
pub const X25519_BYTES: usize = 32;

/// AES-256-GCM 封装：返回 `ciphertext || tag`（**不带 nonce**）。
///
/// Kotlin 的 `Cipher.doFinal` 就是这个形状（tag 附在密文后面），所以两边的"封装结果"能直接互认；
/// nonce 由调用方自己放在前面（`SyncCrypto.seal` 拼、`LibrarySyncFile` 放文件头）。
///
/// # Panics
/// 密钥不是 32 字节时 panic——这是编程错误（Kotlin 那边 `require` 抛 `IllegalArgumentException`）。
pub fn gcm_seal(key: &[u8], nonce: &[u8], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    assert_eq!(key.len(), KEY_BYTES, "同步密钥必须是 {KEY_BYTES} 字节");
    assert_eq!(nonce.len(), NONCE_BYTES, "GCM nonce 必须是 {NONCE_BYTES} 字节");
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 的密钥长度已经断言");
    cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .unwrap_or_else(|_| unreachable!("AES-GCM 加密不会失败（入参已经检查过长度）"))
}

/// 解开 [`gcm_seal`] 的结果；**密钥不对 / 内容被改 / AAD 不匹配 / 长度不对一律 `None`**
/// （不抛细节，免得调用方误当成"再试一次"——和 Kotlin `SyncCrypto.open` 一个口径）。
pub fn gcm_open(key: &[u8], nonce: &[u8], sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    if key.len() != KEY_BYTES || nonce.len() != NONCE_BYTES {
        return None;
    }
    if sealed.len() < TAG_BYTES {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: sealed,
                aad,
            },
        )
        .ok()
}

/// PBKDF2-HMAC-SHA256 → 32 字节（`LibrarySyncFile` 的密码模式）。
///
/// 口令按 **UTF-8** 编码：SunJCE 的 `PBKDF2WithHmacSHA256` 把 `char[]` 按 UTF-8 转字节（不是 JIS / 平台编码），
/// 所以中文 / emoji 密码两边算出来一样（`crypto::tests` 里有一组中文口令的定值向量）。
pub fn password_key(password: &str, salt: &[u8], iterations: u32) -> [u8; KEY_BYTES] {
    let mut out = [0u8; KEY_BYTES];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, iterations, &mut out);
    out
}

/// 生成一把新的 X25519 密钥对 `(私钥, 公钥)`。
///
/// 私钥是**未做 clamping 的原始 32 字节**——和 BouncyCastle 的 `X25519PrivateKeyParameters.encoded` 一致
/// （clamp 在算点的时候做）。所以私钥可以原样落盘、原样读回来。
pub fn x25519_generate() -> ([u8; X25519_BYTES], [u8; X25519_BYTES]) {
    let mut secret = [0u8; X25519_BYTES];
    getrandom::getrandom(&mut secret).expect("读系统随机数失败");
    (secret, x25519_public_key(&secret))
}

/// 私钥 → 公钥（`X25519PrivateKeyParameters.generatePublicKey().encoded`）。
pub fn x25519_public_key(secret: &[u8; X25519_BYTES]) -> [u8; X25519_BYTES] {
    let sk = x25519_dalek::StaticSecret::from(*secret);
    x25519_dalek::PublicKey::from(&sk).to_bytes()
}

/// X25519 共享秘密。对方公钥不是 32 字节、或者是个低阶点（共享秘密全 0）→ `None`
/// （Kotlin 抛 `SyncException("和对方的设备公钥算不出共享密钥（公钥不合法）")`）。
pub fn x25519_agree(secret: &[u8; X25519_BYTES], peer_public: &[u8]) -> Option<[u8; X25519_BYTES]> {
    if peer_public.len() != X25519_BYTES {
        return None;
    }
    let mut peer = [0u8; X25519_BYTES];
    peer.copy_from_slice(peer_public);
    let sk = x25519_dalek::StaticSecret::from(*secret);
    let shared = sk.diffie_hellman(&x25519_dalek::PublicKey::from(peer));
    // 低阶点：dalek 的 was_contributory() 检查共享秘密不是全 0（等价于 Kotlin 那边 `secret.all { it == 0 }`）
    if !shared.was_contributory() {
        return None;
    }
    Some(shared.to_bytes())
}

/// HKDF-SHA256（RFC 5869），输出 [`KEY_BYTES`]。
///
/// `length` 可以不是 32（`Hkdf.sha256` 的调用方现在只用 32，但 KAT 里可能量别的长度）。
pub fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = vec![0u8; length];
    hk.expand(info, &mut out)
        .expect("HKDF 输出长度不合法（最多 255*32 字节）");
    out
}

/// SHA-256。
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// 配对时给用户看的 **6 位短校验码**（Kotlin `PairingCode.of`）。
///
/// 两台设备算出来必须一样（和设备顺序无关），所以按**设备 id** 排序后再拼：
/// `SHA-256("feedhub-pair|v1|" + [id + 公钥] + [id + 公钥])` 的前 4 字节按大端当无符号数，对 1,000,000
/// 取余，左边补 0 到 6 位。
///
/// 返回 `None`：两边设备 id 相同（不能和自己配对）、或者公钥不是 32 字节。Kotlin 那边是 `require` 抛
/// `IllegalArgumentException`——门面映射成同名的 Kotlin 异常。
pub fn pairing_code(
    a_device: &str,
    a_public_key: &[u8],
    b_device: &str,
    b_public_key: &[u8],
) -> Option<String> {
    if a_device == b_device {
        return None;
    }
    if a_public_key.len() != X25519_BYTES || b_public_key.len() != X25519_BYTES {
        return None;
    }
    // 按设备 id 的**字符串**比较（Kotlin 的 `aDevice < bDevice` 是 `String.compareTo`，UTF-16 码元序；
    // 设备 id 是 base64url 的 ASCII，所以和 UTF-8 字节序一致）
    let ordered: [(&str, &[u8]); 2] = if a_device < b_device {
        [(a_device, a_public_key), (b_device, b_public_key)]
    } else {
        [(b_device, b_public_key), (a_device, a_public_key)]
    };
    let mut hasher = Sha256::new();
    hasher.update(b"feedhub-pair|v1|");
    for (device, key) in ordered {
        hasher.update(device.as_bytes());
        hasher.update(key);
    }
    let digest = hasher.finalize();
    let value = ((digest[0] as u64) << 24)
        | ((digest[1] as u64) << 16)
        | ((digest[2] as u64) << 8)
        | (digest[3] as u64);
    Some(format!("{:06}", value % 1_000_000))
}

/// HLC（混合逻辑时钟）的编解码与比较（`core/sync/LibrarySyncModel.kt` 的 `Hlc`）。
///
/// 布局：`物理毫秒 << 16 | 逻辑计数`（计数 0..65535）。比较两个"版本"要连设备 id 一起比：
/// HLC 相同（两台恰好同一毫秒、计数也相同）时按设备 id 字典序定胜负，交换律才成立。
pub mod hlc {
    /// 计数占多少位。
    pub const COUNTER_BITS: u32 = 16;
    /// 计数的最大值。
    pub const MAX_COUNTER: i32 = 0xFFFF;

    /// 把物理毫秒 + 计数拼成一个 HLC。
    pub fn of(physical_ms: i64, counter: i32) -> i64 {
        (physical_ms << COUNTER_BITS) | (counter & MAX_COUNTER) as i64
    }

    /// HLC 里的物理毫秒（无符号右移：HLC 永远是非负的）。
    pub fn physical(hlc: i64) -> i64 {
        ((hlc as u64) >> COUNTER_BITS) as i64
    }

    /// HLC 里的逻辑计数。
    pub fn counter(hlc: i64) -> i32 {
        (hlc & 0xFFFF) as i32
    }

    /// 先比 HLC，再比设备 id（返回值同 `Comparable.compareTo`：负 / 0 / 正）。
    pub fn compare_version(hlc_a: i64, dev_a: &str, hlc_b: i64, dev_b: &str) -> i32 {
        if hlc_a != hlc_b {
            return if hlc_a < hlc_b { -1 } else { 1 };
        }
        // Kotlin 的 `String.compareTo` 是 UTF-16 码元序；设备 id 是 base64url 的 ASCII，
        // 和 Rust 的 `str` 比较（UTF-8 字节序）在 ASCII 上一致。
        match dev_a.cmp(dev_b) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }

    /// 取两个版本里较大的那一个的 HLC（平局时返回 `hlc_a`）。
    pub fn max_version(hlc_a: i64, dev_a: &str, hlc_b: i64, dev_b: &str) -> i64 {
        if compare_version(hlc_a, dev_a, hlc_b, dev_b) >= 0 {
            hlc_a
        } else {
            hlc_b
        }
    }
}

