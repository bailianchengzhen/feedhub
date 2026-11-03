//! **UniFFI 导出：分页状态机、会话缓存、快照、缩略图提示与评论预计算**。
//!
//! 对应 Kotlin 原版：
//! - `app/src/commonMain/kotlin/feedhub/app/detail/Paged.kt`
//! - `app/src/commonMain/kotlin/feedhub/app/detail/Comments.kt`
//! - `app/src/commonMain/kotlin/feedhub/app/detail/DetailCache.kt`
//! - `app/src/commonMain/kotlin/feedhub/app/feed/FeedSnapshot.kt`
//! - `app/src/commonMain/kotlin/feedhub/app/feed/CardImageHints.kt`
//! - `app/src/commonMain/kotlin/feedhub/app/detail/DetailThumbnails.kt`

use std::sync::{Arc, Mutex};
use feedhub_core::model::TimePrecision;
use feedhub_core::present::cache as core_cache;
use feedhub_core::present::comments as core_comments;
use feedhub_core::present::hints as core_hints;
use feedhub_core::present::paging as core_paging;
use feedhub_core::present::snapshot as core_snapshot;

use crate::FfiTimePrecision;

// ===========================================================================
// 1. PagedEngine (分页状态机)
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPagingBeginLoad {
    pub cursor: Option<String>,
    pub refetch_first: bool,
    pub is_first_page: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPagingCompleteResult {
    pub kept_indices: Vec<u32>,
    pub replace: bool,
    pub ended: bool,
    pub loaded: bool,
    pub next_cursor: Option<String>,
    pub is_first_page: bool,
}

#[derive(uniffi::Object)]
pub struct FfiPagingEngine {
    inner: Mutex<core_paging::PagedEngine>,
}

#[uniffi::export]
impl FfiPagingEngine {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(FfiPagingEngine {
            inner: Mutex::new(core_paging::PagedEngine::new()),
        })
    }

    pub fn seed(&self, keys: Vec<String>, next: Option<String>, ended: bool, fresh: bool) {
        self.inner.lock().unwrap().seed(keys, next, ended, fresh);
    }

    pub fn begin_load(&self) -> Option<FfiPagingBeginLoad> {
        self.inner.lock().unwrap().begin_load().map(|b| FfiPagingBeginLoad {
            cursor: b.cursor,
            refetch_first: b.refetch_first,
            is_first_page: b.is_first_page,
        })
    }

    pub fn complete_page(
        &self,
        item_keys: Vec<String>,
        next: Option<String>,
        refetch_first: bool,
        is_first_page: bool,
    ) -> FfiPagingCompleteResult {
        let r = self.inner.lock().unwrap().complete_page(
            item_keys,
            next,
            refetch_first,
            is_first_page,
        );
        FfiPagingCompleteResult {
            kept_indices: r.kept_indices,
            replace: r.replace,
            ended: r.ended,
            loaded: r.loaded,
            next_cursor: r.next_cursor,
            is_first_page: r.is_first_page,
        }
    }

    pub fn fail_page(&self, friendly_err: String, technical_err: String, refetch_first: bool) {
        self.inner
            .lock()
            .unwrap()
            .fail_page(friendly_err, technical_err, refetch_first);
    }

    pub fn cancel(&self) {
        self.inner.lock().unwrap().cancel();
    }

    pub fn is_loading(&self) -> bool {
        self.inner.lock().unwrap().is_loading()
    }

    pub fn is_ended(&self) -> bool {
        self.inner.lock().unwrap().is_ended()
    }

    pub fn is_loaded(&self) -> bool {
        self.inner.lock().unwrap().is_loaded()
    }

    pub fn is_reseed(&self) -> bool {
        self.inner.lock().unwrap().is_reseed()
    }

    pub fn error(&self) -> Option<String> {
        self.inner.lock().unwrap().error()
    }

    pub fn technical_error(&self) -> Option<String> {
        self.inner.lock().unwrap().technical_error()
    }

    pub fn cursor(&self) -> Option<String> {
        self.inner.lock().unwrap().cursor()
    }

    pub fn keys_count(&self) -> u32 {
        self.inner.lock().unwrap().keys_count() as u32
    }

    pub fn has_key(&self, key: String) -> bool {
        self.inner.lock().unwrap().has_key(&key)
    }
}

#[uniffi::export]
pub fn ffi_paging_comments_supported(platform: String) -> bool {
    core_paging::comments_supported(&platform)
}

#[uniffi::export]
pub fn ffi_paging_replies_supported(capability: Option<bool>) -> bool {
    core_paging::replies_supported(capability)
}

#[uniffi::export]
pub fn ffi_paging_comments_sortable(capability: Option<bool>) -> bool {
    core_paging::comments_sortable(capability)
}

#[uniffi::export]
pub fn ffi_paging_comments_off_text() -> String {
    core_paging::COMMENTS_OFF.to_string()
}

#[uniffi::export]
pub fn ffi_paging_should_switch_sort(current: String, next: String) -> bool {
    core_paging::should_switch_sort(&current, &next)
}

// ===========================================================================
// 2. DetailCache (会话级缓存)
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiDetailCacheCommentSeed {
    pub handle: u64,
    pub next: Option<String>,
    pub ended: bool,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiDetailCacheEvicted {
    pub detail_handles: Vec<u64>,
    pub comments_handles: Vec<u64>,
}

#[derive(uniffi::Object)]
pub struct FfiDetailCache {
    inner: Mutex<core_cache::DetailCache>,
}

#[uniffi::export]
impl FfiDetailCache {
    #[uniffi::constructor]
    pub fn new(max_entries: u32, fresh_ms: i64) -> Arc<Self> {
        Arc::new(FfiDetailCache {
            inner: Mutex::new(core_cache::DetailCache::new(max_entries as usize, fresh_ms)),
        })
    }

    #[uniffi::constructor]
    pub fn default_cache() -> Arc<Self> {
        Arc::new(FfiDetailCache {
            inner: Mutex::new(core_cache::DetailCache::default()),
        })
    }

    pub fn alloc_handle(&self) -> u64 {
        self.inner.lock().unwrap().alloc_handle()
    }

    pub fn get(&self, key: String) -> Option<u64> {
        self.inner.lock().unwrap().get(&key)
    }

    pub fn is_fresh(&self, key: String, now: i64) -> bool {
        self.inner.lock().unwrap().is_fresh(&key, now)
    }

    pub fn put(&self, key: String, detail_handle: u64, now: i64) -> FfiDetailCacheEvicted {
        let ev = self.inner.lock().unwrap().put(&key, detail_handle, now);
        FfiDetailCacheEvicted {
            detail_handles: ev.detail_handles,
            comments_handles: ev.comments_handles,
        }
    }

    pub fn remove(&self, key: String) -> FfiDetailCacheEvicted {
        let ev = self.inner.lock().unwrap().remove(&key);
        FfiDetailCacheEvicted {
            detail_handles: ev.detail_handles,
            comments_handles: ev.comments_handles,
        }
    }

    pub fn comments(&self, key: String, sort: String) -> Option<FfiDetailCacheCommentSeed> {
        self.inner.lock().unwrap().comments(&key, &sort).map(|s| {
            FfiDetailCacheCommentSeed {
                handle: s.handle,
                next: s.next,
                ended: s.ended,
                at: s.at,
            }
        })
    }

    pub fn comments_fresh(&self, key: String, sort: String, now: i64) -> bool {
        self.inner.lock().unwrap().comments_fresh(&key, &sort, now)
    }

    pub fn put_comments(
        &self,
        key: String,
        sort: String,
        comments_handle: u64,
        next: Option<String>,
        ended: bool,
        now: i64,
    ) -> FfiDetailCacheEvicted {
        let ev = self.inner.lock().unwrap().put_comments(
            &key,
            &sort,
            comments_handle,
            next,
            ended,
            now,
        );
        FfiDetailCacheEvicted {
            detail_handles: ev.detail_handles,
            comments_handles: ev.comments_handles,
        }
    }

    pub fn clear(&self) -> FfiDetailCacheEvicted {
        let ev = self.inner.lock().unwrap().clear();
        FfiDetailCacheEvicted {
            detail_handles: ev.detail_handles,
            comments_handles: ev.comments_handles,
        }
    }

    pub fn size(&self) -> u32 {
        self.inner.lock().unwrap().len() as u32
    }
}

#[uniffi::export]
pub fn ffi_detail_cache_key(platform: String, id: String) -> String {
    core_cache::detail_cache_key(&platform, &id)
}

// ===========================================================================
// 3. FeedSnapshot (快照读写)
// ===========================================================================

#[uniffi::export]
pub fn ffi_feed_snapshot_file_name(kind: String) -> String {
    core_snapshot::file_name(&kind)
}

#[uniffi::export]
pub fn ffi_feed_snapshot_save(path: String, saved_at: i64, items_json: Vec<String>) -> bool {
    core_snapshot::save_snapshot(&path, saved_at, &items_json)
}

#[uniffi::export]
pub fn ffi_feed_snapshot_load(path: String, now: i64) -> Vec<String> {
    core_snapshot::load_snapshot(&path, now)
}

#[uniffi::export]
pub fn ffi_feed_snapshot_max_items() -> u32 {
    core_snapshot::MAX_ITEMS as u32
}

#[uniffi::export]
pub fn ffi_feed_snapshot_max_age_ms() -> i64 {
    core_snapshot::MAX_AGE_MS
}

// ===========================================================================
// 4. CardImageHints & DetailThumbnails
// ===========================================================================

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiCardImageHint {
    pub source: String,
    pub thumbnail: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

impl From<core_hints::CardImageHint> for FfiCardImageHint {
    fn from(h: core_hints::CardImageHint) -> Self {
        FfiCardImageHint {
            source: h.source,
            thumbnail: h.thumbnail,
            width: h.width,
            height: h.height,
        }
    }
}

impl From<FfiCardImageHint> for core_hints::CardImageHint {
    fn from(h: FfiCardImageHint) -> Self {
        core_hints::CardImageHint::new(h.source, h.thumbnail, h.width, h.height)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiDetailImagePlan {
    pub url: String,
    pub placeholder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiMediaImageInput {
    pub is_video: bool,
    pub url_or_cover: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCardImageSizeResult {
    pub width: Option<i32>,
    pub height: Option<i32>,
}

#[derive(uniffi::Object)]
pub struct FfiCardHintsStore {
    inner: Mutex<core_hints::CardImageHintsStore>,
}

#[uniffi::export]
impl FfiCardHintsStore {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(FfiCardHintsStore {
            inner: Mutex::new(core_hints::CardImageHintsStore::default()),
        })
    }

    pub fn record(&self, item_key: String, hint: FfiCardImageHint) {
        self.inner.lock().unwrap().record(&item_key, hint.into());
    }

    pub fn record_grid(&self, item_key: String, hints: Vec<FfiCardImageHint>) {
        let core_hints = hints.into_iter().map(Into::into).collect();
        self.inner.lock().unwrap().record_grid(&item_key, core_hints);
    }

    pub fn of(&self, item_key: String) -> Vec<FfiCardImageHint> {
        self.inner
            .lock()
            .unwrap()
            .of(&item_key)
            .into_iter()
            .map(Into::into)
            .collect()
    }

    pub fn size(&self) -> u32 {
        self.inner.lock().unwrap().len() as u32
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}

#[uniffi::export]
pub fn ffi_card_hints_ratio(hint: FfiCardImageHint) -> Option<f32> {
    let core_hint: core_hints::CardImageHint = hint.into();
    core_hint.ratio()
}

#[uniffi::export]
pub fn ffi_card_hints_size(media: Vec<FfiMediaImageInput>, url: String) -> FfiCardImageSizeResult {
    let core_media: Vec<core_hints::MediaImageSpec> = media
        .into_iter()
        .map(|m| {
            if m.is_video {
                core_hints::MediaImageSpec::Video {
                    cover: m.url_or_cover,
                    width: m.width,
                    height: m.height,
                }
            } else {
                core_hints::MediaImageSpec::Image {
                    url: m.url_or_cover,
                    width: m.width,
                    height: m.height,
                }
            }
        })
        .collect();
    let (w, h) = core_hints::card_image_size(&core_media, &url);
    FfiCardImageSizeResult {
        width: w,
        height: h,
    }
}

#[uniffi::export]
pub fn ffi_detail_thumbnails_hint(
    hints: Vec<FfiCardImageHint>,
    url: String,
) -> Option<FfiCardImageHint> {
    let core_hints: Vec<core_hints::CardImageHint> = hints.into_iter().map(Into::into).collect();
    core_hints::find_thumbnail_hint(&core_hints, &url).cloned().map(Into::into)
}

#[uniffi::export]
pub fn ffi_detail_image_plan(
    url: String,
    original: Option<String>,
    original_images: bool,
    hint: Option<FfiCardImageHint>,
) -> FfiDetailImagePlan {
    let core_hint: Option<core_hints::CardImageHint> = hint.map(Into::into);
    let plan = core_hints::detail_image_plan(url, original, original_images, core_hint);
    FfiDetailImagePlan {
        url: plan.url,
        placeholder: plan.placeholder,
    }
}

#[uniffi::export]
pub fn ffi_detail_image_ratio(declared: Option<f32>, hint: Option<FfiCardImageHint>) -> f32 {
    let core_hint: Option<core_hints::CardImageHint> = hint.map(Into::into);
    core_hints::detail_image_ratio(declared, core_hint.as_ref())
}

#[uniffi::export]
pub fn ffi_default_body_image_ratio() -> f32 {
    core_hints::DEFAULT_BODY_IMAGE_RATIO
}

// ===========================================================================
// 5. Comments batch & metadata assembly
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPresentCommentInput {
    pub id: String,
    pub author_id: Option<String>,
    pub author_name: Option<String>,
    pub likes: Option<i64>,
    pub floor: Option<i64>,
    pub published_at: Option<i64>,
    pub published_at_precision: Option<FfiTimePrecision>,
    pub ip_location: Option<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_name: Option<String>,
    pub reply_count: Option<i64>,
    pub preview_replies: Vec<FfiPresentPreviewReplyInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPresentPreviewReplyInput {
    pub author_id: Option<String>,
    pub author_name: Option<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPresentCommentDisplayItem {
    pub id: String,
    pub author_name: String,
    pub is_poster: bool,
    pub likes_text: Option<String>,
    pub meta_text: String,
    pub reply_to_name: Option<String>,
    pub has_previews: bool,
    pub more_text: Option<String>,
    pub previews: Vec<FfiPresentPreviewReplyDisplayItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPresentPreviewReplyDisplayItem {
    pub author_name: String,
    pub is_poster: bool,
    pub reply_to_name: Option<String>,
}

fn to_core_precision(prec: Option<FfiTimePrecision>) -> Option<TimePrecision> {
    prec.map(|p| match p {
        FfiTimePrecision::Second => TimePrecision::Second,
        FfiTimePrecision::Minute => TimePrecision::Minute,
        FfiTimePrecision::Hour => TimePrecision::Hour,
        FfiTimePrecision::Day => TimePrecision::Day,
        FfiTimePrecision::Month => TimePrecision::Month,
        FfiTimePrecision::Year => TimePrecision::Year,
    })
}

#[uniffi::export]
pub fn ffi_comments_prepare_batch(
    comments: Vec<FfiPresentCommentInput>,
    poster_id: Option<String>,
    reply_to_root_author_id: Option<String>,
    now_ms: i64,
    offset_sec: i64,
    dot: String,
    on_open_replies_allowed: bool,
    platform_label: Option<String>,
) -> Vec<FfiPresentCommentDisplayItem> {
    let dot_char = dot.chars().next().unwrap_or('.');
    let core_comments: Vec<core_comments::CommentBatchInput> = comments
        .into_iter()
        .map(|c| core_comments::CommentBatchInput {
            id: c.id,
            author_id: c.author_id,
            author_name: c.author_name,
            likes: c.likes,
            floor: c.floor,
            published_at: c.published_at,
            published_at_precision: to_core_precision(c.published_at_precision),
            ip_location: c.ip_location,
            reply_to_id: c.reply_to_id,
            reply_to_name: c.reply_to_name,
            reply_count: c.reply_count,
            preview_replies: c
                .preview_replies
                .into_iter()
                .map(|pr| core_comments::PreviewReplyInput {
                    author_id: pr.author_id,
                    author_name: pr.author_name,
                    reply_to_id: pr.reply_to_id,
                    reply_to_name: pr.reply_to_name,
                })
                .collect(),
        })
        .collect();

    let results = core_comments::prepare_comment_batch(
        &core_comments,
        poster_id.as_deref(),
        reply_to_root_author_id.as_deref(),
        now_ms,
        offset_sec,
        dot_char,
        on_open_replies_allowed,
        platform_label.as_deref(),
    );

    results
        .into_iter()
        .map(|r| FfiPresentCommentDisplayItem {
            id: r.id,
            author_name: r.author_name,
            is_poster: r.is_poster,
            likes_text: r.likes_text,
            meta_text: r.meta_text,
            reply_to_name: r.reply_to_name,
            has_previews: r.has_previews,
            more_text: r.more_text,
            previews: r
                .previews
                .into_iter()
                .map(|p| FfiPresentPreviewReplyDisplayItem {
                    author_name: p.author_name,
                    is_poster: p.is_poster,
                    reply_to_name: p.reply_to_name,
                })
                .collect(),
        })
        .collect()
}

#[uniffi::export]
pub fn ffi_comments_header_title(total: Option<i64>, dot: String) -> String {
    let dot_char = dot.chars().next().unwrap_or('.');
    core_comments::comments_header_title(total, dot_char)
}

#[uniffi::export]
pub fn ffi_comments_meta_text(
    floor: Option<i64>,
    time_text: Option<String>,
    ip_location: Option<String>,
) -> String {
    core_comments::comment_meta_text(floor, time_text.as_deref(), ip_location.as_deref())
}

#[uniffi::export]
pub fn ffi_comments_replies_more_text(
    total: Option<i64>,
    previews_count: u32,
    on_open_replies_allowed: bool,
    platform_label: Option<String>,
) -> Option<String> {
    core_comments::replies_more_text(
        total,
        previews_count as usize,
        on_open_replies_allowed,
        platform_label.as_deref(),
    )
}

#[uniffi::export]
pub fn ffi_comments_replies_unsupported_text(platform_label: String, plugin_installed: bool) -> String {
    core_comments::replies_unsupported_text(&platform_label, plugin_installed)
}
