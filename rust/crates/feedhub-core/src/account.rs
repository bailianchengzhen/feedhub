//! **账号层**：扫码登录 / "我是谁" / 关注 的共用模型。
//!
//! 五家国内平台（B站 / 知乎 / 抖音 / 小黑盒 / 贴吧）的这三种能力形态差别很大，但**出去的形状一样**：
//!
//! - [`QrLogin`]：前端拿到二维码内容（画成二维码）和轮询用的 token；
//! - [`QrState`]：一次轮询的结果（等着 / 扫了等确认 / 成功 / 过期 / 失败）；
//! - [`FollowPage`]：关注列表的一页（只有 B站 / 贴吧有）。
//!
//! 和 `detail.rs` 的分工：`FollowTarget` / `FollowKind` 已经在 `detail.rs`（详情页的"关注"按钮要用），
//! 这里只补"列表 + 扫码"这两样，**不另造一份**。
//!
//! 三条写进类型里的规矩：
//! 1. **成功之前不动 `ctx.cookies`**：临时罐在宿主（[`crate::host::QrHost`]），落地只在 [`QrState::Success`] 那一步；
//! 2. **`Failed` 带一句人话**（前端直接显示；不许把 Cookie / token / 临时凭据放进去）；
//! 3. **二维码内容 / 轮询间隔 / 超时照 Kotlin**：`QrLogin` 的字段就是 Kotlin `QrLogin` 的字段，一个不多一个不少。

use crate::detail::{FollowTarget, FollowKind};
pub use crate::model::{PlatformId, UserRef};

/// 一次扫码登录的二维码（Kotlin `core/QrLogin.kt` 的 `QrLogin`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrLogin {
    pub platform: PlatformId,
    /// 二维码里编码的内容（通常是一个 URL），前端自己画成二维码。
    pub content: String,
    /// 平台用来查询状态的 key（B站 qrcode_key、知乎 token、百度 sign / channel_id……）。
    pub token: String,
    /// 平台另外需要记住的东西（贴吧的 `gid`），调用方原样传回。
    pub extra: Vec<(String, String)>,
    /// 什么时候过期（Unix 毫秒），不知道就 `None`。
    pub expires_at_ms: Option<i64>,
    /// 建议的轮询间隔（毫秒）。
    pub poll_interval_ms: i64,
    /// 给用户的提示，例如"用哔哩哔哩 App 扫码"。
    pub hint: String,
}

impl QrLogin {
    /// 从 [`extra`] 里取一个键（贴吧的 `gid`）。
    pub fn extra(&self, key: &str) -> Option<&str> {
        self.extra
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// 这个二维码是不是已经超时（[`QrState::Waiting`] 里贴吧自己判过期用）。
    pub fn expired_at(&self, now_ms: i64, grace_ms: i64) -> bool {
        matches!(self.expires_at_ms, Some(at) if now_ms > at.saturating_add(grace_ms))
    }
}

/// 一次轮询的结果（Kotlin `QrLoginState`）。
#[derive(Debug, Clone, PartialEq)]
pub enum QrState {
    /// 还没扫。
    Waiting,
    /// 扫了，等手机上确认。
    Scanned,
    /// 登录成功，Cookie 已经写进 `ctx.cookies`；`user` 是新登录的账号（验证拿不到身份时是 `None`）。
    Success(Option<UserRef>),
    /// 二维码过期或被取消，要重新开始。
    Expired,
    /// 失败，带一句给用户看的话（**不含凭据**）。
    Failed(String),
}

/// 一页关注对象（Kotlin `Page<FollowTarget>`）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FollowPage {
    pub items: Vec<FollowTarget>,
    pub next: Option<String>,
}

// ===========================================================================
// 临时 Cookie 罐的记账（五家都是"token → 宿主句柄"，LRU 8，和 Kotlin `QrSessions` 一样）
// ===========================================================================

use crate::host::QrHost;
use std::sync::Mutex;

/// 临时罐在宿主里的句柄（宿主内部编号）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrSession(pub String);

/// `token → 临时罐句柄`，语义照 Kotlin `QrSessions`：最多 8 个（访问序 LRU）、重复登记覆盖、
/// 取不到时给一个**新的空罐**（进程重启 / 换了 Source 实例也能继续轮询，只是没有生成时的 Cookie）。
pub struct QrSessions {
    host: std::sync::Arc<dyn QrHost>,
    entries: Mutex<Vec<(String, String)>>,
    max: usize,
}

impl QrSessions {
    pub fn new(host: std::sync::Arc<dyn QrHost>) -> Self {
        Self {
            host,
            entries: Mutex::new(Vec::new()),
            max: 8,
        }
    }

    pub fn host(&self) -> &std::sync::Arc<dyn QrHost> {
        &self.host
    }

    /// 登记 `token` 的临时罐（生成二维码那一步开罐、拿到 token 后调这个）。
    pub fn begin(&self, token: &str, session: &QrSession) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = entries.iter().position(|(t, _)| t == token) {
            entries.remove(index);
        }
        entries.push((token.to_string(), session.0.clone()));
        while entries.len() > self.max {
            let (_, evicted) = entries.remove(0);
            // Kotlin 的 `removeEldestEntry` 只是丢掉 Map 里那一项，罐子交给 GC；
            // 这里显式 `end` 一次，宿主那份内存早点释放（行为等价：之后用同一个 token 会拿到新的空罐）。
            self.host.end(&evicted);
        }
    }

    /// 取 `token` 的临时罐；没有就**开一个新的空罐**并登记（Kotlin 的 `getOrPut(MemoryCookieJar())`）。
    pub fn jar(&self, token: &str) -> QrSession {
        {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(index) = entries.iter().position(|(t, _)| t == token) {
                let entry = entries.remove(index);
                entries.push(entry.clone());
                return QrSession(entry.1);
            }
        }
        let session = QrSession(self.host.begin());
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.push((token.to_string(), session.0.clone()));
        while entries.len() > self.max {
            let (_, evicted) = entries.remove(0);
            self.host.end(&evicted);
        }
        session
    }

    /// 终态（成功 / 过期 / 失败）后丢掉。
    pub fn end(&self, token: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = entries.iter().position(|(t, _)| t == token) {
            let (_, session) = entries.remove(index);
            self.host.end(&session);
        }
    }
}

/// 关注对象的构造小工具：五家里只有两家有，字段一样，省得每家抄一遍 `FollowTarget { .. }`。
pub fn follow_target(
    platform: &str,
    kind: FollowKind,
    id: impl Into<String>,
    name: impl Into<String>,
    avatar: Option<String>,
    description: Option<String>,
    url: Option<String>,
) -> FollowTarget {
    FollowTarget {
        platform: platform.to_string(),
        kind,
        id: id.into(),
        name: name.into(),
        avatar,
        description,
        url,
    }
}

