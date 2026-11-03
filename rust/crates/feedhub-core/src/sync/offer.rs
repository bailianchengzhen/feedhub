//! **登录同步二维码**（`core/sync/SyncOffer.kt` + `SyncCrypto` 里那几个协议常量）。
//!
//! 二维码里装的是：电脑的局域网地址、端口、一次性会话 id、256 位一次性密钥、协议版本。
//! **不含任何 Cookie**；二维码本身就是"两分钟内能用一次的入场券"。
//!
//! ```text
//! feedhub-sync:1?h=192.168.0.5&p=43127&s=<22 字符 base64url>&k=<43 字符 base64url>
//! ```
//! 字段都是必填、不认识的多余字段直接拒（协议宁严勿松）；host 只认 IPv4 字面量，
//! 免得二维码把客户端引去做 DNS 或连到 `localhost` 以外的东西。
//!
//! 二维码多一个方向字段；v1 的码新版本仍然照扫
//! （v1 没有方向，按"发出去"处理）。
//!
//! **收发不在这一层**：一次性小服务 / 取密文在 `sync/login.rs`（Rust `std::net`，局域网直连）。

use super::b64;
use std::net::Ipv4Addr;

/// 同步的方向（由**出码方**选）。
///
/// - [`Direction::Send`]：出码方把本机登录发出去，扫码方取走（v1 的现有流程）；
/// - [`Direction::Receive`]：出码方要收，扫码方把自己的密文 `POST` 上来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}

impl Direction {
    /// 二维码里的 `d=` 值（v2）。**故意用直白的单词**：用户扫到坏码时排障看得懂。
    pub fn token(self) -> &'static str {
        match self {
            Direction::Send => "send",
            Direction::Receive => "receive",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "send" => Some(Direction::Send),
            "receive" => Some(Direction::Receive),
            _ => None,
        }
    }
}

/// 同步协议的常量（`SyncProtocol`）。
pub mod protocol {
    use super::Direction;
    /// 二维码 / URL 的协议版本。
    /// 改协议就 +1（旧客户端会明着拒绝，不会猜着解）。
    pub const VERSION: i32 = 2;
    /// 老版本（没有方向字段）的协议版本。**v1 的码新版本仍然能扫**。
    pub const VERSION_V1: i32 = 1;
    /// 二维码内容的开头，用来和别的二维码区分。
    pub const QR_PREFIX: &str = "feedhub-sync";
    /// 服务端只认这一个路径前缀，后面接会话 id。
    pub const PATH_PREFIX: &str = "/v2/sync/";
    /// v1 的路径前缀（兼容老码）。
    pub const PATH_PREFIX_V1: &str = "/v1/sync/";
    /// 二维码有效期：两分钟。
    pub const TTL_MS: i64 = 120_000;
    /// 同步密钥字节数（AES-256）。
    pub const KEY_BYTES: usize = crate::sync::crypto::KEY_BYTES;
    /// 会话 id 字节数（128 位 → base64url 22 个字符）。
    pub const SESSION_BYTES: usize = 16;
    /// 响应体（密文）上限。
    pub const MAX_SEALED_BYTES: usize = 1_200_000;
    /// 明文上限。
    pub const MAX_PLAIN_BYTES: usize = 1_000_000;
    /// 一次同步最多带多少个 Cookie。
    pub const MAX_COOKIES: usize = 2_000;

    /// GCM 的附加认证数据：协议版本 + 会话 id（v1）。
    pub fn aad_v1(session_id: &str) -> Vec<u8> {
        format!("feedhub-sync|v{VERSION_V1}|{session_id}").into_bytes()
    }

    /// GCM 的附加认证数据：协议版本 + 会话 id + 方向（v2）。
    ///
    /// 绑方向是为了让"拿一份发出去的密文冒充推上来的包"这种方向翻转直接被 GCM 拒掉。
    pub fn aad(session_id: &str, direction: Direction) -> Vec<u8> {
        format!(
            "feedhub-sync|v{VERSION}|{session_id}|{}",
            direction.token()
        )
        .into_bytes()
    }
}

/// 二维码里的东西。`Debug` 里**不含密钥**（`SyncOffer.toString` 也只有地址和会话 id）。
#[derive(Clone, PartialEq, Eq)]
pub struct SyncOffer {
    pub host: String,
    pub port: u16,
    pub session_id: String,
    /// 一次性密钥（32 字节）。
    key: Vec<u8>,
    pub version: i32,
    /// 方向（v1 的码没有方向字段，解析出来固定是 [`Direction::Send`]）。
    pub direction: Direction,
}

impl std::fmt::Debug for SyncOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SyncOffer({}:{}, session={}, v={}, dir={:?})",
            self.host, self.port, self.session_id, self.version, self.direction
        )
    }
}

impl SyncOffer {
    /// 构造并校验，**口径同 Kotlin 的 `SyncOffer` 构造器**（`init` 里的四个 `require`）：
    /// 版本、IPv4 **字面量**、端口、密钥长度、会话 id 长度。
    ///
    /// **不含**"这个地址手机能不能连上"那一档（`0.0.0.0` / 回环 / 组播 / 链路本地）——那是
    /// [`parse`](Self::parse) 才做的：Kotlin 原实现里构造器只查正则，只有扫码解析才 `isUsableLanHost`。
    /// 测试里对 `127.0.0.1` 起服务就靠这条（回环地址在构造器上合法、在二维码上不合法）。
    pub fn new(
        host: &str,
        port: u16,
        session_id: &str,
        key: &[u8],
        version: i32,
        direction: Direction,
    ) -> Option<Self> {
        if version != protocol::VERSION && version != protocol::VERSION_V1 {
            return None;
        }
        // v1 没有方向字段：只在"发出去"这一种老流程里出现过。
        if version == protocol::VERSION_V1 && direction != Direction::Send {
            return None;
        }
        if !is_ipv4_literal(host) {
            return None;
        }
        if port == 0 {
            return None;
        }
        if key.len() != protocol::KEY_BYTES {
            return None;
        }
        if b64::decode(session_id).map(|b| b.len()) != Some(protocol::SESSION_BYTES) {
            return None;
        }
        Some(SyncOffer {
            host: host.to_string(),
            port,
            session_id: session_id.to_string(),
            key: key.to_vec(),
            version,
            direction,
        })
    }

    /// 一次性密钥的副本（调用方可以随便改）。
    pub fn key(&self) -> Vec<u8> {
        self.key.clone()
    }

    /// 二维码内容。
    ///
    /// v2 末尾多一个 `d=`（`send` / `receive`）；v1 还是原来的五个字段。
    pub fn content(&self) -> String {
        if self.version == protocol::VERSION_V1 {
            format!(
                "{}:{}?h={}&p={}&s={}&k={}",
                protocol::QR_PREFIX,
                self.version,
                self.host,
                self.port,
                self.session_id,
                b64::encode(&self.key)
            )
        } else {
            format!(
                "{}:{}?h={}&p={}&s={}&k={}&d={}",
                protocol::QR_PREFIX,
                self.version,
                self.host,
                self.port,
                self.session_id,
                b64::encode(&self.key),
                self.direction.token()
            )
        }
    }

    /// 服务端只认的那一个路径。
    pub fn path(&self) -> String {
        let prefix = if self.version == protocol::VERSION_V1 {
            protocol::PATH_PREFIX_V1
        } else {
            protocol::PATH_PREFIX
        };
        format!("{prefix}{}", self.session_id)
    }

    /// 手机端去取的地址（明文 HTTP 只传密文）。
    pub fn url(&self) -> String {
        format!("http://{}:{}{}", self.host, self.port, self.path())
    }

    /// GCM 的附加认证数据。
    pub fn aad(&self) -> Vec<u8> {
        if self.version == protocol::VERSION_V1 {
            protocol::aad_v1(&self.session_id)
        } else {
            protocol::aad(&self.session_id, self.direction)
        }
    }

    /// 换一个端口（绑定之后把 0 换成内核给的端口）。
    pub fn with_port(&self, port: u16) -> Option<Self> {
        Self::new(
            &self.host,
            port,
            &self.session_id,
            &self.key,
            self.version,
            self.direction,
        )
    }

    /// 这个字符串是不是我们这种二维码（别的二维码一律不理）。
    pub fn looks_like_sync(content: &str) -> bool {
        content
            .trim_start()
            .starts_with(&format!("{}:", protocol::QR_PREFIX))
    }

    /// 解析扫到 / 粘进来的二维码内容；**任何一处不对都返回 `None`**（不猜、不自动修正）。
    pub fn parse(content: &str) -> Option<SyncOffer> {
        let prefix = format!("{}:", protocol::QR_PREFIX);
        let text = super::text::trim(content);
        let rest = text.strip_prefix(&prefix)?;
        let q = rest.find('?')?;
        if q == 0 {
            return None;
        }
        let version: i32 = rest[..q].parse().ok()?;
        if version != protocol::VERSION && version != protocol::VERSION_V1 {
            return None;
        }
        let query = &rest[q + 1..];
        if super::text::is_blank(query) {
            return None;
        }
        // v1：键必须是 {h,p,s,k}、四个都要有；v2：多一个 d（五个都要有）。
        // 不认识的键 / 重复的键 / 缺键一律拒——不猜。
        let mut params: Vec<(String, String)> = Vec::new();
        for part in query.split('&') {
            let i = part.find('=')?;
            if i == 0 || i == part.len() - 1 {
                return None;
            }
            let key = &part[..i];
            let value = &part[i + 1..];
            let known = matches!(key, "h" | "p" | "s" | "k")
                || (version == protocol::VERSION && key == "d");
            if !known {
                return None;
            }
            if params.iter().any(|(k, _)| k == key) {
                return None;
            }
            params.push((key.to_string(), value.to_string()));
        }
        let expected = if version == protocol::VERSION_V1 { 4 } else { 5 };
        if params.len() != expected {
            return None;
        }
        let get = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
        let host = get("h")?;
        let port: u16 = get("p")?.parse().ok()?;
        let session_id = get("s")?;
        if session_id.chars().count() != 22 {
            return None;
        }
        if !is_usable_lan_host(host) {
            return None;
        }
        let key = b64::decode(get("k")?)?;
        if key.len() != protocol::KEY_BYTES {
            return None;
        }
        let direction = if version == protocol::VERSION_V1 {
            Direction::Send
        } else {
            Direction::parse(get("d")?)?
        };
        SyncOffer::new(host, port, session_id, &key, version, direction)
    }
}

/// 点分十进制 IPv4（每段 0–255，不带前导零以外的限制——和 Kotlin 的正则一致）。
///
/// Kotlin 的正则是 `^(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)$`：
/// 每段允许 `0`～`255`，**允许前导零**（`010` 是合法的 10），但 `256` / `1a` / 空段不行。
pub fn is_ipv4_literal(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|part| {
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        part.parse::<u32>().map(|n| n <= 255).unwrap_or(false)
    })
}

/// 二维码里的地址必须是一个"能连的局域网 IPv4"：不是 0.0.0.0 / 回环 / 组播 / 链路本地，
/// 而且**只认私网段**：10/8、172.16/12、192.168/16，外加 100.64/10（Tailscale 这类 CGNAT）。
/// 原来公网地址也算"可用"，陌生人给一个"收进来"方向的码就能让本机把登录推到公网上。
///
/// Kotlin 用 `InetAddress.getByName(host).isAnyLocalAddress || isLoopbackAddress || isMulticastAddress ||
/// isLinkLocalAddress` 判；这里按 IPv4 的数值范围手算（等价，而且不去碰 DNS）。
pub fn is_usable_lan_host(host: &str) -> bool {
    if !is_ipv4_literal(host) {
        return false;
    }
    let addr: Ipv4Addr = match host.parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let octets = addr.octets();
    // 0.0.0.0（any local）：JDK 的 isAnyLocalAddress 对 IPv4 就是 0.0.0.0
    if octets == [0, 0, 0, 0] {
        return false;
    }
    // 回环 127/8
    if octets[0] == 127 {
        return false;
    }
    // 组播 224/4
    if (224..=239).contains(&octets[0]) {
        return false;
    }
    // 链路本地 169.254/16
    if octets[0] == 169 && octets[1] == 254 {
        return false;
    }
    match octets {
        [10, ..] | [192, 168, ..] => true,
        [172, b, ..] => (16..=31).contains(&b),
        [100, b, ..] => (64..=127).contains(&b),
        _ => false,
    }
}

