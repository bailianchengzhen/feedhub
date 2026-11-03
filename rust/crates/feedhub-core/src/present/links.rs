//! 展示层：链接清洗、分享文本拼装与外部打开决策。
//!
//! 对应 Kotlin：
//! - `app/src/commonMain/kotlin/feedhub/app/detail/Share.kt`
//! - `app/src/commonMain/kotlin/feedhub/app/detail/OpenLinks.kt`
//!
//! ## 空白字符口径
//! - `clean_share_url` 的首尾空白使用 [`crate::html::kotlin_trim`]（认 Kotlin `Char.isWhitespace()`，
//!   含 NBSP `\u{00A0}` / `\u{2007}` / `\u{202F}` / `\u{001C}..=\u{001F}` 等，不含 `\u{0085}`）。
//! - `share_text` 针对标题的处理分两步：
//!   1. 连续的 Java 正则空白 `[ \t\n\x0B\x0C\r]` 压成单个 ASCII 空格 `" "`（不匹配 NBSP 或全角空格 `\u{3000}`）；
//!   2. 对折叠后的结果执行 [`crate::html::kotlin_trim`]，去除首尾全部 Kotlin 空白字符。

const COMMON_PARAMS: [&str; 8] = [
    "from", "timestamp", "vd_source", "sharekey", "shareuid", "sharefrom", "gclid", "fbclid",
];

const BILIBILI_PARAMS: [&str; 18] = [
    "from_spmid", "spmid", "buvid", "is_story_h5", "unique_k", "bbid", "plat_id", "-arouter", "mid", "up_id", "refer_from",
    "from_source", "bsource", "launch_id", "session_id", "visit_id", "trackid", "track_id",
];

const TIEBA_PARAMS: [&str; 7] = [
    "share", "fr", "sfc", "client_type", "client_version", "st", "unique",
];

const HEYBOX_PARAMS: [&str; 1] = ["heybox_id"];

const XHS_PARAMS: [&str; 5] = ["xhsshare", "appuid", "apptime", "share_id", "source"];

fn host_params(base: &str) -> &'static [&'static str] {
    if base.contains("bilibili.com") || base.contains("b23.tv") {
        &BILIBILI_PARAMS
    } else if base.contains("baidu.com") {
        &TIEBA_PARAMS
    } else if base.contains("xiaoheihe.cn") {
        &HEYBOX_PARAMS
    } else if base.contains("xiaohongshu.com") {
        &XHS_PARAMS
    } else {
        &[]
    }
}

fn is_tracking_param(raw_key: &str, extra: &[&str]) -> bool {
    let key = raw_key.to_ascii_lowercase();
    COMMON_PARAMS.contains(&key.as_str())
        || extra.contains(&key.as_str())
        || key.starts_with("utm_")
        || key.starts_with("share_")
        || key.starts_with("spm")
}

/// 清洗分享链接里的跟踪 / 来源参数，保留内容相关参数、原参数顺序、转义与 `#fragment`。
pub fn clean_share_url(url: &str) -> String {
    let trimmed = crate::html::kotlin_trim(url);
    let (body, fragment) = match trimmed.find('#') {
        Some(idx) => (&trimmed[..idx], &trimmed[idx..]),
        None => (trimmed, ""),
    };
    let (base, query) = match body.find('?') {
        Some(idx) => (&body[..idx], &body[idx + 1..]),
        None => return trimmed.to_string(),
    };
    let extra = host_params(base);
    let mut kept = Vec::new();
    for part in query.split('&') {
        if part.is_empty() {
            continue;
        }
        let raw_key = match part.find('=') {
            Some(eq) => &part[..eq],
            None => part,
        };
        if !is_tracking_param(raw_key, extra) {
            kept.push(part);
        }
    }
    if kept.is_empty() {
        format!("{base}{fragment}")
    } else {
        format!("{base}?{}{fragment}", kept.join("&"))
    }
}

/// 匹配 Java / Kotlin 默认正则表达式 `\s`：`[ \t\n\x0B\x0C\r]`（不含 NBSP 与全角空格）。
fn is_ascii_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r')
}

fn collapse_ascii_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if is_ascii_whitespace(c) {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            in_space = false;
            out.push(c);
        }
    }
    out
}

/// 复制到剪贴板的分享文字：标题一行、链接一行；没有标题就只有链接。
pub fn share_text(title: Option<&str>, url: &str) -> String {
    let clean_url = clean_share_url(url);
    let t = title.and_then(|raw| {
        let collapsed = collapse_ascii_ws(raw);
        let trimmed = crate::html::kotlin_trim(&collapsed);
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    match t {
        Some(title_text) => format!("{title_text}\n{clean_url}"),
        None => clean_url,
    }
}

/// 外部打开的目标与标签。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalOpen {
    pub deep_link: Option<String>,
    pub web_url: Option<String>,
    pub label: String,
}

/// 决策外部打开的候选链接与按钮标签。
pub fn plan_external_open(
    deep_link: Option<&str>,
    web_url: Option<&str>,
    platform_label: &str,
) -> Option<ExternalOpen> {
    let web = match web_url {
        Some(u) => {
            let trimmed = crate::html::kotlin_trim(u);
            if trimmed.is_empty() {
                None
            } else {
                Some(clean_share_url(u))
            }
        }
        None => None,
    };
    let deep = deep_link.map(ToString::to_string);
    if deep.is_none() && web.is_none() {
        return None;
    }
    let label = if deep.is_some() {
        format!("在 {platform_label} 打开")
    } else {
        "用浏览器打开".to_string()
    };
    Some(ExternalOpen {
        deep_link: deep,
        web_url: web,
        label,
    })
}

/// `perform_external_open` 的最终执行结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalOpenResult {
    OpenedApp,
    OpenedWeb,
    Copied,
    Failed,
}

/// 外部打开执行过程中的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalOpenStage {
    Initial,
    AppFailed,
    WebFailed,
}

/// 外部打开状态机推导出的下一步动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalOpenStep {
    OpenApp { target: String },
    OpenWeb { target: String },
    CopyWeb { target: String },
    Completed { result: ExternalOpenResult },
}

/// 根据当前阶段和候选链接推导下一步动作。
pub fn next_external_open_step(
    deep_link: Option<&str>,
    web_url: Option<&str>,
    stage: ExternalOpenStage,
) -> ExternalOpenStep {
    match stage {
        ExternalOpenStage::Initial => {
            if let Some(deep) = deep_link {
                ExternalOpenStep::OpenApp {
                    target: deep.to_string(),
                }
            } else if let Some(web) = web_url {
                ExternalOpenStep::OpenWeb {
                    target: web.to_string(),
                }
            } else {
                ExternalOpenStep::Completed {
                    result: ExternalOpenResult::Failed,
                }
            }
        }
        ExternalOpenStage::AppFailed => {
            if let Some(web) = web_url {
                ExternalOpenStep::OpenWeb {
                    target: web.to_string(),
                }
            } else {
                ExternalOpenStep::Completed {
                    result: ExternalOpenResult::Failed,
                }
            }
        }
        ExternalOpenStage::WebFailed => {
            if let Some(web) = web_url {
                ExternalOpenStep::CopyWeb {
                    target: web.to_string(),
                }
            } else {
                ExternalOpenStep::Completed {
                    result: ExternalOpenResult::Failed,
                }
            }
        }
    }
}

