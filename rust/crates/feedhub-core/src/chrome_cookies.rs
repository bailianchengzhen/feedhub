//! **从本机 Chrome 导入 Cookie**：`app/desktopMain/.../login/ChromeCookieImporter.kt` 的 Rust 版
//!
//! 这里搬的是**决策和计算**那一半：
//! - Cookie 库里每个平台收哪些行（`CookieImportSpec` 的 `matches`：后缀 / 精确 host）；
//! - `expires_utc`（Windows 的 1601 纪元，单位微秒）→ Unix 毫秒；
//! - 前缀 `v10` / `v11`、AES-128-CBC（IV 是 16 个空格）、PBKDF2-HMAC-SHA1(密码, `"saltysalt"`, 1 次, 16 字节)
//!   → 明文；库版本 ≥ 24 时校验 / 去掉前面 32 字节的 `sha256(host_key)`；
//! - 一条 Cookie 的最终形状（带不带前导点 = hostOnly、`path` 空则 `/`、过期的不收）；
//! - **导入动作的顺序**：先按域分组、再整组 `clear_domain` + `put`（Kotlin 那边就是这个顺序）。
//!
//! **留在宿主的**：
//! - 打开 SQLite 文件（`androidx.sqlite` 的 bundled 驱动 + `BundledSqliteNative` 的预加载）；
//!   Rust 这边只认 [`ChromeCookieRow`]，行是宿主读出来交给我们的；
//! - 复制库文件到私有临时目录（Chrome 运行时锁着库）——`std::fs` 其实也能做，但"临时目录 + 600 权限 +
//!   读完删"是宿主 <b>文件系统策略</b>的一部分，和 `cookies.json` 的落盘放在一起（桌面 / 安卓各一份）；
//! - 调 `kwallet-query` / `secret-tool` 拿 Chrome Safe Storage 密码：**命令怎么跑**（超时 120 秒、
//!   读 stdout、丢弃 stderr）留在宿主，**拿到的密码怎么变成 key**（PBKDF2）在 Rust。
//!
//! ## 密钥绝不外泄
//!
//! 密码 / key / 明文都只在 Rust 内存里过一遍：过 FFI 的只有 [`ChromeCookieRow`]（密文）+ 密码（宿主交进来
//! 的那一份）+ 导入结果的数量。**返回的错误里不带任何值**（Kotlin 那边也是如此，只带 host / 原因）。

use crate::cookies::StoredCookie;

/// 一条从 Chrome 的 `cookies` 表读出来的行（宿主读，Rust 解）。
///
/// 列就是 `ChromeCookieImporter.readRows` 读的那六列：`host_key` / `name` / `path` / `expires_utc` /
/// `is_secure` / `is_httponly` / `encrypted_value`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeCookieRow {
    pub host_key: String,
    pub name: String,
    pub path: String,
    pub expires_utc: i64,
    pub is_secure: bool,
    pub is_httponly: bool,
    pub encrypted_value: Vec<u8>,
}

/// 导入范围（Kotlin `CookieImportSpec`）。
///
/// - `domain`：导入前先清掉的域（`FeedCookieJar.clearDomain`），一般是主域名；
/// - `suffixes`：这些域名本身和它们的子域都收；
/// - `exact_hosts`：只收这几个 host（百度只要 `.baidu.com` 和 `tieba.baidu.com`，网盘等子域不碰）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieImportSpec {
    pub domain: String,
    pub suffixes: Vec<String>,
    pub exact_hosts: Vec<String>,
}

impl CookieImportSpec {
    /// 只有一个后缀（大多数平台）：`CookieImportSpec("bilibili.com")`。
    pub fn new(domain: &str) -> Self {
        Self {
            domain: domain.to_string(),
            suffixes: vec![domain.to_string()],
            exact_hosts: Vec::new(),
        }
    }

    /// Kotlin `CookieImportSpec.matches`：精确 host 先命中，否则去前导点之后比后缀（本身或子域）。
    pub fn matches(&self, host: &str) -> bool {
        if self.exact_hosts.iter().any(|h| h == host) {
            return true;
        }
        let d = host.trim_start_matches('.');
        self.suffixes
            .iter()
            .any(|s| d == s.as_str() || d.ends_with(&format!(".{s}")))
    }
}

/// Chrome 的纪元：1601-01-01 到 1970-01-01 的毫秒数（`expires_utc` 是**微秒**，先除 1000）。
const WINDOWS_EPOCH_OFFSET_MS: i64 = 11_644_473_600_000;
/// 浏览器请求头 / Chrome 里没有过期时间时给一年。
const YEAR_MS: i64 = 365 * 24 * 3600 * 1000;

/// 一次导入的结论（只有数量，**从来没有值**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    /// 收进来的条数。
    pub imported: usize,
    /// 解得开但有坏值的条数（Kotlin 的 `failed`：解密抛了非致命异常就 `continue`）。
    pub failed: usize,
    /// 先清哪个域（Kotlin 的 `cookies.clearDomain(target.domain)`；`imported == 0` 时不清）。
    pub clear_domain: Option<String>,
    /// 要写进罐子的条目，**已经按域分好组、组内保持库里的顺序**。
    pub groups: Vec<(String, Vec<StoredCookie>)>,
}

/// 把整个 Chrome Cookie 库解成一条条 Cookie（按平台）。
///
/// 参数：
/// - `db_version`：`SELECT value FROM meta WHERE key = 'version'` 读出来的那个数
///   （≥ 24 的库在明文前面多 32 字节 `sha256(host_key)`）；
/// - `password_v11`：`v11` 那批用的密码（KWallet / 钥匙环）；
/// - `rows`：整张 `cookies` 表（宿主读的，**顺序就是库里的顺序**）；
/// - `spec`：这个平台收哪些 host；
/// - `now_ms`：注入的时钟（判过期）。
///
/// 返回 `None` = 一条都没收（和 Kotlin 一样：`imported` 为空时**不清**这个平台的域）。
///
/// **解不开的行**：`v10` / `v11` 以外的前缀、AES 填充不对、`sha256(host_key)` 不对 → 计入 `failed`
/// 继续下一行（Kotlin 那边 `catch (_: Exception) { failed++; continue }`）。
/// **空名字**的行直接跳过（不算 failed）。
pub fn import_rows(
    db_version: i64,
    password_v10: &str,
    password_v11: &str,
    rows: &[ChromeCookieRow],
    spec: &CookieImportSpec,
    now_ms: i64,
) -> Option<ImportOutcome> {
    let mut imported: Vec<StoredCookie> = Vec::new();
    let mut failed = 0usize;
    for r in rows {
        if !spec.matches(&r.host_key) || r.name.trim().is_empty() {
            // Kotlin: `if (!target.matches(r.host) || r.name.isBlank()) continue`
            continue;
        }
        let password = if r.encrypted_value.starts_with(b"v10") {
            password_v10
        } else {
            password_v11
        };
        let value = match decrypt_value(&r.encrypted_value, &r.host_key, db_version, password) {
            Ok(v) => v,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        let expires = if r.expires_utc > 0 {
            r.expires_utc / 1000 - WINDOWS_EPOCH_OFFSET_MS
        } else {
            now_ms + YEAR_MS
        };
        if expires <= now_ms {
            continue;
        }
        let host = r.host_key.trim_start_matches('.');
        imported.push(StoredCookie {
            name: r.name.clone(),
            value,
            domain: host.to_string(),
            path: if r.path.is_empty() { "/".to_string() } else { r.path.clone() },
            expires_at_ms: expires,
            secure: r.is_secure,
            http_only: r.is_httponly,
            // 带前导点 = 域 Cookie；不带 = hostOnly（Kotlin 的 `if (r.host.startsWith('.'))`）
            host_only: !r.host_key.starts_with('.'),
        });
    }
    if imported.is_empty() {
        return None;
    }
    // 按域分组（组的先后 = 这个域第一次出现的先后；Kotlin 用的是 `groupBy`，LinkedHashMap 的语义）
    let mut groups: Vec<(String, Vec<StoredCookie>)> = Vec::new();
    for c in imported {
        match groups.iter_mut().find(|(d, _)| *d == c.domain) {
            Some((_, list)) => list.push(c),
            None => groups.push((c.domain.clone(), vec![c])),
        }
    }
    let total = groups.iter().map(|(_, l)| l.len()).sum();
    Some(ImportOutcome {
        imported: total,
        failed,
        clear_domain: Some(spec.domain.trim_start_matches('.').to_string()),
        groups,
    })
}

/// 解一行 `encrypted_value`（Chrome 在 Linux 上的 `v10` / `v11`，AES-128-CBC）。
///
/// 逐条照 `ChromeCookieImporter.decrypt`：
/// 1. 前 3 个字节必须是 `v10` / `v11`（别的抛 "unexpected prefix"）；
/// 2. 剩下的按 AES-128-CBC / PKCS5 解，key 由 [`derive_key`] 从密码算；
/// 3. `db_version >= 24` 时明文前面是 32 字节 `sha256(host_key)`，对不上就是坏行，对上则去掉；
/// 4. 剩下的按 UTF-8 解。
pub fn decrypt_value(
    encrypted: &[u8],
    host_key: &str,
    db_version: i64,
    password: &str,
) -> Result<String, String> {
    if encrypted.len() < 3 {
        return Err("unexpected prefix".into());
    }
    let prefix = &encrypted[..3];
    if prefix != b"v10" && prefix != b"v11" {
        return Err("unexpected prefix".into());
    }
    let key = derive_key(password)?;
    let mut plain = aes_128_cbc_decrypt(&key, &[b' '; 16], &encrypted[3..])?;
    if db_version >= 24 {
        if plain.len() < 32 {
            return Err("domain hash mismatch".into());
        }
        let hash = sha256(host_key.as_bytes());
        if plain[..32] != hash[..] {
            return Err("domain hash mismatch".into());
        }
        plain.drain(..32);
    }
    String::from_utf8(plain).map_err(|_| "不是 UTF-8".to_string())
}

/// PBKDF2-HMAC-SHA1(密码, `"saltysalt"`, **1** 次, 16 字节)（Kotlin `derive`）。
///
/// 而那个 crate 当时不在
/// 离线缓存里）。
/// SHA-1 输出 20 字节）。
/// 定值向量（JDK 21 的 `SecretKeyFactory.getInstance("PBKDF2WithHmacSHA1")` 跑出来的 key）在
/// `derive_key_matches_jdk` 里钉着。
pub fn derive_key(password: &str) -> Result<[u8; 16], String> {
    let mut out = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password.as_bytes(), b"saltysalt", 1, &mut out);
    Ok(out)
}

/// HMAC-SHA1（RFC 2104）。
///
/// `pbkdf2` 内部用的就是它，但 `chrome_cookies` 的定值向量测试（RFC 2202 那三条）要一个能直接调的入口，
/// 所以这里显式用 `hmac` + `sha1` 再包一层。**不是**自己实现 HMAC：那两行就是 RustCrypto 的
/// `Hmac<Sha1>` 的标准用法。
pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<sha1::Sha1> as Mac>::new_from_slice(key).expect("HMAC 接受任意长度的 key");
    mac.update(data);
    let out = mac.finalize().into_bytes();
    let mut buf = [0u8; 20];
    buf.copy_from_slice(&out);
    buf
}

/// SHA-1（FIPS 180-4）。只为上面的 HMAC / PBKDF2 和定值向量用。
pub fn sha1(data: &[u8]) -> [u8; 20] {
    use sha1::Digest;
    let mut h = sha1::Sha1::new();
    h.update(data);
    let out = h.finalize();
    let mut buf = [0u8; 20];
    buf.copy_from_slice(&out);
    buf
}

/// SHA-256（`sha256(host_key)` 那一步用；`sha2` 已经在依赖里）。
fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&out);
    buf
}

/// AES-128-CBC + PKCS#7 填充解密（`cbc` crate 的 `Decryptor` + `Pkcs7`）。
///
/// **填充校验是严格的**（`Pkcs7::unpad` 会逐字节比）：坏填充 = 坏行（调用方算 `failed`），
/// 和 JCE 的 `AES/CBC/PKCS5Padding` 一样。
pub fn aes_128_cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit};
    use cbc::cipher::block_padding::Pkcs7;
    if data.is_empty() || data.len() % 16 != 0 {
        return Err("密文长度不是 16 的倍数".into());
    }
    let mut buf = data.to_vec();
    cbc::Decryptor::<aes::Aes128>::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map(|plain| plain.to_vec())
        .map_err(|_| "填充不对".to_string())
}

/// AES-128-CBC + PKCS#7 填充加密（测试造夹具用：把明文做成 Chrome 那种 `v10` 密文）。
pub fn aes_128_cbc_encrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit};
    use cbc::cipher::block_padding::Pkcs7;
    let mut buf = vec![0u8; data.len() + 16];
    buf[..data.len()].copy_from_slice(data);
    let n = cbc::Encryptor::<aes::Aes128>::new(key.into(), iv.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
        .expect("缓冲区够大")
        .len();
    buf.truncate(n);
    buf
}

/// 造一条 Chrome 那种 `v10` / `v11` 密文（**测试 / 夹具用**）：`prefix + AES-CBC(sha256(host) + 明文)`。
///
/// `db_version >= 24` 时前面带 32 字节 `sha256(host_key)`（Chrome 在 Linux 上的写法）。
pub fn seal_value(prefix: &str, host_key: &str, db_version: i64, password: &str, plain: &str) -> Vec<u8> {
    let key = derive_key(password).expect("PBKDF2 不会失败");
    let mut body = Vec::new();
    if db_version >= 24 {
        body.extend_from_slice(&sha256(host_key.as_bytes()));
    }
    body.extend_from_slice(plain.as_bytes());
    let mut out = prefix.as_bytes().to_vec();
    out.extend_from_slice(&aes_128_cbc_encrypt(&key, &[b' '; 16], &body));
    out
}

/// 默认的 Chrome / Chromium profile 目录（Kotlin `ChromeCookieImporter.defaultProfile`）。
///
/// 参数是 `XDG_CONFIG_HOME`（可以给空）和 `$HOME`：
/// `$XDG_CONFIG_HOME/google-chrome/Default` 或 `$XDG_CONFIG_HOME/chromium/Default`，取第一个**有 `Cookies`
/// 文件**的；都没有就退回 `google-chrome/Default`（Kotlin 也是这样，后面打开时会报"没找到"）。
pub fn default_profile(xdg_config_home: Option<&str>, home: &str) -> std::path::PathBuf {
    let config = match xdg_config_home {
        Some(x) if !crate::hosted::is_kotlin_blank(x) => x.to_string(),
        _ => format!("{home}/.config"),
    };
    let candidates = ["google-chrome", "chromium"];
    for name in candidates {
        let p = std::path::Path::new(&config).join(name).join("Default");
        if p.join("Cookies").exists() {
            return p;
        }
    }
    std::path::Path::new(&config).join("google-chrome").join("Default")
}

/// **整条导入**：复制库到私有临时目录 → `rusqlite` 读 →（真有 `v11` 行才）
/// 跑 `kwallet-query` / `secret-tool` 取密码 → 解密 / 分组 → 删副本。宿主只剩"用户点了导入"。
/// 见 [`import`] 的模块头。
pub mod import;

pub use import::{
    default_password_commands, run_password_command, ChromeImportError, ChromeImporter, ChromeTable,
    PlatformImportOutcome, PlatformImportSpec, TempDir, PASSWORD_TIMEOUT, PASSWORD_V10,
};

