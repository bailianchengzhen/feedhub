//! 酷安**详情**（`/v6/feed/detail`）的解析，逐条对照 `source-coolapk/CoolapkParser.detail` +
//! `CoolapkVideo` 的详情那半边。
//!
//! 请求由 [`super::source::CoolapkHostedSource::detail`] 发（宿主回调），本模块一个 socket 都不碰。
//!
//! 正文有两种形状（Kotlin 用 `article != null` 分流）：
//! - **图文**（`feedType=feedArticle`）：`message_raw_output` 是一个**字符串形式的 JSON 数组**，
//!   里面按顺序排着 `text` / `image` / `shareUrl` 块；前面再放封面（`message_cover`）；
//! - **普通动态**：`message` 按 HTML 切成段落，后面接站外视频卡片（如果有）和 `picArr` 里的图片。
//!
//! 视频：酷安自己托管的（`*.coolapk.com` 的视频文件）进 [`FeedDetail::video`]（`partId=main`），
//! 站外视频（B站 / 微博 / 抖音……）是正文里的一张卡片（[`super::video::external_card`]）。

use super::emoji::EmoteTable;
use super::html;
use super::video;
use super::CoolapkError;
use crate::detail::{Block, FeedDetail, RichText, Span};
use crate::json::{self, JsonMap};

/// 详情。`resolved_media` 是 `b23.tv` 短链跟完重定向后的地址（短链才需要，其余传 `None`）。
pub fn detail(
    json: &JsonMap,
    resolved_media: Option<&str>,
    emotes: &[(String, String)],
) -> Result<FeedDetail, CoolapkError> {
    let table = EmoteTable::new(emotes);
    let data = json::obj(json, "data").ok_or_else(|| CoolapkError::Parse {
        detail: "详情缺少 data".to_string(),
    })?;
    let item = super::feed(data, &table, 0).ok_or_else(|| CoolapkError::Parse {
        detail: format!("详情里没有可用的内容（id={}）", display_id(data)),
    })?;

    let article = article_blocks(data, &table);
    let hosted = video::hosted(data);
    let external = video::external_card(data, resolved_media);
    let blocks = match article {
        Some(article) => {
            let mut blocks: Vec<Block> = Vec::new();
            if let Some(cover) = json::text(data, "message_cover") {
                blocks.push(html::detail_image(&cover));
            }
            blocks.extend(article);
            blocks
        }
        None => {
            let mut blocks =
                html::to_detail_blocks(&json::text(data, "message").unwrap_or_default(), &table);
            blocks.extend(external);
            blocks.extend(pictures(data));
            blocks
        }
    };
    Ok(FeedDetail {
        item,
        body: RichText { blocks },
        video: video::video_info(hosted.as_ref()),
        follow: None,
        related: Vec::new(),
    })
}

/// `CoolapkParser.articleBlocks`：`message_raw_output` 里的块。不是数组 / 空数组就是 `None`。
fn article_blocks(o: &JsonMap, table: &EmoteTable) -> Option<Vec<Block>> {
    let raw = json::text(o, "message_raw_output")?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let arr = value.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut out: Vec<Block> = Vec::new();
    for entry in arr {
        let Some(b) = entry.as_object() else {
            continue;
        };
        match json::string(b, "type").as_deref() {
            Some("text") => {
                out.extend(html::to_detail_blocks(
                    &json::string(b, "message").unwrap_or_default(),
                    table,
                ));
            }
            Some("image") => {
                if let Some(url) = json::text(b, "url") {
                    out.push(html::detail_image(&url));
                }
                if let Some(description) = json::text(b, "description") {
                    out.push(Block::Paragraph {
                        spans: vec![Span::Text {
                            text: crate::hosted::kotlin_trim(&description).to_string(),
                            bold: false,
                            italic: false,
                        }],
                    });
                }
            }
            Some("shareUrl") => {
                if let Some(url) = json::text(b, "url") {
                    let title = json::text(b, "title").unwrap_or_else(|| url.clone());
                    out.push(Block::Paragraph {
                        spans: vec![Span::Link {
                            text: title,
                            url,
                            target: None,
                        }],
                    });
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// `picArr`（没有就退回 `pic`）→ 图片块。和 Kotlin 一样：去重保序、统一 https、带尺寸。
fn pictures(o: &JsonMap) -> Vec<Block> {
    super::pictures(o)
        .into_iter()
        .map(|(url, width, height)| Block::Image {
            url,
            width,
            height,
            original: None,
            caption: None,
        })
        .collect()
}

/// Kotlin 字符串模板里 `${data.id()}` 的样子（`id()` 自己会 trim 并且把 `0` 当没有）。
fn display_id(o: &JsonMap) -> String {
    super::id(o).unwrap_or_else(|| "null".to_string())
}

