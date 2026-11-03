//! **设备配对的两块纯逻辑**（`core/sync/SyncPairing.kt` 里不碰网络的半边）：
//! 配对二维码 [`PairingOffer`] + 配对报文的编解码 [`PairingCodec`]（含设备名清洗）。
//!
//! **收发不在这一层**：`PairingServer`（局域网一次性小服务）/ `PairingClient`（OkHttp POST）/ `PairingServers`
//! 留在 Kotlin——它们要 `ServerSocket` / `InetAddress` / `OkHttpClient`，和平台层"收发留宿主"一个道理。
//!
//! 二维码内容（`feedhub-pair:` 前缀，和登录同步的 `feedhub-sync:` 分开，扫错了不会串）：
//! ```text
//! feedhub-pair:1?h=192.168.0.5&p=43127&s=<22 字符 base64url>&k=<43 字符 base64url>
//! ```
//! **二维码里没有公钥、没有设备名、没有 Cookie**；只有"这个会话的一次性密钥"。
//! 交换完各自算一个 6 位短校验码（[`super::crypto::pairing_code`]），用户在两台设备上目测一致后点「确认」。

use super::b64;
use super::crypto;
use super::json::{self, ObjectWriter, Value};
use super::text;
use std::net::Ipv4Addr;

/// 配对协议的常量（`PairProtocol`）。
pub mod protocol {
    /// 配对协议版本。
    pub const VERSION: i32 = 1;
    /// 配对二维码的开头（和登录同步的 `feedhub-sync` 区分开）。
    pub const QR_PREFIX: &str = "feedhub-pair";
    /// 服务端只认这一个路径前缀，后面接会话 id。
    pub const PATH_PREFIX: &str = "/v1/pair/";
    /// 配对二维码有效期：三分钟（比登录同步的两分钟宽裕一点，要等用户核对 6 位码）。
    pub const TTL_MS: i64 = 180_000;
    /// 一次性密钥字节数。
    pub const KEY_BYTES: usize = crate::sync::crypto::KEY_BYTES;
    /// 会话 id 字节数。
    pub const SESSION_BYTES: usize = 16;
    /// 报文里随机数的字节数。
    pub const NONCE_BYTES: usize = 16;
    /// 一条配对报文（明文 JSON）的上限。
    pub const MAX_MESSAGE_BYTES: usize = 1_024;
    /// 请求体（密文）上限。
    pub const MAX_BODY_BYTES: usize = 4_096;
    /// 设备名最长多少个字符。
    pub const NAME_MAX: usize = 40;

    /// 和登录同步同一套 AAD 形状，只换域名前缀。
    pub fn aad(session_id: &str) -> Vec<u8> {
        format!("feedhub-pair|v{VERSION}|{session_id}").into_bytes()
    }
}

/// 配对二维码：电脑（或另一台手机）的局域网地址 + 端口 + 一次性会话 id + 一次性密钥。
#[derive(Clone, PartialEq, Eq)]
pub struct PairingOffer {
    pub host: String,
    pub port: u16,
    pub session_id: String,
    key: Vec<u8>,
    pub version: i32,
}

impl std::fmt::Debug for PairingOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PairingOffer({}:{}, session={}, v={})",
            self.host, self.port, self.session_id, self.version
        )
    }
}

impl PairingOffer {
    /// 构造并校验，**口径同 Kotlin 的 `PairingOffer` 构造器**（`init` 里的四个 `require`）：
    /// 版本、IPv4 字面量、端口、密钥长度、会话 id 长度。**不含**"地址可不可连"那一档
    /// （那是 [`parse`](Self::parse) 的事，见 `SyncOffer::new` 的说明）。
    pub fn new(host: &str, port: u16, session_id: &str, key: &[u8], version: i32) -> Option<Self> {
        if version != protocol::VERSION {
            return None;
        }
        if !super::offer::is_ipv4_literal(host) {
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
        Some(PairingOffer {
            host: host.to_string(),
            port,
            session_id: session_id.to_string(),
            key: key.to_vec(),
            version,
        })
    }

    pub fn key(&self) -> Vec<u8> {
        self.key.clone()
    }

    pub fn content(&self) -> String {
        format!(
            "{}:{}?h={}&p={}&s={}&k={}",
            protocol::QR_PREFIX,
            self.version,
            self.host,
            self.port,
            self.session_id,
            b64::encode(&self.key)
        )
    }

    pub fn path(&self) -> String {
        format!("{}{}", protocol::PATH_PREFIX, self.session_id)
    }

    pub fn url(&self) -> String {
        format!("http://{}:{}{}", self.host, self.port, self.path())
    }

    pub fn aad(&self) -> Vec<u8> {
        protocol::aad(&self.session_id)
    }

    pub fn looks_like_pairing(content: &str) -> bool {
        content
            .trim_start()
            .starts_with(&format!("{}:", protocol::QR_PREFIX))
    }

    /// 严格解析；任何一处不对都返回 `None`。
    pub fn parse(content: &str) -> Option<PairingOffer> {
        let prefix = format!("{}:", protocol::QR_PREFIX);
        let text = text::trim(content);
        let rest = text.strip_prefix(&prefix)?;
        let q = rest.find('?')?;
        if q == 0 {
            return None;
        }
        let version: i32 = rest[..q].parse().ok()?;
        if version != protocol::VERSION {
            return None;
        }
        let query = &rest[q + 1..];
        if text::is_blank(query) {
            return None;
        }
        let mut params: Vec<(String, String)> = Vec::new();
        for part in query.split('&') {
            let i = part.find('=')?;
            if i == 0 || i == part.len() - 1 {
                return None;
            }
            let key = &part[..i];
            let value = &part[i + 1..];
            if !matches!(key, "h" | "p" | "s" | "k") {
                return None;
            }
            if params.iter().any(|(k, _)| k == key) {
                return None;
            }
            params.push((key.to_string(), value.to_string()));
        }
        if params.len() != 4 {
            return None;
        }
        let get = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
        let host = get("h")?;
        let port: u16 = get("p")?.parse().ok()?;
        let session_id = get("s")?;
        // `sessionId.length != 22` 是 Kotlin 的**字符数**（base64url 只有 ASCII，字节数也一样）
        if text::utf16_len(session_id) != 22 {
            return None;
        }
        if !super::offer::is_usable_lan_host(host) {
            return None;
        }
        if !super::offer::is_usable_lan_host(host) {
            return None;
        }
        let key = b64::decode(get("k")?)?;
        if key.len() != protocol::KEY_BYTES {
            return None;
        }
        PairingOffer::new(host, port, session_id, &key, version)
    }
}

/// 配对时交换的那一份信息：设备 id + 设备名 + 公钥（+ 请求随机数，响应里原样带回）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingMessage {
    pub v: i32,
    pub device: String,
    pub name: String,
    pub public_key: String,
    pub nonce: String,
}

/// **配对报文的编解码**（`PairingCodec`）。
///
/// 和快照编解码不同，这边用的是 `Json { encodeDefaults = true; ignoreUnknownKeys = **false** }`：
/// 字段**全部写出来**（含 `v`），多出来一个不认识的字段就**整条拒**（配对报文是我们自己的两端发的，
/// 宁可明着拒也不要猜）。字段顺序 = 声明顺序：`v, device, name, publicKey, nonce`。
pub mod codec {
    use super::*;

    /// 编码（`PairingCodec.encode`）。
    pub fn encode(message: &PairingMessage) -> Vec<u8> {
        let mut w = ObjectWriter::new();
        w.field_i64("v", message.v as i64);
        w.field_str("device", &message.device);
        w.field_str("name", &message.name);
        w.field_str("publicKey", &message.public_key);
        w.field_str("nonce", &message.nonce);
        w.finish().into_bytes()
    }

    /// 解码（`PairingCodec.decode`）：解不开 / 字段不对 / 版本不对 / 公钥不合法 → `None`。
    ///
    /// **注意**：`ignoreUnknownKeys = false` —— 多一个字段就整条拒。
    pub fn decode(bytes: &[u8]) -> Option<PairingMessage> {
        if bytes.is_empty() || bytes.len() > protocol::MAX_MESSAGE_BYTES {
            return None;
        }
        let text = std::str::from_utf8(bytes).ok()?;
        let root = json::parse(text).ok()?;
        let Value::Object(fields) = &root else {
            return None;
        };
        // 只允许这五个键（`ignoreUnknownKeys = false`）
        const ALLOWED: [&str; 5] = ["v", "device", "name", "publicKey", "nonce"];
        if fields.iter().any(|(k, _)| !ALLOWED.contains(&k.as_str())) {
            return None;
        }
        // 五个键都要在（kotlinx 对没有默认值的字段缺了就是 "Field 'x' is required"）
        if fields.len() != ALLOWED.len() {
            return None;
        }
        let v = root.i64_field("v")?;
        let device = root.str_field("device")?;
        let name = root.str_field("name")?;
        let public_key = root.str_field("publicKey")?;
        let nonce = root.str_field("nonce")?;
        let message = PairingMessage {
            v: v as i32,
            device: device.to_string(),
            name: name.to_string(),
            public_key: public_key.to_string(),
            nonce: nonce.to_string(),
        };
        if message.v != protocol::VERSION {
            return None;
        }
        if b64::decode(&message.device).map(|b| b.len()) != Some(8) {
            return None;
        }
        if text::is_blank(&message.name) || text::utf16_len(&message.name) > protocol::NAME_MAX {
            return None;
        }
        if message.name.chars().any(text::is_iso_control) {
            return None;
        }
        let pub_key = b64::decode(&message.public_key)?;
        if pub_key.len() != crypto::X25519_BYTES {
            return None;
        }
        if pub_key.iter().all(|b| *b == 0) {
            return None;
        }
        if b64::decode(&message.nonce).map(|b| b.len()) != Some(protocol::NONCE_BYTES) {
            return None;
        }
        Some(message)
    }

    /// 本机的一条报文（新随机数由调用方给：`nonce` 是 16 字节）。
    pub fn own(device_id: &str, name: &str, public_key: &[u8], nonce: &[u8]) -> PairingMessage {
        PairingMessage {
            v: protocol::VERSION,
            device: device_id.to_string(),
            name: clean_name(name),
            public_key: b64::encode(public_key),
            nonce: b64::encode(nonce),
        }
    }

    /// 界面 / 库里显示的设备名（`PairingCodec.cleanName`）：
    /// 去掉控制字符 → 把连续的 Java `\s`（`[ \t\n\x0B\f\r]`）压成一个空格 → `trim()` →
    /// 空白就叫"未命名设备" → `take(40)`（**按 UTF-16 码元**）。
    pub fn clean_name(name: &str) -> String {
        // 控制字符换成空格
        let mut no_controls = String::with_capacity(name.len());
        for c in name.chars() {
            no_controls.push(if text::is_iso_control(c) { ' ' } else { c });
        }
        let collapsed = text::collapse_java_ws(&no_controls);
        let trimmed = text::trim(&collapsed);
        let source = if text::is_blank(trimmed) { "未命名设备" } else { trimmed };
        text::take_utf16(source, protocol::NAME_MAX)
    }
}

/// 配对成功后的对端（还没落库；用户点「确认」才写 `sync_peer`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingPeer {
    pub device: String,
    pub name: String,
    pub public_key: Vec<u8>,
}

/// 配对结果：对端 + 两边应该一致的 6 位校验码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingResult {
    pub peer: PairingPeer,
    pub code: String,
}

/// 两台设备的配对结果（`PairingServer.code` / `PairingClient.exchange` 的最后一步）：
/// 本机报文 + 对端报文 → 对端 + 6 位码。
///
/// 返回 `None`：设备 id 撞上（不能和自己配对）、两边公钥长度不对（Kotlin 抛 `IllegalArgumentException`）。
pub fn pair_result(own: &PairingMessage, peer: &PairingMessage) -> Option<PairingResult> {
    if own.device == peer.device {
        return None;
    }
    let own_key = b64::decode(&own.public_key)?;
    let peer_key = b64::decode(&peer.public_key)?;
    let code = crypto::pairing_code(&own.device, &own_key, &peer.device, &peer_key)?;
    Some(PairingResult {
        peer: PairingPeer {
            device: peer.device.clone(),
            name: codec::clean_name(&peer.name),
            public_key: peer_key,
        },
        code,
    })
}

/// `PairingOffer::create` 的确定性版本（会话 id / 密钥都由调用方给，生产是宿主的 `SecureRandom`）。
pub fn pairing_offer(host: &str, port: u16, session_id: &str, key: &[u8]) -> Option<PairingOffer> {
    PairingOffer::new(host, port, session_id, key, protocol::VERSION)
}

/// 只给测试 / 排障用：把 IPv4 字面量解析成 4 个字节（`LanAddresses` 那边不用它）。
#[allow(dead_code)]
pub fn ipv4_octets(host: &str) -> Option<[u8; 4]> {
    if !super::offer::is_ipv4_literal(host) {
        return None;
    }
    host.parse::<Ipv4Addr>().ok().map(|a| a.octets())
}

