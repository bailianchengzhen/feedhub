//! 路由参数校验、搜索混排与续页、评论与楼中楼分页、资料库过滤格式化。

use crate::appapi::cursor::encode as encode_cursor;
use crate::appapi::protocol::{MAX_LIBRARY_LIMIT, MAX_PAGE_LIMIT, PAGE_BUDGET};
use crate::appapi::{ERROR_BAD_INPUT, ErrorCode};
use crate::sync::json::Value;

// ===========================================================================
// 参数解析与校验
// ===========================================================================

pub fn validate_page_limit(raw: Option<&str>) -> Result<Option<i32>, (ErrorCode, String)> {
    let raw = match raw {
        Some(r) => r.trim(),
        None => return Ok(None),
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let limit = raw
        .parse::<i32>()
        .map_err(|_| (ERROR_BAD_INPUT, format!("limit 要是整数（收到「{raw}」）")))?;
    if limit < 1 {
        return Err((ERROR_BAD_INPUT, format!("limit 要 ≥ 1（收到 {limit}）")));
    }
    if limit > MAX_PAGE_LIMIT {
        return Err((
            ERROR_BAD_INPUT,
            format!("limit 最多 {MAX_PAGE_LIMIT}（收到 {limit}）；一次调用最多给一页，要更多请用 done.next 续页"),
        ));
    }
    Ok(Some(limit))
}

pub fn validate_library_limit(raw: Option<&str>) -> Result<i32, (ErrorCode, String)> {
    let raw = match raw {
        Some(r) => r.trim(),
        None => return Ok(crate::appapi::protocol::DEFAULT_LIBRARY_LIMIT),
    };
    if raw.is_empty() {
        return Ok(crate::appapi::protocol::DEFAULT_LIBRARY_LIMIT);
    }
    let limit = raw
        .parse::<i32>()
        .map_err(|_| (ERROR_BAD_INPUT, format!("limit 要是整数（收到「{raw}」）")))?;
    if limit < 1 {
        return Err((ERROR_BAD_INPUT, format!("limit 要 ≥ 1（收到 {limit}）")));
    }
    if limit > MAX_LIBRARY_LIMIT {
        return Err((
            ERROR_BAD_INPUT,
            format!("limit 最多 {MAX_LIBRARY_LIMIT}（收到 {limit}），要更多请分页查"),
        ));
    }
    Ok(limit)
}

pub fn validate_int_param(name: &str, raw: Option<&str>) -> Result<Option<i32>, (ErrorCode, String)> {
    let raw = match raw {
        Some(r) => r.trim(),
        None => return Ok(None),
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let val = raw
        .parse::<i32>()
        .map_err(|_| (ERROR_BAD_INPUT, format!("{name} 要是整数（收到「{raw}」）")))?;
    Ok(Some(val))
}

pub fn validate_image_params(
    idx_raw: Option<&str>,
    maxw_raw: Option<&str>,
    seg_raw: Option<&str>,
) -> Result<(i32, i32, Option<i32>), (ErrorCode, String)> {
    let idx = match idx_raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(raw) => raw
            .parse::<i32>()
            .map_err(|_| (ERROR_BAD_INPUT, format!("idx 要是整数（收到「{raw}」）")))?,
        None => {
            return Err((
                ERROR_BAD_INPUT,
                "缺少参数 idx（0 起，就是 /detail 里 img 块的 idx）".to_string(),
            ))
        }
    };
    if idx < 0 {
        return Err((ERROR_BAD_INPUT, format!("idx 从 0 起（收到 {idx}）")));
    }

    let maxw = match maxw_raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(raw) => raw
            .parse::<i32>()
            .map_err(|_| (ERROR_BAD_INPUT, format!("maxw 要是整数（收到「{raw}」）")))?,
        None => 1024,
    };
    if maxw < 1 {
        return Err((ERROR_BAD_INPUT, format!("maxw 要 ≥ 1（收到 {maxw}）")));
    }
    if maxw > 2048 {
        return Err((
            ERROR_BAD_INPUT,
            format!("maxw 最多 2048（收到 {maxw}）；要更大的图请在 App 里看"),
        ));
    }

    let seg = match seg_raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(raw) => {
            let s = raw
                .parse::<i32>()
                .map_err(|_| (ERROR_BAD_INPUT, format!("seg 要是整数（收到「{raw}」）")))?;
            if s < 0 {
                return Err((ERROR_BAD_INPUT, format!("seg 从 0 起（收到 {s}）")));
            }
            Some(s)
        }
        None => None,
    };

    Ok((idx, maxw, seg))
}

pub fn validate_comments_params(
    replies: bool,
    comment_raw: Option<&str>,
    comment_id_raw: Option<&str>,
    pages_raw: Option<&str>,
    sort_raw: Option<&str>,
    limit_raw: Option<&str>,
) -> Result<(Option<String>, i32, String, Option<i32>), (ErrorCode, String)> {
    let comment_id = if replies {
        let cid = comment_raw
            .or(comment_id_raw)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        match cid {
            Some(id) => Some(id.to_string()),
            None => return Err((ERROR_BAD_INPUT, "缺少参数 comment（一级评论 id）".to_string())),
        }
    } else {
        None
    };

    let pages = match pages_raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(raw) => raw
            .parse::<i32>()
            .map_err(|_| (ERROR_BAD_INPUT, format!("pages 要是整数（收到「{raw}」）")))?,
        None => 1,
    };
    if pages < 1 {
        return Err((ERROR_BAD_INPUT, "pages 要 ≥ 1".to_string()));
    }

    let sort = match sort_raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => {
            let lower = s.to_ascii_lowercase();
            if lower == "default" || lower == "hot" || lower == "newest" {
                lower
            } else {
                return Err((
                    ERROR_BAD_INPUT,
                    format!("未知的 sort「{s}」，可选：default, hot, newest"),
                ));
            }
        }
        None => "default".to_string(),
    };

    let limit = validate_page_limit(limit_raw)?;

    Ok((comment_id, pages, sort, limit))
}

pub fn validate_search_params(
    q: Option<&str>,
    platform: Option<&str>,
    only: Option<&str>,
    cursor: Option<&str>,
    page_raw: Option<&str>,
    pages_raw: Option<&str>,
    kind_raw: Option<&str>,
    sort_raw: Option<&str>,
    limit_raw: Option<&str>,
) -> Result<(String, String, Vec<String>, i32, Option<i32>, String, String, Option<i32>), (ErrorCode, String)> {
    let query = match q.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => {
            if q.is_none() {
                return Err((ERROR_BAD_INPUT, "缺少参数 q（搜索词）".to_string()));
            } else {
                return Err((ERROR_BAD_INPUT, "搜索词不能为空".to_string()));
            }
        }
    };

    let platform_key = platform.unwrap_or("all").trim().to_string();
    let is_all = platform_key.eq_ignore_ascii_case("all");

    let kind = match kind_raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(k) => {
            let lower = k.to_ascii_lowercase();
            if matches!(lower.as_str(), "all" | "video" | "post" | "user" | "topic") {
                lower
            } else {
                return Err((
                    ERROR_BAD_INPUT,
                    format!("未知的 kind「{k}」，可选：all, video, post, user, topic"),
                ));
            }
        }
        None => "all".to_string(),
    };

    let sort = match sort_raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => {
            let lower = s.to_ascii_lowercase();
            if matches!(lower.as_str(), "relevance" | "latest" | "hot") {
                lower
            } else {
                return Err((
                    ERROR_BAD_INPUT,
                    format!("未知的 sort「{s}」，可选：relevance, latest, hot"),
                ));
            }
        }
        None => "relevance".to_string(),
    };

    let only_list: Vec<String> = only
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let limit = validate_page_limit(limit_raw)?;

    let page = match page_raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(raw) => raw
            .parse::<i32>()
            .map_err(|_| (ERROR_BAD_INPUT, format!("page 要是整数（收到「{raw}」）")))?,
        None => 1,
    };

    let pages = match pages_raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(raw) => Some(
            raw.parse::<i32>()
                .map_err(|_| (ERROR_BAD_INPUT, format!("pages 要是整数（收到「{raw}」）")))?,
        ),
        None => None,
    };

    if !only_list.is_empty() && !is_all {
        return Err((ERROR_BAD_INPUT, "only 只能和 platform=all 一起用".to_string()));
    }

    if cursor.is_some() && is_all {
        return Err((
            ERROR_BAD_INPUT,
            "cursor 只能和单个平台一起用（all 是好几家的游标，见 done.next）".to_string(),
        ));
    }

    if page < 1 {
        return Err((ERROR_BAD_INPUT, "page 从 1 开始".to_string()));
    }

    if let Some(p) = pages {
        if p < 1 {
            return Err((ERROR_BAD_INPUT, "pages 要 ≥ 1".to_string()));
        }
    }

    let wanted_deepest = if let Some(ps) = pages { ps } else { page };
    if wanted_deepest > PAGE_BUDGET {
        return Err((
            ERROR_BAD_INPUT,
            format!("一次搜索每家最多 {PAGE_BUDGET} 页（和 App 搜索页一致），page {wanted_deepest} 超了"),
        ));
    }

    Ok((query, platform_key, only_list, page, pages, kind, sort, limit))
}

// ===========================================================================
// 搜索混排与续页
// ===========================================================================

pub struct MergedItem {
    pub item_json: String,
    pub item_key: String,
    pub platform: String,
    pub page: i32,
    pub index: i32,
}

pub struct PlatformPageSlice {
    pub items: Vec<(String, String)>, // (item_key, item_json)
    pub offset: i32,
}

pub fn round_robin_merge(
    buffers: &std::collections::HashMap<String, PlatformPageSlice>,
    order: &[String],
    page: i32,
) -> Vec<MergedItem> {
    let active_lists: Vec<(&String, &PlatformPageSlice)> = order
        .iter()
        .filter_map(|p| buffers.get(p).filter(|s| !s.items.is_empty()).map(|s| (p, s)))
        .collect();
    if active_lists.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut round = 0;
    loop {
        let mut added = false;
        for (p, slice) in &active_lists {
            if let Some((k, j)) = slice.items.get(round) {
                added = true;
                out.push(MergedItem {
                    item_json: j.clone(),
                    item_key: k.clone(),
                    platform: (*p).clone(),
                    page,
                    index: slice.offset + round as i32,
                });
            }
        }
        if !added {
            break;
        }
        round += 1;
    }
    out
}

pub fn build_search_done_next(
    order: &[String],
    truncated: bool,
    shown: &[MergedItem],
    next_by_platform: &std::collections::HashMap<String, Option<String>>,
    page_starts: &std::collections::HashMap<i32, std::collections::HashMap<String, Option<String>>>,
    first_page: i32,
    failed: &std::collections::HashSet<String>,
) -> Value {
    let mut obj_fields = Vec::new();
    for p in order {
        if !truncated || failed.contains(p) {
            let c = if failed.contains(p) {
                None
            } else {
                next_by_platform.get(p).cloned().flatten()
            };
            obj_fields.push((
                p.clone(),
                c.map(Value::String).unwrap_or(Value::Null),
            ));
            continue;
        }
        let deepest = shown
            .iter()
            .filter(|it| &it.platform == p)
            .max_by(|a, b| a.page.cmp(&b.page).then_with(|| a.index.cmp(&b.index)));

        let resume = match deepest {
            None => {
                let start_c = page_starts
                    .get(&first_page)
                    .and_then(|m| m.get(p))
                    .cloned()
                    .flatten();
                encode_cursor(start_c.as_deref(), 0)
            }
            Some(it) => {
                let page_c = page_starts
                    .get(&it.page)
                    .and_then(|m| m.get(p))
                    .cloned()
                    .flatten();
                encode_cursor(page_c.as_deref(), it.index + 1)
            }
        };
        obj_fields.push((p.clone(), Value::String(resume)));
    }
    Value::Object(obj_fields)
}

// ===========================================================================
// 图片响应头组装
// ===========================================================================

pub struct ImageReplyHeaders {
    pub idx: i32,
    pub width: i32,
    pub height: i32,
    pub full_width: i32,
    pub full_height: i32,
    pub segments: i32,
    pub segment: i32,
    pub image_count: i32,
    pub requests: i32,
}

impl ImageReplyHeaders {
    pub fn to_headers(&self) -> Vec<(String, String)> {
        vec![
            ("X-Feedhub-Idx".to_string(), self.idx.to_string()),
            ("X-Feedhub-Width".to_string(), self.width.to_string()),
            ("X-Feedhub-Height".to_string(), self.height.to_string()),
            ("X-Feedhub-Full-Width".to_string(), self.full_width.to_string()),
            ("X-Feedhub-Full-Height".to_string(), self.full_height.to_string()),
            ("X-Feedhub-Segments".to_string(), self.segments.to_string()),
            (
                "X-Feedhub-Seg".to_string(),
                if self.segments > 1 {
                    self.segment.to_string()
                } else {
                    "0".to_string()
                },
            ),
            ("X-Feedhub-Image-Count".to_string(), self.image_count.to_string()),
            ("X-Feedhub-Requests".to_string(), self.requests.to_string()),
        ]
    }
}

