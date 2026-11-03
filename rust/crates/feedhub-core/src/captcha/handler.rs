//! 验证码状态机与校验规则（对齐 `CaptchaHandlerBase.kt`）。
//!
//! 包括 proof 形状校验、token 生成、URL 挂载匹配与回传校验四态判定。

pub const DEFAULT_TIMEOUT_MS: i64 = 5 * 60 * 1000;
pub const PROOF_PATH: &str = "/proof";

pub const PROMPT_STATUS_WAITING: &str = "请在下面的验证码里完成验证…";
pub const PROMPT_STATUS_TIMEOUT: &str = "没有等到验证结果。下次打开评论会再试。";

/// 挂载中的验证页信息（纯规则与匹配数据，不含 HTML 大文本与 CompletableDeferred，避免子资源高频拦截时跨 FFI 拷贝整份 HTML）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivePage {
    pub scheme: String,
    pub host: String,
    pub port: i32,
    pub path: String,
    pub token: String,
}

/// 页面回传请求的判定结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitResult {
    Accepted { proof: String },
    WrongToken,
    BadProof,
    NoPage,
}

/// 对齐 OkHttp `toHttpUrlOrNull()` 的解析结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl {
    pub scheme: String,
    pub host: String,
    pub port: i32,
    pub encoded_path: String,
    pub query: Option<String>,
}

impl HttpUrl {
    /// 取 query 里第一个名字对得上的参数（百分号与 `+` 解码，对齐 OkHttp `queryParameter`）
    pub fn query_parameter(&self, name: &str) -> Option<String> {
        let q = self.query.as_deref()?;
        for pair in q.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, val) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            if percent_decode(key, true) == name {
                return Some(percent_decode(val, true));
            }
        }
        None
    }
}

/// 解析 HTTP/HTTPS URL，与 OkHttp `toHttpUrlOrNull()` 逐字段对齐：
/// - 仅支持 http / https 协议；
/// - 默认端口 http 80 / https 443；
/// - 路径缺省时补 "/"；
/// - 用户信息 `user:pass@` 丢弃；
/// - IPv6 字面量去掉两端括号（如 `[::1]` -> `::1`）。
pub fn parse_http_url(raw: &str) -> Option<HttpUrl> {
    let text = raw.trim_matches(|c: char| (c as u32) <= 0x20);
    let (scheme, rest) = if let Some(r) = text.strip_prefix("http://") {
        ("http", r)
    } else if let Some(r) = text.strip_prefix("https://") {
        ("https", r)
    // 用 `get` 而不是 `[..7]`：第 7 / 8 个字节落在多字节字符中间时（如 `htt中…`）下标切片会 panic，
    // panic 过 FFI 会把整个进程带走
    } else if text.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://")) {
        ("http", &text[7..])
    } else if text.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://")) {
        ("https", &text[8..])
    } else {
        return None;
    };

    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let mut authority = &rest[..authority_end];
    if let Some(at) = authority.rfind('@') {
        authority = &authority[at + 1..];
    }
    if authority.is_empty() {
        return None;
    }

    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let close = stripped.find(']')?;
        let host = &stripped[..close];
        let tail = &stripped[close + 1..];
        let port = if let Some(p) = tail.strip_prefix(':') {
            p.parse::<i32>().ok()?
        } else if scheme == "http" {
            80
        } else {
            443
        };
        (host.to_ascii_lowercase(), port)
    } else if let Some(sep) = authority.rfind(':') {
        let host = authority[..sep].to_ascii_lowercase();
        let port_str = &authority[sep + 1..];
        let port = port_str.parse::<i32>().ok()?;
        (host, port)
    } else {
        let host = authority.to_ascii_lowercase();
        let port = if scheme == "http" { 80 } else { 443 };
        (host, port)
    };

    if host.is_empty() || port <= 0 || port > 65535 {
        return None;
    }

    let tail = &rest[authority_end..];
    let without_hash = match tail.find('#') {
        Some(i) => &tail[..i],
        None => tail,
    };
    let (path_part, query_part) = match without_hash.find('?') {
        Some(i) => (&without_hash[..i], Some(&without_hash[i + 1..])),
        None => (without_hash, None),
    };

    let encoded_path = if path_part.is_empty() {
        "/".to_string()
    } else if !path_part.starts_with('/') {
        format!("/{path_part}")
    } else {
        path_part.to_string()
    };

    Some(HttpUrl {
        scheme: scheme.to_string(),
        host,
        port,
        encoded_path,
        query: query_part.map(|q| q.to_string()),
    })
}

/// OkHttp 的 `percentDecode(plusIsSpace = true)`
pub fn percent_decode(input: &str, plus_is_space: bool) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push((hi << 4) | lo);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `NEC:<captchaId 前 8 位>:<validate>`，和 Source 侧的 `CaptchaChallenge.proofPrefix` 一致。
///
/// 校验规则：
/// - `proof.startsWith("NEC:")`
/// - UTF-16 length in 14..=8205（注意不是 UTF-8 字节数）
/// - `none { it.isWhitespace() || it.code < 0x20 }`（空白对齐 Kotlin Char.isWhitespace）
pub fn valid_proof(proof: &str) -> bool {
    if !proof.starts_with("NEC:") {
        return false;
    }
    let utf16_len = proof.encode_utf16().count();
    if !(14..=8205).contains(&utf16_len) {
        return false;
    }
    for u in proof.encode_utf16() {
        if u < 0x20 {
            return false;
        }
        if let Some(ch) = char::from_u32(u as u32) {
            if crate::html::kotlin_is_whitespace(ch) {
                return false;
            }
        }
    }
    true
}

/// 格式化 16 字节随机数为 32 位小写十六进制 token
pub fn format_token(bytes: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(32);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0F) as usize] as char);
    }
    s
}

/// 生成 32 位随机十六进制 token（生产路径使用系统随机源）
pub fn generate_random_token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("生成随机 token 失败: {e}"))?;
    Ok(format_token(&bytes))
}

pub fn submit_url(token: &str) -> String {
    format!("{PROOF_PATH}?token={token}")
}

pub fn prompt_message(_platform_label: &str, challenge_message: &str) -> String {
    format!("评论接口要求人机验证（{challenge_message}）。请在详情页里的验证码完成，成功后评论会自动继续。")
}

pub fn current_url(page: &ActivePage) -> String {
    format!("{}://{}:{}{}", page.scheme, page.host, page.port, page.path)
}

pub fn matches_active_page(current: Option<&ActivePage>, url: &str) -> bool {
    let Some(cur) = current else {
        return false;
    };
    let Some(parsed) = parse_http_url(url) else {
        return false;
    };
    parsed.scheme == cur.scheme
        && parsed.host == cur.host
        && parsed.port == cur.port
        && parsed.encoded_path == cur.path
}

pub fn check_submit(current: Option<&ActivePage>, url: &str) -> SubmitResult {
    let Some(cur) = current else {
        return SubmitResult::NoPage;
    };
    let Some(parsed) = parse_http_url(url) else {
        return SubmitResult::NoPage;
    };
    if parsed.scheme != cur.scheme || parsed.host != cur.host || parsed.port != cur.port {
        return SubmitResult::NoPage;
    }
    if parsed.encoded_path != PROOF_PATH {
        return SubmitResult::NoPage;
    }
    if parsed.query_parameter("token").as_deref() != Some(&cur.token) {
        return SubmitResult::WrongToken;
    }
    let proof = parsed.query_parameter("value").unwrap_or_default();
    if !valid_proof(&proof) {
        return SubmitResult::BadProof;
    }
    SubmitResult::Accepted { proof }
}

