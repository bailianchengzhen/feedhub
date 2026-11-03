//! 小黑盒 Source 的**平台层**：推荐流的请求怎么拼（URL / 参数顺序 / 请求头顺序 / 公共参数 /
//! `hkey` 签名 / 翻页游标）、表情表什么时候去取（和怎么缓存）、错误怎么判。**收发一律通过宿主**
//! （[`HttpHost`] → OkHttp），这里不碰 socket。
//!
//! 逐条对照 `source-heybox/HeyboxSource.kt` 的同名方法（`recommend` / `getBoth` / `checkStatus` /
//! `signUrl` / `emotesFor` / `emoteTable` / `COMMON_PARAMS`）：方法 + URL + 参数顺序 + 请求头顺序 +
//! body 必须和 Kotlin 版**逐字节一样**，`HeyboxRequestEquivalenceTest` 用 MockWebServer 录两次对比守着。
//!
//! 小黑盒只有**一条流**（推荐，`FeedKind.RECOMMEND`），所以跨边界不带流种类：Kotlin 门面在调 Rust 之前
//! 就按能力拒掉别的流（`UnsupportedFeedException`，和 Kotlin 版 `feed()` 的 `when` 一字不差）。
//!
//! 当前只搬推荐流这一条（含它带出来的表情表请求）；**详情 / 评论 / 楼中楼 / 播放 / 扫码 / whoami /
//! 游戏卡片一行没动**，还在 Kotlin（`RustHeyboxSource` 逐方法委托）。
//!
//! 时间戳（`_time` 和表情表的"今天"）与 nonce 都**从外部注入**：Rust 不读系统时钟、不取随机数
//! （nonce 要 `SecureRandom`，留在 Kotlin）。表情表的缓存键 / 格式和 Kotlin 完全一样
//! （`ctx.store` 的 `heybox.emotes` = `{"at":秒,"map":{…}}`，7 天），所以两条路共用同一份缓存。

use std::sync::{Arc, Mutex};

use super::detail::{self, BodyEnv, GameInfo};
use super::{
    comment, contains_emote_token, feed_texts, is_blank, parse_feeds_object, sign, HeyboxError,
};
use crate::detail::{CommentPage, FeedDetail};
use crate::host::{CookieHost, HttpHost, HttpRequest, StoreHost};
use crate::hosted::{push_json_string, query_encode};
use crate::json::{self, JsonMap, JsonValue};
use crate::model::{Page, UserRef};
use crate::play::PlayInfo;

/// 接口根（和 `HeyboxSource.API` 一样）。测试里换成 MockWebServer 的地址。
pub const API: &str = "https://api.xiaoheihe.cn";

/// 取 Cookie / 判登录态用的"网页 URL"（和 Kotlin 的 `COOKIE_URL` 一样，注意是 www）。
pub const COOKIE_URL: &str = "https://www.xiaoheihe.cn/";

/// 表情表在 `ctx.store` 里的键（Kotlin `HeyboxSource.EMOTES_KEY`）。
pub const EMOTES_KEY: &str = "heybox.emotes";

/// 表情表的有效期（Kotlin `EMOTE_TTL_S`：一周）。
const EMOTE_TTL_S: i64 = 7 * 24 * 3600;

/// 网页信息流列宽，网页首页请求里就是 604（Kotlin `FEED_DW`）。
const FEED_DW: &str = "604";

/// 网页的公共参数，照 xiaoheihe-cli `http.py`，**顺序**也照 Kotlin 的 `COMMON_PARAMS`。
///
/// 为什么 Rust 里再写一份：这些属于"请求怎么发"，归 Rust；宿主拦截器留着当兜底
/// （它只补缺失的头 / 参数，所以线上字节不变）。两份漂移会被请求等价门抓住。
const COMMON_PARAMS: [(&str, &str); 8] = [
    ("os_type", "web"),
    ("app", "heybox"),
    ("client_type", "web"),
    ("version", "999.0.4"),
    ("web_version", "3.0"),
    ("x_app", "heybox_website"),
    ("x_client_type", "web"),
    ("x_os_type", "Mac"),
];

/// 签名参数的名字和**顺序**（Kotlin `SIGN_PARAMS`）：拼进 query 时追加在最后。
const SIGN_PARAMS: [&str; 3] = ["hkey", "_time", "nonce"];

/// 实测未登录调 `restore_login` 的返回（"请登录后使用该功能"），其余照 xiaoheihe-cli `http.py`。
pub(super) const NOT_LOGGED_IN: [&str; 5] = ["login", "relogin", "not_login", "need_login", "login_required"];

/// 验证码 / 二次验证，照 xiaoheihe-cli `http.py`。
const CAPTCHA: [&str; 3] = ["show_captcha", "need_captcha", "need_google_check"];

/// 一级评论每页的"组数"（Kotlin `COMMENT_LIMIT`）。
const COMMENT_LIMIT: i32 = 20;

/// 评论排序（Kotlin `CommentSort`）。`Default` 和 `Hot` 都是热门（`sort_filter=hot`），
/// 最新是 `time_desc`；`time_aes`（最早在前）网页里也有，但 Kotlin 没接，这里照旧不接。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentSort {
    Default,
    Hot,
    Newest,
}

/// 详情 / 播放共用的"最近取过的帖子"时限（Kotlin `LINK_REUSE_MS` = 60 秒）。
const LINK_REUSE_SEC: i64 = 60;

/// 游戏卡片一次最多取多少批、每批多少个（Kotlin `MAX_GAME_BATCHES` / `GAME_BATCH`）。
const MAX_GAME_BATCHES: usize = 8;
const GAME_BATCH: usize = 20;

/// 平台默认头，**顺序照 Kotlin 的 `DefaultHeadersInterceptor`**（`mapOf` 的插入顺序）：
/// User-Agent / Origin / Referer / Accept。宿主拦截器只补缺失的同名头，所以两边最终一致，
/// 后面接 OkHttp 自己加的 Host / Connection / Accept-Encoding / Cookie。
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

const ORIGIN: &str = "https://www.xiaoheihe.cn";

/// 表情表的运行时状态：内存缓存（Kotlin `emoteCache`）+ 单飞标记。
#[derive(Debug, Default)]
struct EmoteState {
    /// 拿到就存，**不再看 TTL**（过期判断在读 `ctx.store` 那一步，和 Kotlin 一样）。
    table: Option<Vec<(String, String)>>,
    /// 有没有一个在飞的取表请求。
    fetching: bool,
}

/// 表情表：`cube_喜欢 → 图片地址`（**顺序**照 `/bbs/app/api/emojis/list` 里出现的顺序，
/// 和 Kotlin 的 `LinkedHashMap` 一样；重复的键覆盖值但保留原位）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmoteTable {
    pub map: Vec<(String, String)>,
    /// 取到这份表的时刻（秒）。只用来判 7 天过期。
    pub fetched_at: i64,
}

impl EmoteTable {
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, code: &str) -> Option<&str> {
        self.map
            .iter()
            .find(|(k, _)| k == code)
            .map(|(_, v)| v.as_str())
    }

    /// `/bbs/app/api/emojis/list` 的响应 → 表情表（Kotlin `EmoteTable.fromEmojiList`）：
    /// `result.emoji_groups[].{group_name, emojis[].{code, img}}` → `group_name_code → img.toHttps()`。
    pub fn from_emoji_list(root: &JsonMap, now: i64) -> Self {
        let mut map: Vec<(String, String)> = Vec::new();
        if let Some(result) = json::obj(root, "result") {
            for group in json::arr(result, "emoji_groups")
                .map(|v| v.as_slice())
                .unwrap_or_default()
            {
                let Some(g) = group.as_object() else { continue };
                let Some(name) = json::text(g, "group_name") else {
                    continue;
                };
                for e in json::arr(g, "emojis").map(|v| v.as_slice()).unwrap_or_default() {
                    let Some(o) = e.as_object() else { continue };
                    let Some(code) = json::text(o, "code") else {
                        continue;
                    };
                    let Some(img) = json::text(o, "img") else {
                        continue;
                    };
                    let key = format!("{name}_{code}");
                    let value = json::to_https(&img);
                    // Kotlin 是 LinkedHashMap：重复键覆盖值、位置留在第一次出现的地方
                    match map.iter_mut().find(|(k, _)| *k == key) {
                        Some(slot) => slot.1 = value,
                        None => map.push((key, value)),
                    }
                }
            }
        }
        Self {
            map,
            fetched_at: now,
        }
    }

    /// `ctx.store` 里的形式（Kotlin `EmoteTable.fromStored`）：`{"at":秒,"map":{…}}`。
    /// 解析不出来就当没有缓存（Kotlin 是 `runCatching { … }.getOrNull()`）。
    pub fn from_stored(text: &str) -> Option<Self> {
        let root = json::parse_object(text).ok()?;
        let map = json::obj(&root, "map")?;
        let entries = map
            .iter()
            .filter_map(|(k, v)| json::as_string(v).map(|s| (k.clone(), s)))
            .collect();
        // Kotlin `o.string("at")?.toLongOrNull() ?: 0`（toLongOrNull 不 trim）
        let fetched_at = json::string(&root, "at")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        Some(Self {
            map: entries,
            fetched_at,
        })
    }

    /// 写回 `ctx.store` 的形式。**逐字节**照 Kotlin `buildJsonObject{…}.toString()`：
    /// `{"at":1791900000,"map":{"cube_喜欢":"https://…"}}`——键序是插入顺序（不是字典序），
    /// 所以这里手写序列化而不用 `serde_json::Value`（它默认按 BTreeMap 排序）。
    pub fn to_stored(&self) -> String {
        let mut out = String::with_capacity(32 + self.map.len() * 64);
        out.push_str("{\"at\":");
        out.push_str(&self.fetched_at.to_string());
        out.push_str(",\"map\":{");
        for (i, (k, v)) in self.map.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            push_json_string(&mut out, k);
            out.push(':');
            push_json_string(&mut out, v);
        }
        out.push_str("}}");
        out
    }
}

/// 小黑盒的平台对象：持有三个宿主端口 + 表情表的内存缓存。
///
/// 生命周期：宿主（`rust-bridge` 的 `RustHeyboxHosted`）建一次、跟着 Source 活；Source 被 App 缓存到
/// 进程结束。**没有后台线程、没有定时器、没有连接池**——闲置时零唤醒。
/// 自己的状态只有一把锁：表情表的内存缓存（Kotlin 那个 `@Volatile var emoteCache`）。
pub struct HeyboxHostedSource {
    base: String,
    http: Arc<dyn HttpHost>,
    store: Arc<dyn StoreHost>,
    cookies: Arc<dyn CookieHost>,
    /// 表情表的内存缓存 + "有没有一个在飞的取表请求"（单飞）。
    ///
    /// Kotlin 版没有单飞这一步；和 `xhs/emotes.rs` 同一条口径：
    /// 首页刷新 / 预取并发进来时，表只取一次，后来的人先用旧表（可能空）——表情是锦上添花，
    /// 不该为它多占 600ms 限速槽和风控名额。
    emotes: Mutex<EmoteState>,
    /// 游戏卡片的内存缓存（Kotlin 的 `ConcurrentHashMap<String, GameInfo>`）：只在详情里有卡片时才用。
    games: Mutex<Vec<GameInfo>>,
    /// 最近取过的 `link/tree` 原始响应（`(id, 秒, body)`，最多 8 条，访问序）：Kotlin 的 `recentLinks`。
    /// 播放还是 Kotlin（`HeyboxVideo.play`），60 秒内点开同一条视频帖时用它，省一次 600ms 限速的请求。
    recent: Mutex<Vec<(String, i64, String)>>,
    /// 一次调用里每个请求各用一个 nonce。
    nonces: sign::NonceLedger,
}

impl HeyboxHostedSource {
    pub fn new(
        base: impl Into<String>,
        http: Arc<dyn HttpHost>,
        store: Arc<dyn StoreHost>,
        cookies: Arc<dyn CookieHost>,
    ) -> Self {
        Self {
            base: base.into(),
            http,
            store,
            cookies,
            emotes: Mutex::new(EmoteState::default()),
            games: Mutex::new(Vec::new()),
            recent: Mutex::new(Vec::new()),
            nonces: sign::NonceLedger::default(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 接上宿主的 nonce 来源（一次调用里第二个起的请求各要一个新的，见 [`sign::NonceLedger`]）。
    pub fn set_nonce_source(&self, source: Arc<dyn sign::NonceSource>) {
        self.nonces.set_host(source);
    }

    /// 推荐流一页。
    ///
    /// `cursor` 就是页游标（`offset` 写成十进制；`null` / 不是数字都是第 1 页 —— Kotlin
    /// `cursor?.value?.toIntOrNull() ?: 0`）；`now_sec` 是秒级时间戳（`_time` 和表情表的
    /// "今天"都用它，由调用方注入，Rust 不读系统时钟）；`nonce` 也是调用方给的（`SecureRandom` 在 Kotlin）。
    ///
    /// 请求序列和 Kotlin 版一样：`/bbs/app/feeds` 一次；响应里的标题 / 摘要带表情代码时，
    /// 再取一次 `/bbs/app/api/emojis/list`（内存缓存 / `ctx.store` 里 7 天内的那份都算命中）。
    pub fn feed(
        &self,
        cursor: Option<&str>,
        now_sec: i64,
        nonce: &str,
    ) -> Result<Page, HeyboxError> {
        let offset = cursor
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0);
        let params = [
            ("pull".to_string(), "0".to_string()),
            ("offset".to_string(), offset.to_string()),
            ("dw".to_string(), FEED_DW.to_string()),
        ];
        // 树已经查过 status 了，直接拿它解析（不再把 98.7 KB 的 body 解析第二遍）
        let root = self.get_both("/bbs/app/feeds", &params, true, now_sec, nonce)?;
        // 标题 / 摘要里有 `[cube_喜欢]` 这样的表情代码才去取表情表（Kotlin `emotesFor(feedTexts(json))`）
        let texts = feed_texts(&root);
        let emotes = if contains_emote_token(&texts) {
            self.emote_table(now_sec, nonce)?
        } else {
            Vec::new()
        };
        parse_feeds_object(&root, offset, &emotes)
    }

    // 详情 / 评论 / 楼中楼

    /// 详情：`/bbs/app/link/tree?link_id=..&page=1&limit=1&is_first=1`。
    ///
    /// `limit` 是评论组数，详情只要帖子本身，所以取 1。正文里的游戏卡片按 20 个一批取 `/game/get_game_infos/`
    /// （最多 8 批），失败不影响详情、卡片降级成只有 id。
    pub fn detail(&self, id: &str, now_sec: i64, nonce: &str) -> Result<FeedDetail, HeyboxError> {
        // `link_first` 已经把拿到 `result.link` 的那份响应记进 recent（播放复用）
        let (root, _body) = self.link_first(id, now_sec, nonce)?;
        let link = json::obj(&root, "result")
            .and_then(|r| json::obj(r, "link"))
            .ok_or_else(|| HeyboxError::parse("详情缺少 result.link"))?;
        // 顺序照 Kotlin：`BodyEnv(emotesFor(link), games(ids))` 是先表情表、后游戏卡片
        let emotes = if any_object_matches(link, contains_emote_token) {
            self.emote_table(now_sec, nonce)?
        } else {
            Vec::new()
        };
        let game_ids = detail::game_ids(link);
        let games = self.games(&game_ids, now_sec, nonce);
        let env = BodyEnv { emotes, games };
        detail::parse_detail_object(&root, &env)
    }

    /// 详情和播放共用的那一个请求：`link/tree` 第一页、热门序、只要一组评论。
    ///
    /// **拿到 `result.link` 就记进 [`recent`](Self::recent_link)**（Kotlin `linkFirst` 的
    /// `.also { if (…obj("result")?.obj("link") != null) recentLinks[id] = … }`）：详情和播放走的是
    /// 同一个请求，播放 60 秒内要复用这一份。所以"详情 → 播放"和"播放 → 再播放"都是 1 个请求。
    fn link_first(
        &self,
        id: &str,
        now_sec: i64,
        nonce: &str,
    ) -> Result<(JsonMap, String), HeyboxError> {
        let params = link_tree_params(id, 1, 1, "hot");
        let (root, body) =
            self.get_both_with_body("/bbs/app/link/tree", &params, true, now_sec, nonce)?;
        if json::obj(&root, "result")
            .and_then(|r| json::obj(r, "link"))
            .is_some()
        {
            self.remember_link(id, now_sec, &body);
        }
        Ok((root, body))
    }

    /// 播放地址（Kotlin `HeyboxSource.play`）：视频帖自己的视频（`partId=main`）或正文里夹着的
    /// `seg:<n>`，地址取 `link/tree` 第一页里的 `video_url` / 正文视频段。
    ///
    /// [`LINK_REUSE_SEC`]（60 秒）内 [`detail`](Self::detail)（或上一次 [`play`](Self::play)）取过
    /// 同一条帖子就直接用那份响应（点开视频帖时 `detail` 刚取过同一个请求），没有才现取一次
    /// ——地址里的 `auth` 是临时签名，会过期。
    ///
    /// **参数先校验**：不认识的 `part_id` 在**发请求之前**就报错（Kotlin 那边是 `HeyboxVideo.play`
    /// 里判断，那时 `link/tree` 已经发了；这里提前一步，把"参数不对时一个请求都不发"做成硬约束，
    /// 请求等价门守着）。
    pub fn play(
        &self,
        id: &str,
        part_id: &str,
        now_sec: i64,
        nonce: &str,
    ) -> Result<PlayInfo, HeyboxError> {
        super::play::check_part(part_id)?;
        let root = match self.recent_link_json(id, now_sec) {
            Some(json) => json,
            None => self.link_first(id, now_sec, nonce)?.0,
        };
        let link = json::obj(&root, "result")
            .and_then(|r| json::obj(r, "link"))
            .ok_or_else(|| HeyboxError::parse("详情缺少 result.link"))?;
        super::play::play(link, part_id)
    }

    /// [`recent_link`](Self::recent_link) 的解析版（`link/tree` 的树）：命中就直接拿来播放。
    fn recent_link_json(&self, id: &str, now_sec: i64) -> Option<JsonMap> {
        let text = self.recent_link(id, now_sec)?;
        json::parse_object(&text).ok()
    }

    /// 一级评论：同一个 `link/tree`，`page` 翻页（游标就是页码），每页 20 组。
    /// 排序 `sort_filter`：`hot`（热门）、`time_desc`（最新在前）；`Default` 按热门（和 Kotlin `when` 一样）。
    pub fn comments(
        &self,
        id: &str,
        cursor: Option<&str>,
        sort: CommentSort,
        now_sec: i64,
        nonce: &str,
    ) -> Result<CommentPage, HeyboxError> {
        // Kotlin：`cursor?.value?.toIntOrNull()?.coerceAtLeast(1) ?: 1`
        let page = cursor
            .and_then(|v| v.parse::<i32>().ok())
            .map(|p| p.max(1))
            .unwrap_or(1);
        let sort = match sort {
            CommentSort::Newest => "time_desc",
            CommentSort::Default | CommentSort::Hot => "hot",
        };
        let params = link_tree_params(id, page, COMMENT_LIMIT, sort);
        let root = self.get_both("/bbs/app/link/tree", &params, true, now_sec, nonce)?;
        let emotes = if any_object_matches(&root, contains_emote_token) {
            self.emote_table(now_sec, nonce)?
        } else {
            Vec::new()
        };
        comment::parse_comments_object(&root, page, &emotes)
    }

    /// 楼中楼：`/bbs/app/comment/sub/comments?link_id=..&root_comment_id=..&limit=20[&lastval=..]`。
    /// 不带 `lastval` 从第一条开始，响应的 `has_more` / `lastval` 就是下一页的游标。
    pub fn replies(
        &self,
        id: &str,
        comment_id: &str,
        cursor: Option<&str>,
        now_sec: i64,
        nonce: &str,
    ) -> Result<CommentPage, HeyboxError> {
        let mut params = vec![
            ("link_id".to_string(), id.to_string()),
            ("root_comment_id".to_string(), comment_id.to_string()),
            ("limit".to_string(), COMMENT_LIMIT.to_string()),
        ];
        // Kotlin：`cursor?.value?.takeIf { it.isNotBlank() }`
        if let Some(value) = cursor.filter(|c| !crate::html::kotlin_is_blank(c)) {
            params.push(("lastval".to_string(), value.to_string()));
        }
        let root = self.get_both("/bbs/app/comment/sub/comments", &params, true, now_sec, nonce)?;
        let emotes = if any_object_matches(&root, contains_emote_token) {
            self.emote_table(now_sec, nonce)?
        } else {
            Vec::new()
        };
        comment::parse_replies_object(&root, comment_id, &emotes, cursor)
    }

    /// 详情之后 60 秒内的同一条帖子原始响应（Kotlin `recentLinks`）：播放还在 Kotlin，
    /// 点开视频帖时 `detail` 刚取过同一个请求，拿它就不用再发一次（600ms 限速里排队）。
    pub fn recent_link(&self, id: &str, now_sec: i64) -> Option<String> {
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let index = recent.iter().position(|(rid, _, _)| rid == id)?;
        let entry = recent.remove(index);
        let fresh = now_sec - entry.1 <= LINK_REUSE_SEC;
        // Kotlin 是访问序 LinkedHashMap：命中就挪到最近
        recent.push(entry.clone());
        if fresh {
            Some(entry.2)
        } else {
            None
        }
    }

    /// 记一条"最近取过的帖子"，最多留 8 条（Kotlin `recentLinks` 的 `RECENT_LINKS`）。
    fn remember_link(&self, id: &str, now_sec: i64, body: &str) {
        const RECENT_LINKS: usize = 8;
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.retain(|(rid, _, _)| rid != id);
        recent.push((id.to_string(), now_sec, body.to_string()));
        while recent.len() > RECENT_LINKS {
            recent.remove(0);
        }
    }

    /// 游戏卡片（Kotlin `games`）：只取缓存里没有的，20 个一批，最多 8 批；
    /// 任何一批失败就**不重试**（风控也不），剩下的卡片只显示 id。
    fn games(&self, ids: &[String], now_sec: i64, nonce: &str) -> Vec<GameInfo> {
        if ids.is_empty() {
            return Vec::new();
        }
        let missing: Vec<String> = {
            let cache = self.games.lock().unwrap_or_else(|e| e.into_inner());
            ids.iter()
                .filter(|id| !cache.iter().any(|g| &g.id == *id))
                .cloned()
                .collect()
        };
        let taken: Vec<String> = missing
            .into_iter()
            .take(MAX_GAME_BATCHES * GAME_BATCH)
            .collect();
        for batch in taken.chunks(GAME_BATCH) {
            let appids = batch.join(",");
            let params = vec![
                ("appids".to_string(), appids),
                ("from".to_string(), "link".to_string()),
            ];
            match self.get_both("/game/get_game_infos/", &params, true, now_sec, nonce) {
                Ok(root) => {
                    let fetched = detail::games_from_response(&root);
                    let mut cache = self.games.lock().unwrap_or_else(|e| e.into_inner());
                    for game in fetched {
                        match cache.iter_mut().find(|g| g.id == game.id) {
                            Some(slot) => *slot = game,
                            None => cache.push(game),
                        }
                    }
                }
                // 不重试，剩下的卡片只显示 id（Kotlin 也是 `break`）
                Err(_) => break,
            }
        }
        let cache = self.games.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<GameInfo> = Vec::new();
        for id in ids {
            if out.iter().any(|g| &g.id == id) {
                continue;
            }
            if let Some(game) = cache.iter().find(|g| &g.id == id) {
                out.push(game.clone());
            }
        }
        out
    }

    /// 表情表（Kotlin `emoteTable`）。三级：内存缓存 → `ctx.store` 里 7 天内的那份 → 现取一次。
    /// **取不到就降级成空表**（Kotlin 也是 `catch (SourceException / IOException) → stored ?: EMPTY`），
    /// 风控也**不重试**。取到的空表同样进内存缓存（Kotlin 也这么写）。
    ///
    /// **同一时间只许一个在飞**：并发进来的人在取表期间
    /// 先用旧表（可能空），不会各发一个 600ms 限速的请求。
    pub fn emote_table(
        &self,
        now_sec: i64,
        nonce: &str,
    ) -> Result<Vec<(String, String)>, HeyboxError> {
        self.emote_map(now_sec);
        {
            let mut state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
            if state.table.is_some() || state.fetching {
                return Ok(state.table.clone().unwrap_or_default());
            }
            state.fetching = true;
        }
        // 单飞抢到之后重新看一眼 store（可能别人刚好写完）
        let stored = self
            .store
            .get(EMOTES_KEY)
            .and_then(|s| EmoteTable::from_stored(&s));
        let table = match self.get_both("/bbs/app/api/emojis/list", &[], true, now_sec, nonce) {
            Ok(root) => {
                let t = EmoteTable::from_emoji_list(&root, now_sec);
                if !t.is_empty() {
                    self.store.put(EMOTES_KEY, Some(&t.to_stored()));
                }
                t.map
            }
            // 风控 / 网络错都不重试，这次先用旧表（可能是过期的），没有就空表
            Err(_) => stored.map(|t| t.map).unwrap_or_default(),
        };
        let mut state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
        state.fetching = false;
        state.table = Some(table.clone());
        Ok(table)
    }

    /// 手上已经有的那份表（内存 → `ctx.store` 里 [`EMOTE_TTL_S`] 内那份）。**不发请求。**
    /// `None` = 内存和 store 里都没有能用的（要不要现取由 [`Self::emote_table`] 决定）。
    pub fn cached_emote_table(&self, now_sec: i64) -> Option<Vec<(String, String)>> {
        self.emote_map(now_sec);
        self.emotes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .table
            .clone()
    }

    /// 下一趟取表会不会真的发请求（纯查询：不改状态、不发请求）。
    pub fn emote_fetch_needed(&self, now_sec: i64) -> bool {
        if self.cached_emote_table(now_sec).is_some() {
            return false;
        }
        !self.emotes.lock().unwrap_or_else(|e| e.into_inner()).fetching
    }

    /// 内存里没表时试着从 `ctx.store` 装一份（7 天内的），返回内存里现在那份。
    fn emote_map(&self, now_sec: i64) -> Vec<(String, String)> {
        {
            let state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(map) = &state.table {
                return map.clone();
            }
        }
        let stored = self
            .store
            .get(EMOTES_KEY)
            .and_then(|s| EmoteTable::from_stored(&s));
        match stored {
            Some(t) if now_sec - t.fetched_at < EMOTE_TTL_S => {
                let mut state = self.emotes.lock().unwrap_or_else(|e| e.into_inner());
                state.table = Some(t.map.clone());
                t.map
            }
            _ => Vec::new(),
        }
    }

    /// 发一个请求并做**和 Kotlin `getBoth` 一字不差**的判定，返回已经查过 `status` 的树。
    ///
    /// 顺序也很重要：先 HTTP 状态码 → 解析 JSON → `show_captcha` + 没有 `pkey` 当未登录 → `status` 外壳。
    pub(super) fn get_both(
        &self,
        path: &str,
        params: &[(String, String)],
        require_ok: bool,
        now_sec: i64,
        nonce: &str,
    ) -> Result<JsonMap, HeyboxError> {
        self.get_both_with_body(path, params, require_ok, now_sec, nonce)
            .map(|(root, _)| root)
    }

    /// 同 [`Self::get_both`]，但把**原始 body** 也交回来（详情要留一份给播放复用）。
    pub(super) fn get_both_with_body(
        &self,
        path: &str,
        params: &[(String, String)],
        require_ok: bool,
        now_sec: i64,
        nonce: &str,
    ) -> Result<(JsonMap, String), HeyboxError> {
        let url = self.signed_url(path, params, now_sec, nonce);
        let response = self
            .http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url,
                headers: default_headers(),
                body: Vec::new(),
            })
            .map_err(HeyboxError::from)?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        match response.status {
            200..=299 => {}
            401 => return Err(HeyboxError::NotLoggedIn { detail: None }),
            403 => {
                return Err(HeyboxError::RiskControl {
                    code: Some("HTTP 403".to_string()),
                    detail: "请求被拒绝".to_string(),
                })
            }
            429 => {
                return Err(HeyboxError::RiskControl {
                    code: Some("HTTP 429".to_string()),
                    detail: "请求过于频繁".to_string(),
                })
            }
            code => {
                return Err(HeyboxError::Api {
                    code: format!("HTTP {code}"),
                    detail: json::take_chars(&body, 200),
                })
            }
        }
        let root = json::parse_object(&body).map_err(HeyboxError::parse)?;
        // 详情接口未登录时回 show_captcha（同一个请求带登录态就正常），这不是真验证码，是要登录
        if json::string(&root, "status").as_deref() == Some("show_captcha") {
            let pkey = self.cookies.find(COOKIE_URL, "pkey");
            if pkey.map(|v| is_blank(&v)).unwrap_or(true) {
                return Err(HeyboxError::NotLoggedIn {
                    detail: Some("这个接口需要登录（未登录时返回 show_captcha）".to_string()),
                });
            }
        }
        if require_ok {
            check_status(&root)?;
        }
        Ok((root, body))
    }

    /// 补公共参数、去掉旧的签名参数、按路径重新算 `hkey` / `_time` / `nonce`，再拼成完整 URL。
    /// 逐行照 Kotlin `signUrl` + `(api + path).toHttpUrl().newBuilder()`。
    fn signed_url(&self, path: &str, params: &[(String, String)], now_sec: i64, nonce: &str) -> String {
        signed_url_for(&self.base, path, params, now_sec, &self.nonces.next(nonce, now_sec))
    }

    // 账号：我是谁

    /// `/account/restore_login`（网页接口，`requireOk = false`）：未登录时 `status` 是 `login` 之类，
    /// 或者 HTTP 401（Kotlin 也是这么判的），两种都给 `None`；`status` 真的异常照常抛。
    pub fn whoami(&self, now_sec: i64, nonce: &str) -> Result<Option<UserRef>, HeyboxError> {
        let root = match self.get_both("/account/restore_login", &[], false, now_sec, nonce) {
            Ok(root) => root,
            // HTTP 401：Kotlin 的 `getBoth` 在 401 时抛 NotLoggedIn，`whoami` 接住返回 null
            Err(HeyboxError::NotLoggedIn { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        // 未登录时 status=login、msg="请登录后使用该功能"
        if let Some(status) = json::string(&root, "status") {
            if NOT_LOGGED_IN.contains(&status.as_str()) {
                return Ok(None);
            }
        }
        check_status(&root)?;
        Ok(super::account(&root))
    }
}

/// [`HeyboxHostedSource::signed_url`] 的自由函数版：扫码登录那边也要用同一份签名口径（`heybox/qr.rs`）。
pub(super) fn signed_url_for(
    base: &str,
    path: &str,
    params: &[(String, String)],
    now_sec: i64,
    nonce: &str,
) -> String {
    let mut all: Vec<(String, String)> = params.to_vec();
    for (k, v) in COMMON_PARAMS {
        // Kotlin 看的是**原始** url 里有没有这个参数（`url.queryParameter(k) == null`）
        if !all.iter().any(|(name, _)| name == k) {
            all.push((k.to_string(), v.to_string()));
        }
    }
    all.retain(|(name, _)| !SIGN_PARAMS.contains(&name.as_str()));
    all.extend(sign::signed_params(path, now_sec, nonce));
    url_for(base, path, &all)
}

/// 平台默认头（**有序**，见 [`USER_AGENT`] 的说明）。宿主拦截器只补缺失的同名头，所以两边最终一致。
pub(super) fn default_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("Origin".to_string(), ORIGIN.to_string()),
        ("Referer".to_string(), format!("{ORIGIN}/")),
        (
            "Accept".to_string(),
            "application/json, text/plain, */*".to_string(),
        ),
    ]
}

/// 拼完整 URL。每一项都按 OkHttp 的 `addQueryParameter` 口径编码（[`query_encode`]），
/// **顺序**就是传进来的顺序（签名三个在最后）。
pub(super) fn url_for(base: &str, path: &str, params: &[(String, String)]) -> String {
    let mut url = format!("{base}{path}");
    if !params.is_empty() {
        url.push('?');
        let query = params
            .iter()
            .map(|(k, v)| format!("{}={}", query_encode(k), query_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        url.push_str(&query);
    }
    url
}

// `query_encode` / `push_json_string` 从 `crate::hosted` 导入（口径和注释都在那里）。

/// `link/tree` 的参数（**顺序**照 Kotlin `linkTree`）。
fn link_tree_params(id: &str, page: i32, limit: i32, sort: &str) -> Vec<(String, String)> {
    vec![
        ("link_id".to_string(), id.to_string()),
        ("page".to_string(), page.to_string()),
        ("limit".to_string(), limit.to_string()),
        ("is_first".to_string(), if page == 1 { "1" } else { "0" }.to_string()),
        ("index".to_string(), "1".to_string()),
        ("owner_only".to_string(), "0".to_string()),
        ("sort_filter".to_string(), sort.to_string()),
    ]
}

/// Kotlin `JsonObject.anyString { … }`：整棵树里有没有哪个字符串满足条件（决定要不要取表情表）。
fn any_object_matches(m: &JsonMap, predicate: fn(&str) -> bool) -> bool {
    m.values().any(|v| any_value_matches(v, predicate))
}

fn any_value_matches(v: &JsonValue, predicate: fn(&str) -> bool) -> bool {
    match v {
        JsonValue::String(s) => predicate(s),
        JsonValue::Array(items) => items.iter().any(|v| any_value_matches(v, predicate)),
        JsonValue::Object(o) => any_object_matches(o, predicate),
        _ => false,
    }
}

/// 小黑盒统一的 `{status, msg, result}` 外壳（Kotlin `checkStatus`）。
///
/// 异常里只放 status 和截短的 msg（xiaoheihe-cli 提醒过上游消息里可能带会话信息）。
pub fn check_status(root: &JsonMap) -> Result<(), HeyboxError> {
    let Some(status) = json::string(root, "status") else {
        return Err(HeyboxError::parse("响应缺少 status"));
    };
    let msg = json::text(root, "msg")
        .map(|m| json::take_chars(&m, 100))
        .unwrap_or_default();
    if status == "ok" {
        return Ok(());
    }
    if NOT_LOGGED_IN.contains(&status.as_str()) {
        return Err(HeyboxError::NotLoggedIn {
            detail: Some(format!("未登录或登录已失效（{status}）")),
        });
    }
    if CAPTCHA.contains(&status.as_str()) {
        return Err(HeyboxError::RiskControl {
            code: Some(status),
            detail: if msg.is_empty() {
                "需要验证".to_string()
            } else {
                msg
            },
        });
    }
    // 实测：不带签名 → "缺少必要参数[_time]"；hkey 不对 → "非法请求"
    if SIGN_PARAMS.iter().any(|k| msg.contains(&format!("[{k}]"))) || msg == "非法请求" {
        return Err(HeyboxError::Signature {
            detail: format!("{status} {msg}"),
        });
    }
    if msg.contains("频繁") || msg.contains("验证") {
        return Err(HeyboxError::RiskControl {
            code: Some(status),
            detail: msg,
        });
    }
    Err(HeyboxError::Api {
        code: status,
        detail: msg,
    })
}

