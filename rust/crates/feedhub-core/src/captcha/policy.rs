//! 验证码浏览器的导航白名单策略（对齐 `CaptchaNavigationPolicy.kt`）。
//!
//! 100% 纯函数，不碰 CEF，不发起网络请求。

use crate::html::kotlin_trim;

/// 易盾验证码会开 iframe / 加载脚本的自家域名
pub const SUB_FRAME_DOMAINS: &[&str] = &["126.net", "163.com", "163yun.com"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub scheme: String,
    pub host: String,
    pub port: i32,
}

impl ParsedUrl {
    pub fn effective_port(&self) -> i32 {
        if self.port >= 0 {
            self.port
        } else if self.scheme == "http" {
            80
        } else if self.scheme == "https" {
            443
        } else {
            -1
        }
    }
}

/// 拆 `scheme://[user@]host[:port]/path?query#frag`；没有 `//` 的（`about:blank`、`data:…`）
/// host 给空串。畸形（没有 scheme、IPv6 没闭合）返回 None。
///
/// 注意：开头必须用 [`crate::html::kotlin_trim`]，对齐 Kotlin `raw.trim()`。
pub fn parse_url(raw: &str) -> Option<ParsedUrl> {
    let text = kotlin_trim(raw);
    let colon = text.find(':')?;
    if colon == 0 {
        return None;
    }
    let scheme = text[..colon].to_lowercase();
    if !scheme
        .chars()
        .all(|c| c.is_alphanumeric() || c == '+' || c == '-' || c == '.')
    {
        return None;
    }
    let rest = &text[colon + 1..];
    if !rest.starts_with("//") {
        return Some(ParsedUrl {
            scheme,
            host: String::new(),
            port: -1,
        });
    }
    let body = &rest[2..];
    let end = body.find(['/', '?', '#']).unwrap_or(body.len());
    let authority = &body[..end];

    // 去掉 user:password@
    let host_port = authority.rfind('@').map_or(authority, |at| &authority[at + 1..]);
    if host_port.starts_with('[') {
        let close = host_port.find(']')?;
        let host = host_port[1..close].to_ascii_lowercase();
        let tail = &host_port[close + 1..];
        let port_str = tail.strip_prefix(':').unwrap_or(tail);
        let port = port_str.parse::<i32>().unwrap_or(-1);
        return Some(ParsedUrl { scheme, host, port });
    }

    let sep = host_port.rfind(':');
    if let Some(sep) = sep {
        let host = host_port[..sep].to_ascii_lowercase();
        let port = host_port[sep + 1..].parse::<i32>().unwrap_or(-1);
        Some(ParsedUrl { scheme, host, port })
    } else {
        let host = host_port.to_ascii_lowercase();
        Some(ParsedUrl {
            scheme,
            host,
            port: -1,
        })
    }
}

/// 从页面地址里取本次验证页的端口；取不到给 -1（那就什么都不放行）。
pub fn port_of(url: &str) -> i32 {
    parse_url(url).map_or(-1, |p| p.effective_port())
}

/// 主框架：只准本机验证页（http + 127.0.0.1 + 本次端口，路径不限）。
pub fn is_main_frame_allowed(port: i32, url: &str) -> bool {
    let Some(parsed) = parse_url(url) else {
        return false;
    };
    parsed.scheme == "http" && parsed.host == "127.0.0.1" && parsed.effective_port() == port
}

/// 子框架：易盾的域名 + about:blank。其余一律不放行。
pub fn is_sub_frame_allowed(url: &str) -> bool {
    let Some(parsed) = parse_url(url) else {
        return false;
    };
    if parsed.scheme == "about" {
        return true;
    }
    if parsed.scheme != "http" && parsed.scheme != "https" {
        return false;
    }
    SUB_FRAME_DOMAINS.iter().any(|d| matches_domain(&parsed.host, d))
}

/// [host] 是不是 [domain] 或它的子域：`dun.126.net` 算，`126.net.evil.com` / `evil126.net` 不算。
pub fn matches_domain(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    if host.ends_with(domain) {
        let prefix = &host[..host.len() - domain.len()];
        return prefix.ends_with('.');
    }
    false
}

