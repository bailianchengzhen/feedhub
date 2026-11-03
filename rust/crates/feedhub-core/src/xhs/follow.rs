//! **小红书关注流**：存盘（[`store`] 那部分）+ 纯逻辑（选谁重拉 / 并入 / 跨作者归并）。
//!
//! 逐条对照 Kotlin：
//!
//! | 这里 | Kotlin |
//! |---|---|
//! | [`NoteKey`] | `XhsFollowMerge.NoteKey`（`<秒>:<笔记 id>`，同秒比 id） |
//! | [`AuthorState`] | `XhsFollowMerge.AuthorState`（`id` / `name` / `xsecToken` / `items` / `fetchedAt` / `unread` / `unreadAtFetch` / `closedUntil`） |
//! | [`is_due`] / [`due`] | `XhsFollowFreshness.isDue` / `due` |
//! | [`integrate`] | `XhsFollowMerge.integrate` |
//! | [`next_page`] | `XhsFollowMerge.nextPage` |
//! | [`FollowStore`] | `XhsFollowStore`（两个键、`gz1:` + base64(gzip(JSON))、版本不对就当没有） |
//!
//! 全是纯函数 / 纯数据：不联网、不取时钟（`now` 由调用方给）、不取随机数。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::host::StoreHost;
use crate::model::FeedItem;
use crate::xhs::deflate;
use crate::xhs::feeditem_json;
use crate::xhs::jsonord::{self, J};

// ===========================================================================
// NoteKey
// ===========================================================================

/// 笔记的排序键：先比发布时间（秒），同一秒的比笔记 id（24 位小写十六进制，前 8 位是创建时间）。
/// **越大越新**（Kotlin 的 `compareTo` 就是这个口径）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NoteKey {
    pub time: i64,
    pub id: String,
}

impl NoteKey {
    pub fn new(time: i64, id: impl Into<String>) -> Self {
        Self {
            time,
            id: id.into(),
        }
    }

    /// 游标里的写法：`<秒>:<笔记 id>`。
    pub fn encode(&self) -> String {
        format!("{}:{}", self.time, self.id)
    }

    /// [`Self::encode`] 的逆运算。`None` = 游标无效。
    ///
    /// 口径照 Kotlin：`substringBefore(':')` 的 `toLongOrNull()` + `substringAfter(':')` 非空。
    /// 注意 `substringAfter` 在**第一个** `:` 之后切（笔记 id 里本来就没有 `:`，这里照抄）。
    pub fn decode(text: &str) -> Option<NoteKey> {
        let (time_text, id) = match text.find(':') {
            Some(i) => (&text[..i], &text[i + 1..]),
            // 没有 `:`：`substringBefore` 是整串、`substringAfter` 是空串（Kotlin 也是这个结果）
            None => (text, ""),
        };
        let time = time_text.trim().parse::<i64>().ok()?;
        if id.is_empty() {
            return None;
        }
        Some(NoteKey {
            time,
            id: id.to_string(),
        })
    }
}

/// 缓冲里的笔记一定有发布时间（解析时没有时间的已经丢掉）。
///
/// Kotlin 那份是 `publishedAt ?: error(...)`；这里给 `i64::MIN`（排到最老）而不是崩——能走到这一步
/// 说明上游解析有 bug，但存盘 / 翻页不该因此炸掉（`note_key_or_oldest` 单独暴露这个口径）。
pub fn note_key(item: &FeedItem) -> NoteKey {
    NoteKey::new(item.published_at.unwrap_or(i64::MIN), item.id.clone())
}

// ===========================================================================
// AuthorState
// ===========================================================================

/// 一个关注的作者的缓冲和调度信息。不可变，改动用 [`AuthorState::copy_with`]。
///
/// 字段含义（和 Kotlin `AuthorState` 一字不差）：
/// - `xsec_token`：关注列表项里的 token（`user_posted` 要用）；
/// - `items`：第一页的笔记，按 [`NoteKey`] 严格降序、无重复，最多 [`KEEP_PER_AUTHOR`] 条；
/// - `fetched_at`：上次拉第一页的时间（毫秒），0 = 从没拉过；
/// - `unread`：`followings/extra` 最近一次说的"未看笔记数"；
/// - `unread_at_fetch`：上次拉第一页时的 `unread`；
/// - `closed_until`：这个人的主页读不了时，到这个时间（毫秒）之前不再请求。
#[derive(Debug, Clone, PartialEq)]
pub struct AuthorState {
    pub id: String,
    pub name: String,
    pub xsec_token: Option<String>,
    pub items: Vec<FeedItem>,
    pub fetched_at: i64,
    pub unread: i32,
    pub unread_at_fetch: i32,
    pub closed_until: i64,
}

impl AuthorState {
    pub fn new(id: impl Into<String>, name: impl Into<String>, xsec_token: Option<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            xsec_token,
            items: Vec::new(),
            fetched_at: 0,
            unread: 0,
            unread_at_fetch: 0,
            closed_until: 0,
        }
    }

    pub fn is_closed(&self, now: i64) -> bool {
        self.closed_until > now
    }

    /// 上次拉过之后新增的"未看"数（没拉过时就是全部）。
    pub fn new_since_fetch(&self) -> i32 {
        (self.unread - self.unread_at_fetch).max(0)
    }

    /// 记下 `followings/extra` 新给的未看数 `n`。数变小（用户在官方 App / 网页里看过了）时，
    /// "上次拉的时候的数"也跟着降到 `n`，免得 2 → 0（看过了）→ 1（又发了一篇）被当成"没变"而漏拉。
    pub fn with_unread(&self, n: i32) -> Self {
        // Kotlin 的 `minOf(unreadAtFetch, n)` 是 i32 的 min
        let mut next = self.clone();
        next.unread = n;
        next.unread_at_fetch = self.unread_at_fetch.min(n);
        next
    }

    /// `AuthorState.copy(...)` 的等价物（Rust 里没有具名参数，用闭包改一份）。
    pub fn copy_with(&self, f: impl FnOnce(&mut AuthorState)) -> Self {
        let mut next = self.clone();
        f(&mut next);
        next
    }
}

/// 每个作者缓存最新的几条（Kotlin `XhsFollowFeed.KEEP_PER_AUTHOR`）。
pub const KEEP_PER_AUTHOR: usize = 40;

/// 关注流的页大小（Kotlin `XhsFollowFeed.PAGE_SIZE`）。
pub const PAGE_SIZE: usize = 20;

/// 距上次拉第一页超过这么久（一天）就重拉（Kotlin `XhsFollowFreshness.STALE_MS`）。
pub const STALE_MS: i64 = 24 * 3_600_000;

/// 两次拉同一个作者之间的最短间隔（Kotlin `XhsFollowFreshness.MIN_GAP_MS`）。
pub const MIN_GAP_MS: i64 = 60_000;

// ===========================================================================
// 选谁重拉（XhsFollowFreshness）
// ===========================================================================

/// 一个作者要拉，当且仅当：
/// 1. 从没拉过；或者
/// 2. 未看数比上次拉的时候多了（又发了新笔记）；或者
/// 3. 距上次拉超过 [`STALE_MS`]（一天）。
///
/// **不靠"未看数 > 0"**：它不会因为我们拉过主页而减少，靠它会每次刷新都重拉同一批人。
pub fn is_due(a: &AuthorState, now: i64) -> bool {
    if a.is_closed(now) || a.xsec_token.as_deref().unwrap_or("").is_empty() {
        return false;
    }
    if a.fetched_at <= 0 {
        return true;
    }
    let age = now - a.fetched_at;
    if age < MIN_GAP_MS {
        return false;
    }
    age >= STALE_MS || a.new_since_fetch() > 0
}

/// 该重拉的作者 id，按顺序：**有新笔记的在前**（新得多的更前），然后是没拉过的，然后是最久没拉的；
/// 一次最多 `limit` 个，剩下的下次再拉。
///
/// 排序键照 Kotlin：`compareBy { newSinceFetch > 0 ? 0 : 1 }` → `thenByDescending { newSinceFetch }`
/// → `thenBy { fetchedAt }`，**稳定排序**（同键的按输入顺序，也就是 `states` 的迭代顺序）。
pub fn due(states: &[AuthorState], now: i64, limit: usize) -> Vec<String> {
    let mut picked: Vec<&AuthorState> = states.iter().filter(|a| is_due(a, now)).collect();
    // 稳定排序：`sort_by` 是稳定的（Rust 的 `slice::sort_by` 保证稳定）
    picked.sort_by(|a, b| {
        let ka = if a.new_since_fetch() > 0 { 0 } else { 1 };
        let kb = if b.new_since_fetch() > 0 { 0 } else { 1 };
        ka.cmp(&kb)
            .then_with(|| b.new_since_fetch().cmp(&a.new_since_fetch()))
            .then_with(|| a.fetched_at.cmp(&b.fetched_at))
    });
    picked.into_iter().take(limit).map(|a| a.id.clone()).collect()
}

// ===========================================================================
// 并入 / 归并（XhsFollowMerge）
// ===========================================================================

/// 并入新拉到的第一页：以新的为准（还在的被新版本替换，不在的是被删了），按 [`NoteKey`] 降序排，
/// 只留最新的 `keep` 条。`now` 记成 `fetched_at`，`unread` 记成 `unread_at_fetch`，`closed_until` 清零。
///
/// 去重口径照 Kotlin `fresh.distinctBy { it.id }`：**先出现的为准**（后面的同 id 丢掉），然后再排序。
pub fn integrate(a: &AuthorState, fresh: Vec<FeedItem>, now: i64, keep: usize) -> AuthorState {
    let mut seen: HashSet<String> = HashSet::with_capacity(fresh.len());
    let mut items: Vec<FeedItem> = Vec::with_capacity(fresh.len());
    for item in fresh {
        if seen.insert(item.id.clone()) {
            items.push(item);
        }
    }
    // Kotlin `sortedByDescending { it.noteKey }`：稳定排序、降序
    items.sort_by(|x, y| note_key(y).cmp(&note_key(x)));
    items.truncate(keep);
    AuthorState {
        id: a.id.clone(),
        name: a.name.clone(),
        xsec_token: a.xsec_token.clone(),
        items,
        fetched_at: now,
        unread: a.unread,
        unread_at_fetch: a.unread,
        closed_until: 0,
    }
}

/// 跨作者合并出的一页。`more` 为 true 表示后面（缓存里）至少还有一条。
#[derive(Debug)]
pub struct MergePage {
    pub items: Vec<FeedItem>,
    pub more: bool,
}

/// 取比 `before` 更旧的 `limit` 条（`before` 为 `None` = 从最新开始）：所有作者的缓冲合在一起，
/// 按发布时间倒序。**不联网**——要新的靠刷新时重拉第一页。
///
/// 口径照 Kotlin：`flatMap` → `filter` → `distinctBy { id }`（先出现的为准）→ 降序稳定排序 →
/// `take(limit + 1)` → 前 `limit` 条 + `more = size > limit`。
///
/// `states` 的顺序会影响"同 id 先出现的是谁"，所以调用方要按 `cache` 的插入顺序传
/// （Kotlin 那边是 `states.values`，也就是 `LinkedHashMap` 的顺序——Rust 这边用 [`Authors::order`]）。
pub fn next_page(states: &[AuthorState], before: Option<&NoteKey>, limit: usize) -> MergePage {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut all: Vec<&FeedItem> = Vec::new();
    for state in states {
        for item in &state.items {
            if let Some(b) = before {
                if note_key(item) >= *b {
                    continue;
                }
            }
            if seen.insert(item.id.as_str()) {
                all.push(item);
            }
        }
    }
    all.sort_by(|x, y| note_key(y).cmp(&note_key(x)));
    let taken: Vec<FeedItem> = all.iter().take(limit + 1).map(|i| (*i).clone()).collect();
    let more = taken.len() > limit;
    MergePage {
        items: taken.into_iter().take(limit).collect(),
        more,
    }
}

// ===========================================================================
// 存盘（XhsFollowStore）
// ===========================================================================

/// 关注列表的键（和 Kotlin 一样，老数据照读）。
pub const KEY_USERS: &str = "xhs.following.users";
/// 作者缓冲的键。
pub const KEY_AUTHORS: &str = "xhs.following.authors";
/// 存盘格式的版本（Kotlin `XhsFollowStore.VERSION`）。
pub const VERSION: i64 = 1;
/// 缓冲那个键的前缀（gzip + base64）。
const GZIP_PREFIX: &str = "gz1:";

/// 关注列表和它的取得时间（毫秒）。
#[derive(Debug, Clone, PartialEq)]
pub struct UserList {
    pub at: i64,
    pub users: Vec<FollowedAuthor>,
}

/// 各作者的状态和上次问 `followings/extra` 的时间（毫秒）。
///
/// `order` 是状态的**插入顺序**（Kotlin 那边是 `Map<String, AuthorState>`，`states.values` 的顺序
/// 就是它）。Rust 用 `Vec` 显式记着：`next_page` 的"同 id 先出现的为准"和 `due` 的稳定排序都靠它。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Authors {
    pub extra_at: i64,
    pub states: HashMap<String, AuthorState>,
    /// 状态的插入顺序（`states` 的键）。存盘时按它写，读回来也按它建。
    pub order: Vec<String>,
}

impl Authors {
    pub fn new(extra_at: i64, states: Vec<AuthorState>) -> Self {
        let order = states.iter().map(|s| s.id.clone()).collect();
        let states = states.into_iter().map(|s| (s.id.clone(), s)).collect();
        Self {
            extra_at,
            states,
            order,
        }
    }

    pub fn get(&self, id: &str) -> Option<&AuthorState> {
        self.states.get(id)
    }

    pub fn insert(&mut self, state: AuthorState) {
        let id = state.id.clone();
        if !self.states.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.states.insert(id, state);
    }

    /// 只留下 `keep` 里有的 id（取关的人从缓冲里删掉），保持插入顺序。
    pub fn retain(&mut self, keep: &HashSet<String>) {
        self.order.retain(|id| keep.contains(id));
        self.states.retain(|id, _| keep.contains(id));
    }

    /// 按插入顺序迭代（= Kotlin 的 `states.values`）。
    pub fn values(&self) -> impl Iterator<Item = &AuthorState> {
        self.order.iter().filter_map(move |id| self.states.get(id))
    }

    /// 按插入顺序取一份切片（`next_page` / `due` 要吃 `&[AuthorState]`）。
    pub fn ordered(&self) -> Vec<AuthorState> {
        self.values().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

/// 关注列表里的一个人（`followings/self` 的 `user_list[]`）。
///
/// 和 `xhs::FollowedAuthor` 是同一个东西；这里再定义一次是为了让存盘模块不依赖解析模块
/// （`feeditem_json` 已经依赖 `model`，再纠缠下去就成环了）。两者用 [`Self::from_parsed`] 互转。
#[derive(Debug, Clone, PartialEq)]
pub struct FollowedAuthor {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub xsec_token: Option<String>,
}

impl FollowedAuthor {
    pub fn from_parsed(a: &crate::xhs::FollowedAuthor) -> Self {
        Self {
            id: a.id.clone(),
            name: a.name.clone(),
            avatar: a.avatar.clone(),
            xsec_token: a.xsec_token.clone(),
        }
    }

    pub fn to_parsed(&self) -> crate::xhs::FollowedAuthor {
        crate::xhs::FollowedAuthor {
            id: self.id.clone(),
            name: self.name.clone(),
            avatar: self.avatar.clone(),
            xsec_token: self.xsec_token.clone(),
        }
    }
}

/// 关注流的持久化（键都以 `xhs.following.` 开头），照 Kotlin `XhsFollowStore`：
///
/// - 关注列表（几十个人、几 KB）：**明文 JSON**，最多一天刷新一次；
/// - 各作者的缓冲（每人最多 [`KEEP_PER_AUTHOR`] 条）：JSON 之后 **gzip + base64**，整体一个键；
/// - 读不出来（版本不对、损坏）就当没有，重新取；**不会因此抛异常**。
pub struct FollowStore {
    store: Arc<dyn StoreHost>,
}

impl FollowStore {
    pub fn new(store: Arc<dyn StoreHost>) -> Self {
        Self { store }
    }

    // -----------------------------------------------------------------------
    // 关注列表（明文 JSON）
    // -----------------------------------------------------------------------

    /// `XhsFollowStore.loadUsers`：版本不对 / 读不出来 → `None`。
    pub fn load_users(&self) -> Option<UserList> {
        let text = self.store.get(KEY_USERS)?;
        let root = jsonord::parse(&text).ok()?;
        if root.get("v")?.as_i64()? != VERSION {
            return None;
        }
        let at = root.get("at")?.as_i64()?;
        let users = root
            .get("users")?
            .as_arr()?
            .iter()
            .filter_map(|u| {
                let obj = u.as_obj()?;
                let field = |name: &str| obj.iter().find(|(k, _)| k == name).map(|(_, v)| v);
                Some(FollowedAuthor {
                    id: field("id")?.as_str()?.to_string(),
                    name: field("name")?.as_str()?.to_string(),
                    avatar: field("avatar").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    xsec_token: field("xsecToken").and_then(|v| v.as_str()).map(|s| s.to_string()),
                })
            })
            .collect();
        Some(UserList { at, users })
    }

    /// `XhsFollowStore.saveUsers`：`{"v":1,"at":<毫秒>,"users":[…]}`（明文，字段顺序照 Kotlin）。
    pub fn save_users(&self, list: &UserList) {
        let mut out = String::with_capacity(64 + list.users.len() * 128);
        out.push_str("{\"v\":");
        out.push_str(&VERSION.to_string());
        out.push_str(",\"at\":");
        out.push_str(&list.at.to_string());
        out.push_str(",\"users\":[");
        for (i, u) in list.users.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"id\":");
            jsonord::quote(&mut out, &u.id);
            out.push_str(",\"name\":");
            jsonord::quote(&mut out, &u.name);
            if let Some(v) = &u.avatar {
                out.push_str(",\"avatar\":");
                jsonord::quote(&mut out, v);
            }
            if let Some(v) = &u.xsec_token {
                out.push_str(",\"xsecToken\":");
                jsonord::quote(&mut out, v);
            }
            out.push('}');
        }
        out.push_str("]}");
        self.store.put(KEY_USERS, Some(&out));
    }

    // -----------------------------------------------------------------------
    // 作者缓冲（gz1: + base64(gzip(JSON))）
    // -----------------------------------------------------------------------

    /// `XhsFollowStore.loadAuthors`：**读不出来就当全空**（`Authors { extra_at: 0, states: {} }`），
    /// 版本不对也一样。gzip 解不开 / base64 坏了 / JSON 坏了都不抛。
    pub fn load_authors(&self) -> Authors {
        self.try_load_authors().unwrap_or_default()
    }

    fn try_load_authors(&self) -> Option<Authors> {
        let raw = self.store.get(KEY_AUTHORS)?;
        let text = match raw.strip_prefix(GZIP_PREFIX) {
            Some(b64) => {
                // 标准字母表（`java.util.Base64.getDecoder()`），复用酷安 token 那份（`coolapk::token`）
                let bytes = crate::coolapk::token::base64_decode(b64.as_bytes()).ok()?;
                let unzipped = deflate::gunzip(&bytes).ok()?;
                String::from_utf8(unzipped).ok()?
            }
            None => raw,
        };
        parse_authors_json(&text)
    }

    /// `XhsFollowStore.saveAuthors`：JSON → gzip → base64，前面加 `gz1:`。
    ///
    /// gzip 头里的 mtime / OS 字节和 Java `GZIPOutputStream` 不一样（那边 mtime 是 0、OS 是 255，
    /// 这边沿用 [`deflate::gzip`] 的"现在 + 3("Unix")）——**只要两边都能 gunzip 就不算破坏格式**
    pub fn save_authors(&self, authors: &Authors) {
        let text = encode_authors(authors);
        match deflate::gzip(text.as_bytes(), 0, 6) {
            Ok(gz) => self
                .store
                .put(KEY_AUTHORS, Some(&format!("{GZIP_PREFIX}{}", crate::coolapk::token::base64(&gz, true)))),
            Err(_) => {
                // gzip 失败（理论上不会）：退回明文，读的时候 `starts_with("gz1:")` 为假就当明文解析
                self.store.put(KEY_AUTHORS, Some(&text));
            }
        }
    }
}

/// **解析一段作者缓冲的明文 JSON**（`{"v":1,"extraAt":…,"states":[…]}`）→ [`Authors`]。
///
/// 和 [`FollowStore::load_authors`] 共用同一套解码（版本不对 / 坏 JSON / 缺 `states` 都是 `None`），
/// 单独暴露出来给 FFI 的对拍用（"Kotlin 写的 JSON → Rust 读出来"这一条）。
pub fn parse_authors_json(text: &str) -> Option<Authors> {
    let root = jsonord::parse(text).ok()?;
    if root.get("v")?.as_i64()? != VERSION {
        return None;
    }
    let extra_at = root.get("extraAt").and_then(|v| v.as_i64()).unwrap_or(0);
    let states: Vec<AuthorState> = root
        .get("states")?
        .as_arr()?
        .iter()
        .filter_map(decode_author)
        .collect();
    Some(Authors::new(extra_at, states))
}

/// 作者缓冲 → JSON 文本（`{"v":1,"extraAt":…,"states":[…]}`，字段顺序照 Kotlin）。
pub fn encode_authors(authors: &Authors) -> String {
    let mut out = String::with_capacity(256 + authors.states.len() * 512);
    out.push_str("{\"v\":");
    out.push_str(&VERSION.to_string());
    out.push_str(",\"extraAt\":");
    out.push_str(&authors.extra_at.to_string());
    out.push_str(",\"states\":[");
    for (i, state) in authors.values().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"id\":");
        jsonord::quote(&mut out, &state.id);
        out.push_str(",\"name\":");
        jsonord::quote(&mut out, &state.name);
        if let Some(v) = &state.xsec_token {
            out.push_str(",\"xsecToken\":");
            jsonord::quote(&mut out, v);
        }
        out.push_str(",\"items\":[");
        for (j, item) in state.items.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&feeditem_json::encode(item));
        }
        out.push_str("],\"fetchedAt\":");
        out.push_str(&state.fetched_at.to_string());
        out.push_str(",\"unread\":");
        out.push_str(&state.unread.to_string());
        out.push_str(",\"unreadAtFetch\":");
        out.push_str(&state.unread_at_fetch.to_string());
        out.push_str(",\"closedUntil\":");
        out.push_str(&state.closed_until.to_string());
        out.push('}');
    }
    out.push_str("]}");
    out
}

/// 一个 `AuthorState` 的 JSON（`states[]` 里的一项）。
///
/// `items` 是数组；`id` / `name` 是必填（缺了这个人就跳过）；`xsecToken` 为 `None` 时不写
/// （`explicitNulls = false`）；四个数字字段都有默认值（0），缺了当 0。
fn decode_author(value: &J) -> Option<AuthorState> {
    let obj = value.as_obj()?;
    let field = |name: &str| obj.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let id = field("id")?.as_str()?.to_string();
    let name = field("name")?.as_str()?.to_string();
    let items = field("items")
        .and_then(|v| v.as_arr())
        .map(|list| list.iter().filter_map(feeditem_json::decode).collect())
        .unwrap_or_default();
    Some(AuthorState {
        id,
        name,
        xsec_token: field("xsecToken").and_then(|v| v.as_str()).map(|s| s.to_string()),
        items,
        fetched_at: field("fetchedAt").and_then(|v| v.as_i64()).unwrap_or(0),
        unread: field("unread").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        unread_at_fetch: field("unreadAtFetch").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        closed_until: field("closedUntil").and_then(|v| v.as_i64()).unwrap_or(0),
    })
}

