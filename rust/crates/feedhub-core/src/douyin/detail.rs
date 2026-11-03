//! 抖音详情（`/aweme/v1/web/aweme/detail/` → `FeedDetail`）。
//!
//! 逐行照抄 `source-douyin/DouyinDetail.kt`（`detail` / `notFound` / `body` / `image` / `isAd`）与
//! `DouyinVideo.info` 的**详情那一小半**。
//!
//! 两个口径上的坑：
//!
//! - 响应里的作品在 `aweme_detail`（**对象**，不像推荐流那样是一段 JSON 字符串），作品本身的映射
//!   复用 [`crate::douyin::item`]，这样详情里的 `id` / `key` 和信息流里同一条一致（收藏 / 历史按 key 记）。
//! - 作品看不到时服务端给 `filter_detail` + `status_msg`（已删除 / 私密 / 需要登录）：把平台的话原样带出来，
//!   抛 `Api`（code `no_aweme`），**不要假装是解析失败**（[`not_found`] 和 `DouyinSource.play` 共用这一份）。
//!
//! 正文 = `desc` 一个段落（带文字表情）+ 图文作品的 `images[]`。`text_extra` 里的 `#话题` 现在**只是文字**：
//! 站内话题页我们不做原生详情，官方话题页的地址形态也没有实测出处，所以不编 `Span::Link`。

use crate::detail::{Block, FeedDetail, RichText, VideoInfo};
use crate::douyin::DouyinError;
use crate::json::{self, JsonMap};

/// 详情 id 的网页地址（`DouyinParser.videoUrl`；和 `DouyinSource.fetchAweme` 的 Referer 是同一个）。
pub fn video_url(aweme_id: &str) -> String {
    crate::douyin::video_url(aweme_id)
}

/// 解析已经解析成对象的响应。`emotes` 是表情表（文字代码 → 图片地址），由 Kotlin 传进来。
pub fn parse(
    json: &JsonMap,
    id: &str,
    emotes: &[(String, String)],
) -> Result<FeedDetail, DouyinError> {
    let aweme = json::obj(json, "aweme_detail").ok_or_else(|| not_found(json, id))?;
    let item = crate::douyin::item(aweme, emotes).ok_or_else(|| DouyinError::Parse {
        detail: format!("aweme/detail 里的作品映射不出条目（id={id}）"),
    })?;
    Ok(FeedDetail {
        item,
        body: body(aweme, emotes),
        video: video_info(aweme),
        // 关注 / 取关当前不做，所以不给 FollowState
        follow: None,
        related: Vec::new(),
    })
}

/// 从响应体直接解析。
pub fn parse_body(
    body: &str,
    id: &str,
    emotes: &[(String, String)],
) -> Result<FeedDetail, DouyinError> {
    let json = json::parse_object(body).map_err(|detail| DouyinError::Parse { detail })?;
    parse(&json, id, emotes)
}

/// 作品看不到时的错误（带平台自己给的原因）。
pub fn not_found(json: &JsonMap, id: &str) -> DouyinError {
    let filter = json::obj(json, "filter_detail");
    let reason = filter
        .and_then(|f| json::text(f, "detail_msg"))
        .or_else(|| filter.and_then(|f| json::text(f, "reason")))
        .or_else(|| json::text(json, "status_msg"));
    let detail = match reason.filter(|r| !r.trim().is_empty()) {
        Some(reason) => format!("拿不到作品 {id} 的详情：{reason}"),
        None => format!("拿不到作品 {id} 的详情"),
    };
    DouyinError::Api {
        code: "no_aweme".to_string(),
        detail,
    }
}

/// 判断要不要取表情表用的拼串：详情就是 `aweme_detail.desc`（Kotlin `DouyinSource.detail` 里那一行）。
pub fn texts(json: &JsonMap) -> String {
    json::obj(json, "aweme_detail")
        .and_then(|aweme| json::text(aweme, "desc"))
        .unwrap_or_default()
}

/// 详情正文（`desc` 一个段落 + 图文作品的图片）。
pub fn body(aweme: &JsonMap, emotes: &[(String, String)]) -> RichText {
    let mut blocks: Vec<Block> = Vec::new();
    if let Some(desc) = json::text(aweme, "desc").filter(|d| !d.trim().is_empty()) {
        blocks.push(Block::Paragraph {
            spans: super::text::split_emotes(&desc, emotes),
        });
    }
    for value in json::arr(aweme, "images").into_iter().flatten() {
        if let Some(image) = value.as_object().and_then(image_block) {
            blocks.push(image);
        }
    }
    RichText { blocks }
}

fn image_block(o: &JsonMap) -> Option<Block> {
    let urls: Vec<String> = json::arr(o, "url_list")
        .map(|list| {
            list.iter()
                .filter_map(json::primitive_content)
                .filter(|u| !u.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();
    let url = urls.first()?;
    Some(Block::Image {
        url: json::to_https(url),
        width: json::int(o, "width"),
        height: json::int(o, "height"),
        // 第二张通常是原图 / 无损图（`url_list` 里前两张同尺寸不同清晰度）
        original: urls.get(1).map(|u| json::to_https(u)),
        caption: None,
    })
}

/// 详情页的 `FeedDetail.video`：**和播放共用一份**流挑选（[`super::play::info`]）。
///
/// `info` 和 `play` 必须挑同一批流（否则"详情说有视频、
/// 点播放却说没有地址"这种漂移会出现），所以这一份实现搬到了 [`super::play::info`]。
pub fn video_info(aweme: &JsonMap) -> Option<VideoInfo> {
    super::play::info(aweme)
}

