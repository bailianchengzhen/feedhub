//! 「我的 → 账号」的状态判定 + 本地缓存（`app/.../mine/AccountsState.kt` 的判定部分 + `AccountsCache.kt`）。
//!
//! 而且这样凭空加请求有点捞"。
//! 所以进页面**一个请求都不发**，先摆上次存下来的；只有这几种时候才真的问平台：
//! - 登录成功（扫码 / 网页登录 / 设备码）、退出登录、从浏览器导入、从电脑同步：**只问那一个平台**；
//! - 用户点右上角的刷新：问全部；
//! - 本地一条都没存的平台（第一次用、新装的插件）：问一次。
//!
//! 这一层搬过来的是**"该不该问、问哪个、存下来的怎么读怎么写"**：
//! - [`AccountStore`]：`app.accounts` 这一项的**编解码**（格式一个字节都不能变，见 [`AccountStore::encode`]）；
//! - [`AccountEngine`]：状态表 + 上面那几种"什么时候问"的决策（宿主拿到 [`Plan`] 去真的发 whoami）。
//!
//! **留宿主（Kotlin）的**：
//! - 真正的 `whoami()`（OkHttp，走平台对象）；
//! - Cookie 罐 / 系统 WebView 的 Cookie / 设备码 token 的清与写（`logout` 那三处）；
//! - 浏览器导入（`ChromeCookieImporter`）、同步包的应用（`SyncApplier`）；
//! - Compose 状态容器、协程。
//!
//! 缓存里**只存四样东西**：昵称、头像地址、平台内的用户 id、状态（已登录 / 未登录 / 是不是过期），
//! 绝不存 Cookie / token / 签名（`AccountsCacheTest` 的字段白名单守着）。

use std::collections::HashMap;

use crate::xhs::jsonord::{self, J};

/// 老平台在存储里的写法（Kotlin `Platform.toStored`：老的 11 个写 enum 名，新的写 id）。
pub const LEGACY_BY_KEY: &[(&str, &str)] = &[
    ("bili", "BILIBILI"),
    ("zhihu", "ZHIHU"),
    ("tieba", "TIEBA"),
    ("heybox", "HEYBOX"),
    ("coolapk", "COOLAPK"),
    ("xhs", "XHS"),
    ("douyin", "DOUYIN"),
    ("x", "X"),
    ("yt", "YOUTUBE"),
    ("reddit", "REDDIT"),
    ("tiktok", "TIKTOK"),
];

/// 存储里那一项的键（和 `app.settings` 同一层）。
pub const STORE_KEY: &str = "app.accounts";

/// 存档格式版本（现在只会写 1）。
pub const SCHEMA: i32 = 1;

const IN: &str = "in";
const OUT: &str = "out";

/// 平台 id → 存储里的写法。
pub fn to_stored(platform: &str) -> &str {
    LEGACY_BY_KEY
        .iter()
        .find(|(key, _)| *key == platform)
        .map(|(_, name)| *name)
        .unwrap_or(platform)
}

/// 存储里的写法 → 平台 id（老的 enum 名换回来，别的原样当 id）。
pub fn from_stored(text: &str) -> String {
    LEGACY_BY_KEY
        .iter()
        .find(|(_, name)| *name == text)
        .map(|(key, _)| (*key).to_string())
        .unwrap_or_else(|| text.to_string())
}

/// 平台内的用户（`UserRef`）。**这里只有平台 / 昵称 / 头像 / id / 主页**，没有凭据。
///
/// `platform` 必须写：Kotlin 的 `UserRef` 是 `@Serializable`，它的第一个字段就是 `platform`
/// （写进 `app.accounts` 的 JSON 里也在），少写它老版本就读不回来。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserRef {
    pub platform: String,
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub url: Option<String>,
}

/// 一个平台账号现在的状态（`AccountStatus`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountStatus {
    /// 正在问平台（**不写库**）。
    Checking,
    LoggedIn(UserRef),
    /// `expired`：本机还留着登录用的 Cookie，但平台说没登录（过期了）；false = 压根没登录过。
    LoggedOut { expired: bool },
    /// 查的时候出错了（网络、风控、接口变了……），`message` 是一行原因（**不写库**）。
    Failed { message: String },
}

impl AccountStatus {
    /// 存不存：只有"已登录 / 未登录"两种写库（`Checking` / `Failed` 是一次查询的临时结果）。
    pub fn storable(&self) -> bool {
        matches!(self, AccountStatus::LoggedIn(_) | AccountStatus::LoggedOut { .. })
    }
}

/// 存档里一个平台的状态（`StoredAccount`）。**字段就是全部**：没有 Cookie、没有 token、没有签名参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAccount {
    pub state: String,
    pub user: Option<UserRef>,
    pub expired: bool,
    pub checked_at: i64,
}

/// 读出来的整份存档。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub schema: i32,
    /// 平台 id → 那一次查到的状态（**保持文件里的顺序**）。
    pub accounts: Vec<(String, StoredAccount)>,
}

impl Default for Stored {
    /// `schema` 的默认值是 [`SCHEMA`]（和 Kotlin 的 `val schema: Int = AccountsCache.SCHEMA` 一样）：
    /// 等于默认值时不写进 JSON（`encodeDefaults = false`），所以空存档写出来是 `{"accounts":{}}`。
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            accounts: Vec::new(),
        }
    }
}

/// `app.accounts` 的编解码（`AccountsCache`）。
///
/// 口径（`AccountsCacheTest` 守着"逐字节"）：
/// - 写：`{"schema":1,"accounts":{…}}`，键的顺序 = 插入顺序（Kotlin 的 `Map` 是 `LinkedHashMap`）；
///   `user` 为 null 时不写，`expired` / `checkedAt` **总是写**（Kotlin 的 `explicitNulls=false` +
///   `encodeDefaults` 默认 false → 默认值不写！见下面 `encode` 的注释）；
/// - 读：任何毛病（没有这一项、JSON 坏了、`state=in` 却没有 `user`）都当成"本地什么都没存"。
#[derive(Debug, Default, Clone)]
pub struct AccountStore;

impl AccountStore {
    pub fn new() -> Self {
        Self
    }

    /// 读一份存档；读不出来 = 空（不是异常）。
    pub fn load(&self, text: Option<&str>) -> Stored {
        let Some(text) = text else {
            return Stored::default();
        };
        let Ok(root) = jsonord::parse(text) else {
            return Stored::default();
        };
        let schema = root
            .get("schema")
            .and_then(|v| v.as_i64())
            .unwrap_or(SCHEMA as i64) as i32;
        let mut accounts = Vec::new();
        let Some(fields) = root.get("accounts").and_then(|v| v.as_obj()) else {
            return Stored {
                schema,
                accounts,
            };
        };
        for (stored_key, value) in fields {
            let Some(obj) = value.as_obj() else { continue };
            let state = obj
                .iter()
                .find(|(k, _)| k == "state")
                .and_then(|(_, v)| v.as_str())
                .unwrap_or("");
            let user = obj
                .iter()
                .find(|(k, _)| k == "user")
                .and_then(|(_, v)| read_user(v));
            let expired = obj
                .iter()
                .find(|(k, _)| k == "expired")
                .and_then(|(_, v)| v.as_bool())
                .unwrap_or(false);
            let checked_at = obj
                .iter()
                .find(|(k, _)| k == "checkedAt")
                .and_then(|(_, v)| v.as_i64())
                .unwrap_or(0);
            // `state=in` 但没有 user：这一条跳过（不抛异常）
            if state == IN && user.is_none() {
                continue;
            }
            accounts.push((
                from_stored(stored_key),
                StoredAccount {
                    state: state.to_string(),
                    user,
                    expired,
                    checked_at,
                },
            ));
        }
        Stored { schema, accounts }
    }

    /// 这份存档里"上次查到"的时间（毫秒）：所有 `checkedAt` 的最大值，`> 0` 才算；一条都没有 = `None`。
    pub fn checked_at(&self, stored: &Stored) -> Option<i64> {
        stored
            .accounts
            .iter()
            .map(|(_, entry)| entry.checked_at)
            .max()
            .filter(|t| *t > 0)
    }

    /// 读成"平台 id → 状态"（`load()` 的上层视图）。`in` 没 user 的已经在上一步滤掉了。
    pub fn statuses(&self, stored: &Stored) -> Vec<(String, AccountStatus)> {
        let mut out = Vec::with_capacity(stored.accounts.len());
        for (platform, entry) in &stored.accounts {
            match entry.state.as_str() {
                IN => {
                    if let Some(user) = &entry.user {
                        out.push((platform.clone(), AccountStatus::LoggedIn(user.clone())));
                    }
                }
                OUT => out.push((
                    platform.clone(),
                    AccountStatus::LoggedOut {
                        expired: entry.expired,
                    },
                )),
                _ => {}
            }
        }
        out
    }

    /// 存一个平台刚查到的状态（`Checking` / `Failed` 忽略）。
    ///
    /// `now_ms` 是宿主传进来的时钟（Kotlin 默认 `System.currentTimeMillis()`）。
    pub fn put(
        &self,
        stored: &mut Stored,
        platform: &str,
        status: &AccountStatus,
        now_ms: i64,
    ) {
        let entry = match status {
            AccountStatus::LoggedIn(user) => StoredAccount {
                state: IN.to_string(),
                user: Some(user.clone()),
                expired: false,
                checked_at: now_ms,
            },
            AccountStatus::LoggedOut { expired } => StoredAccount {
                state: OUT.to_string(),
                user: None,
                expired: *expired,
                checked_at: now_ms,
            },
            _ => return,
        };
        match stored.accounts.iter_mut().find(|(p, _)| p == platform) {
            Some((_, slot)) => *slot = entry,
            None => stored.accounts.push((platform.to_string(), entry)),
        }
    }

    /// 编码成存储里的那一段文本。
    ///
    /// **和 Kotlin 的 `Json` 逐字节对齐**（`FeedJson`：`explicitNulls = false`、`encodeDefaults` 默认
    /// `false`）：所以 `schema` 是默认值 1 时**不写**、`expired = false` 且 `checkedAt = 0` 时也不写；
    /// `user` 为 null 时不写；**null 字段和默认值字段都不写**，其余按声明顺序：`state` → `user` →
    /// `expired` → `checkedAt`。
    pub fn encode(&self, stored: &Stored) -> String {
        let mut out = String::from("{");
        // schema 也是字段、也有默认值 1：等于默认值就不写（和 Kotlin 的 encodeDefaults=false 一样）
        let mut first = true;
        if stored.schema != SCHEMA {
            out.push_str("\"schema\":");
            out.push_str(&stored.schema.to_string());
            first = false;
        }
        if !first {
            out.push(',');
        }
        out.push_str("\"accounts\":{");
        for (i, (platform, entry)) in stored.accounts.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            // `jsonord::quote` 自带一对引号（和 kotlinx.serialization 逐字节相同），别再补一层
            jsonord::quote(&mut out, &to_stored(platform));
            out.push_str(":{");
            out.push_str("\"state\":");
            push_str(&mut out, &entry.state);
            if let Some(user) = &entry.user {
                out.push_str(",\"user\":{");
                out.push_str("\"platform\":");
                // `UserRef.platform` 也是 `Platform`，Kotlin 的序列化器写**老的 enum 名**（`Platform.toStored`），
                // 和上面那一层的键同一个口径；内存里放的是 id，所以这里要换回去
                push_str(&mut out, to_stored(&user.platform));
                out.push_str(",\"id\":");
                push_str(&mut out, &user.id);
                out.push_str(",\"name\":");
                push_str(&mut out, &user.name);
                if let Some(avatar) = &user.avatar {
                    out.push_str(",\"avatar\":");
                    push_str(&mut out, avatar);
                }
                if let Some(url) = &user.url {
                    out.push_str(",\"url\":");
                    push_str(&mut out, url);
                }
                out.push('}');
            }
            if entry.expired {
                out.push_str(",\"expired\":true");
            }
            if entry.checked_at != 0 {
                out.push_str(",\"checkedAt\":");
                out.push_str(&entry.checked_at.to_string());
            }
            out.push('}');
        }
        out.push_str("}}");
        out
    }
}

fn push_str(out: &mut String, value: &str) {
    jsonord::quote(out, value);
}

fn read_user(value: &J) -> Option<UserRef> {
    let obj = value.as_obj()?;
    let field = |name: &str| -> Option<&J> {
        obj.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    };
    // 老文件（含 Kotlin 写的那份）里这里是 enum 名（`ZHIHU`），内存里统一用 id（`zhihu`）——
    // 不然 `status_of("zhihu")` 找不到它，宿主拿到的 `UserRef.platform` 也是个脏 id。
    let platform = from_stored(field("platform").and_then(|v| v.as_str())?);
    let id = field("id").and_then(|v| v.as_str())?.to_string();
    let name = field("name").and_then(|v| v.as_str())?.to_string();
    Some(UserRef {
        platform,
        id,
        name,
        avatar: field("avatar").and_then(|v| v.as_str()).map(str::to_string),
        url: field("url").and_then(|v| v.as_str()).map(str::to_string),
    })
}

/// 宿主拿到它去真的问 whoami（**问谁、为什么问**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub platform: String,
    /// 问完之后要不要顺手写库（`ensure_checked` 补问的那几条和手动刷新都写）。
    pub remember: bool,
}

/// 一次决策：要问哪几家。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub queries: Vec<Query>,
    /// 状态表变了（宿主把新的 `status` 抄回 Compose）。
    pub status_changed: bool,
    /// `checkedAt` 变了。
    pub checked_at_changed: bool,
}

/// 账号页的状态机。同步、不加锁：宿主在 Compose 的 `scope`（主线程）上调。
///
/// 只做**决策**：`ensure_checked` / `refresh` / `mark_expired` / `logged_in` / `check` 分别返回
/// "要问哪几家"；真正的 `whoami()` 在宿主（它要 OkHttp + Cookie 罐）。回来之后宿主调
/// [`on_whoami`](Self::on_whoami) 把结果交给这里，状态和存档一起更新。
#[derive(Debug)]
pub struct AccountEngine {
    store: AccountStore,
    platforms: Vec<String>,
    /// 平台 id → 状态（保持插入顺序，和 Kotlin 的 `Map` 一样）。
    status: Vec<(String, AccountStatus)>,
    /// 平台 id → 本地存档（`AccountsCache` 那一份，读出来就一直留在内存里）。
    stored: Stored,
    checked_at: Option<i64>,
    started: bool,
    /// 正在问的平台（对应 Kotlin 的 `jobs[p]?.isActive`）。
    in_flight: Vec<String>,
}

impl AccountEngine {
    pub fn new(platforms: &[String]) -> Self {
        Self {
            store: AccountStore::new(),
            platforms: platforms.to_vec(),
            status: Vec::new(),
            stored: Stored::default(),
            checked_at: None,
            started: false,
            in_flight: Vec::new(),
        }
    }

    pub fn status(&self) -> &[(String, AccountStatus)] {
        &self.status
    }

    pub fn status_of(&self, platform: &str) -> Option<&AccountStatus> {
        self.status.iter().find(|(p, _)| p == platform).map(|(_, s)| s)
    }

    /// 本地存着的"上次查到"时间（毫秒）；`None` = 本地还没存过。
    pub fn checked_at(&self) -> Option<i64> {
        self.checked_at
    }

    /// 宿主有多久没调 `ensure_checked` 了（对应 `started`）。
    pub fn started(&self) -> bool {
        self.started
    }

    /// 进「我的 → 账号」时调：把上次存下来的状态**直接摆出来、一个请求都不发**。
    ///
    /// 本地一条都没存的平台（第一次用、或者新装了个插件）才照旧问一次——不然界面上它会一直转圈。
    /// 存过的平台一律不问（要重查点右上角的刷新）。这个方法进几次页面都只做一次。
    ///
    /// `text` 是 `store.get("app.accounts")` 的值（宿主读，Rust 不碰 IO）。
    pub fn ensure_checked(&mut self, text: Option<&str>) -> Plan {
        if self.started {
            return Plan::default();
        }
        self.started = true;
        self.stored = self.store.load(text);
        let saved = self.store.statuses(&self.stored);
        let mut changed = false;
        // 只补还没查过的：万一调用前已经有人手动查过（`check`），那份结果更新
        for (platform, status) in saved {
            if self.status.iter().any(|(p, _)| *p == platform) {
                continue;
            }
            self.status.push((platform, status));
            changed = true;
        }
        self.checked_at = self.store.checked_at(&self.stored);
        let todo: Vec<String> = self
            .platforms
            .clone()
            .into_iter()
            .filter(|p| !self.status.iter().any(|(s, _)| s == p))
            .collect();
        // Kotlin 原版末尾是 `platforms.filter { it !in status }.forEach(::check)`——`check` 会先把它们
        // 标成 `Checking`、再扔进 `jobs`。所以进页面之后界面上那几家是"正在查"，不是一片空白
        // （查到之前用户看到的是"正在查"而不是没有这个平台）。
        for platform in &todo {
            self.set_status(platform, AccountStatus::Checking);
            self.in_flight.retain(|p| p != platform);
            self.in_flight.push(platform.clone());
        }
        // 只有这次真的新标了"正在查"才算变了（别把之前就在飞的那几家也报成一次变化）
        let marked = !todo.is_empty();
        Plan {
            queries: todo
                .into_iter()
                .map(|platform| Query {
                    platform,
                    remember: true,
                })
                .collect(),
            status_changed: changed || marked,
            checked_at_changed: changed,
        }
    }

    /// 用户点了刷新：重新问所有平台（并发）。这是唯一"问全部"的入口。
    pub fn refresh(&mut self) -> Plan {
        self.started = true;
        for platform in self.platforms.clone() {
            // 和 Kotlin 的 `check()` 一样：先标成"正在查"，回来再落结果
            self.set_status(&platform, AccountStatus::Checking);
            self.in_flight.retain(|p| *p != platform);
            self.in_flight.push(platform);
        }
        let queries = self
            .platforms
            .iter()
            .map(|p| Query {
                platform: p.clone(),
                remember: true,
            })
            .collect();
        Plan {
            queries,
            status_changed: true,
            checked_at_changed: false,
        }
    }

    /// 问一个平台（登录 / 退出 / 导入 / 同步 / 补问都走它）：先标成"正在查"，回来由 `on_whoami` 落地。
    pub fn check(&mut self, platform: &str) -> Plan {
        self.started = true;
        self.set_status(platform, AccountStatus::Checking);
        self.in_flight.retain(|p| p != platform);
        self.in_flight.push(platform.to_string());
        Plan {
            queries: vec![Query {
                platform: platform.to_string(),
                remember: true,
            }],
            status_changed: true,
            checked_at_changed: false,
        }
    }

    /// 别处（刷信息流的那个平台）报了"没登录"：把这个平台标成**过期**，**不发请求**——
    /// 下次点刷新才重新问它。
    ///
    /// 正在查这个平台时不动：查回来的结果更准（可能是刚刚才登录上）。本机没有它的登录 Cookie 时
    /// 标成"未登录"而不是"登录已失效"（`has_login_cookies` 由宿主按插件描述算好）。
    pub fn mark_expired(&mut self, platform: &str, has_login_cookies: bool, now_ms: i64) -> Plan {
        let mut plan = Plan::default();
        if !self.platforms.iter().any(|p| p == platform) {
            return plan;
        }
        if self.in_flight.iter().any(|p| p == platform) {
            return plan;
        }
        if let Some(AccountStatus::LoggedOut { expired: true }) = self.status_of(platform) {
            return plan;
        }
        let next = AccountStatus::LoggedOut {
            expired: has_login_cookies,
        };
        self.set_status(platform, next.clone());
        self.store.put(&mut self.stored, platform, &next, now_ms);
        self.checked_at = self.store.checked_at(&self.stored);
        plan.status_changed = true;
        plan.checked_at_changed = true;
        plan
    }

    /// 一次 `whoami()` 的结果。
    pub fn on_whoami(
        &mut self,
        platform: &str,
        outcome: WhoamiOutcome,
        has_login_cookies: bool,
        now_ms: i64,
    ) -> Plan {
        self.in_flight.retain(|p| p != platform);
        let status = match outcome {
            WhoamiOutcome::User(user) => AccountStatus::LoggedIn(user),
            WhoamiOutcome::NoUser => AccountStatus::LoggedOut {
                expired: has_login_cookies,
            },
            WhoamiOutcome::NotLoggedIn => AccountStatus::LoggedOut { expired: true },
            WhoamiOutcome::Failed { message } => AccountStatus::Failed { message },
        };
        let storable = status.storable();
        self.set_status(platform, status.clone());
        let mut plan = Plan {
            queries: Vec::new(),
            status_changed: true,
            checked_at_changed: false,
        };
        if storable {
            self.store.put(&mut self.stored, platform, &status, now_ms);
            self.checked_at = self.store.checked_at(&self.stored);
            plan.checked_at_changed = true;
        }
        plan
    }

    /// 退出登录：凭据已经由宿主清掉了，状态就是确定的"未登录"——**先存下来**，万一接下来那次 whoami
    /// 因为断网失败，也不会把本地那份"已登录"留着、下次进页面又显示已登录。
    pub fn logged_out(&mut self, platform: &str, now_ms: i64) -> Plan {
        let out = AccountStatus::LoggedOut { expired: false };
        self.set_status(platform, out.clone());
        self.store.put(&mut self.stored, platform, &out, now_ms);
        self.checked_at = self.store.checked_at(&self.stored);
        Plan {
            queries: Vec::new(),
            status_changed: true,
            checked_at_changed: true,
        }
    }

    /// 把内存里那份存档编码回 `store.put("app.accounts", …)` 的文本（宿主去落盘）。
    pub fn encode_store(&self) -> String {
        self.store.encode(&self.stored)
    }

    /// 现在内存里的存档（测试 / 调试用）。
    pub fn stored(&self) -> &Stored {
        &self.stored
    }

    fn set_status(&mut self, platform: &str, status: AccountStatus) {
        match self.status.iter_mut().find(|(p, _)| p == platform) {
            Some((_, slot)) => *slot = status,
            None => self.status.push((platform.to_string(), status)),
        }
    }
}

/// `whoami()` 的三种结果（宿主按异常类型分好）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhoamiOutcome {
    User(UserRef),
    /// 平台回了"没有账号"（没登录，但不是异常）。
    NoUser,
    /// 抛了 `NotLoggedInException`。
    NotLoggedIn,
    /// 别的异常（网络 / 风控 / 接口变了），`message` 是一行原因。
    Failed { message: String },
}

/// 存档里那张"平台 → 状态"的表（宿主有时候要 `Map<Platform, AccountStatus>`）。
pub fn status_map(statuses: &[(String, AccountStatus)]) -> HashMap<String, AccountStatus> {
    statuses.iter().cloned().collect()
}
