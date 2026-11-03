//! **评论区展示规则与批量预计算**。
//!
//! 对应 Kotlin 原版 `app/src/commonMain/kotlin/feedhub/app/detail/Comments.kt` 里的非 UI 文本 / 判定规则：
//! - 匿名作者兜底 `"匿名"`、`isPoster` 判定；
//! - 元信息 `meta` 行拼装（`floor` + `commentTime` + `ipLocation`，使用 [`crate::html::kotlin_trim`] 处理空白）；
//! - 楼中楼预览折叠判定、更多提示文案、楼中楼不支持提示；
//! - **批量 FFI 预算**：一整页评论一次性预计算好全部展示文本（点赞数、时间、IP、楼主标签、预览文本等），
//!   使得单条 `CommentItem` 重组时 0 次跨边界 FFI 调用！

use crate::model::TimePrecision;
use crate::present::text::{comment_time, compact_count};

/// 一条评论的原始输入数据（用于批量预计算）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentBatchInput {
    pub id: String,
    pub author_id: Option<String>,
    pub author_name: Option<String>,
    pub likes: Option<i64>,
    pub floor: Option<i64>,
    pub published_at: Option<i64>,
    pub published_at_precision: Option<TimePrecision>,
    pub ip_location: Option<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_name: Option<String>,
    pub reply_count: Option<i64>,
    pub preview_replies: Vec<PreviewReplyInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewReplyInput {
    pub author_id: Option<String>,
    pub author_name: Option<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_name: Option<String>,
}

/// 一条评论预计算好的展示字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentDisplayItem {
    pub id: String,
    pub author_name: String,
    pub is_poster: bool,
    pub likes_text: Option<String>,
    pub meta_text: String,
    pub reply_to_name: Option<String>,
    pub has_previews: bool,
    pub more_text: Option<String>,
    pub previews: Vec<PreviewReplyDisplayItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewReplyDisplayItem {
    pub author_name: String,
    pub is_poster: bool,
    pub reply_to_name: Option<String>,
}

/// 评论标题行文案（例如 `"评论"` 或 `"评论 1.2万"`）。
pub fn comments_header_title(total: Option<i64>, dot: char) -> String {
    match total {
        Some(tot) => format!("评论 {}", compact_count(tot, dot)),
        None => "评论".to_string(),
    }
}

/// 是否为正文作者。
pub fn is_poster(poster_id: Option<&str>, author_id: Option<&str>) -> bool {
    match (poster_id, author_id) {
        (Some(p), Some(a)) => p == a,
        _ => false,
    }
}

/// 作者名（兜底 `"匿名"`，对齐 Kotlin `author?.name ?: "匿名"`，不吞空串/空白）。
pub fn author_name_or_anonymous(name: Option<&str>) -> String {
    name.unwrap_or("匿名").to_string()
}

/// 点赞数字符串（`<= 0` 或 `None` 时返回 `None`）。
pub fn likes_text(likes: Option<i64>, dot: char) -> Option<String> {
    likes.and_then(|l| {
        if l > 0 {
            Some(compact_count(l, dot))
        } else {
            None
        }
    })
}

/// 元信息行拼装（`floor` `"${it}楼"` + `commentTime` + `ipLocation`，以 `" · "` 连接）。
///
/// **关键口径**：
/// - IP 属地空白判定必须使用 [`crate::html::kotlin_trim`]（对齐 Kotlin `isNotBlank()` 含 NBSP）；
/// - 若已包含 `"IP"` 字样则原样显示，否则拼上 `"IP属地：$it"`。
pub fn comment_meta_text(
    floor: Option<i64>,
    time_text: Option<&str>,
    ip_location: Option<&str>,
) -> String {
    let mut parts = Vec::with_capacity(3);

    if let Some(fl) = floor {
        parts.push(format!("{fl}楼"));
    }

    if let Some(tt) = time_text {
        if !crate::html::kotlin_trim(tt).is_empty() {
            parts.push(tt.to_string());
        }
    }

    if let Some(ip) = ip_location {
        let trimmed = crate::html::kotlin_trim(ip);
        if !trimmed.is_empty() {
            if ip.contains("IP") {
                parts.push(ip.to_string());
            } else {
                parts.push(format!("IP属地：{ip}"));
            }
        }
    }

    parts.join(" · ")
}

/// 楼中楼预览底部的更多提示文案。
pub fn replies_more_text(
    total: Option<i64>,
    previews_count: usize,
    on_open_replies_allowed: bool,
    platform_label: Option<&str>,
) -> Option<String> {
    if !on_open_replies_allowed {
        if let Some(tot) = total {
            if tot > previews_count as i64 {
                let who = match platform_label {
                    Some(label) => format!("{label} 没有开放楼中楼"),
                    None => "这个平台没有开放楼中楼".to_string(),
                };
                Some(format!("共 {tot} 条回复 · {who}"))
            } else {
                None
            }
        } else {
            None
        }
    } else if let Some(tot) = total {
        if tot > previews_count as i64 {
            Some(format!("共 {tot} 条回复 >"))
        } else {
            None
        }
    } else if previews_count > 0 {
        Some("查看全部回复 >".to_string())
    } else {
        None
    }
}

/// 是否显示楼中楼预览区。
pub fn has_replies_preview(previews_count: usize, total: Option<i64>) -> bool {
    previews_count > 0 || total.unwrap_or(0) > 0
}

/// 楼中楼面板不支持时的提示文案。
pub fn replies_unsupported_text(platform_label: &str, plugin_installed: bool) -> String {
    if !plugin_installed {
        format!("{platform_label} 的插件没装，看不了楼中楼")
    } else {
        format!("{platform_label} 没有开放楼中楼（平台限制，不是这边出错），只能看一级评论")
    }
}

/// 一整页评论批量预计算。
pub fn prepare_comment_batch(
    comments: &[CommentBatchInput],
    poster_id: Option<&str>,
    reply_to_root_author_id: Option<&str>,
    now_ms: i64,
    offset_sec: i64,
    dot: char,
    on_open_replies_allowed: bool,
    platform_label: Option<&str>,
) -> Vec<CommentDisplayItem> {
    let mut out = Vec::with_capacity(comments.len());

    for c in comments {
        let is_post = is_poster(poster_id, c.author_id.as_deref());
        let author_str = author_name_or_anonymous(c.author_name.as_deref());
        let l_text = likes_text(c.likes, dot);

        let time_str = c.published_at.map(|t| {
            let prec = c.published_at_precision.unwrap_or(TimePrecision::Second);
            comment_time(t, now_ms, prec, offset_sec, offset_sec)
        });

        let meta = comment_meta_text(c.floor, time_str.as_deref(), c.ip_location.as_deref());

        let reply_to = if let Some(ref reply_id) = c.reply_to_id {
            if Some(reply_id.as_str()) != reply_to_root_author_id {
                c.reply_to_name.clone()
            } else {
                None
            }
        } else {
            None
        };

        let previews_take3: Vec<_> = c.preview_replies.iter().take(3).collect();
        let has_prev = has_replies_preview(previews_take3.len(), c.reply_count);
        let more = replies_more_text(
            c.reply_count,
            previews_take3.len(),
            on_open_replies_allowed,
            platform_label,
        );

        let preview_items: Vec<PreviewReplyDisplayItem> = previews_take3
            .into_iter()
            .map(|pr| PreviewReplyDisplayItem {
                author_name: author_name_or_anonymous(pr.author_name.as_deref()),
                is_poster: is_poster(poster_id, pr.author_id.as_deref()),
                reply_to_name: pr.reply_to_name.clone(),
            })
            .collect();

        out.push(CommentDisplayItem {
            id: c.id.clone(),
            author_name: author_str,
            is_poster: is_post,
            likes_text: l_text,
            meta_text: meta,
            reply_to_name: reply_to,
            has_previews: has_prev,
            more_text: more,
            previews: preview_items,
        });
    }

    out
}

