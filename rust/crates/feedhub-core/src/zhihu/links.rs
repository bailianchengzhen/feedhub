//! `source-zhihu/ZhihuLinks.kt` 的 Rust 版本：解 `link.zhihu.com/?target=` 跳转、
//! 把指向知乎内容的地址换成能原生打开的 [`DetailTarget::Native`]、拆 `answer:123` 这种条目 id。
//!
//! 正则一律手写（`rust/` 里没有 regex 依赖，`bili::text` 也是手写的）。五条规则逐字照 Kotlin：
//!
//! | 类型 | 地址 |
//! |---|---|
//! | answer | `https?://(www.)?zhihu.com/(question/<数字>/)?answer/<数字>` |
//! | article | `https?://(zhuanlan.zhihu.com|www.zhihu.com)/p/<数字>` |
//! | pin | `https?://(www.)?zhihu.com/pin/<数字>` |
//! | zvideo | `https?://(www.)?zhihu.com/zvideo/<数字>` |
//! | question | `https?://(www.)?zhihu.com/question/<数字>/?([?#]…)?`（**整串**匹配） |
//!
//! 匹配顺序就是上表的顺序（和 Kotlin 一样，先命中先用）。

use crate::model::DetailTarget;

/// 认识的条目类型（`ZhihuLinks.TYPES` 的插入顺序，报错文案里要按这个顺序拼）。
pub const TYPES: &[&str] = &["answer", "article", "pin", "zvideo", "question"];

/// 外链跳转页：`https://link.zhihu.com/?target=https%3A//example.com/x` → `https://example.com/x`。
/// 不是跳转页就原样返回。
pub fn unwrap(href: &str) -> String {
    let Some(url) = super::html::url::parse(href) else {
        return href.to_string();
    };
    if url.host != "link.zhihu.com" {
        return href.to_string();
    }
    match url.query_param("target") {
        // Kotlin `takeIf { it.isNotBlank() }`：全空白也算没有
        Some(target) if !target.chars().all(crate::hosted::is_java_whitespace) => target,
        _ => href.to_string(),
    }
}

/// 指向知乎回答 / 文章 / 想法 / 视频 / 问题的链接 → 条目 id（`answer:123`）；别的（人、话题、专栏、外站）`None`。
pub fn item_id(url: &str) -> Option<String> {
    let (host, path) = split(url)?;
    if let Some(id) = answer_id(host, path) {
        return Some(id);
    }
    if let Some(id) = article_id(host, path) {
        return Some(id);
    }
    if matches!(host, "zhihu.com" | "www.zhihu.com") {
        for (prefix, kind) in [("pin/", "pin"), ("zvideo/", "zvideo")] {
            if let Some(rest) = path.strip_prefix(prefix) {
                if let Some(digits) = leading_digits(rest) {
                    return Some(format!("{kind}:{digits}"));
                }
            }
        }
        if let Some(rest) = path.strip_prefix("question/") {
            if let Some(digits) = leading_digits(rest) {
                // `question/<数字>/?([?#]…)?$`：数字之后只能是可选的 `/`，再往后必须是结尾或 ? / #
                let after = &rest[digits.len()..];
                let after = after.strip_prefix('/').unwrap_or(after);
                if after.is_empty() || after.starts_with('?') || after.starts_with('#') {
                    return Some(format!("question:{digits}"));
                }
            }
        }
    }
    None
}

/// 能原生打开的给 `Native`，否则 `None`。
pub fn target(url: &str) -> Option<DetailTarget> {
    item_id(url).map(|id| DetailTarget::Native { id, platform: None })
}

/// `answer:123` → `("answer", "123")`；格式不对返回 `None`。
pub fn split_id(id: &str) -> Option<(String, String)> {
    let (kind, number) = id.split_once(':')?;
    if kind.is_empty() || !TYPES.contains(&kind) {
        return None;
    }
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((kind.to_string(), number.to_string()))
}

// ---------------------------------------------------------------------------
// 内部
// ---------------------------------------------------------------------------

/// 只看小写的 `http://` / `https://`（Kotlin 的正则是大小写敏感的），拆出 host 和 path。
fn split(url: &str) -> Option<(&str, &str)> {
    let rest = if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://") {
        rest
    } else {
        return None;
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    // host 里可能有 userinfo / 端口；Kotlin 的正则只认那几种写法，这里不做宽松处理
    let path = &rest[end..];
    Some((host, path.strip_prefix('/').unwrap_or(path_after_slash(path))))
}

fn path_after_slash(path: &str) -> &str {
    path.strip_prefix('/').unwrap_or(path)
}

fn answer_id<'a>(host: &str, path: &'a str) -> Option<String> {
    if !matches!(host, "zhihu.com" | "www.zhihu.com") {
        return None;
    }
    // `(question/<数字>/)?answer/<数字>`
    if let Some(rest) = path.strip_prefix("question/") {
        let digits = leading_digits(rest)?;
        let after = &rest[digits.len()..];
        if let Some(rest) = after.strip_prefix('/') {
            if let Some(rest) = rest.strip_prefix("answer/") {
                if let Some(answer) = leading_digits(rest) {
                    return Some(format!("answer:{answer}"));
                }
            }
        }
        return None;
    }
    let rest = path.strip_prefix("answer/")?;
    let digits = leading_digits(rest)?;
    Some(format!("answer:{digits}"))
}

fn article_id(host: &str, path: &str) -> Option<String> {
    if !matches!(host, "zhuanlan.zhihu.com" | "www.zhihu.com") {
        return None;
    }
    let rest = path.strip_prefix("p/")?;
    let digits = leading_digits(rest)?;
    Some(format!("article:{digits}"))
}

/// 开头的连续 ASCII 数字（至少一位）。
fn leading_digits(s: &str) -> Option<&str> {
    let end = s.bytes().take_while(u8::is_ascii_digit).count();
    if end == 0 {
        None
    } else {
        Some(&s[..end])
    }
}

