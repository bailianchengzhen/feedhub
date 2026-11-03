//! **桌面对外接口的协议常量 + 取图决策**。
//!
//! ## 进 Rust 的
//!
//! - **错误码表**：[`ErrorCode`]（11 个）→ HTTP 状态码 + 能不能重试。这是**稳定的接口**（agent 按 `error`
//!   分支，状态码只是粗分类），所以"哪一类错对应哪个码 + 什么状态码"只能有一份。
//! - **异常 → 错误码**的归类：[`classify`]（原 `Throwable.toApiFailure()`）：风控 / 没登录 / 不支持 /
//!   签名 / 接口错 / 解析错 / 网络 / 别的。
//! - **图片几何**：[`ImageGeometry`]（缩放、长图阈值、分段方案）——纯数学，长图按
//!   "每段高 ≈ 宽 × 1.5、段间重叠段高的 8%"切。
//! - **一条内容的图片清单**：[`plan_detail_images`] / [`plan_comment_images`]（封面的挑选、
//!   正文 `img` 块 → `idx`、`original` 还是 `url`）。
//!
//! ## 留在宿主的
//!
//! - **unix socket 的监听循环 / HTTP 子集**（`UnixHttpServer`）——操作系统的活，这一批不动
//!   （桌面去 JVM 时再搬）；
//! - **Coil 取图**（`ApiImageService.render`）：真正发请求 / 读磁盘缓存 / 解码 / 编码 JPEG 是 Coil + Skia
//!   的调用；Rust 只给"裁哪一段、缩到多宽"的几何；
//! - **JSON 编解码与路由分发**（`ApiRoutes` / `apiProtocol` 的 record）：那是 `FeedDetail` / `Comment`
//!   的序列化，格式的真相在 Kotlin 的 `@Serializable`。
//!
//! ## 一张图（[`ApiImage`]）
//!
//! `url` **只用来取图，任何输出里都不出现**（带签名的地址）。对外只有 `idx` / `isCover` / `width` /
//! `height` / `segments` / `note`。

use crate::detail::{Block, FeedDetail};
use crate::model::Media;

/// 输出格式的版本（`ApiProtocol.SCHEMA_VERSION`）。
pub const SCHEMA_VERSION: i64 = 1;

// ===========================================================================
// 错误码
// ===========================================================================

/// 一个对外的错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCode {
    /// 稳定的字符串（`"bad_input"` / `"risk_control"` …）。
    pub code: &'static str,
    /// HTTP 状态码（粗分类）。
    pub http_status: u16,
    /// agent 能不能重试（`retryable`）。
    pub retryable: bool,
}

/// 参数字面量（和 Kotlin `ApiError` 的 `code` **逐字一样**，agent 按它分支）。
pub const ERROR_BAD_INPUT: ErrorCode = ErrorCode {
    code: "bad_input",
    http_status: 400,
    retryable: false,
};
pub const ERROR_NOT_LOGGED_IN: ErrorCode = ErrorCode {
    code: "not_logged_in",
    http_status: 401,
    retryable: false,
};
pub const ERROR_RISK_CONTROL: ErrorCode = ErrorCode {
    code: "risk_control",
    http_status: 403,
    retryable: false,
};
pub const ERROR_CAPTCHA_REQUIRED: ErrorCode = ErrorCode {
    code: "captcha_required",
    http_status: 409,
    retryable: false,
};
pub const ERROR_PROXY_UNAVAILABLE: ErrorCode = ErrorCode {
    code: "proxy_unavailable",
    http_status: 503,
    retryable: false,
};
pub const ERROR_NOT_SUPPORTED: ErrorCode = ErrorCode {
    code: "not_supported",
    http_status: 404,
    retryable: false,
};
pub const ERROR_NOT_FOUND: ErrorCode = ErrorCode {
    code: "not_found",
    http_status: 404,
    retryable: false,
};
pub const ERROR_NETWORK: ErrorCode = ErrorCode {
    code: "network",
    http_status: 504,
    retryable: true,
};
pub const ERROR_PLATFORM_ERROR: ErrorCode = ErrorCode {
    code: "platform_error",
    http_status: 502,
    retryable: false,
};
pub const ERROR_BUSY: ErrorCode = ErrorCode {
    code: "busy",
    http_status: 503,
    retryable: true,
};
pub const ERROR_INTERNAL: ErrorCode = ErrorCode {
    code: "internal",
    http_status: 500,
    retryable: false,
};

/// 全部错误码，**按 Kotlin `ApiError` 的声明顺序**（`/capabilities` 里的 `errorCodes` 就按这个顺序列）。
pub const ERROR_CODES: [ErrorCode; 11] = [
    ERROR_BAD_INPUT,
    ERROR_NOT_LOGGED_IN,
    ERROR_RISK_CONTROL,
    ERROR_CAPTCHA_REQUIRED,
    ERROR_PROXY_UNAVAILABLE,
    ERROR_NOT_SUPPORTED,
    ERROR_NOT_FOUND,
    ERROR_NETWORK,
    ERROR_PLATFORM_ERROR,
    ERROR_BUSY,
    ERROR_INTERNAL,
];

/// 按字符串找一个错误码；不认识就是 `None`（Kotlin `ApiError.ofKey`）。
pub fn error_of_key(key: &str) -> Option<ErrorCode> {
    ERROR_CODES.iter().copied().find(|e| e.code == key)
}

/// 一次失败的**种类**（原 Kotlin 的 `Throwable.toApiFailure()` 分支，只是不带异常对象）。
///
/// 宿主把异常翻成这个 enum 再调 [`classify`]：这样"哪一类错对应哪个码"只有一份，
/// 而 Kotlin 那边的 `is RiskControlException -> …` 那串 `when` 只是"异常 → 种类"的映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// 风控（403 / 412 / 429、平台的风控错误码）。
    RiskControl,
    /// 没登录 / 登录失效。
    NotLoggedIn,
    /// 平台 / 插件没这个能力。
    NotSupported,
    /// 平台要人机验证（验证只能用户本人在 App 里做）。
    CaptchaRequired,
    /// **fail-closed**：这个平台要走代理，但这次没有可用的代理。
    ProxyUnavailable,
    /// 平台回了错误码 / 签名被拒。
    PlatformError,
    /// 响应解析不了 / JSON 坏了。
    Parse,
    /// 网络层失败（连不上 / 超时 / DNS）。
    Network,
    /// 内容 / 图片不存在。
    NotFound,
    /// 别的意外（App 的 bug）。
    Internal,
}

/// 种类 → 错误码（`retryable` 跟着码走）。
///
/// **和 Kotlin 那张表一致**：`Signature` / `Api` / `Parse` 都归 `platform_error`（502，别重试）；
/// `IOException` 归 `network`（504，可重试）；`UnsupportedFeed` 归 `not_supported`。
pub fn classify(kind: FailureKind) -> ErrorCode {
    match kind {
        FailureKind::RiskControl => ERROR_RISK_CONTROL,
        FailureKind::NotLoggedIn => ERROR_NOT_LOGGED_IN,
        FailureKind::NotSupported => ERROR_NOT_SUPPORTED,
        FailureKind::CaptchaRequired => ERROR_CAPTCHA_REQUIRED,
        FailureKind::ProxyUnavailable => ERROR_PROXY_UNAVAILABLE,
        FailureKind::PlatformError | FailureKind::Parse => ERROR_PLATFORM_ERROR,
        FailureKind::Network => ERROR_NETWORK,
        FailureKind::NotFound => ERROR_NOT_FOUND,
        FailureKind::Internal => ERROR_INTERNAL,
    }
}

// ===========================================================================
// 图片几何（Kotlin `ImageGeometry`）
// ===========================================================================

/// `maxw` 的默认值。
pub const DEFAULT_MAX_WIDTH: i32 = 1024;
/// `maxw` 的上限（再大就不像"给 agent 看的缩略图"了）。
pub const MAX_MAX_WIDTH: i32 = 2048;
/// 高 > 宽 × 3 = 长图。
pub const LONG_RATIO: i32 = 3;

/// 每段高 ≈ 缩放后宽 × 3/2。
const SEGMENT_HEIGHT_NUM: i32 = 3;
const SEGMENT_HEIGHT_DEN: i32 = 2;
/// 段间重叠：段高的 8%（至少 1 像素）。
const OVERLAP_PERCENT: i32 = 8;
/// 窄于这个宽度不分段（段高太小，切出来的段没意义）。
const MIN_SEGMENT_WIDTH: i32 = 32;

/// 图片几何：缩放 + 长图分段（原 `ImageGeometry`）。纯函数，离线可测。
pub struct ImageGeometry;

impl ImageGeometry {
    /// 缩放后的宽高：宽缩到 `max_width`（**不放大**），高按比例；至少 1。
    ///
    /// 四舍五入（`(h × tw + w/2) / w`，和 Kotlin 一样），高至少 1。
    pub fn scaled(width: i32, height: i32, max_width: i32) -> (i32, i32) {
        let w = width.max(1);
        let h = height.max(1);
        if w <= max_width {
            return (w, h);
        }
        let tw = max_width.max(1);
        let th = ((h as i64 * tw as i64 + w as i64 / 2) / w as i64) as i32;
        (tw, th.max(1))
    }

    /// 这张（已经缩过的）图要不要分段：高 > 宽 × 3，而且不算太窄。
    pub fn is_long(width: i32, height: i32) -> bool {
        width >= MIN_SEGMENT_WIDTH && height as i64 > width as i64 * LONG_RATIO as i64
    }

    /// 分段方案（对**缩放之后**的宽高算）。
    ///
    /// `count == 1` 表示不分段（这时 `segment_height` = 整张高、`overlap` = 0）。
    pub fn segments(width: i32, height: i32) -> Segments {
        let w = width.max(1);
        let h = height.max(1);
        if !Self::is_long(w, h) {
            return Segments {
                width: w,
                height: h,
                segment_height: h,
                overlap: 0,
                count: 1,
            };
        }
        let seg_h = ((w as i64 * SEGMENT_HEIGHT_NUM as i64 + SEGMENT_HEIGHT_DEN as i64 / 2)
            / SEGMENT_HEIGHT_DEN as i64) as i32;
        let seg_h = seg_h.max(1);
        let overlap = ((seg_h as i64 * OVERLAP_PERCENT as i64 / 100) as i32).clamp(1, seg_h - 1);
        let step = (seg_h - overlap).max(1);
        if h <= seg_h {
            return Segments {
                width: w,
                height: h,
                segment_height: h,
                overlap: 0,
                count: 1,
            };
        }
        let count = 1 + ceil_div(h - seg_h, step);
        Segments {
            width: w,
            height: h,
            segment_height: seg_h,
            overlap,
            count,
        }
    }
}

fn ceil_div(a: i32, b: i32) -> i32 {
    (a + b - 1) / b
}

/// 一张图的分段方案。`top(i)` 是第 `i` 段从缩放后的第几行开始，`height_at(i)` 是它有多高。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segments {
    pub width: i32,
    pub height: i32,
    pub segment_height: i32,
    pub overlap: i32,
    pub count: i32,
}

impl Segments {
    /// 第 `index` 段在缩放后图片里的起始行。
    pub fn top(&self, index: i32) -> i32 {
        if index <= 0 || self.count <= 1 {
            return 0;
        }
        let step = (self.segment_height - self.overlap).max(1);
        (index * step).clamp(0, (self.height - 1).max(0))
    }

    /// 第 `index` 段有多少行（最后一段可能短）。
    pub fn height_at(&self, index: i32) -> i32 {
        if self.count <= 1 {
            return self.height;
        }
        (self.height - self.top(index)).clamp(1, self.segment_height)
    }
}

// ===========================================================================
// 一条内容的图片清单（Kotlin `ApiImagePlan`）
// ===========================================================================

/// 视频帖那条"封面"块的说明：它是一张图，**不是**视频画面。
pub const VIDEO_COVER_NOTE: &str = "这是视频的封面图，不是视频画面本身（视频要在 App 里看）";

/// 一张图（服务端内部用）。`url` **只用来取图，任何输出里都不出现**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiImage {
    pub url: String,
    /// 平台（或封面）声明的像素宽，可能没有。
    pub width: Option<i32>,
    pub height: Option<i32>,
    /// 封面 / 首图。
    pub is_cover: bool,
    /// 附加说明（只有视频封面有）。
    pub note: Option<String>,
}

/// 一条详情 / 一条评论的图片清单（`entries` 的下标就是对外的 `idx`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentImages {
    pub entries: Vec<ApiImage>,
}

impl ContentImages {
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    pub fn at(&self, idx: usize) -> Option<&ApiImage> {
        self.entries.get(idx)
    }

    /// 这张图要分几段（按默认 1024 宽缩放后算；比例不变，所以段数和 `maxw` 基本无关）。
    ///
    /// 宽高拿不到 → `None`（能算就是 ≥ 1）。
    pub fn segments_at(&self, idx: usize) -> Option<i32> {
        let image = self.at(idx)?;
        let w = image.width.filter(|w| *w > 0)?;
        let h = image.height.filter(|h| *h > 0)?;
        let (tw, th) = ImageGeometry::scaled(w, h, DEFAULT_MAX_WIDTH);
        Some(ImageGeometry::segments(tw, th).count)
    }
}

/// 一条**详情**的图片清单：`images` + 视频帖要补在最前面的封面块 `cover_block`（是 `images` 里的第 0 项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailImages {
    pub images: ContentImages,
    /// 封面（`None` = 这条没有封面，正文第一张就是"首图"）。
    pub cover_index: Option<usize>,
}

impl DetailImages {
    /// 正文里**第几个** `img` 块（0 起，只数 `img`，不是 `blocks` 的下标）→（`idx`, 那张图）。
    ///
    /// 封面在 `entries[0]`、正文图从 1 开始（有封面时）；没有封面时正文图从 0 开始。
    pub fn image_for_img_block(&self, img_ordinal: usize) -> Option<(usize, &ApiImage)> {
        let start = if self.cover_index.is_some() { 1 } else { 0 };
        let idx = start + img_ordinal;
        let image = self.images.at(idx)?;
        Some((idx, image))
    }
}

/// 一条详情里的图片清单（按对外的 `idx` 顺序）。
///
/// `original_images` = 设置里"默认显示原图"：和 App 画图时选地址的规则一样
/// （`Block.Image.original ?: url`），这样 `/image` 命中的就是 App 已经缓存过的那一份。
pub fn plan_detail_images(detail: &FeedDetail, original_images: bool) -> DetailImages {
    let body_images: Vec<&Block> = detail.body.blocks.iter().filter(|b| matches!(b, Block::Image { .. })).collect();
    let body_urls: Vec<&str> = body_images
        .iter()
        .flat_map(|b| match b {
            Block::Image { url, original, .. } => {
                vec![Some(url.as_str()), original.as_deref()]
            }
            _ => Vec::new(),
        })
        .flatten()
        .collect();
    // 视频封面：`media` 里第一个带 cover 的 Video
    let video_cover = detail.item.media.iter().find_map(|m| match m {
        Media::Video {
            cover: Some(cover),
            width,
            height,
            ..
        } if !crate::hosted::is_kotlin_blank(cover) => Some((cover.clone(), *width, *height)),
        _ => None,
    });
    // 正文一张图都没有的图文帖：第一张 media 图就是它的首图
    let first_media_image = detail.item.media.iter().find_map(|m| match m {
        Media::Image { url, width, height } if !crate::hosted::is_kotlin_blank(url) => Some((url.clone(), *width, *height)),
        _ => None,
    });
    // 封面：视频封面优先（正文里已经有同一个地址就不补，小红书那种"正文 = 图片"）；
    // 没有视频再看"正文一张图都没有的图文帖"的第一张 media 图（那就是它的首图）。
    let cover: Option<ApiImage> = if let Some((url, w, h)) = video_cover {
        if body_urls.contains(&url.as_str()) {
            None
        } else {
            Some(ApiImage {
                url,
                width: w,
                height: h,
                is_cover: true,
                note: Some(VIDEO_COVER_NOTE.to_string()),
            })
        }
    } else if body_images.is_empty() {
        first_media_image.map(|(url, w, h)| ApiImage {
            url,
            width: w,
            height: h,
            is_cover: true,
            note: None,
        })
    } else {
        None
    };
    let cover_index = cover.as_ref().map(|_| 0);
    let mut entries: Vec<ApiImage> = Vec::with_capacity(body_images.len() + 1);
    if let Some(cover) = cover {
        entries.push(cover);
    }
    for (n, block) in body_images.iter().enumerate() {
        if let Block::Image {
            url,
            original,
            width,
            height,
            ..
        } = block
        {
            entries.push(ApiImage {
                url: image_url_of(url, original.as_deref(), original_images),
                width: *width,
                height: *height,
                // 没有封面时，正文第一张就是"首图"
                is_cover: cover_index.is_none() && n == 0,
                note: None,
            });
        }
    }
    DetailImages {
        images: ContentImages { entries },
        cover_index,
    }
}

/// 一条评论里的图片清单（`/image?comment=` 用；`idx` 是**这条评论里**的序号）。
pub fn plan_comment_images(comment_blocks: &[Block], original_images: bool) -> ContentImages {
    let entries = comment_blocks
        .iter()
        .filter_map(|b| match b {
            Block::Image {
                url,
                original,
                width,
                height,
                ..
            } => Some(ApiImage {
                url: image_url_of(url, original.as_deref(), original_images),
                width: *width,
                height: *height,
                is_cover: false,
                note: None,
            }),
            _ => None,
        })
        .collect();
    ContentImages { entries }
}

/// 正文图该请求哪个地址：和 App 的 `RichTextView` 一样（"默认显示原图"开着就用 `original`）。
pub fn image_url_of(url: &str, original: Option<&str>, original_images: bool) -> String {
    if original_images {
        original
            .filter(|s| !crate::hosted::is_kotlin_blank(s))
            .map(|s| s.to_string())
            .unwrap_or_else(|| url.to_string())
    } else {
        url.to_string()
    }
}

pub mod cursor;
pub mod detail_json;
pub mod index;
pub mod protocol;
pub mod routes;
pub mod since;

pub use cursor::{decode as decode_resume_cursor, encode as encode_resume_cursor, DecodedCursor};
pub use detail_json::{extract_comments_images, rewrite_detail_json};
pub use index::ApiImageIndex;
pub use protocol::{
    error_json_line, json_line, render_capabilities, render_platforms, DEFAULT_LIBRARY_LIMIT,
    JPEG_QUALITY, MAX_COMMENT_SEARCH_PAGES, MAX_CONCURRENCY, MAX_LIBRARY_LIMIT, MAX_PAGE_LIMIT,
    PAGE_BUDGET, QUEUE_DEPTH,
};
pub use routes::{
    build_search_done_next, round_robin_merge, validate_comments_params, validate_image_params,
    validate_int_param, validate_library_limit, validate_page_limit, validate_search_params,
    ImageReplyHeaders, MergedItem, PlatformPageSlice,
};
pub use since::{parse_since, SinceError};

