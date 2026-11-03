//! **桌面对外接口的协议常量 + 取图几何**。
//!
//! 只有纯常量 / 纯计算：错误码表、异常 → 错误码的归类、图片缩放与长图分段。
//! 真正取图（Coil + Skia）和 unix socket 监听仍在宿主。
//!
//! **图片清单**（`plan_detail_images` / `plan_comment_images`）没有导出：它的入参是
//! `FeedDetail` / `Block` 的树，过 FFI 要造一整套 record（`FfiDetail` / `FfiBlock` 已经有了，
//! 但把整棵正文搬过去再搬回来只为算"第几张图是封面"不划算）。宿主侧那份 Kotlin 现在只做
//! "按 Rust 给的几何算段数 + 挑 url"，**规则（封面在前、正文顺序、original 还是 url）在 Rust**
//! 由 `plan_detail_images` 守着单测；宿主那份是它的逐行镜像（`ApiImagePlan.kt` 的注释指回来）。

use std::collections::{HashMap, HashSet};

use feedhub_core::appapi as core;

// ===========================================================================
// 错误码与通用错误
// ===========================================================================

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiApiProtocolError {
    #[error("{detail}")]
    Failure {
        code: String,
        detail: String,
    },
}

impl From<(core::ErrorCode, String)> for FfiApiProtocolError {
    fn from(err: (core::ErrorCode, String)) -> Self {
        Self::Failure {
            code: err.0.code.to_string(),
            detail: err.1,
        }
    }
}

impl From<core::since::SinceError> for FfiApiProtocolError {
    fn from(err: core::since::SinceError) -> Self {
        Self::Failure {
            code: "bad_input".to_string(),
            detail: err.message(),
        }
    }
}

/// 一个对外错误码（`code` 是稳定的字符串，agent 按它分支）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiApiErrorCode {
    pub code: String,
    pub http_status: u16,
    pub retryable: bool,
}

fn to_ffi(code: core::ErrorCode) -> FfiApiErrorCode {
    FfiApiErrorCode {
        code: code.code.to_string(),
        http_status: code.http_status,
        retryable: code.retryable,
    }
}

/// 输出格式的版本（`/capabilities` 的 `schema`）。
#[uniffi::export]
pub fn api_schema_version() -> i64 {
    core::SCHEMA_VERSION
}

/// 全部错误码，**按 `ApiError` 的声明顺序**。
#[uniffi::export]
pub fn api_error_codes() -> Vec<FfiApiErrorCode> {
    core::ERROR_CODES.iter().copied().map(to_ffi).collect()
}

/// 按字符串找错误码；不认识就是 `None`。
#[uniffi::export]
pub fn api_error_of_key(key: String) -> Option<FfiApiErrorCode> {
    core::error_of_key(&key).map(to_ffi)
}

/// 一次失败的**种类**（Kotlin 把异常翻成这个，再问 Rust 该用哪个码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiApiFailureKind {
    RiskControl,
    NotLoggedIn,
    NotSupported,
    CaptchaRequired,
    ProxyUnavailable,
    PlatformError,
    Parse,
    Network,
    NotFound,
    Internal,
}

impl From<FfiApiFailureKind> for core::FailureKind {
    fn from(kind: FfiApiFailureKind) -> Self {
        match kind {
            FfiApiFailureKind::RiskControl => core::FailureKind::RiskControl,
            FfiApiFailureKind::NotLoggedIn => core::FailureKind::NotLoggedIn,
            FfiApiFailureKind::NotSupported => core::FailureKind::NotSupported,
            FfiApiFailureKind::CaptchaRequired => core::FailureKind::CaptchaRequired,
            FfiApiFailureKind::ProxyUnavailable => core::FailureKind::ProxyUnavailable,
            FfiApiFailureKind::PlatformError => core::FailureKind::PlatformError,
            FfiApiFailureKind::Parse => core::FailureKind::Parse,
            FfiApiFailureKind::Network => core::FailureKind::Network,
            FfiApiFailureKind::NotFound => core::FailureKind::NotFound,
            FfiApiFailureKind::Internal => core::FailureKind::Internal,
        }
    }
}

/// 种类 → 错误码（"哪一类错对应哪个码 + 什么状态码"唯一的一份）。
#[uniffi::export]
pub fn api_classify(kind: FfiApiFailureKind) -> FfiApiErrorCode {
    to_ffi(core::classify(kind.into()))
}

// ===========================================================================
// 图片几何
// ===========================================================================

/// `maxw` 的默认值 / 上限、长图阈值（`/capabilities` 和路由都在用）。
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiImageLimits {
    pub default_max_width: i32,
    pub max_max_width: i32,
    pub long_ratio: i32,
}

#[uniffi::export]
pub fn api_image_limits() -> FfiImageLimits {
    FfiImageLimits {
        default_max_width: core::DEFAULT_MAX_WIDTH,
        max_max_width: core::MAX_MAX_WIDTH,
        long_ratio: core::LONG_RATIO,
    }
}

/// 一张图的分段方案（`top` / `height_at` 都是纯查表）。
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiImageSegments {
    pub width: i32,
    pub height: i32,
    pub segment_height: i32,
    pub overlap: i32,
    pub count: i32,
}

impl From<core::Segments> for FfiImageSegments {
    fn from(s: core::Segments) -> Self {
        FfiImageSegments {
            width: s.width,
            height: s.height,
            segment_height: s.segment_height,
            overlap: s.overlap,
            count: s.count,
        }
    }
}

/// 缩放：宽缩到 `max_width`（**不放大**），高按比例、四舍五入，都至少 1。
#[uniffi::export]
pub fn api_scaled(width: i32, height: i32, max_width: i32) -> FfiScaledImage {
    let (w, h) = core::ImageGeometry::scaled(width, height, max_width);
    FfiScaledImage { width: w, height: h }
}

/// 缩放后的宽高。
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiScaledImage {
    pub width: i32,
    pub height: i32,
}

/// 这张图要不要分段（高 > 宽 × 3 且不算太窄）。
#[uniffi::export]
pub fn api_is_long(width: i32, height: i32) -> bool {
    core::ImageGeometry::is_long(width, height)
}

/// 分段方案（对**缩放之后**的宽高算）。
#[uniffi::export]
pub fn api_segments(width: i32, height: i32) -> FfiImageSegments {
    core::ImageGeometry::segments(width, height).into()
}

/// 第 `index` 段在缩放后图片里的起始行。
#[uniffi::export]
pub fn api_segment_top(plan: FfiImageSegments, index: i32) -> i32 {
    core::Segments {
        width: plan.width,
        height: plan.height,
        segment_height: plan.segment_height,
        overlap: plan.overlap,
        count: plan.count,
    }
    .top(index)
}

/// 第 `index` 段有多少行（最后一段可能短）。
#[uniffi::export]
pub fn api_segment_height(plan: FfiImageSegments, index: i32) -> i32 {
    core::Segments {
        width: plan.width,
        height: plan.height,
        segment_height: plan.segment_height,
        overlap: plan.overlap,
        count: plan.count,
    }
    .height_at(index)
}

/// 一条内容里的某张图分几段（按默认 1024 宽算；宽高没给就是 `None`）。
#[uniffi::export]
pub fn api_segments_for(width: Option<i32>, height: Option<i32>) -> Option<i32> {
    let content = core::ContentImages {
        entries: vec![core::ApiImage {
            url: String::new(),
            width,
            height,
            is_cover: false,
            note: None,
        }],
    };
    content.segments_at(0)
}

// ===========================================================================
// 时间与游标
// ===========================================================================

#[uniffi::export(callback_interface)]
pub trait FfiDateToEpoch: Send + Sync {
    fn to_epoch_millis(
        &self,
        year: i32,
        month: u32,
        day: u32,
        hour: Option<u32>,
        minute: Option<u32>,
        second: Option<u32>,
    ) -> Option<i64>;
}

#[uniffi::export]
pub fn api_parse_since(
    text: String,
    now_ms: i64,
    date_to_epoch: Box<dyn FfiDateToEpoch>,
) -> Result<i64, FfiApiProtocolError> {
    core::since::parse_since(&text, now_ms, |y, m, d, hms| match hms {
        Some((h, min, s)) => date_to_epoch.to_epoch_millis(y, m, d, Some(h), Some(min), Some(s)),
        None => date_to_epoch.to_epoch_millis(y, m, d, None, None, None),
    })
    .map_err(Into::into)
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiResumeCursorDecoded {
    pub platform: Option<String>,
    pub skip: i32,
}

#[uniffi::export]
pub fn api_resume_cursor_decode(raw: Option<String>) -> FfiResumeCursorDecoded {
    let d = core::cursor::decode(raw.as_deref());
    FfiResumeCursorDecoded {
        platform: d.platform,
        skip: d.skip,
    }
}

#[uniffi::export]
pub fn api_resume_cursor_encode(platform: Option<String>, skip: i32) -> String {
    core::cursor::encode(platform.as_deref(), skip)
}

// ===========================================================================
// 参数解析与校验
// ===========================================================================

#[uniffi::export]
pub fn api_validate_page_limit(raw: Option<String>) -> Result<Option<i32>, FfiApiProtocolError> {
    core::routes::validate_page_limit(raw.as_deref()).map_err(Into::into)
}

#[uniffi::export]
pub fn api_validate_library_limit(raw: Option<String>) -> Result<i32, FfiApiProtocolError> {
    core::routes::validate_library_limit(raw.as_deref()).map_err(Into::into)
}

#[uniffi::export]
pub fn api_validate_int_param(name: String, raw: Option<String>) -> Result<Option<i32>, FfiApiProtocolError> {
    core::routes::validate_int_param(&name, raw.as_deref()).map_err(Into::into)
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiImageParams {
    pub idx: i32,
    pub maxw: i32,
    pub seg: Option<i32>,
}

#[uniffi::export]
pub fn api_validate_image_params(
    idx_raw: Option<String>,
    maxw_raw: Option<String>,
    seg_raw: Option<String>,
) -> Result<FfiImageParams, FfiApiProtocolError> {
    let (idx, maxw, seg) = core::routes::validate_image_params(
        idx_raw.as_deref(),
        maxw_raw.as_deref(),
        seg_raw.as_deref(),
    )?;
    Ok(FfiImageParams { idx, maxw, seg })
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiCommentsParams {
    pub comment_id: Option<String>,
    pub pages: i32,
    pub sort: String,
    pub limit: Option<i32>,
}

#[uniffi::export]
pub fn api_validate_comments_params(
    replies: bool,
    comment_raw: Option<String>,
    comment_id_raw: Option<String>,
    pages_raw: Option<String>,
    sort_raw: Option<String>,
    limit_raw: Option<String>,
) -> Result<FfiCommentsParams, FfiApiProtocolError> {
    let (comment_id, pages, sort, limit) = core::routes::validate_comments_params(
        replies,
        comment_raw.as_deref(),
        comment_id_raw.as_deref(),
        pages_raw.as_deref(),
        sort_raw.as_deref(),
        limit_raw.as_deref(),
    )?;
    Ok(FfiCommentsParams {
        comment_id,
        pages,
        sort,
        limit,
    })
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSearchParams {
    pub query: String,
    pub platform: String,
    pub only: Vec<String>,
    pub page: i32,
    pub pages: Option<i32>,
    pub kind: String,
    pub sort: String,
    pub limit: Option<i32>,
}

#[uniffi::export]
pub fn api_validate_search_params(
    q: Option<String>,
    platform: Option<String>,
    only: Option<String>,
    cursor: Option<String>,
    page_raw: Option<String>,
    pages_raw: Option<String>,
    kind_raw: Option<String>,
    sort_raw: Option<String>,
    limit_raw: Option<String>,
) -> Result<FfiSearchParams, FfiApiProtocolError> {
    let (query, platform, only, page, pages, kind, sort, limit) = core::routes::validate_search_params(
        q.as_deref(),
        platform.as_deref(),
        only.as_deref(),
        cursor.as_deref(),
        page_raw.as_deref(),
        pages_raw.as_deref(),
        kind_raw.as_deref(),
        sort_raw.as_deref(),
        limit_raw.as_deref(),
    )?;
    Ok(FfiSearchParams {
        query,
        platform,
        only,
        page,
        pages,
        kind,
        sort,
        limit,
    })
}

// ===========================================================================
// 协议渲染与输出
// ===========================================================================

#[uniffi::export]
pub fn api_render_capabilities(socket_path: String, platforms_json: String) -> String {
    let platform_json_arr = feedhub_core::sync::json::parse(&platforms_json)
        .unwrap_or(feedhub_core::sync::json::Value::Array(Vec::new()));
    core::protocol::render_capabilities(&socket_path, platform_json_arr)
}

#[uniffi::export]
pub fn api_render_platforms(platforms_json: String, requests: i32) -> String {
    let parsed: Vec<feedhub_core::sync::json::Value> = feedhub_core::sync::json::parse(&platforms_json)
        .ok()
        .and_then(|v| match v {
            feedhub_core::sync::json::Value::Array(arr) => Some(arr),
            _ => None,
        })
        .unwrap_or_default();
    core::protocol::render_platforms(&parsed, requests)
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPlatformErrorItem {
    pub platform: Option<String>,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub extra_json: Option<String>,
}

fn parse_extra_fields(extra_json: Option<&str>) -> Vec<(String, feedhub_core::sync::json::Value)> {
    match extra_json.and_then(|s| feedhub_core::sync::json::parse(s).ok()) {
        Some(feedhub_core::sync::json::Value::Object(fields)) => fields,
        _ => Vec::new(),
    }
}

#[uniffi::export]
pub fn api_render_error_line(
    code: String,
    platform: Option<String>,
    message: String,
    retryable: bool,
    extra_json: Option<String>,
    requests: Option<i32>,
) -> String {
    core::protocol::error_json_line(
        &code,
        platform.as_deref(),
        &message,
        retryable,
        parse_extra_fields(extra_json.as_deref()),
        requests,
    )
}

#[uniffi::export]
pub fn api_render_inline_errors(errors: Vec<FfiPlatformErrorItem>, requests: i32) -> String {
    let mut sb = String::new();
    for err in errors {
        sb.push_str(&core::protocol::error_json_line(
            &err.code,
            err.platform.as_deref(),
            &err.message,
            err.retryable,
            parse_extra_fields(err.extra_json.as_deref()),
            Some(requests),
        ));
        sb.push('\n');
    }
    sb
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiHttpHeader {
    pub name: String,
    pub value: String,
}

#[uniffi::export]
pub fn api_image_reply_headers(
    idx: i32,
    width: i32,
    height: i32,
    full_width: i32,
    full_height: i32,
    segments: i32,
    segment: i32,
    image_count: i32,
    requests: i32,
) -> Vec<FfiHttpHeader> {
    let h = core::routes::ImageReplyHeaders {
        idx,
        width,
        height,
        full_width,
        full_height,
        segments,
        segment,
        image_count,
        requests,
    };
    h.to_headers()
        .into_iter()
        .map(|(name, value)| FfiHttpHeader { name, value })
        .collect()
}

// ===========================================================================
// 图片模型、缓存与改写
// ===========================================================================

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiApiImage {
    pub url: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub is_cover: bool,
    pub note: Option<String>,
}

impl From<core::ApiImage> for FfiApiImage {
    fn from(img: core::ApiImage) -> Self {
        Self {
            url: img.url,
            width: img.width,
            height: img.height,
            is_cover: img.is_cover,
            note: img.note,
        }
    }
}

impl From<FfiApiImage> for core::ApiImage {
    fn from(img: FfiApiImage) -> Self {
        Self {
            url: img.url,
            width: img.width,
            height: img.height,
            is_cover: img.is_cover,
            note: img.note,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiContentImages {
    pub entries: Vec<FfiApiImage>,
}

impl From<core::ContentImages> for FfiContentImages {
    fn from(c: core::ContentImages) -> Self {
        Self {
            entries: c.entries.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<FfiContentImages> for core::ContentImages {
    fn from(c: FfiContentImages) -> Self {
        Self {
            entries: c.entries.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDetailImages {
    pub images: FfiContentImages,
    pub has_cover: bool,
}

impl From<core::DetailImages> for FfiDetailImages {
    fn from(d: core::DetailImages) -> Self {
        Self {
            has_cover: d.cover_index.is_some(),
            images: d.images.into(),
        }
    }
}

impl From<FfiDetailImages> for core::DetailImages {
    fn from(d: FfiDetailImages) -> Self {
        Self {
            cover_index: if d.has_cover { Some(0) } else { None },
            images: d.images.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiIndexSize {
    pub details: i32,
    pub comments: i32,
}

#[derive(uniffi::Object)]
pub struct FfiApiImageIndex {
    inner: core::index::ApiImageIndex,
}

#[uniffi::export]
impl FfiApiImageIndex {
    #[uniffi::constructor]
    pub fn new(detail_capacity: i32, comment_capacity: i32) -> Self {
        Self {
            inner: core::index::ApiImageIndex::new(
                detail_capacity as usize,
                comment_capacity as usize,
            ),
        }
    }

    pub fn detail(&self, platform: String, id: String) -> Option<FfiDetailImages> {
        self.inner.detail(&platform, &id).map(Into::into)
    }

    pub fn put_detail(&self, platform: String, id: String, images: FfiDetailImages) {
        self.inner.put_detail(&platform, &id, images.into());
    }

    pub fn comment(&self, platform: String, id: String, comment_id: String) -> Option<FfiContentImages> {
        self.inner.comment(&platform, &id, &comment_id).map(Into::into)
    }

    pub fn put_comment(&self, platform: String, id: String, comment_id: String, images: FfiContentImages) {
        self.inner.put_comment(&platform, &id, &comment_id, images.into());
    }

    pub fn size(&self) -> FfiIndexSize {
        let (d, c) = self.inner.size();
        FfiIndexSize {
            details: d as i32,
            comments: c as i32,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDetailJsonResult {
    pub rewritten_json: String,
    pub images: FfiDetailImages,
}

#[uniffi::export]
pub fn api_rewrite_detail_json(
    detail_json: String,
    original_images: bool,
) -> Result<FfiDetailJsonResult, FfiApiProtocolError> {
    let (rewritten_json, images) = core::detail_json::rewrite_detail_json(&detail_json, original_images)
        .map_err(|e| FfiApiProtocolError::Failure {
            code: "internal".to_string(),
            detail: e,
        })?;
    Ok(FfiDetailJsonResult {
        rewritten_json,
        images: images.into(),
    })
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiCommentImagePair {
    pub comment_id: String,
    pub images: FfiContentImages,
}

#[uniffi::export]
pub fn api_extract_comments_images(
    comments_json: String,
    original_images: bool,
) -> Vec<FfiCommentImagePair> {
    core::detail_json::extract_comments_images(&comments_json, original_images)
        .into_iter()
        .map(|(comment_id, images)| FfiCommentImagePair {
            comment_id,
            images: images.into(),
        })
        .collect()
}

// ===========================================================================
// 搜索混排与续页游标
// ===========================================================================

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMergedItemInput {
    pub item_key: String,
    pub item_json: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPlatformSliceInput {
    pub platform: String,
    pub items: Vec<FfiMergedItemInput>,
    pub offset: i32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPageSlicesInput {
    pub page: i32,
    pub slices: Vec<FfiPlatformSliceInput>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPlatformCursor {
    pub platform: String,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPageStartCursors {
    pub page: i32,
    pub cursors: Vec<FfiPlatformCursor>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMergedItemOutput {
    pub item_json: String,
    pub item_key: String,
    pub platform: String,
    pub page: i32,
    pub index: i32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSearchMergeOutput {
    pub shown: Vec<FfiMergedItemOutput>,
    pub truncated: bool,
    pub next_json: String,
}

#[uniffi::export]
pub fn api_merge_search_results(
    order: Vec<String>,
    slices_by_page: Vec<FfiPageSlicesInput>,
    limit: Option<i32>,
    next_by_platform: Vec<FfiPlatformCursor>,
    page_starts: Vec<FfiPageStartCursors>,
    first_page: i32,
    failed_platforms: Vec<String>,
) -> FfiSearchMergeOutput {
    let mut all_entries = Vec::new();
    for page_slice in slices_by_page {
        let mut buffers = HashMap::new();
        for s in page_slice.slices {
            buffers.insert(
                s.platform,
                core::routes::PlatformPageSlice {
                    items: s.items.into_iter().map(|it| (it.item_key, it.item_json)).collect(),
                    offset: s.offset,
                },
            );
        }
        all_entries.extend(core::routes::round_robin_merge(&buffers, &order, page_slice.page));
    }

    let mut unique_entries = Vec::new();
    let mut seen_keys = HashSet::new();
    for entry in all_entries {
        if seen_keys.insert(entry.item_key.clone()) {
            unique_entries.push(entry);
        }
    }

    let truncated = match limit {
        Some(lim) => unique_entries.len() > lim as usize,
        None => false,
    };

    let shown: Vec<core::routes::MergedItem> = match limit {
        Some(lim) => unique_entries.into_iter().take(lim as usize).collect(),
        None => unique_entries,
    };

    let mut next_plat = HashMap::new();
    for p in next_by_platform {
        next_plat.insert(p.platform, p.cursor);
    }

    let mut p_starts = HashMap::new();
    for ps in page_starts {
        let mut map = HashMap::new();
        for c in ps.cursors {
            map.insert(c.platform, c.cursor);
        }
        p_starts.insert(ps.page, map);
    }

    let failed_set: HashSet<String> = failed_platforms.into_iter().collect();

    let next_val = core::routes::build_search_done_next(
        &order,
        truncated,
        &shown,
        &next_plat,
        &p_starts,
        first_page,
        &failed_set,
    );

    let next_json = feedhub_core::sync::json::to_json_string(&next_val);

    FfiSearchMergeOutput {
        shown: shown
            .into_iter()
            .map(|it| FfiMergedItemOutput {
                item_json: it.item_json,
                item_key: it.item_key,
                platform: it.platform,
                page: it.page,
                index: it.index,
            })
            .collect(),
        truncated,
        next_json,
    }
}

