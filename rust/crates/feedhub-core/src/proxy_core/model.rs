//! 内置代理核心的**数据模型 + `proxy-core.json` 的读写**（`core/proxy/ProxyNode.kt` 的 Rust 版）。
//!
//! 这里是当前唯一"机密落在盘上"的地方：订阅地址、节点里的 uuid / 密码全在**一个权限 600 的 JSON**
//! 里。三条纪律（和 Kotlin 版一字不差）：
//!
//! 1. `to_string()` / `Debug` **不打印**出站内容——[`ProxyNode`] 手写了 `Debug`，uuid / 密码一律打码；
//! 2. 写盘是"[`write_private`]：临时文件 → 600 → 原子改名"，和 `core/local/LocalFiles.kt` 一样；
//! 3. 文件坏了 / 读不出来 → 当成空状态，**不让 App 起不来**（Kotlin 版也是 `runCatching` 兜住）。
//!
//! ## 格式逐字节不变
//!
//! `proxy-core.json` 是 `kotlinx.serialization` 的 `Json { prettyPrint = true; ignoreUnknownKeys = true;
//! encodeDefaults = true }` 写出来的：4 空格缩进、`": "`、字段顺序**按声明的顺序**、
//! `encodeDefaults = true` 所以 `lastLatencyMs = null` 这种也会写成 `"lastLatencyMs": null`。
//!
//! 所以这一层**不用 `serde_json` 的 `Map`**（它是 `BTreeMap`，会按键名排序），而是用
//! [`json_text::V`] 的保序写法，字段顺序在 [`ProxyCoreState::to_json`] 里照 Kotlin 的声明顺序逐个写出来。
//! 读的时候两边都宽容：不认识的键丢掉、缺的用默认值（`ignoreUnknownKeys` + 默认值）。
//!
//! `outbound` 是**任意** JSON 对象（订阅里给什么就是什么），顺序也得保住——那也是 Kotlin 的
//! `JsonObject`（`LinkedHashMap`）。所以 [`ProxyNode::outbound`] 存的是保序的 [`json_text::V::Obj`]。

use crate::hosted::is_kotlin_blank;
use crate::proxy_core::json_text::{self, V};

/// sing-box 的 `log.level`（`CORE_LOG_LEVELS`）。默认 `warn`，进阶设置页里可调。
pub const CORE_LOG_LEVELS: [&str; 7] = ["trace", "debug", "info", "warn", "error", "fatal", "panic"];

/// 延迟测试地址的默认值（用户可改）。
pub const DEFAULT_TEST_URL: &str = "https://www.gstatic.com/generate_204";

/// 一个节点都没有时给用户看的原因（起核心之前拒绝启动用）。
pub const NO_NODES_REASON: &str = "没有可用节点（先去「进阶设置 → 订阅 / 导入节点」加一个）";

/// 取订阅默认的 UA：机场一般按 UA 决定返回什么格式。
pub const DEFAULT_SUBSCRIPTION_UA: &str = "clash.meta";

/// 自动更新间隔可选值（小时，0 = 关）。
pub const AUTO_UPDATE_HOURS: [i32; 4] = [0, 6, 12, 24];

/// 取订阅走哪条路（默认直连）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionRoute {
    Direct,
    Core,
    System,
}

impl SubscriptionRoute {
    pub const ALL: [SubscriptionRoute; 3] = [
        SubscriptionRoute::Direct,
        SubscriptionRoute::Core,
        SubscriptionRoute::System,
    ];

    /// 存进 JSON 的枚举名（kotlinx 默认写枚举的 `name`）。
    pub fn name(self) -> &'static str {
        match self {
            SubscriptionRoute::Direct => "DIRECT",
            SubscriptionRoute::Core => "CORE",
            SubscriptionRoute::System => "SYSTEM",
        }
    }

    /// 从 JSON 的枚举名读回来。认不出来时给默认值 `DIRECT`（`coerceInputValues` 的口径）。
    pub fn parse(text: &str) -> SubscriptionRoute {
        match text {
            "CORE" => SubscriptionRoute::Core,
            "SYSTEM" => SubscriptionRoute::System,
            _ => SubscriptionRoute::Direct,
        }
    }
}

/// 一个节点。[`outbound`] 是 sing-box 的出站对象（不含 `tag`，tag 由我们按 [`id`](Self::id) 填）。
///
/// **机密**：`outbound` 里有 uuid / password。[`ProxyNode::debug`] 打码，别直接 `{:?}`。
#[derive(Clone, PartialEq)]
pub struct ProxyNode {
    /// 稳定 id：按 `outbound` 的内容算哈希，同一份链接重复导入得到同一个 id（不重复添加）。
    pub id: String,
    pub name: String,
    /// sing-box 出站类型，例如 `vless` / `shadowsocks`。
    pub kind: String,
    pub server: String,
    pub port: i32,
    /// **机密**（含 uuid / password）。保序。
    pub outbound: V,
    /// 来自哪个订阅（`None` = 手动导入）。
    pub subscription_id: Option<String>,
    /// 上次测速的延迟（毫秒，`None` = 没测过 / 测不通）。持久化，重开 App 还看得见。
    pub last_latency_ms: Option<i32>,
    pub last_latency_at: Option<i64>,
}

impl std::fmt::Debug for ProxyNode {
    /// **不打印出站内容**（里面是 uuid / 密码 / 订阅给的所有东西）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyNode")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("server", &self.server)
            .field("port", &self.port)
            .field("outbound", &"<机密>")
            .field("subscription_id", &self.subscription_id)
            .field("last_latency_ms", &self.last_latency_ms)
            .field("last_latency_at", &self.last_latency_at)
            .finish()
    }
}

impl ProxyNode {
    /// 界面上的一行：`vless · hk1.example.com:443`。
    pub fn label(&self) -> String {
        format!("{} · {}:{}", self.kind, self.server, self.port)
    }

    fn to_json(&self) -> V {
        V::Obj(vec![
            ("id".into(), V::Str(self.id.clone())),
            ("name".into(), V::Str(self.name.clone())),
            ("kind".into(), V::Str(self.kind.clone())),
            ("server".into(), V::Str(self.server.clone())),
            ("port".into(), V::Int(self.port as i64)),
            ("outbound".into(), self.outbound.clone()),
            (
                "subscriptionId".into(),
                opt_str(self.subscription_id.clone()),
            ),
            ("lastLatencyMs".into(), opt_int(self.last_latency_ms)),
            ("lastLatencyAt".into(), opt_long(self.last_latency_at)),
        ])
    }

    fn from_json(v: &V) -> Option<ProxyNode> {
        let obj = crate::proxy_core::subscription::as_obj(v)?;
        let get = |k: &str| obj.iter().find(|(name, _)| name == k).map(|(_, v)| v);
        // `subscriptionId` / `lastLatencyMs` / `lastLatencyAt` 有默认值，缺了就是 None；
        // 其余五个没默认值，缺了这条就废（Kotlin 那边是 `MissingFieldException` 整份文件退成空状态，
        // 这里更宽容：**只丢这一条**，别的节点照读——反正读不出来的东西也用不了）
        Some(ProxyNode {
            id: get("id")?.as_str()?.to_string(),
            name: get("name")?.as_str()?.to_string(),
            kind: get("kind")?.as_str()?.to_string(),
            server: get("server")?.as_str()?.to_string(),
            port: get("port")?.as_i64()? as i32,
            outbound: get("outbound")?.clone(),
            subscription_id: get("subscriptionId").and_then(|v| v.as_str()).map(String::from),
            last_latency_ms: get("lastLatencyMs")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            last_latency_at: get("lastLatencyAt").and_then(|v| v.as_i64()),
        })
    }
}

/// 一次导入里被跳过的条目：只说第几条、为什么，**不回显原文**（原文里可能有密码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    pub index: i32,
    pub reason: String,
}

/// 导入结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub added: i32,
    pub updated: i32,
    pub skipped: Vec<SkippedEntry>,
}

impl ImportReport {
    /// 界面上那一句：`新增 2 条，更新 1 条，跳过 1 条`。
    pub fn summary(&self) -> String {
        let mut out = format!("新增 {} 条", self.added);
        if self.updated > 0 {
            out.push_str(&format!("，更新 {} 条", self.updated));
        }
        if !self.skipped.is_empty() {
            out.push_str(&format!("，跳过 {} 条", self.skipped.len()));
        }
        out
    }
}

/// 订阅的流量 / 到期信息（HTTP 响应头 `subscription-userinfo`，机场一般会给）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubscriptionUserInfo {
    pub upload: Option<i64>,
    pub download: Option<i64>,
    pub total: Option<i64>,
    /// Unix 秒。
    pub expire: Option<i64>,
}

impl SubscriptionUserInfo {
    /// 已用量（上传 + 下载；两个都没有就是 `None`）。
    pub fn used(&self) -> Option<i64> {
        if self.upload.is_none() && self.download.is_none() {
            None
        } else {
            Some(self.upload.unwrap_or(0) + self.download.unwrap_or(0))
        }
    }

    fn to_json(&self) -> V {
        V::Obj(vec![
            ("upload".into(), opt_long(self.upload)),
            ("download".into(), opt_long(self.download)),
            ("total".into(), opt_long(self.total)),
            ("expire".into(), opt_long(self.expire)),
        ])
    }

    fn from_json(v: &V) -> Option<SubscriptionUserInfo> {
        let obj = crate::proxy_core::subscription::as_obj(v)?;
        let get = |k: &str| obj.iter().find(|(name, _)| name == k).map(|(_, v)| v);
        Some(SubscriptionUserInfo {
            upload: get("upload").and_then(|v| v.as_i64()),
            download: get("download").and_then(|v| v.as_i64()),
            total: get("total").and_then(|v| v.as_i64()),
            expire: get("expire").and_then(|v| v.as_i64()),
        })
    }
}

/// 一个订阅。**[`url`](Self::url) 是机密**：界面上默认打码，
#[derive(Clone, PartialEq)]
pub struct Subscription {
    pub id: String,
    pub name: String,
    pub url: String,
    pub added_at: i64,
    pub last_update_at: Option<i64>,
    /// 上次更新失败的原因（成功时为 `None`）。不含机密。
    pub last_error: Option<String>,
    pub node_count: i32,
    pub user_info: Option<SubscriptionUserInfo>,
    /// 这条订阅自己的自动更新间隔（小时，0 = 关）。**每条一份**（和 SFA 的 profile 一致）。
    pub auto_update_hours: i32,
}

impl std::fmt::Debug for Subscription {
    /// **不打印 url**（订阅地址就是凭据）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("url", &"<机密>")
            .field("added_at", &self.added_at)
            .field("last_update_at", &self.last_update_at)
            .field("last_error", &self.last_error)
            .field("node_count", &self.node_count)
            .field("user_info", &self.user_info)
            .field("auto_update_hours", &self.auto_update_hours)
            .finish()
    }
}

impl Subscription {
    fn to_json(&self) -> V {
        V::Obj(vec![
            ("id".into(), V::Str(self.id.clone())),
            ("name".into(), V::Str(self.name.clone())),
            ("url".into(), V::Str(self.url.clone())),
            ("addedAt".into(), V::Int(self.added_at)),
            ("lastUpdateAt".into(), opt_long(self.last_update_at)),
            ("lastError".into(), opt_str(self.last_error.clone())),
            ("nodeCount".into(), V::Int(self.node_count as i64)),
            (
                "userInfo".into(),
                self.user_info.as_ref().map(|u| u.to_json()).unwrap_or(V::Null),
            ),
            (
                "autoUpdateHours".into(),
                V::Int(self.auto_update_hours as i64),
            ),
        ])
    }

    fn from_json(v: &V) -> Option<Subscription> {
        let obj = crate::proxy_core::subscription::as_obj(v)?;
        let get = |k: &str| obj.iter().find(|(name, _)| name == k).map(|(_, v)| v);
        Some(Subscription {
            id: get("id")?.as_str()?.to_string(),
            name: get("name")?.as_str()?.to_string(),
            url: get("url")?.as_str()?.to_string(),
            added_at: get("addedAt")?.as_i64()?,
            last_update_at: get("lastUpdateAt").and_then(|v| v.as_i64()),
            last_error: get("lastError").and_then(|v| v.as_str()).map(String::from),
            node_count: get("nodeCount").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            user_info: get("userInfo").and_then(SubscriptionUserInfo::from_json),
            auto_update_hours: get("autoUpdateHours").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        })
    }
}

/// 内置代理核心的全部状态。**整个是机密**（订阅地址 + 节点密码 / uuid）。
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyCoreState {
    pub subscriptions: Vec<Subscription>,
    pub nodes: Vec<ProxyNode>,
    /// 选中的节点 id；`None` = 自动（延迟最低，用 sing-box 的 `urltest` 出站）。
    pub selected_node_id: Option<String>,
    pub test_url: String,
    pub log_level: String,
    pub subscription_route: SubscriptionRoute,
    pub subscription_ua: String,
}

impl Default for ProxyCoreState {
    fn default() -> Self {
        ProxyCoreState {
            subscriptions: Vec::new(),
            nodes: Vec::new(),
            selected_node_id: None,
            test_url: DEFAULT_TEST_URL.to_string(),
            log_level: "warn".to_string(),
            subscription_route: SubscriptionRoute::Direct,
            subscription_ua: DEFAULT_SUBSCRIPTION_UA.to_string(),
        }
    }
}

impl ProxyCoreState {
    pub fn nodes_of(&self, subscription_id: &str) -> Vec<&ProxyNode> {
        self.nodes
            .iter()
            .filter(|n| n.subscription_id.as_deref() == Some(subscription_id))
            .collect()
    }

    /// 选中的节点（没选中或者节点已经不在了 → `None`）。
    pub fn selected_node(&self) -> Option<&ProxyNode> {
        let id = self.selected_node_id.as_deref()?;
        self.nodes.iter().find(|n| n.id == id)
    }

    /// 写成 `proxy-core.json` 的文本（**逐字节和 Kotlin 一样**，见模块说明）。
    pub fn to_json_text(&self) -> String {
        let value = V::Obj(vec![
            (
                "subscriptions".into(),
                V::Arr(self.subscriptions.iter().map(|s| s.to_json()).collect()),
            ),
            (
                "nodes".into(),
                V::Arr(self.nodes.iter().map(|n| n.to_json()).collect()),
            ),
            ("selectedNodeId".into(), opt_str(self.selected_node_id.clone())),
            ("testUrl".into(), V::Str(self.test_url.clone())),
            ("logLevel".into(), V::Str(self.log_level.clone())),
            (
                "subscriptionRoute".into(),
                V::Str(self.subscription_route.name().to_string()),
            ),
            ("subscriptionUa".into(), V::Str(self.subscription_ua.clone())),
        ]);
        json_text::write_pretty(&value)
    }

    /// 从 `proxy-core.json` 的文本读回来。**坏了就当空状态**（和 Kotlin 的 `runCatching` 一致）。
    ///
    /// 单个订阅 / 节点读不出来时**只丢那一条**（Kotlin 是整份退成空状态；这里更宽容——反正那一条也用不了，
    /// 别的东西没理由是坏的）。
    pub fn from_json_text(text: &str) -> ProxyCoreState {
        match crate::xhs::jsonord::parse(text) {
            Ok(root) => Self::from_json(&root),
            Err(_) => ProxyCoreState::default(),
        }
    }

    fn from_json(root: &crate::xhs::jsonord::J) -> ProxyCoreState {
        use crate::xhs::jsonord::J;
        let J::Obj(fields) = root else {
            return ProxyCoreState::default();
        };
        let get = |k: &str| fields.iter().find(|(name, _)| name == k).map(|(_, v)| v);
        let default = ProxyCoreState::default();
        // 数组元素：对象认，不是对象的丢掉
        let arr_of = |key: &str| -> Vec<&J> {
            match get(key) {
                Some(J::Arr(items)) => items.iter().collect(),
                _ => Vec::new(),
            }
        };
        let to_v = |j: &J| -> V { v_from_j(j) };
        ProxyCoreState {
            subscriptions: arr_of("subscriptions")
                .into_iter()
                .filter_map(|j| Subscription::from_json(&to_v(j)))
                .collect(),
            nodes: arr_of("nodes")
                .into_iter()
                .filter_map(|j| ProxyNode::from_json(&to_v(j)))
                .collect(),
            selected_node_id: get("selectedNodeId")
                .and_then(|v| v.as_str())
                .map(String::from),
            test_url: get("testUrl")
                .and_then(|v| v.as_str())
                .unwrap_or(&default.test_url)
                .to_string(),
            log_level: get("logLevel")
                .and_then(|v| v.as_str())
                .unwrap_or(&default.log_level)
                .to_string(),
            subscription_route: get("subscriptionRoute")
                .and_then(|v| v.as_str())
                .map(SubscriptionRoute::parse)
                .unwrap_or(default.subscription_route),
            subscription_ua: get("subscriptionUa")
                .and_then(|v| v.as_str())
                .unwrap_or(&default.subscription_ua)
                .to_string(),
        }
    }
}

/// 把保序 JSON 的 [`crate::xhs::jsonord::J`] 转成 [`V`]（数字保留原始字面量，见 `json_text` 的说明）。
pub(crate) fn v_from_j(j: &crate::xhs::jsonord::J) -> V {
    use crate::xhs::jsonord::J;
    match j {
        J::Null => V::Null,
        J::Bool(b) => V::Bool(*b),
        J::Num(raw) => V::number(raw),
        J::Str(s) => V::Str(s.clone()),
        J::Arr(items) => V::Arr(items.iter().map(v_from_j).collect()),
        J::Obj(fields) => V::Obj(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), v_from_j(v)))
                .collect(),
        ),
    }
}

fn opt_str(v: Option<String>) -> V {
    v.map(V::Str).unwrap_or(V::Null)
}

fn opt_int(v: Option<i32>) -> V {
    v.map(|n| V::Int(n as i64)).unwrap_or(V::Null)
}

fn opt_long(v: Option<i64>) -> V {
    v.map(V::Int).unwrap_or(V::Null)
}

// ---------------------------------------------------------------------------
// 落盘：权限 600 + 原子改名
// ---------------------------------------------------------------------------

/// `core/local/LocalFiles.kt` 的 `writePrivate`：建目录 → 写 `*.tmp` → 权限 600 → 原子改名。
///
/// **不用 `std::fs::write` 直接写目标文件**：写到一半崩了会留下半份 JSON（读的时候虽然当空状态，
/// 但那等于用户白加了节点）。临时文件 + 改名是原子的。
///
/// 权限只在 unix 上设（Windows 上没有 POSIX 权限，`set_permissions` 会失败——吞掉，和 Kotlin 的
/// `runCatching` 一致）。
pub fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    std::fs::write(&tmp, text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 失败不算错（和 Kotlin 吞掉 UnsupportedOperationException / IOException 一样）
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// 读整个文件当 UTF-8 文本；文件不在 / 读不了就是 `None`。
pub fn read_text(path: &std::path::Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// 读一份 `proxy-core.json`。文件不在 / 坏了 → 空状态（**不让 App 起不来**）。
pub fn load_state(path: &std::path::Path) -> ProxyCoreState {
    read_text(path)
        .map(|text| ProxyCoreState::from_json_text(&text))
        .unwrap_or_default()
}

/// 写一份 `proxy-core.json`（权限 600）。返回的错误不含文件内容。
pub fn save_state(path: &std::path::Path, state: &ProxyCoreState) -> std::io::Result<()> {
    write_private(path, &state.to_json_text())
}

// ---------------------------------------------------------------------------
// 节点 id / 规范 JSON
// ---------------------------------------------------------------------------

/// 排序过的 JSON 文本：只用来算 [`ProxyNode::id`]，保证"同一份配置无论键的顺序怎么变都是同一个 id"。
///
/// **和 Kotlin 的 `canonicalJson` 逐字节一致**（它决定了 id，id 决定"更新订阅是更新还是重复添加"）：
/// 对象按键名排序（`sortedBy { it.key }` 是**字符串比较**，Rust 的 `str::cmp` 是**字节**比较——
/// 对 UTF-8 来说同一件事，因为 UTF-8 保持码点顺序）；数组保序；`JsonPrimitive.toString()` 是
/// **原始字面量**（数字 `1.50` 就是 `1.50`，不做归一化），这里的 [`V`] 也保留字面量。
///
/// **只算哈希，不打印**（内容里可能有密码）。
pub fn canonical_json(value: &V) -> String {
    let mut out = String::new();
    push_canonical(&mut out, value);
    out
}

fn push_canonical(out: &mut String, value: &V) {
    match value {
        V::Null => out.push_str("null"),
        V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        V::Int(n) => out.push_str(&n.to_string()),
        V::UInt(n) => out.push_str(&n.to_string()),
        V::Double(f) => out.push_str(&json_text::double_text(*f)),
        // 规范 JSON 用的是**原始字面量**（Kotlin 的 `JsonPrimitive.toString()` 就是它，不做归一化）
        V::Num(raw) => out.push_str(raw),
        V::Str(s) => json_text::V::write_string(out, s),
        V::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_canonical(out, item);
            }
            out.push(']');
        }
        V::Obj(fields) => {
            let mut sorted: Vec<&(String, V)> = fields.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            out.push('{');
            for (i, (key, item)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_text::V::write_string(out, key);
                out.push(':');
                push_canonical(out, item);
            }
            out.push('}');
        }
    }
}

/// SHA-256 的小写十六进制（节点 id、订阅 id 都用它）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 按出站内容算节点 id（前缀类型，方便肉眼分辨）。
pub fn node_id_of(kind: &str, outbound: &V) -> String {
    let hash = sha256_hex(canonical_json(outbound).as_bytes());
    format!("{kind}-{}", &hash[..12])
}

/// 去掉出站里的 `tag`（tag 由我们按节点 id 填，不能让订阅里的 tag 干扰）。
pub fn without_tag(outbound: &V) -> V {
    match outbound {
        V::Obj(fields) => V::Obj(
            fields
                .iter()
                .filter(|(k, _)| k != "tag")
                .cloned()
                .collect(),
        ),
        other => other.clone(),
    }
}

/// 订阅 id：`sub-` + URL 的 sha256 前 12 位。
pub fn subscription_id_of(url: &str) -> String {
    let hash = sha256_hex(url.as_bytes());
    format!("sub-{}", &hash[..12])
}

/// Kotlin 的 `String.isBlank()`（Kotlin 的 `Char.isWhitespace`，含 NBSP 类）。
pub fn is_blank(text: &str) -> bool {
    is_kotlin_blank(text)
}

