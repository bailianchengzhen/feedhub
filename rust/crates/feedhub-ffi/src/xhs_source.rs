//! **小红书平台对象**：推荐流（homefeed）、关注流三个接口（`followings/self` / `followings/extra` /
//! `user_posted`）、搜索（`search/notes`）和**详情 / 一级评论 / 楼中楼**的整个请求逻辑
//! （拼 URL / 参数顺序 / body / 请求头顺序 / 签名 / 翻页游标 / 状态码与业务码判定）+ 解析，
//! 都在这一个 `uniffi::Object` 后面。
//!
//! 宿主要做的只有三件（构造时塞进来）：发请求、读写 store、读写 Cookie 罐。见 [`crate::host`]。
//!
//! 另外还有两个口子是给 "Kotlin 那边还没搬的请求"（详情 / 评论 / 楼中楼 / 扫码 / 表情表）用的：
//! [`FfiXhsSource::sign`]（签名，**和五条自建请求共用一份会话计数**）和几个 `*_pad_len`
//! （`x-rap-param` 的补位长度；补位字符要照这个数去取，见 `feedhub_core::xhs::sign::rap_pad_len`）。
//!
//! 用法（Kotlin）：
//! ```kotlin
//! val src = FfiXhsSource(base, profileJson, state, hostHttp, hostStore, hostCookies)
//! val page = withContext(Dispatchers.IO) { src.homefeed(cursor, nowMs, rand) }
//! ```
//! 一个 Source 建一个对象，跟着 Source 活。对象的状态只有一份签名计数（`Mutex`），没有内部线程 / 定时器。

use std::sync::Arc;

use feedhub_core::model::Page;
use crate::FfiItem;
use feedhub_core::xhs;
use feedhub_core::xhs::sign as xsign;
use feedhub_core::xhs::follow_feed::FollowConfig;
use feedhub_core::xhs::source::XhsHostedSource;

use crate::host::{
    ClockAdapter, CookiesAdapter, HostCookies, HostHttp, HostStore, HostXhsClock, HostXhsSelfId,
    HostXhsSignRandom, HttpAdapter, SelfIdAdapter, SignRandomAdapter, StoreAdapter,
};
use crate::play::to_ffi_play;
use crate::{to_ffi_comment_page, to_ffi_detail, FfiCommentPage, FfiCommentSort, FfiDetail, FfiEmote, FfiPlayInfo};
use crate::xhs::{
    FfiXhsAuthor, FfiXhsFollowings, FfiXhsHeader, FfiXhsHomefeed, FfiXhsPosted, FfiXhsSearch,
    FfiXhsSignRandom, FfiXhsSignRequest, FfiXhsSignState, FfiXhsUnread,
};
use crate::{map_error, to_ffi_page, FfiError};

const PLATFORM: &str = "xhs";

/// 详情 + "正文 / 标题里有没有表情记号"。
///
/// 为什么要带这个布尔：Kotlin 那边**只在正文真出现 `[笑哭R]` 这种记号时才去补取表情表**
/// （`XhsSource.emotesForBody`：没有记号就一个请求都不加）。表在 Rust 解析完之后才拿得到正文，
/// 所以由 Rust 顺手算好带回去——否则 Kotlin 只能"表不在手上就先取表"，白占一个风控名额。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsHostedDetail {
    pub detail: FfiDetail,
    pub has_emotes: bool,
    /// **原始响应 body**。Kotlin 用它在"这是视频笔记"时回填 `recentNotes`
    /// （`play` 5 分钟内复用同一份 `POST feed` 的结果，不再多发一个详情请求——小红书风控最看详情次数）。
    /// 别的笔记不用、也不解析它。
    pub body: String,
}

/// 一页评论 / 楼中楼 + 同一个"有没有表情记号"的标志（口径见 [`FfiXhsHostedDetail`]）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsHostedComments {
    pub page: FfiCommentPage,
    pub has_emotes: bool,
}

/// 关注流一页：条目 + 游标 + 这次刷新有没有真的联网。
///
/// 调度（关注列表 TTL / 谁该重拉 / 预算 / 冷却 / 存盘）全在 Rust 平台对象里；
/// Kotlin 只给三样东西：时钟、签名随机源、自己的 user_id（`whoami` 没搬）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsFollowPage {
    pub nodes: Vec<FfiItem>,
    pub roots: Vec<u32>,
    pub next: Option<String>,
    /// 这次刷新有没有碰网络（`false` = 纯缓存翻页 / 什么都没有需要拉）。
    pub refreshed: bool,
}

/// 小红书平台对象（`uniffi::Object`）。
#[derive(uniffi::Object)]
pub struct FfiXhsSource {
    inner: XhsHostedSource,
    /// 关注流的签名随机源（构造时挂上；别的流不用它，随机量还是按参数传）。
    random: Arc<SignRandomAdapter>,
    /// 关注流的时钟（构造时挂上）。
    clock: Arc<ClockAdapter>,
    /// "我自己是谁"（关注流：真要拉关注列表时才问）。
    self_id: Arc<SelfIdAdapter>,
}

#[uniffi::export]
impl FfiXhsSource {
    /// [`base`] 是接口根（线上是 `https://edith.xiaohongshu.com`，测试指向 MockWebServer）；
    /// [`profile_json`] 是 `sign_profile.json`（或 `ctx.store` 里的覆盖版）序列化出来的 JSON，Rust 不读文件；
    /// [`state`] 是签名计数的初值（Kotlin 按 `XhsSigner` 构造函数的顺序抽好）。
    #[uniffi::constructor]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        base: String,
        profile_json: String,
        state: FfiXhsSignState,
        http: Arc<dyn HostHttp>,
        store: Arc<dyn HostStore>,
        cookies: Arc<dyn HostCookies>,
        // 关注流的签名随机源 / 时钟：**必填**（UniFFI 的 `with_foreign` 参数没有默认值）。
    // 别的流不调它们，但构造时要给（Kotlin 侧一直是同一个 `XhsSignRandomDrawer` 和同一个钟）。
        random: Arc<dyn HostXhsSignRandom>,
        clock: Arc<dyn HostXhsClock>,
        self_id: Arc<dyn HostXhsSelfId>,
    ) -> Result<Arc<Self>, FfiError> {
        let inner = XhsHostedSource::new(
            base,
            &profile_json,
            state_of(state),
            Arc::new(HttpAdapter(http)),
            Arc::new(StoreAdapter(store)),
            Arc::new(CookiesAdapter(cookies)),
        )
        .map_err(|detail| FfiError::Internal {
            platform: PLATFORM.to_string(),
            detail,
        })?;
        Ok(Arc::new(Self {
            inner,
            random: Arc::new(SignRandomAdapter(random)),
            clock: Arc::new(ClockAdapter(clock)),
            self_id: Arc::new(SelfIdAdapter(self_id)),
        }))
    }

    /// **Rust 联网层的构造**：请求由 `feedhub-net` 直接发，一次 FFI 回调都不走。
    ///
    /// 签名随机源 / 时钟 / 自己的 user_id 仍然过 FFI（那是小红书关注流的三个宿主端口，
    /// 见 `feedhub_core::host::SignRandomHost`）——挪进 Rust 不是这一条的事。
    #[uniffi::constructor]
    #[allow(clippy::too_many_arguments)]
    pub fn with_net(
        base: String,
        profile_json: String,
        state: FfiXhsSignState,
        stack: Arc<crate::net_host::FfiNetStack>,
        spec: crate::net_host::FfiHostSpec,
        store: Arc<dyn HostStore>,
        random: Arc<dyn HostXhsSignRandom>,
        clock: Arc<dyn HostXhsClock>,
        self_id: Arc<dyn HostXhsSelfId>,
    ) -> Result<Arc<Self>, FfiError> {
        let http = stack
            .build_host_public(spec)
            .map_err(|e| FfiError::Internal {
                platform: PLATFORM.to_string(),
                detail: e.to_string(),
            })?;
        let inner = XhsHostedSource::new(
            base,
            &profile_json,
            state_of(state),
            http,
            Arc::new(StoreAdapter(store)),
            stack.cookies_host(),
        )
        .map_err(|detail| FfiError::Internal {
            platform: PLATFORM.to_string(),
            detail,
        })?;
        Ok(Arc::new(Self {
            inner,
            random: Arc::new(SignRandomAdapter(random)),
            clock: Arc::new(ClockAdapter(clock)),
            self_id: Arc::new(SelfIdAdapter(self_id)),
        }))
    }

    /// 接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }

    /// 推荐流一页（Rust 拼请求 + 发 + 判错 + 解析；`cursor` 是 `<cursor_score>|<note_index>`）。
    pub fn homefeed(
        &self,
        cursor: Option<String>,
        now_ms: i64,
        rand: FfiXhsSignRandom,
    ) -> Result<FfiXhsHomefeed, FfiError> {
        let page = self
            .inner
            .homefeed(cursor.as_deref(), now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        let ffi = to_ffi_page(&page.page);
        Ok(FfiXhsHomefeed {
            nodes: ffi.nodes,
            roots: ffi.roots,
            next: ffi.next,
            titles: page.titles,
        })
    }

    /// 这一次 `homefeed` 的 `x-rap-param` 要取几个补位字符（纯计算，不占状态、不发请求）。
    pub fn homefeed_pad_len(
        &self,
        cursor: Option<String>,
        now_ms: i64,
        rap_mask: i32,
        rap_session_key: String,
    ) -> Result<u32, FfiError> {
        self.inner
            .homefeed_pad_len(cursor.as_deref(), now_ms, rap_mask, &rap_session_key)
            .map(|n| n as u32)
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 关注列表一页（`followings/self`）。
    pub fn followings(
        &self,
        self_id: String,
        cursor: Option<String>,
        now_ms: i64,
        rand: FfiXhsSignRandom,
    ) -> Result<FfiXhsFollowings, FfiError> {
        let page = self
            .inner
            .followings(&self_id, cursor.as_deref().unwrap_or(""), now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        Ok(FfiXhsFollowings {
            users: page
                .users
                .into_iter()
                .map(|u| FfiXhsAuthor {
                    id: u.id,
                    name: u.name,
                    avatar: u.avatar,
                    xsec_token: u.xsec_token,
                })
                .collect(),
            cursor: page.cursor,
            has_more: page.has_more,
            total: page.total,
        })
    }

    /// 谁有新笔记（`followings/extra`，一批人一次）：用户 id → 未看数。
    pub fn unread(
        &self,
        self_id: String,
        user_ids: Vec<String>,
        now_ms: i64,
        rand: FfiXhsSignRandom,
    ) -> Result<Vec<FfiXhsUnread>, FfiError> {
        let list = self
            .inner
            .unread(&self_id, &user_ids, now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        Ok(list
            .into_iter()
            .map(|(user_id, count)| FfiXhsUnread { user_id, count })
            .collect())
    }

    /// 一个作者的主页第一页（`user_posted`）。[`sticky`] 和 `roots` 一一对应（置顶笔记排在最前）。
    pub fn posted(
        &self,
        author: FfiXhsAuthor,
        now_ms: i64,
        rand: FfiXhsSignRandom,
    ) -> Result<FfiXhsPosted, FfiError> {
        let page = self
            .inner
            .posted(&author_of(author), now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        let sticky = page.notes.iter().map(|(_, s)| *s).collect();
        let items = page.notes.into_iter().map(|(item, _)| item).collect();
        let ffi = to_ffi_page(&Page { items, next: None });
        Ok(FfiXhsPosted {
            nodes: ffi.nodes,
            roots: ffi.roots,
            sticky,
            has_more: page.has_more,
            titles: page.titles,
        })
    }

    /// 这一次 `user_posted` 的 `x-rap-param` 要取几个补位字符（纯计算）。
    pub fn posted_pad_len(
        &self,
        author: FfiXhsAuthor,
        now_ms: i64,
        rap_mask: i32,
        rap_session_key: String,
    ) -> Result<u32, FfiError> {
        self.inner
            .posted_pad_len(&author_of(author), now_ms, rap_mask, &rap_session_key)
            .map(|n| n as u32)
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// **关注流一页**（`XhsSource.feed(FOLLOWING)`）：调度 + 三个接口的请求 + 归并 + 存盘全在 Rust。
    ///
    /// `cursor` 是上一页最后一条的 `<发布时间秒>:<笔记 id>`（[`FfiXhsFollowPage::next`]）；
    /// `None` 或空串 = 刷新（会联网），非空 = **只在缓存里继续往下排、一个请求都不发**。
    ///
    /// **随机量 / 时钟 / "我自己是谁"都通过构造时挂上的回调要**（一次刷新要发 N 个请求，
    /// 每个的签名随机量都不一样；`user_id` 只在真要拉关注列表时才问——`whoami` 没搬）。
    pub fn follow_page(&self, cursor: Option<String>) -> Result<FfiXhsFollowPage, FfiError> {
        let drive = follow_drive(self);
        let config = FollowConfig::default();
        let page = self
            .inner
            .follow_page(cursor.as_deref(), &drive, &config)
            .map_err(|e| map_error(PLATFORM, e))?;
        let ffi = to_ffi_page(&page.page);
        Ok(FfiXhsFollowPage {
            nodes: ffi.nodes,
            roots: ffi.roots,
            next: ffi.next,
            refreshed: page.refreshed,
        })
    }

    /// 关注流**手上那份缓存**（排障 / 测试用）：`states` 里每个作者一条摘要。
    ///
    /// 不发请求、不读盘（内存里那份；还没读过就是空表）。
    pub fn follow_cached(&self) -> Vec<FfiXhsFollowAuthor> {
        match self.inner.follow_state() {
            None => Vec::new(),
            Some(state) => state
                .values()
                .map(|a| FfiXhsFollowAuthor {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    xsec_token: a.xsec_token.clone(),
                    items: a.items.len() as u32,
                    fetched_at: a.fetched_at,
                    unread: a.unread,
                    unread_at_fetch: a.unread_at_fetch,
                    closed_until: a.closed_until,
                })
                .collect(),
        }
    }

    /// 关注流的 `extraAt`（上次问 `followings/extra` 的时间；排障用）。
    pub fn follow_extra_at(&self) -> Option<i64> {
        self.inner.follow_state().map(|s| s.extra_at)
    }

    /// 搜索一页（`POST /api/sns/web/v1/search/notes`，v1 / edith；带 `x-rap-param`，不带 `xy-direction`）。
    ///
    /// `cursor` 是 `<search_id>|<下一页页码>`：`None`（或空串）= 新一轮搜索，这时 Rust 按
    /// `(now_ms, random_part)` 现造 `search_id`、从第 1 页开始；同一轮的所有页复用这个 id。
    /// `random_part` 是 `1..=2147483646` 的随机数（Kotlin 给，Rust 不取随机数）；
    /// [`Self::search_pad_len`] 要用**同一对** `(now_ms, random_part)`，否则补位长度和 body 对不上。
    pub fn search(
        &self,
        keyword: String,
        cursor: Option<String>,
        now_ms: i64,
        random_part: i64,
        rand: FfiXhsSignRandom,
    ) -> Result<FfiXhsSearch, FfiError> {
        let page = self
            .inner
            .search(&keyword, cursor.as_deref(), now_ms, random_part, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        let ffi = to_ffi_page(&page.page);
        Ok(FfiXhsSearch {
            nodes: ffi.nodes,
            roots: ffi.roots,
            next: ffi.next,
            titles: page.titles,
        })
    }

    /// 这一次搜索的 `x-rap-param` 要取几个补位字符（纯计算，不占状态、不发请求）。
    pub fn search_pad_len(
        &self,
        keyword: String,
        cursor: Option<String>,
        now_ms: i64,
        random_part: i64,
        rap_mask: i32,
        rap_session_key: String,
    ) -> Result<u32, FfiError> {
        self.inner
            .search_pad_len(
                &keyword,
                cursor.as_deref(),
                now_ms,
                random_part,
                rap_mask,
                &rap_session_key,
            )
            .map(|n| n as u32)
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 笔记详情（`POST /api/sns/web/v1/feed`，带 `x-rap-param` + `xy-direction`）。
    ///
    /// `id` 是卡片 `detail` 里的 `<笔记 id>@<xsec_source>@<xsec_token>`；缺 `xsec_token` 在**发请求之前**报错。
    /// 这三条是"用户正等着的请求"：Rust 在请求头里多挂一个内部伪请求头，宿主发出去之前会删掉、转成
    /// OkHttp 的优先级 tag（见 `feedhub_core::hosted::PRIORITY_HEADER`）。
    ///
    /// 表情表用**Rust 里那份**（内存 → `ctx.store` 里一周内那份）：**不挡首屏、不为表发请求**，
    /// 表不在手上时正文按原文显示记号、`has_emotes` 为 true 由 Kotlin 后台补取一次。
    pub fn detail(
        &self,
        id: String,
        now_ms: i64,
        rand: FfiXhsSignRandom,
    ) -> Result<FfiXhsHostedDetail, FfiError> {
        let (status, body) = self
            .inner
            .detail_body(&id, now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        let emote_table = self.inner.emote_map(now_ms);
        let reference = xhs::parse_native_id(&id);
        let detail = feedhub_core::xhs::detail::parse_detail(
            &body,
            status as i32,
            &reference,
            &emote_table,
            now_ms / 1000,
        )
        .map_err(|e| map_error(PLATFORM, e))?;
        // 解析成功才回填"最近笔记"：视频笔记点开之后再点播放时 `play` 复用这一份（不再多发一个详情请求）。
        // Kotlin 那边也还留着一份自己的 `recentNotes`（参照实现的 `play` 用），两份互不影响。
        self.inner.remember_note(&reference.note_id, &body, now_ms);
        // `has_emotes` 现在是"**要不要后台补取**"：表不在手上、而正文 / 标题里有记号。
        let has_emotes = emote_table.is_empty()
            && (detail.body.blocks.iter().any(|b| match b {
                feedhub_core::detail::Block::Paragraph { spans }
                | feedhub_core::detail::Block::Heading { spans }
                | feedhub_core::detail::Block::Quote { spans } => spans
                    .iter()
                    .any(|s| feedhub_core::xhs::detail::has_emote_token(s.text())),
                _ => false,
            }) || detail
                .item
                .title
                .as_deref()
                .is_some_and(feedhub_core::xhs::detail::has_emote_token));
        Ok(FfiXhsHostedDetail {
            detail: to_ffi_detail(&detail),
            has_emotes,
            body,
        })
    }

    /// 这一次详情请求的 `x-rap-param` 要取几个补位字符（纯计算：不占状态、不发请求）。
    pub fn detail_pad_len(
        &self,
        id: String,
        now_ms: i64,
        rap_mask: i32,
        rap_session_key: String,
    ) -> Result<u32, FfiError> {
        self.inner
            .detail_pad_len(&id, now_ms, rap_mask, &rap_session_key)
            .map(|n| n as u32)
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 一页一级评论（`GET comment/page`）。游标是响应里的 `data.cursor`（不透明，原样传回）。
    ///
    /// `sort` 收下但**不影响请求**：小红书的一级评论没有排序参数（Kotlin 版也是收下不用）。
    pub fn comments(
        &self,
        id: String,
        cursor: Option<String>,
        _sort: FfiCommentSort,
        now_ms: i64,
        rand: FfiXhsSignRandom,
    ) -> Result<FfiXhsHostedComments, FfiError> {
        let emote_table = self.inner.emote_map(now_ms);
        let page = self
            .inner
            .comments(&id, cursor.as_deref(), now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        Ok(FfiXhsHostedComments {
            has_emotes: page_has_emotes(&page, &emote_table),
            page: to_ffi_comment_page(&page),
        })
    }

    /// 一页楼中楼（`GET comment/sub/page`）。`comment_id` 是一级评论的 id，游标同上。
    pub fn replies(
        &self,
        id: String,
        comment_id: String,
        cursor: Option<String>,
        now_ms: i64,
        rand: FfiXhsSignRandom,
    ) -> Result<FfiXhsHostedComments, FfiError> {
        let emote_table = self.inner.emote_map(now_ms);
        let page = self
            .inner
            .replies(&id, &comment_id, cursor.as_deref(), now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        Ok(FfiXhsHostedComments {
            has_emotes: page_has_emotes(&page, &emote_table),
            page: to_ffi_comment_page(&page),
        })
    }

    /// Rust 手上有没有这条笔记 5 分钟内取过的详情（纯查询：不发请求、不签名、不占签名计数）。
    ///
    /// Kotlin 门面在 `play` 之前先问它：命中时这一条不会发请求、不会签名，所以**不能从 `Random` 里取随机量**
    /// （否则和 Kotlin 参照实现从同一个种子出发时，后面的请求会对不齐）。
    pub fn play_cached(&self, id: String, now_ms: i64) -> bool {
        let reference = xhs::parse_native_id(&id);
        self.inner.recent_note(&reference.note_id, now_ms).is_some()
    }

    // ---------------- 表情表 ----------------

    /// 下一趟 [`Self::ensure_emotes`] 会不会真的发请求（纯查询：不取表、不发请求、不占签名计数）。
    ///
    /// Kotlin 门面先问这一句：**不发请求的那一趟不能从 `Random` 里抽随机量**
    /// （签名的取用顺序要和 Kotlin 参照实现逐位对齐）。
    pub fn emote_fetch_needed(&self, now_ms: i64) -> bool {
        self.inner.emote_fetch_needed(now_ms)
    }

    /// 手上已经有的表情表（**不发请求**）：内存 → `ctx.store` 里一周内那份。没有就是空表。
    ///
    /// Kotlin 参照实现（`KotlinXhsDetailApi`）用它取"手上的表"，Rust 那条路不需要（表在 Rust 里）。
    pub fn cached_emotes(&self, now_ms: i64) -> Vec<FfiEmote> {
        self.inner
            .cached_emote_table(now_ms)
            .map(|t| t.map.into_iter().map(|(code, url)| FfiEmote { code, url }).collect())
            .unwrap_or_default()
    }

    /// 信息流 / 搜索 / 关注流用：`text` 里**真有表情记号**才去取表（可能要发一次请求），
    /// 返回这段文字里真出现过的那几项（`Emotes.used` 的口径）。
    ///
    /// 风控照常抛（Kotlin `emotesFor` 不吞）；其它错误在核心里已经降级成"用旧表 / 10 分钟不再试"。
    pub fn emotes_for(
        &self,
        text: String,
        now_ms: i64,
        rand: FfiXhsSignRandom,
    ) -> Result<Vec<FfiEmote>, FfiError> {
        let used = self
            .inner
            .emotes_for(&text, now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        Ok(used.into_iter().map(|(code, url)| FfiEmote { code, url }).collect())
    }

    /// 后台把表情表补上（详情 / 评论看到 `has_emotes` 时调）：同一时间只许一个在飞，
    /// 失败（非风控）降级成旧表 + 10 分钟内不再试；**风控照常抛**。
    ///
    /// 返回表里有多少条（0 = 这次没取到 / 没有表），排障和测试用。
    pub fn ensure_emotes(&self, now_ms: i64, rand: FfiXhsSignRandom) -> Result<u32, FfiError> {
        let table = self
            .inner
            .ensure_emote_table(now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        Ok(table.map.len() as u32)
    }

    /// 记下"这个时刻之前别再试表情表"（`warm` 碰到风控时调；和 Kotlin `warmEmotes` 的
    /// `emoteRetryAtMs = now + 10 分钟` 一样）。
    pub fn defer_emotes(&self, until_ms: i64) {
        self.inner.defer_emotes(until_ms);
    }

    /// 播放地址：`partId` 只认 `main`；先复用**详情刚取过**的那份（5 分钟内、最多记 8 条），
    /// 没有才用 `INTERACTIVE` 优先级现取一次详情。小红书没有弹幕、也没有按清晰度挑流的参数
    /// （`quality` 收下但一个字节都不影响）。
    pub fn play(
        &self,
        id: String,
        part_id: String,
        quality: Option<i32>,
        now_ms: i64,
        rand: FfiXhsSignRandom,
    ) -> Result<FfiPlayInfo, FfiError> {
        let info = self
            .inner
            .play(&id, &part_id, quality, now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        Ok(to_ffi_play(&info))
    }

    /// 签一个**已经由 Kotlin 拼好**的请求（详情 / 评论 / 楼中楼 / 扫码 / 表情表）。
    ///
    /// 和四条自建请求用**同一份会话计数**，所以一次会话里的 `seq` / `signCount` / `xraySeq` 是一条序列。
    pub fn sign(
        &self,
        request: FfiXhsSignRequest,
        now_ms: i64,
        rand: FfiXhsSignRandom,
    ) -> Result<Vec<FfiXhsHeader>, FfiError> {
        let headers = self
            .inner
            .sign_api(&req_of(request), now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        Ok(headers
            .into_iter()
            .map(|(name, value)| FfiXhsHeader { name, value })
            .collect())
    }
}

/// 一页评论**要不要后台补取表情表**：表不在手上、而评论（含楼中楼预览）里有记号。
fn page_has_emotes(page: &feedhub_core::detail::CommentPage, table: &[(String, String)]) -> bool {
    if !table.is_empty() {
        return false;
    }
    page.items.iter().any(comment_has_emote_token)
}

fn comment_has_emote_token(comment: &feedhub_core::detail::Comment) -> bool {
    use feedhub_core::detail::Block;
    let text_has = comment.content.blocks.iter().any(|b| match b {
        Block::Paragraph { spans } | Block::Heading { spans } | Block::Quote { spans } => {
            spans.iter().any(|s| feedhub_core::xhs::detail::has_emote_token(s.text()))
        }
        _ => false,
    });
    text_has || comment.preview_replies.iter().any(comment_has_emote_token)
}

/// 关注流缓存里的一个作者（摘要，排障 / 测试用；`items` 是条数不是条目）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiXhsFollowAuthor {
    pub id: String,
    pub name: String,
    pub xsec_token: Option<String>,
    /// 缓冲里有几条笔记。
    pub items: u32,
    /// 上次拉第一页的时间（毫秒；0 = 从没拉过）。
    pub fetched_at: i64,
    pub unread: i32,
    pub unread_at_fetch: i32,
    /// 读不了的作者关到这个时间（毫秒）。
    pub closed_until: i64,
}

/// 给一次关注流调用准备 [`FollowDrive`]：构造时挂上的三个回调（适配器）借出去给 core。
fn follow_drive(source: &FfiXhsSource) -> feedhub_core::xhs::follow_feed::FollowDrive<'_> {
    feedhub_core::xhs::follow_feed::FollowDrive {
        clock: source.clock.as_ref(),
        random: source.random.as_ref(),
        self_id: None,
        self_id_lazy: Some(source.self_id.as_ref()),
    }
}

fn state_of(s: FfiXhsSignState) -> xsign::SignState {
    xsign::SignState {
        seq: s.seq,
        sign_count: s.sign_count,
        dsllt: s.dsllt,
        xray_seq: s.xray_seq,
    }
}

fn rand_of(r: FfiXhsSignRandom) -> xsign::SignRandom {
    xsign::SignRandom {
        version: r.version,
        b3_trace_id: r.b3_trace_id,
        xray_tail: r.xray_tail,
        xy_random: r.xy_random,
        rap_mask: r.rap_mask,
        rap_session_key: r.rap_session_key,
        rap_key: r.rap_key,
        rap_salt: r.rap_salt,
        rap_pad: r.rap_pad,
        rap_enc_time_ms: r.rap_enc_time_ms,
    }
}

fn req_of(r: FfiXhsSignRequest) -> xsign::SignRequest {
    xsign::SignRequest {
        api: r.api,
        body: r.body,
        a1: r.a1,
        loadts_ms: r.loadts_ms,
        web_build: r.web_build,
        b1: r.b1,
        user_id: r.user_id,
        rap: r.rap,
        xy: r.xy,
    }
}

fn author_of(a: FfiXhsAuthor) -> xhs::FollowedAuthor {
    xhs::FollowedAuthor {
        id: a.id,
        name: a.name,
        avatar: a.avatar,
        xsec_token: a.xsec_token,
    }
}

// ===========================================================================
// 账号层：`whoami` + 网页扫码登录
// ===========================================================================

/// 小红书扫码登录的平台对象。
///
/// **为什么是另一个对象、而且是挂在 [`FfiXhsSource`] 上的构造方法**：
/// - 扫码要一个**临时 Cookie 罐**的宿主能力（[`crate::host::HostQrLogin`]），别的请求用不上；
/// - 它必须和平台对象**共用签名计数**（Kotlin 那边所有签名走同一个 `XhsSigner`）和同一份 `ctx.store`
///   （b1 / `xhs.user_id` 是设备级的）——所以由平台对象自己造，宿主只多塞一个 `qr` 端口。
/// - 写成"另一个对象"（而不是 `FfiXhsSource` 上的方法）和五家国内平台那一期一致：`FfiXhsSource`
///   的构造函数**一个字都不用改**。
#[derive(uniffi::Object)]
pub struct FfiXhsQr {
    inner: feedhub_core::xhs::qr::XhsQrSource,
}

#[uniffi::export]
impl FfiXhsSource {
    /// 造这个平台对象的**扫码登录**那一半（共用签名计数 / profile / store）。
    ///
    /// [`qr`] 是宿主的临时罐端口（Kotlin 的 `RustQrHost`）。
    pub fn qr_object(self: Arc<Self>, qr: Arc<dyn crate::host::HostQrLogin>) -> Result<Arc<FfiXhsQr>, FfiError> {
        let inner = feedhub_core::xhs::qr::XhsQrSource::new(
            self.inner.base(),
            self.inner.profile_text(),
            self.inner.sign_state(),
            self.inner.store_host(),
            Arc::new(crate::host::QrAdapter(qr)),
        )
        .map_err(|detail| FfiError::Internal {
            platform: PLATFORM.to_string(),
            detail,
        })?;
        Ok(Arc::new(FfiXhsQr { inner }))
    }

    /// 登录态查询（`a1` + `web_session` 都在）：纯查询，**不发请求、不占签名计数**。
    ///
    /// 调用方用它决定"要不要抽一组签名随机量"：没登录时 `whoami` 一个请求都不发，
    /// 抽了随机量就会让后面的随机流和 Kotlin 参照实现对不齐。
    pub fn has_login(&self) -> bool {
        self.inner.has_login()
    }

    /// 「我是谁」：`GET /api/sns/web/v2/user/me`，请求 + 签名 + 解析都在 Rust。
    ///
    /// **没登录（没有 `a1` + `web_session`）时一个请求都不发、直接 `None`**；
    /// 401 / 业务码 `-100` / `-101` / 游客态也是 `None`；别的错误（风控 / 签名 / 网络）照常抛。
    pub fn whoami(&self, now_ms: i64, rand: FfiXhsSignRandom) -> Result<Option<crate::FfiUserRef>, FfiError> {
        let user = self
            .inner
            .whoami(now_ms, &rand_of(rand))
            .map_err(|e| map_error(PLATFORM, e))?;
        Ok(user.as_ref().map(crate::to_ffi_user))
    }
}

#[uniffi::export]
impl FfiXhsQr {
    /// Rust 侧记住的接口根（排障 / 测试断言用）。
    pub fn base(&self) -> String {
        self.inner.base().to_string()
    }

    /// 第一步：要一张二维码。`token` 是轮询用的 `qr_id`，`extra` 里带 `code`。
    ///
    /// [`rands`] 是**这一次调用要用的随机量**（每签一个请求一组，`start` 只签 1 次）。
    /// Kotlin 那边每签一个就取一组（`RustXhsParse.sign` 每次被拦截器调一次），所以边界上按"最多要几组"
    /// 一次给足、Rust 按发请求的顺序依次消费（多给的不用）。理由写在 `XhsQrSource::start` 上面。
    ///
    /// **没有 `a1` 设备 Cookie 时一个请求都不发**、直接抛 `Api("a1")`（Kotlin 同一句文案）。
    pub fn start(
        &self,
        now_ms: i64,
        rands: Vec<FfiXhsSignRandom>,
    ) -> Result<crate::account::FfiQrLogin, FfiError> {
        let random = rands.into_iter().map(rand_of).collect::<Vec<_>>();
        self.inner
            .start(now_ms, &feedhub_core::xhs::qr::RandCursor::new(&random))
            .map(|login| crate::account::to_ffi_qr_login(&login))
            .map_err(|e| map_error(PLATFORM, e))
    }

    /// 第二步：问一次状态。终态（成功 / 过期 / 失败）之后临时罐会被丢掉。
    ///
    /// [`rands`] 要够这个 `poll` 用完：未扫 / 过期只要 1 组，确认之后要 3 组
    /// （`userinfo` → `qrcode/status` → 验证 `user/me`）。Kotlin 那边固定给 3 组。
    pub fn poll(
        &self,
        login: crate::account::FfiQrLogin,
        now_ms: i64,
        rands: Vec<FfiXhsSignRandom>,
    ) -> Result<crate::account::FfiQrState, FfiError> {
        let login = crate::account::from_ffi_qr_login(&login);
        let random = rands.into_iter().map(rand_of).collect::<Vec<_>>();
        self.inner
            .poll(&login, now_ms, &feedhub_core::xhs::qr::RandCursor::new(&random))
            .map(|state| crate::account::to_ffi_qr_state(&state))
            .map_err(|e| map_error(PLATFORM, e))
    }
}
