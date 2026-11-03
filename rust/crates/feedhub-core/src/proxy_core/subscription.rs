//! 订阅内容的三种格式：识别 + 解析（`core/proxy/Subscription.kt` 的 Rust 版）。
//!
//! - **sing-box JSON**（有 `outbounds`，或者干脆就是单个出站对象）；
//! - **Clash / mihomo YAML**（有 `proxies:` 那一段，见 [`super::clash`]）；
//! - **分享链接列表**（明文，或者整体套了一层 / 两层 base64）。
//!
//! 跳过的条目**只报"第几条 + 原因"，不回显原文**（原文里可能有密码 / uuid）。原因文案和 Kotlin 逐字一样
//! （界面上直接显示，两边的说法冲突会让人以为出了两种问题）。

use crate::proxy_core::clash;
use crate::proxy_core::json_text::V;
use crate::proxy_core::link_text;
use crate::proxy_core::model::{ProxyNode, SkippedEntry};
use crate::proxy_core::share_links::{self, ShareLinkResult};

/// 订阅内容的三种格式，自动识别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeSourceFormat {
    /// sing-box JSON（有 `outbounds`）。
    SingBoxJson,
    /// Clash / mihomo YAML（有 `proxies:`）。
    ClashYaml,
    /// 分享链接列表（明文，或者整体是 base64）。
    ShareLinks,
    /// 认出来了但原样返回不了（比如 `-1` 这种）。
    #[allow(non_camel_case_types)]
    Unknown,
}

impl NodeSourceFormat {
    /// 存 / 传出去用的名字（和 Kotlin 枚举的 `name` 一样，FFI 直接放字符串）。
    pub fn name(self) -> &'static str {
        match self {
            NodeSourceFormat::SingBoxJson => "SINGBOX_JSON",
            NodeSourceFormat::ClashYaml => "CLASH_YAML",
            NodeSourceFormat::ShareLinks => "SHARE_LINKS",
            NodeSourceFormat::Unknown => "SHARE_LINKS",
        }
    }
}

/// 一次解析的结果：节点 + 跳过原因 + 认出来的格式。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedNodes {
    pub nodes: Vec<ProxyNode>,
    pub skipped: Vec<SkippedEntry>,
    pub format: NodeSourceFormat,
}

/// 认订阅格式：JSON（`{` / `[` 开头）/ Clash（有 `proxies:` 行）/ 其余当分享链接列表（可能是 base64）。
///
/// `(?m)^[ \t]*proxies[ \t]*:` —— 行首（可带空格 / 制表符）跟着 `proxies` + 可选空白 + 冒号。
pub fn detect_node_source_format(text: &str) -> NodeSourceFormat {
    let t = text.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        return NodeSourceFormat::SingBoxJson;
    }
    if has_proxies_line(text) {
        return NodeSourceFormat::ClashYaml;
    }
    NodeSourceFormat::ShareLinks
}

/// 有没有一行是 `proxies:`（行首可带空格 / 制表符）。
fn has_proxies_line(text: &str) -> bool {
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // `[ \t]*` 只管空格和制表符，不用 `trim_start()`（那会把全角空格也算进去）
        let rest = line.trim_start_matches([' ', '\t']);
        if let Some(rest) = rest.strip_prefix("proxies") {
            let rest = rest.trim_start_matches([' ', '\t']);
            if rest.starts_with(':') {
                return true;
            }
        }
    }
    false
}

/// 解析订阅内容（或手动粘贴的内容）：自动识别格式。
///
/// - JSON：取 `outbounds`；给的是**单个出站对象**（有 `type` + `server`）时也认（手动导入那段 JSON 用）；
/// - Clash：取 `proxies:` 那一段；
/// - 其余：按行当分享链接；整段是 base64 时先解一层（最多两层，有些机场套了两层）。
pub fn parse_node_source(text: &str, subscription_id: Option<&str>) -> ParsedNodes {
    let mut body = text.to_string();
    let mut round = 0;
    while round < 2 && link_text::looks_like_base64(&body) {
        let Some(decoded) = link_text::base64_decode(&body) else {
            break;
        };
        body = decoded;
        round += 1;
    }
    let trimmed = crate::hosted::kotlin_trim(&body);
    if trimmed.is_empty() {
        return ParsedNodes {
            nodes: Vec::new(),
            skipped: Vec::new(),
            format: NodeSourceFormat::ShareLinks,
        };
    }
    match detect_node_source_format(&body) {
        NodeSourceFormat::SingBoxJson => parse_singbox_json(&body, subscription_id),
        NodeSourceFormat::ClashYaml => parse_clash(&body, subscription_id),
        _ => parse_links(&body, subscription_id),
    }
}

fn parse_singbox_json(text: &str, subscription_id: Option<&str>) -> ParsedNodes {
    let root = match crate::xhs::jsonord::parse(text) {
        Ok(v) => v,
        Err(_) => {
            return ParsedNodes {
                nodes: Vec::new(),
                skipped: vec![SkippedEntry {
                    index: 1,
                    reason: "不是合法 JSON".to_string(),
                }],
                format: NodeSourceFormat::SingBoxJson,
            }
        }
    };
    let value = super::model::v_from_j(&root);
    let outbounds: Vec<V> = match &value {
        V::Obj(fields) => {
            let get = |k: &str| fields.iter().find(|(name, _)| name == k).map(|(_, v)| v);
            match get("outbounds") {
                Some(V::Arr(items)) => items.clone(),
                _ => {
                    // 单个出站对象：有 `type` + `server`
                    if j_text(get("type")).is_some() && j_text(get("server")).is_some() {
                        vec![value.clone()]
                    } else {
                        return ParsedNodes {
                            nodes: Vec::new(),
                            skipped: vec![SkippedEntry {
                                index: 1,
                                reason: "JSON 里没有 outbounds（也不像单个出站）".to_string(),
                            }],
                            format: NodeSourceFormat::SingBoxJson,
                        };
                    }
                }
            }
        }
        V::Arr(items) => items.clone(),
        _ => {
            return ParsedNodes {
                nodes: Vec::new(),
                skipped: vec![SkippedEntry {
                    index: 1,
                    reason: "JSON 里没有 outbounds（也不像单个出站）".to_string(),
                }],
                format: NodeSourceFormat::SingBoxJson,
            }
        }
    };
    let mut nodes = Vec::new();
    let mut skipped = Vec::new();
    for (i, element) in outbounds.iter().enumerate() {
        let Some(obj) = as_obj(element) else {
            skipped.push(SkippedEntry {
                index: (i + 1) as i32,
                reason: "不是一个对象".to_string(),
            });
            continue;
        };
        let Some(type_name) = obj_text(obj, "type") else {
            skipped.push(SkippedEntry {
                index: (i + 1) as i32,
                reason: "没有 type".to_string(),
            });
            continue;
        };
        if !share_links::is_real_node_type(&type_name) {
            skipped.push(SkippedEntry {
                index: (i + 1) as i32,
                reason: format!("不是节点（type={type_name}）"),
            });
            continue;
        }
        match share_links::outbound_to_node(element, obj_text(obj, "tag").as_deref(), subscription_id)
        {
            Some(node) => nodes.push(node),
            None => skipped.push(SkippedEntry {
                index: (i + 1) as i32,
                reason: "缺少 server / server_port".to_string(),
            }),
        }
    }
    ParsedNodes {
        nodes,
        skipped,
        format: NodeSourceFormat::SingBoxJson,
    }
}

fn parse_clash(text: &str, subscription_id: Option<&str>) -> ParsedNodes {
    let Some(proxies) = clash::parse_clash_proxies(text) else {
        return ParsedNodes {
            nodes: Vec::new(),
            skipped: vec![SkippedEntry {
                index: 1,
                reason: "没找到 proxies: 段".to_string(),
            }],
            format: NodeSourceFormat::ClashYaml,
        };
    };
    let mut nodes = Vec::new();
    let mut skipped = Vec::new();
    for (i, proxy) in proxies.iter().enumerate() {
        match clash::clash_node_to_outbound(proxy, subscription_id) {
            ShareLinkResult::Ok(node) => nodes.push(*node),
            ShareLinkResult::Skip(reason) => skipped.push(SkippedEntry {
                index: (i + 1) as i32,
                reason,
            }),
        }
    }
    ParsedNodes {
        nodes,
        skipped,
        format: NodeSourceFormat::ClashYaml,
    }
}

fn parse_links(text: &str, subscription_id: Option<&str>) -> ParsedNodes {
    let mut nodes = Vec::new();
    let mut skipped = Vec::new();
    for (line_no, line) in link_text::non_empty_lines(text) {
        match share_links::parse_share_link(&line, subscription_id) {
            ShareLinkResult::Ok(node) => nodes.push(*node),
            ShareLinkResult::Skip(reason) => skipped.push(SkippedEntry {
                index: line_no as i32,
                reason,
            }),
        }
    }
    ParsedNodes {
        nodes,
        skipped,
        format: NodeSourceFormat::ShareLinks,
    }
}

/// 解析响应头 `subscription-userinfo`，例如
/// `upload=123; download=456; total=789; expire=1700000000`。没有或者认不出来的字段留 `None`。
pub fn parse_subscription_user_info(header: Option<&str>) -> Option<crate::proxy_core::model::SubscriptionUserInfo> {
    let header = header?;
    if crate::hosted::is_kotlin_blank(header) {
        return None;
    }
    let mut map: Vec<(String, String)> = Vec::new();
    for part in header.split(';') {
        let Some(eq) = part.find('=') else { continue };
        if eq == 0 {
            // Kotlin: `if (eq <= 0) null else …`
            continue;
        }
        let key = crate::hosted::kotlin_trim(&part[..eq]).to_lowercase();
        let value = crate::hosted::kotlin_trim(&part[eq + 1..]).to_string();
        match map.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => map.push((key, value)),
        }
    }
    let value = |keys: &[&str]| -> Option<i64> {
        for k in keys {
            if let Some((_, v)) = map.iter().find(|(name, _)| name == k) {
                if let Ok(n) = v.parse::<i64>() {
                    return Some(n);
                }
            }
        }
        None
    };
    let info = crate::proxy_core::model::SubscriptionUserInfo {
        upload: value(&["upload"]),
        download: value(&["download"]),
        total: value(&["total"]),
        expire: value(&["expire"]),
    };
    if info.upload.is_none()
        && info.download.is_none()
        && info.total.is_none()
        && info.expire.is_none()
    {
        None
    } else {
        Some(info)
    }
}

// ---------------------------------------------------------------------------
// 小工具：从 V 里按名字取
// ---------------------------------------------------------------------------

pub(crate) fn as_obj(v: &V) -> Option<&Vec<(String, V)>> {
    match v {
        V::Obj(fields) => Some(fields),
        _ => None,
    }
}

/// `JsonObject.text(key)`：非空白字符串（数字 / 布尔也转文本），否则 `None`。
pub(crate) fn obj_text(obj: &[(String, V)], key: &str) -> Option<String> {
    j_text(obj.iter().find(|(k, _)| k == key).map(|(_, v)| v))
}

/// 同上，从 `Option<&V>` 取。
pub(crate) fn j_text(v: Option<&V>) -> Option<String> {
    let text = match v? {
        V::Str(s) => s.clone(),
        V::Int(n) => n.to_string(),
        V::UInt(n) => n.to_string(),
        V::Num(raw) => V::number_text(raw),
        V::Double(f) => crate::proxy_core::json_text::double_text(*f),
        V::Bool(b) => b.to_string(),
        _ => return None,
    };
    if crate::hosted::is_kotlin_blank(&text) {
        None
    } else {
        Some(text)
    }
}

/// `JsonObject.int(key)`：数字 / 数字字符串，`1.5` 截断（和 Kotlin 的 `long()` 一样）。
pub(crate) fn obj_int(obj: &[(String, V)], key: &str) -> Option<i32> {
    j_int(obj.iter().find(|(k, _)| k == key).map(|(_, v)| v))
}

pub(crate) fn j_int(v: Option<&V>) -> Option<i32> {
    let n: i64 = match v? {
        V::Int(n) => *n,
        V::UInt(n) => *n as i64,
        V::Double(f) => *f as i64,
        V::Num(raw) => raw.parse::<i64>().ok().or_else(|| {
            // Kotlin: `content.toLongOrNull() ?: content.toDoubleOrNull()?.toLong() ?: content.trim().toLongOrNull()`
            raw.parse::<f64>().ok().map(|f| f as i64)
        })?,
        V::Str(s) => s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().map(|f| f as i64))?,
        _ => return None,
    };
    Some(n as i32)
}

