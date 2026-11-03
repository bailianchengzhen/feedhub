//! 贴吧关注管理那条路的**存盘**过 UniFFI 的那一层。
//!
//! 但当年的关注管理这条路（`followings` /
//! `setFollowing` 里"读一次关注的吧 → 更新缓存 → 已经是目标状态就不写"）还在调 Kotlin 自己那份
//! `ForumStore`——**同一份存盘格式两份实现**。
//! Kotlin 那份只剩"薄门面 + 模型映射"。
//!
//! **各吧缓冲的条目走 JSON 文本**（[`FfiTiebaForumBuffer::items_json`]）：`FeedItem` 的 JSON
//! 写法全仓只有一份（`xhs::feeditem_json`），宿主用 kotlinx 把它变成 `List<FeedItem>`；
//! 这样两边不需要再对一遍 `FeedItem` 的每个字段（过 `FfiItem` 反而多一层可能漂的映射）。

use std::sync::Arc;

use feedhub_core::tieba::forum::{self, FollowedForum, ForumList, ForumState};
use feedhub_core::xhs::{feeditem_json, jsonord};

use crate::host::{HostStore, StoreAdapter};
use crate::tieba::FfiTiebaForum;

/// 吧列表 + 它的抓取时间（毫秒）。`None` = 没有缓存 / 版本不对 / 读不出来（下次重新取一次）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaForumList {
    pub at: i64,
    pub forums: Vec<FfiTiebaForum>,
}

/// 一个吧的缓冲（`ForumState`）。条目是 `FeedItem` 的 **JSON 数组文本**。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaForumBuffer {
    pub id: i64,
    pub name: String,
    pub day_threads: i32,
    /// `FeedItem` 的 JSON 数组（顺序就是列表顺序，新的在前）。
    pub items_json: String,
    pub tail_pos: i32,
    pub exhausted: bool,
    pub head_fetched_at: i64,
    /// 每天新帖数；`None` = 抓不出来（Kotlin `Double?`）。
    pub per_day: Option<f64>,
    pub closed_until: i64,
}

/// 存盘常量（排障 / 测试断言用；和 Kotlin `ForumStore` 的一样）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTiebaForumStoreKeys {
    pub forums: String,
    pub buffers: String,
    pub version: i64,
    pub forums_version: i64,
    pub keep_per_forum: u32,
}

fn to_followed(f: FfiTiebaForum) -> FollowedForum {
    FollowedForum {
        id: f.id,
        name: f.name,
        day_threads: f.day_threads,
        avatar: f.avatar,
        member_count: f.member_count,
        thread_count: f.thread_count,
    }
}

fn to_ffi_followed(f: &FollowedForum) -> FfiTiebaForum {
    FfiTiebaForum {
        id: f.id,
        name: f.name.clone(),
        day_threads: f.day_threads,
        avatar: f.avatar.clone(),
        member_count: f.member_count,
        thread_count: f.thread_count,
    }
}

/// `FeedItem` 列表 → JSON 数组文本（和存盘格式里 `items` 那一段同一个写法）。
fn encode_items(items: &[feedhub_core::model::FeedItem]) -> String {
    let mut out = String::with_capacity(64 + items.len() * 512);
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&feeditem_json::encode(item));
    }
    out.push(']');
    out
}

/// JSON 数组文本 → `FeedItem` 列表。坏条目跳过（和读缓存时"能读几条算几条"的口径一致）。
fn decode_items(text: &str) -> Vec<feedhub_core::model::FeedItem> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(root) = jsonord::parse(trimmed) else {
        return Vec::new();
    };
    match root.as_arr() {
        Some(items) => items.iter().filter_map(feeditem_json::decode).collect(),
        None => Vec::new(),
    }
}

/// 贴吧关注流的存盘（`ForumStore`）：吧列表 + 各吧缓冲，**键和格式和 Kotlin 完全一样**。
#[derive(uniffi::Object)]
pub struct FfiTiebaForumStore {
    store: Arc<dyn HostStore>,
}

#[uniffi::export]
impl FfiTiebaForumStore {
    /// 绑到 `ctx.store` 上（宿主每次调用新建一个就行：无状态，一次 `put` 一个键）。
    #[uniffi::constructor]
    pub fn new(store: Arc<dyn HostStore>) -> Self {
        FfiTiebaForumStore { store }
    }

    /// `ForumStore.loadForums`：明文 JSON，版本不对 / 读不出来 → `None`。
    pub fn load_forums(&self) -> Option<FfiTiebaForumList> {
        let store = forum::ForumStore::new(Arc::new(StoreAdapter(self.store.clone())));
        store.load_forums().map(|list| FfiTiebaForumList {
            at: list.at,
            forums: list.forums.iter().map(to_ffi_followed).collect(),
        })
    }

    /// `ForumStore.saveForums`：`{"v":2,"at":<毫秒>,"forums":[…]}`。
    pub fn save_forums(&self, at: i64, forums: Vec<FfiTiebaForum>) {
        let store = forum::ForumStore::new(Arc::new(StoreAdapter(self.store.clone())));
        let list = ForumList {
            at,
            forums: forums.into_iter().map(to_followed).collect(),
        };
        store.save_forums(&list);
    }

    /// `ForumStore.invalidateForums`：下次关注流刷新时重新取（各吧缓冲不动）。
    pub fn invalidate_forums(&self) {
        let store = forum::ForumStore::new(Arc::new(StoreAdapter(self.store.clone())));
        store.invalidate_forums();
    }

    /// `ForumStore.loadBuffers`：`gz1:` + base64(gzip(JSON))；读不出来 / 版本不对 → 空表。
    /// 老数据里 `detail` 是 `Web` 的，读出来已经升级成 `Native`（和 Kotlin 一样）。
    pub fn load_buffers(&self) -> Vec<FfiTiebaForumBuffer> {
        let store = forum::ForumStore::new(Arc::new(StoreAdapter(self.store.clone())));
        let mut states: Vec<ForumState> = store.load_buffers().into_values().collect();
        // Kotlin 那边是 `associateBy { it.id }`（HashMap 的顺序）：这里按 id 排一下，对拍时输出稳定
        states.sort_by_key(|f| f.id);
        states
            .into_iter()
            .map(|f| FfiTiebaForumBuffer {
                id: f.id,
                name: f.name,
                day_threads: f.day_threads,
                items_json: encode_items(&f.items),
                tail_pos: f.tail_pos,
                exhausted: f.exhausted,
                head_fetched_at: f.head_fetched_at,
                per_day: f.per_day,
                closed_until: f.closed_until,
            })
            .collect()
    }

    /// `ForumStore.saveBuffers`：每个吧先压到 [`FfiTiebaForumStoreKeys::keep_per_forum`] 条，
    /// 再 `gz1:` + base64(gzip(JSON))，整体一个键。
    pub fn save_buffers(&self, buffers: Vec<FfiTiebaForumBuffer>) {
        let store = forum::ForumStore::new(Arc::new(StoreAdapter(self.store.clone())));
        let states: Vec<ForumState> = buffers
            .into_iter()
            .map(|b| ForumState {
                id: b.id,
                name: b.name,
                day_threads: b.day_threads,
                items: decode_items(&b.items_json),
                tail_pos: b.tail_pos,
                exhausted: b.exhausted,
                head_fetched_at: b.head_fetched_at,
                per_day: b.per_day,
                closed_until: b.closed_until,
            })
            .collect();
        store.save_buffers(&states);
    }
}

#[uniffi::export]
pub fn tieba_forum_store_keys() -> FfiTiebaForumStoreKeys {
    FfiTiebaForumStoreKeys {
        forums: forum::KEY_FORUMS.to_string(),
        buffers: forum::KEY_BUFFERS.to_string(),
        version: forum::VERSION,
        forums_version: forum::FORUMS_VERSION,
        keep_per_forum: forum::KEEP_PER_FORUM as u32,
    }
}
