//! **登录互传**：任意两台 feedhub 客户端扫码互传登录。
//!
//! 老版本只有"电脑发出去 → 手机取走"一种方向。这一版：
//!
//! - **方向由出码方选**：`Send`（发出去，扫码方 `GET` 取走）或 `Receive`（收进来，扫码方 `POST` 上来）；
//! - 二维码 / 路径 / AAD 都带方向，协议版本 2；**v1 的码照样能扫**（按"发出去"处理）；
//! - 打包 / 解包 / 校验 / 先清后写 / 一次性服务 / 局域网直连全在这一个模块里。
//!
//! 留在宿主的只有：相机扫码、界面、网卡列表（`NetworkInterface` 是操作系统接口）、
//! Compose 状态和"写完之后逐个平台 whoami"。**Cookie 值不进日志、不落盘**，错误消息里只有平台 id 和个数。
//!
//! 安全模型没有变：明文 HTTP 上只有 AES-256-GCM 密文；二维码里只有局域网地址、端口、会话 id、
//! 一次性 256 位密钥、版本和方向，**没有 Cookie**；服务只活两分钟、成功一次就关；
//! 别的路径 / 方法一律 404 / 405、不计数（扫描器碰一下不该让码作废）；路径对了却解不开的连续 3 个坏请求提前收摊，最多接 8 个有效连接。
//!
//! ## v1 / v2 的差别
//!
//! | | v1（老客户端） | v2（这一版） |
//! |---|---|---|
//! | 二维码 | `feedhub-sync:1?h&p&s&k` | `...:2?h&p&s&k&d=send\|receive` |
//! | 路径 | `/v1/sync/<会话>` | `/v2/sync/<会话>` |
//! | AAD | `feedhub-sync\|v1\|<会话>` | `feedhub-sync\|v2\|<会话>\|<方向>` |
//! | 明文 v | 1 | 2（接收端 1 / 2 都认） |

use std::fmt;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::b64;
use super::crypto;
use super::json::{self, Value};
use super::offer::{protocol, Direction, SyncOffer};
use crate::cookies::{CookieJar, HttpUrl, StoredCookie};

// ===========================================================================
// 错误
// ===========================================================================

/// 登录互传可以直接给用户看的失败。**消息里不含密钥 / Cookie 值**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginError {
    pub message: String,
}

impl LoginError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for LoginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LoginError {}

type LResult<T> = Result<T, LoginError>;

fn format_error() -> LoginError {
    LoginError::new("同步内容看不懂（格式或版本不对）")
}

// ===========================================================================
// 明文报文
// ===========================================================================

/// 同步的明文（**只在内存里、只在 AES-GCM 里面**出现）：平台 id → Cookie 列表。
///
/// 字段名 / 顺序和 Kotlin 的 `SyncPackage`（kotlinx.serialization）**逐字节一致**：
/// `{"v":2,"sentAtMs":…,"platforms":[{"id":"…","cookies":[{"name":…,"value":…,"domain":…,
/// "path":"/","expiresAt":…,"secure":false,"httpOnly":false,"hostOnly":false}]}]}`。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoginPackage {
    pub v: i32,
    pub sent_at_ms: i64,
    pub platforms: Vec<LoginPlatform>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoginPlatform {
    pub id: String,
    pub cookies: Vec<StoredCookie>,
}

/// 一个平台的"Cookie 地盘"（宿主从插件描述里算出来，规则留在宿主）。
///
/// - `clear_domains`：写之前先 `clear_domain` 的域（主域 + 老域），顺序固定、去重；
/// - `suffixes` / `exact_hosts`：哪些域算这个平台的（`CookieImportSpec` / `login.web.cookieDomain` 的口径）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlatformScope {
    pub platform_id: String,
    pub clear_domains: Vec<String>,
    pub suffixes: Vec<String>,
    pub exact_hosts: Vec<String>,
}

impl PlatformScope {
    /// 这条 Cookie 的域在不在这个平台的地盘里。
    ///
    /// 精确 host 先命中，否则比后缀（本身或子域）——和 Kotlin `CookieImportSpec.matches` 一样。
    pub fn matches(&self, host: &str) -> bool {
        let h = host.trim_start_matches('.').to_lowercase();
        if self
            .exact_hosts
            .iter()
            .any(|x| x.trim_start_matches('.').to_lowercase() == h)
        {
            return true;
        }
        self.suffixes.iter().any(|s| {
            let d = s.trim_start_matches('.').to_lowercase();
            h == d || h.ends_with(&format!(".{d}"))
        })
    }
}

const MAX_PLATFORMS: usize = 64;
const MAX_VALUE_CHARS: usize = 8_192;
const MAX_NAME_CHARS: usize = 256;
const MAX_DOMAIN_CHARS: usize = 253;
const MAX_PATH_CHARS: usize = 1_024;

/// Kotlin `String.length` 是 UTF-16 码元数（emoji 算 2），长度校验按这个口径才和原实现一致。
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `Platform.ID = [a-z][a-z0-9_-]{0,31}`。
fn is_platform_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// Cookie 名允许的字符（RFC 6265 token，和 OkHttp 的检查一致）。
fn is_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn has_control_char(s: &str) -> bool {
    s.chars().any(|c| c == '\r' || c == '\n' || c == '\u{0}')
}

/// 解包之后（以及打包之前）的形状校验。上限都很宽，只用来挡"对端塞爆内存"。
pub fn validate(package: &LoginPackage) -> LResult<()> {
    if package.v != protocol::VERSION && package.v != protocol::VERSION_V1 {
        return Err(LoginError::new(format!(
            "同步协议版本不对（{}）",
            package.v
        )));
    }
    if package.platforms.len() > MAX_PLATFORMS {
        return Err(LoginError::new(format!(
            "同步的平台太多了（{}）",
            package.platforms.len()
        )));
    }
    let mut ids: Vec<&str> = Vec::new();
    let mut total = 0usize;
    for p in &package.platforms {
        if !is_platform_id(&p.id) {
            return Err(LoginError::new("同步里有不合法的平台 id"));
        }
        if ids.iter().any(|id| *id == p.id) {
            return Err(LoginError::new(format!("同步里平台 {} 出现了两次", p.id)));
        }
        ids.push(&p.id);
        total += p.cookies.len();
        if total > protocol::MAX_COOKIES {
            return Err(LoginError::new(format!("同步的 Cookie 太多了（{total}）")));
        }
        for c in &p.cookies {
            validate_cookie(&p.id, c)?;
        }
    }
    Ok(())
}

fn validate_cookie(platform_id: &str, c: &StoredCookie) -> LResult<()> {
    if !is_cookie_name(&c.name) || utf16_len(&c.name) > MAX_NAME_CHARS {
        return Err(LoginError::new(format!(
            "平台 {platform_id} 有不合法的 Cookie 名"
        )));
    }
    if utf16_len(&c.value) > MAX_VALUE_CHARS {
        return Err(LoginError::new(format!(
            "平台 {platform_id} 有超长的 Cookie 值"
        )));
    }
    if has_control_char(&c.value) {
        return Err(LoginError::new(format!(
            "平台 {platform_id} 的 Cookie 值里有控制字符"
        )));
    }
    let domain = c.domain.trim_start_matches('.').to_lowercase();
    if domain.is_empty()
        || domain.len() > MAX_DOMAIN_CHARS
        || !domain
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
    {
        return Err(LoginError::new(format!(
            "平台 {platform_id} 有不合法的 Cookie 域"
        )));
    }
    if c.path.is_empty()
        || utf16_len(&c.path) > MAX_PATH_CHARS
        || !c.path.starts_with('/')
        || has_control_char(&c.path)
    {
        return Err(LoginError::new(format!(
            "平台 {platform_id} 有不合法的 Cookie 路径"
        )));
    }
    if c.expires_at_ms <= 0 {
        return Err(LoginError::new(format!(
            "平台 {platform_id} 有不合法的 Cookie 过期时间"
        )));
    }
    Ok(())
}

/// 编码成和 kotlinx.serialization（`encodeDefaults = true`）**逐字节一致**的 JSON。
pub fn encode(package: &LoginPackage) -> LResult<Vec<u8>> {
    validate(package)?;
    let mut out = String::new();
    out.push_str("{\"v\":");
    out.push_str(&package.v.to_string());
    out.push_str(",\"sentAtMs\":");
    out.push_str(&package.sent_at_ms.to_string());
    out.push_str(",\"platforms\":[");
    for (pi, p) in package.platforms.iter().enumerate() {
        if pi > 0 {
            out.push(',');
        }
        out.push_str("{\"id\":");
        out.push_str(&Value::quote(&p.id));
        out.push_str(",\"cookies\":[");
        for (ci, c) in p.cookies.iter().enumerate() {
            if ci > 0 {
                out.push(',');
            }
            out.push_str("{\"name\":");
            out.push_str(&Value::quote(&c.name));
            out.push_str(",\"value\":");
            out.push_str(&Value::quote(&c.value));
            out.push_str(",\"domain\":");
            out.push_str(&Value::quote(&c.domain));
            out.push_str(",\"path\":");
            out.push_str(&Value::quote(&c.path));
            out.push_str(",\"expiresAt\":");
            out.push_str(&c.expires_at_ms.to_string());
            out.push_str(",\"secure\":");
            out.push_str(if c.secure { "true" } else { "false" });
            out.push_str(",\"httpOnly\":");
            out.push_str(if c.http_only { "true" } else { "false" });
            out.push_str(",\"hostOnly\":");
            out.push_str(if c.host_only { "true" } else { "false" });
            out.push('}');
        }
        out.push_str("]}");
    }
    out.push_str("]}");
    let bytes = out.into_bytes();
    if bytes.len() > protocol::MAX_PLAIN_BYTES {
        return Err(LoginError::new(format!(
            "要同步的内容太大了（{} 字节）",
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// 解包 + 形状校验。`ignoreUnknownKeys = true`，多出来的键忽略；缺省字段照 Kotlin 默认值。
pub fn decode(bytes: &[u8]) -> LResult<LoginPackage> {
    if bytes.is_empty() || bytes.len() > protocol::MAX_PLAIN_BYTES {
        return Err(LoginError::new(format!(
            "同步内容大小不对（{} 字节）",
            bytes.len()
        )));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| format_error())?;
    let value = json::parse(text).map_err(|_| format_error())?;
    if !matches!(value, Value::Object(_)) {
        return Err(format_error());
    }
    let v = match value.get("v") {
        None | Some(Value::Null) => protocol::VERSION,
        Some(Value::Number(_)) => value
            .i64_field("v")
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(format_error)?,
        _ => return Err(format_error()),
    };
    let sent_at_ms = match value.get("sentAtMs") {
        None | Some(Value::Null) => 0,
        Some(Value::Number(_)) => value.i64_field("sentAtMs").ok_or_else(format_error)?,
        _ => return Err(format_error()),
    };
    let platforms_value = match value.get("platforms") {
        None | Some(Value::Null) => &[][..],
        Some(v) if v.as_array().is_some() => v.as_array().map(|a| a.as_slice()).unwrap(),
        _ => return Err(format_error()),
    };
    let mut platforms = Vec::with_capacity(platforms_value.len());
    for p in platforms_value {
        if !matches!(p, Value::Object(_)) {
            return Err(format_error());
        }
        let id = p
            .str_field("id")
            .ok_or_else(format_error)?
            .to_string();
        let cookies_value = match p.get("cookies") {
            None | Some(Value::Null) => &[][..],
            Some(v) if v.as_array().is_some() => v.as_array().map(|a| a.as_slice()).unwrap(),
            _ => return Err(format_error()),
        };
        let mut cookies = Vec::with_capacity(cookies_value.len());
        for c in cookies_value {
            if !matches!(c, Value::Object(_)) {
                return Err(format_error());
            }
            let name = c.str_field("name").ok_or_else(format_error)?.to_string();
            let value = c.str_field("value").ok_or_else(format_error)?.to_string();
            let domain = c.str_field("domain").ok_or_else(format_error)?.to_string();
            let expires_at_ms = match c.get("expiresAt") {
                Some(Value::Number(_)) => c.i64_field("expiresAt").ok_or_else(format_error)?,
                _ => return Err(format_error()),
            };
            let path = match c.get("path") {
                None | Some(Value::Null) => "/".to_string(),
                Some(Value::String(s)) => s.clone(),
                _ => return Err(format_error()),
            };
            let bool_field = |name: &str| -> LResult<bool> {
                match c.get(name) {
                    None | Some(Value::Null) => Ok(false),
                    Some(Value::Bool(b)) => Ok(*b),
                    _ => Err(format_error()),
                }
            };
            cookies.push(StoredCookie {
                name,
                value,
                domain,
                path,
                expires_at_ms,
                secure: bool_field("secure")?,
                http_only: bool_field("httpOnly")?,
                host_only: bool_field("hostOnly")?,
            });
        }
        platforms.push(LoginPlatform { id, cookies });
    }
    let package = LoginPackage {
        v,
        sent_at_ms,
        platforms,
    };
    validate(&package)?;
    Ok(package)
}

// ===========================================================================
// 打包 / 写入 / 默认勾选
// ===========================================================================

/// 发送方打包的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildOutcome {
    pub package: LoginPackage,
    /// 平台 id → 带上多少条（顺序 = 选中顺序）。
    pub counts: Vec<(String, u32)>,
    /// 选了但没有可同步 Cookie 的平台（不会进包，接收端也就不会动它们的域）。
    pub skipped: Vec<String>,
}

/// 接收方应用的结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplyOutcome {
    pub applied: Vec<(String, u32)>,
    /// 平台 id → 没应用的原因（不认识的平台 / 一条可用 Cookie 都没有）。
    pub skipped: Vec<(String, String)>,
}

fn scope_for<'a>(scopes: &'a [PlatformScope], id: &str) -> Option<&'a PlatformScope> {
    scopes.iter().find(|s| s.platform_id == id)
}

fn pick_cookies<'a>(
    all: &'a [StoredCookie],
    scope: &PlatformScope,
    now_ms: i64,
) -> Vec<&'a StoredCookie> {
    let mut seen = std::collections::HashSet::new();
    let mut cookies: Vec<&StoredCookie> = all
        .iter()
        .filter(|c| c.expires_at_ms > now_ms && scope.matches(&c.domain))
        .filter(|c| seen.insert((c.name.clone(), c.domain.clone(), c.path.clone())))
        .collect();
    cookies.sort_by(|a, b| {
        (&a.domain, &a.path, &a.name).cmp(&(&b.domain, &b.path, &b.name))
    });
    cookies
}

/// 发送方：从本机 Cookie 罐里挑出选中平台的 Cookie，做成 [`LoginPackage`]。
///
/// 一个平台**一条都没有就整个跳过**：绝不发一个空平台过去，否则接收端按协议会先清它的域。
pub fn build(
    jar: &CookieJar,
    selected: &[String],
    scopes: &[PlatformScope],
    now_ms: i64,
) -> BuildOutcome {
    let all = jar.all();
    let mut platforms = Vec::new();
    let mut counts = Vec::new();
    let mut skipped = Vec::new();
    for id in selected {
        let Some(scope) = scope_for(scopes, id) else {
            skipped.push(id.clone());
            continue;
        };
        let cookies = pick_cookies(&all, scope, now_ms);
        if cookies.is_empty() {
            skipped.push(id.clone());
            continue;
        }
        counts.push((id.clone(), cookies.len() as u32));
        platforms.push(LoginPlatform {
            id: id.clone(),
            cookies: cookies.into_iter().cloned().collect(),
        });
    }
    BuildOutcome {
        package: LoginPackage {
            v: protocol::VERSION,
            sent_at_ms: now_ms,
            platforms,
        },
        counts,
        skipped,
    }
}

/// 接收方：把这些 Cookie 按平台**只写进它自己的域**——先 `clear_domain` 再写。
///
/// 三条硬规矩（和 Kotlin 原实现一样，但真正执行在 Rust 的罐子上）：
/// 1. 平台必须是本端装了的（`scopes` 里有），否则整段忽略；
/// 2. Cookie 的域必须在本端这个平台的 Scope 里（对端塞别的域一概不要）；
/// 3. 过滤完一条不剩就**不清也不写**（不能让一个坏包清掉本机现有的登录态）。
pub fn apply(
    jar: &CookieJar,
    package: &LoginPackage,
    scopes: &[PlatformScope],
    now_ms: i64,
) -> LResult<ApplyOutcome> {
    validate(package)?;
    let mut outcome = ApplyOutcome::default();
    for entry in &package.platforms {
        let Some(scope) = scope_for(scopes, &entry.id) else {
            outcome
                .skipped
                .push((entry.id.clone(), "这台设备没有这个平台".to_string()));
            continue;
        };
        let mut seen = std::collections::HashSet::new();
        let mut cookies: Vec<&StoredCookie> = entry
            .cookies
            .iter()
            .filter(|c| c.expires_at_ms > now_ms && scope.matches(&c.domain))
            .filter(|c| seen.insert((c.name.clone(), c.domain.clone(), c.path.clone())))
            .collect();
        cookies.sort_by(|a, b| {
            (&a.domain, &a.path, &a.name).cmp(&(&b.domain, &b.path, &b.name))
        });
        if cookies.is_empty() {
            outcome
                .skipped
                .push((entry.id.clone(), "没有能用的 Cookie".to_string()));
            continue;
        }
        for domain in &scope.clear_domains {
            jar.clear_domain(domain, now_ms)
                .map_err(|e| LoginError::new(e.to_string()))?;
        }
        // groupBy(domain)：按排序后的第一次出现分组。
        let mut groups: Vec<(String, Vec<StoredCookie>)> = Vec::new();
        for c in &cookies {
            let domain = c.domain.trim_start_matches('.').to_string();
            match groups.iter_mut().find(|(d, _)| *d == domain) {
                Some((_, list)) => list.push(StoredCookie {
                    domain,
                    ..(*c).clone()
                }),
                None => groups.push((
                    domain.clone(),
                    vec![StoredCookie {
                        domain,
                        ..(*c).clone()
                    }],
                )),
            }
        }
        for (domain, list) in groups {
            // URL 只为和 Kotlin 的 `saveFromResponse(url, cookies)` 形状一致：Rust 罐子按
            // `expires_at_ms` 判过期，不用 URL。
            let _ = HttpUrl::new(domain, "/", true);
            jar.save_from_response(&list, now_ms)
                .map_err(|e| LoginError::new(e.to_string()))?;
        }
        outcome.applied.push((entry.id.clone(), cookies.len() as u32));
    }
    Ok(outcome)
}

/// 出码方界面默认勾哪些：本机 Cookie 罐里**有该平台域下未过期 Cookie** 的平台（按 `scopes` 顺序）。
pub fn default_platforms(jar: &CookieJar, scopes: &[PlatformScope], now_ms: i64) -> Vec<String> {
    let all = jar.all();
    scopes
        .iter()
        .filter(|s| {
            all.iter()
                .any(|c| c.expires_at_ms > now_ms && s.matches(&c.domain))
        })
        .map(|s| s.platform_id.clone())
        .collect()
}

// ===========================================================================
// 一次性小服务（出码方）
// ===========================================================================

/// 服务状态（和 Kotlin `SyncServer.State` 一一对应，多了 `Received`：收进来那一侧收到了包、
/// 等宿主确认后写罐子）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Listening,
    Delivered,
    Received,
    Expired,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSnapshot {
    pub phase: Phase,
    pub bound_port: u16,
    pub expires_at_ms: i64,
    pub failure: Option<String>,
}

struct Shared {
    phase: Mutex<Phase>,
    failure: Mutex<Option<String>>,
    stop: AtomicBool,
    bound_port: u16,
    expires_at_ms: i64,
    /// 收进来那一侧收到的包（写进罐子前压在这里；只可能有一个）。
    received: Mutex<Option<LoginPackage>>,
}

const MAX_BAD_REQUESTS: u32 = 3;
const MAX_CONNECTIONS: u32 = 8;
const MAX_HEADER_BYTES: usize = 8 * 1024;
const READ_TIMEOUT_MS: u64 = 4_000;
const ACCEPT_POLL_MS: u64 = 20;

/// 出码方起的一次性服务。**Drop 会停掉后台线程**（最多等一个 `ACCEPT_POLL_MS`）。
pub struct LoginServer {
    offer: SyncOffer,
    direction: Direction,
    sealed: Option<Vec<u8>>,
    shared: Arc<Shared>,
    deadline: Instant,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// 起服务需要的参数（测试里能固定会话 id / 密钥 / 端口；生产走 [`start_new`]）。
pub struct StartSpec<'a> {
    /// 真正 bind 的地址（`127.0.0.1` 给测试 / 模拟器验收）。
    pub bind_host: &'a str,
    /// 写进二维码、给扫码方连的地址（生产里和 `bind_host` 相同）。
    pub advertise_host: &'a str,
    pub session_id: &'a str,
    pub key: &'a [u8],
    pub version: i32,
    pub direction: Direction,
    /// 0 = 让内核挑一个。
    pub port: u16,
}

impl LoginServer {
    /// 生产入口：随机会话 id + 随机密钥，绑定 `bind_host`，二维码写 `advertise_host`。
    ///
    /// `direction == Send` 时先按 `selected` 打包；一个平台都没有直接报错（界面显示）。
    #[allow(clippy::too_many_arguments)]
    pub fn start_new(
        jar: &CookieJar,
        bind_host: &str,
        advertise_host: &str,
        direction: Direction,
        selected: &[String],
        scopes: &[PlatformScope],
        now_ms: i64,
        ttl_ms: i64,
    ) -> LResult<Self> {
        let mut sid = [0u8; protocol::SESSION_BYTES];
        getrandom::getrandom(&mut sid)
            .map_err(|_| LoginError::new("读系统随机数失败，没法生成同步会话"))?;
        let mut key = [0u8; protocol::KEY_BYTES];
        getrandom::getrandom(&mut key)
            .map_err(|_| LoginError::new("读系统随机数失败，没法生成同步密钥"))?;
        let session_id = b64::encode(&sid);
        let package = if direction == Direction::Send {
            let built = build(jar, selected, scopes, now_ms);
            if built.package.platforms.is_empty() {
                return Err(LoginError::new(
                    "选中的平台都没有可同步的 Cookie。先在本机登录，或从浏览器导入",
                ));
            }
            Some(built.package)
        } else {
            None
        };
        Self::start(
            StartSpec {
                bind_host,
                advertise_host,
                session_id: &session_id,
                key: &key,
                version: protocol::VERSION,
                direction,
                port: 0,
            },
            package,
            now_ms,
            ttl_ms,
        )
    }

    /// 用固定的会话 id / 密钥 / 端口起服务（测试和 v1 兼容门用）。
    pub fn start(
        spec: StartSpec<'_>,
        package: Option<LoginPackage>,
        now_ms: i64,
        ttl_ms: i64,
    ) -> LResult<Self> {
        if spec.direction == Direction::Send && package.is_none() {
            return Err(LoginError::new("发出去的模式缺了要发送的登录信息"));
        }
        let ip: Ipv4Addr = spec.bind_host.parse().map_err(|_| {
            LoginError::new(format!("局域网地址不合法：{}", spec.bind_host))
        })?;
        let listener = TcpListener::bind(SocketAddrV4::new(ip, spec.port)).map_err(|e| {
            LoginError::new(format!(
                "没法在 {}:{} 上监听：{e}",
                spec.bind_host, spec.port
            ))
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|e| LoginError::new(format!("服务起不来：{e}")))?;
        let bound_port = listener
            .local_addr()
            .map_err(|e| LoginError::new(format!("服务起不来：{e}")))?
            .port();
        let offer = SyncOffer::new(
            spec.advertise_host,
            bound_port,
            spec.session_id,
            spec.key,
            spec.version,
            spec.direction,
        )
        .ok_or_else(|| LoginError::new("同步二维码参数不对，没法生成"))?;

        let sealed = match &package {
            Some(package) => {
                let plain = encode(package)?;
                let mut nonce = [0u8; crypto::NONCE_BYTES];
                getrandom::getrandom(&mut nonce)
                    .map_err(|_| LoginError::new("读系统随机数失败，没法加密同步内容"))?;
                let mut out = nonce.to_vec();
                out.extend_from_slice(&crypto::gcm_seal(
                    &offer.key(),
                    &nonce,
                    &plain,
                    &offer.aad(),
                ));
                if out.len() > protocol::MAX_SEALED_BYTES {
                    return Err(LoginError::new("要同步的内容太大了，发不出去"));
                }
                Some(out)
            }
            None => None,
        };

        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        let shared = Arc::new(Shared {
            phase: Mutex::new(Phase::Listening),
            failure: Mutex::new(None),
            stop: AtomicBool::new(false),
            bound_port,
            expires_at_ms,
            received: Mutex::new(None),
        });
        let deadline = Instant::now() + Duration::from_millis(ttl_ms.max(0) as u64);
        let thread = {
            let shared = shared.clone();
            let expected_path = offer.path();
            let key = offer.key();
            let aad = offer.aad();
            let sealed = sealed.clone();
            let direction = spec.direction;
            let handle = thread::Builder::new()
                .name("feedhub-login-sync".to_string())
                .spawn(move || {
                    run_server(
                        listener,
                        shared,
                        deadline,
                        direction,
                        expected_path,
                        key,
                        aad,
                        sealed,
                    )
                })
                .map_err(|e| LoginError::new(format!("服务起不来：{e}")))?;
            Mutex::new(Some(handle))
        };

        Ok(Self {
            offer,
            direction: spec.direction,
            sealed,
            shared,
            deadline,
            thread,
        })
    }

    pub fn offer(&self) -> &SyncOffer {
        &self.offer
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    pub fn snapshot(&self) -> ServerSnapshot {
        let mut phase = self.shared.phase.lock().unwrap_or_else(|e| e.into_inner());
        if *phase == Phase::Listening && Instant::now() >= self.deadline {
            *phase = Phase::Expired;
        }
        ServerSnapshot {
            phase: *phase,
            bound_port: self.shared.bound_port,
            expires_at_ms: self.shared.expires_at_ms,
            failure: self.shared.failure.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        }
    }

    /// 停掉（可重复调用）。已经 Delivered / Received / Expired 时只把套接字收掉。
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let mut phase = self.shared.phase.lock().unwrap_or_else(|e| e.into_inner());
        if *phase == Phase::Listening {
            *phase = Phase::Stopped;
        }
    }

    /// 等后台线程退出。
    pub fn join(&self) {
        if let Some(handle) = self
            .thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = handle.join();
        }
    }

    /// 收进来那一侧：已经收到的平台 id + 条数（没收到 = `None`）。
    pub fn pending_platforms(&self) -> Option<Vec<(String, u32)>> {
        self.shared
            .received
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|p| {
                p.platforms
                    .iter()
                    .map(|e| (e.id.clone(), e.cookies.len() as u32))
                    .collect()
            })
    }

    /// 收进来那一侧：把收到的包写进本机罐子（**只能成功一次**）。
    pub fn apply_pending(
        &self,
        jar: &CookieJar,
        scopes: &[PlatformScope],
        now_ms: i64,
    ) -> LResult<ApplyOutcome> {
        let package = self
            .shared
            .received
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| LoginError::new("还没有收到对方的登录信息"))?;
        apply(jar, &package, scopes, now_ms)
    }

    /// 给测试 / 验收看密文（不打印内容）。生产路径不用。
    pub fn sealed_len(&self) -> Option<usize> {
        self.sealed.as_ref().map(|s| s.len())
    }
}

impl Drop for LoginServer {
    fn drop(&mut self) {
        self.stop();
        self.join();
    }
}

#[allow(clippy::too_many_arguments)]
fn run_server(
    listener: TcpListener,
    shared: Arc<Shared>,
    deadline: Instant,
    direction: Direction,
    expected_path: String,
    key: Vec<u8>,
    aad: Vec<u8>,
    sealed: Option<Vec<u8>>,
) {
    let mut bad = 0u32;
    let mut accepted = 0u32;
    let mut resulting = Phase::Stopped;
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            resulting = Phase::Stopped;
            break;
        }
        if Instant::now() >= deadline {
            resulting = Phase::Expired;
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                accepted += 1;
                let outcome = handle_connection(
                    stream,
                    direction,
                    &expected_path,
                    &key,
                    &aad,
                    sealed.as_deref(),
                    deadline,
                );
                match outcome {
                    ConnOutcome::Delivered(package) => {
                        if let Some(package) = package {
                            *shared.received.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(package);
                            resulting = Phase::Received;
                        } else {
                            resulting = Phase::Delivered;
                        }
                        break;
                    }
                    ConnOutcome::Bad => bad += 1,
                    // 扫描器之类不算坏请求，也不占"最多几个连接"的名额
                    ConnOutcome::Ignored => accepted -= 1,
                    ConnOutcome::Io(message) => {
                        *shared.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
                        resulting = Phase::Failed;
                        break;
                    }
                }
                if bad >= MAX_BAD_REQUESTS || accepted >= MAX_CONNECTIONS {
                    resulting = Phase::Stopped;
                    break;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(ACCEPT_POLL_MS));
            }
            Err(e) => {
                if !shared.stop.load(Ordering::SeqCst) {
                    *shared.failure.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(e.to_string());
                    resulting = Phase::Failed;
                }
                break;
            }
        }
    }
    drop(listener);
    let mut phase = shared.phase.lock().unwrap_or_else(|e| e.into_inner());
    if *phase == Phase::Listening {
        *phase = resulting;
    }
}

enum ConnOutcome {
    /// 成功（`Some` = 收进来的包）。
    Delivered(Option<LoginPackage>),
    /// 路径对了（拿着二维码里的会话 id）但内容不对 / 解密失败 / 半路断开：记一次坏请求，
    /// 那才是有人拿着错的密钥在试。
    Bad,
    /// 路径不对 / 方法不对 / 不是 HTTP / 没发完头就断：局域网里的扫描器、设备发现。
    /// 直接关、**不计数**，只受会话总时长约束。
    Ignored,
    /// 服务自己出问题（不是对端的错）。
    Io(String),
}

#[allow(clippy::too_many_arguments)]
fn handle_connection(
    mut stream: TcpStream,
    direction: Direction,
    expected_path: &str,
    key: &[u8],
    aad: &[u8],
    sealed: Option<&[u8]>,
    deadline: Instant,
) -> ConnOutcome {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)));
    let (head, leftover) = match read_head(&mut stream) {
        Ok(Some(v)) => v,
        Ok(None) => return ConnOutcome::Ignored,
        // 已经 accept 的连接上读失败（对端 RST / 半路断开）是对端的事：直接关、不计数；
        // 记成 `Io` 会让整个会话 Failed——局域网里随便一个扫描器碰一下，二维码就作废了
        Err(_) => return ConnOutcome::Ignored,
    };
    let text = String::from_utf8_lossy(&head);
    let first_line = text.lines().next().unwrap_or("").trim();
    let parts: Vec<&str> = first_line.split(' ').collect();
    if parts.len() != 3 || !parts[2].starts_with("HTTP/1.") {
        let _ = respond(&mut stream, 404, "Not Found", &[], "");
        return ConnOutcome::Ignored;
    }
    let method = parts[0];
    let target = parts[1];
    let want_method = match direction {
        Direction::Send => "GET",
        Direction::Receive => "POST",
    };
    if method != want_method {
        let allow = format!("Allow: {want_method}\r\n");
        let _ = respond(&mut stream, 405, "Method Not Allowed", &[], &allow);
        return ConnOutcome::Ignored;
    }
    // 只认这一个路径（连 query 都不认）：别的请求一个字都不多说，连接直接关。
    if target != expected_path || Instant::now() >= deadline {
        let _ = respond(&mut stream, 404, "Not Found", &[], "");
        return ConnOutcome::Ignored;
    }
    match direction {
        Direction::Send => {
            let Some(sealed) = sealed else {
                return ConnOutcome::Io("服务缺少要发送的密文".to_string());
            };
            match respond(&mut stream, 200, "OK", sealed, "") {
                Ok(()) => ConnOutcome::Delivered(None),
                // 对端断了：不重发，按没成功算（会话还是一次性的）。
                Err(_) => ConnOutcome::Bad,
            }
        }
        Direction::Receive => {
            let body = match read_body(&mut stream, &text, leftover) {
                Ok(body) => body,
                Err(_) => {
                    let _ = respond(&mut stream, 400, "Bad Request", &[], "");
                    return ConnOutcome::Bad;
                }
            };
            if body.len() < crypto::NONCE_BYTES + crypto::TAG_BYTES {
                let _ = respond(&mut stream, 400, "Bad Request", &[], "");
                return ConnOutcome::Bad;
            }
            let nonce = &body[..crypto::NONCE_BYTES];
            let rest = &body[crypto::NONCE_BYTES..];
            let Some(plain) = crypto::gcm_open(key, nonce, rest, aad) else {
                let _ = respond(&mut stream, 400, "Bad Request", &[], "");
                return ConnOutcome::Bad;
            };
            let package = match decode(&plain) {
                Ok(package) => package,
                Err(_) => {
                    let _ = respond(&mut stream, 400, "Bad Request", &[], "");
                    return ConnOutcome::Bad;
                }
            };
            match respond(&mut stream, 200, "OK", &[], "") {
                Ok(()) => ConnOutcome::Delivered(Some(package)),
                Err(_) => ConnOutcome::Bad,
            }
        }
    }
}

/// 读到空行（`\r\n\r\n` 或 `\n\n`）为止；超过上限 / 超时 / 提前断开都算坏请求。
fn read_head(stream: &mut TcpStream) -> Result<Option<(Vec<u8>, Vec<u8>)>, String> {
    let mut out = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => return Ok(None),
            Ok(n) => n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Ok(None)
            }
            Err(e) => return Err(e.to_string()),
        };
        out.extend_from_slice(&buf[..n]);
        if out.len() > MAX_HEADER_BYTES {
            return Ok(None);
        }
        if let Some(pos) = header_end(&out) {
            return Ok(Some((out[..pos].to_vec(), out[pos..].to_vec())));
        }
    }
}

fn header_end(bytes: &[u8]) -> Option<usize> {
    if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some(pos + 4);
    }
    bytes.windows(2).position(|w| w == b"\n\n").map(|p| p + 2)
}

fn read_body(
    stream: &mut TcpStream,
    head_text: &str,
    leftover: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let mut length: Option<usize> = None;
    for line in head_text.lines().skip(1) {
        let Some(colon) = line.find(':') else {
            continue;
        };
        let name = line[..colon].trim().to_ascii_lowercase();
        if name == "content-length" {
            length = line[colon + 1..].trim().parse::<usize>().ok();
        }
    }
    let Some(length) = length else {
        return Err("缺 Content-Length".to_string());
    };
    if length == 0 || length > protocol::MAX_SEALED_BYTES {
        return Err("body 太大或太小".to_string());
    }
    let mut body = leftover;
    // 请求头那一次 read 可能已经把头后面的 body 一起读进来了。
    if body.len() > length {
        body.truncate(length);
    }
    while body.len() < length {
        let mut chunk = [0u8; 4096];
        let want = (length - body.len()).min(chunk.len());
        let n = stream
            .read(&mut chunk[..want])
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("body 不完整".to_string());
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Ok(body)
}

fn respond(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    payload: &[u8],
    extra: &str,
) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n{extra}\r\n",
        payload.len()
    );
    stream
        .write_all(head.as_bytes())
        .map_err(|e| e.to_string())?;
    if !payload.is_empty() {
        stream.write_all(payload).map_err(|e| e.to_string())?;
    }
    stream.flush().map_err(|e| e.to_string())
}

// ===========================================================================
// 扫码方：局域网 HTTP 取 / 推
// ===========================================================================

/// 扫码方取回 + 解密 + 校验。
///
/// **只认 `Send` 方向的码**；`Receive` 方向的码要走 [`push`]。
pub fn fetch(offer: &SyncOffer) -> LResult<LoginPackage> {
    if offer.direction != Direction::Send {
        return Err(LoginError::new(
            "这个二维码是「收进来」的，不能从它取登录信息",
        ));
    }
    let sealed = http_request(offer, None)?;
    if sealed.is_empty() {
        return Err(LoginError::new("对方返回的同步内容是空的"));
    }
    if sealed.len() > protocol::MAX_SEALED_BYTES {
        return Err(LoginError::new("对方返回的同步内容太大，已拒绝"));
    }
    if sealed.len() < crypto::NONCE_BYTES + crypto::TAG_BYTES {
        return Err(LoginError::new("对方返回的同步内容不完整"));
    }
    let nonce = &sealed[..crypto::NONCE_BYTES];
    let rest = &sealed[crypto::NONCE_BYTES..];
    let plain = crypto::gcm_open(&offer.key(), nonce, rest, &offer.aad()).ok_or_else(|| {
        LoginError::new("同步内容校验失败（密钥不对或内容被改过）。在出码的那台设备上重新生成二维码再扫")
    })?;
    decode(&plain)
}

/// 扫码方把自己的登录**推上去**（只认 `Receive` 方向的码）。
///
/// 返回本机打包的结果（哪些平台带上了、各多少条），和发送方界面显示的个数一致。
pub fn push(
    offer: &SyncOffer,
    jar: &CookieJar,
    selected: &[String],
    scopes: &[PlatformScope],
    now_ms: i64,
) -> LResult<BuildOutcome> {
    if offer.direction != Direction::Receive {
        return Err(LoginError::new(
            "这个二维码是「发出去」的，不能往上推登录信息",
        ));
    }
    let built = build(jar, selected, scopes, now_ms);
    if built.package.platforms.is_empty() {
        return Err(LoginError::new(
            "本机选中的平台都没有可同步的 Cookie。先在本机登录，或从浏览器导入",
        ));
    }
    let plain = encode(&built.package)?;
    let mut nonce = [0u8; crypto::NONCE_BYTES];
    getrandom::getrandom(&mut nonce)
        .map_err(|_| LoginError::new("读系统随机数失败，没法加密同步内容"))?;
    let mut sealed = nonce.to_vec();
    sealed.extend_from_slice(&crypto::gcm_seal(&offer.key(), &nonce, &plain, &offer.aad()));
    if sealed.len() > protocol::MAX_SEALED_BYTES {
        return Err(LoginError::new("要同步的内容太大了，发不出去"));
    }
    http_request(offer, Some(&sealed))?;
    Ok(built)
}

fn connect(offer: &SyncOffer) -> LResult<TcpStream> {
    let ip: Ipv4Addr = offer
        .host
        .parse()
        .map_err(|_| LoginError::new(format!("局域网地址不合法：{}", offer.host)))?;
    let addr = SocketAddrV4::new(ip, offer.port);
    let stream = TcpStream::connect_timeout(&addr.into(), Duration::from_secs(5)).map_err(|_| {
        LoginError::new(format!(
            "连不上对方（{}:{}）。确认两台设备在同一个局域网；也可能二维码已经过期或传过了，在出码的那台设备上重新生成一个",
            offer.host, offer.port
        ))
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| LoginError::new(e.to_string()))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| LoginError::new(e.to_string()))?;
    Ok(stream)
}

fn http_request(offer: &SyncOffer, body: Option<&[u8]>) -> LResult<Vec<u8>> {
    let mut stream = connect(offer)?;
    let method = if body.is_some() { "POST" } else { "GET" };
    let mut request = format!(
        "{method} {} HTTP/1.1\r\nHost: {}:{}\r\nAccept: application/octet-stream\r\n",
        offer.path(),
        offer.host,
        offer.port
    );
    if let Some(body) = body {
        request.push_str("Content-Type: application/octet-stream\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("Connection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| LoginError::new(format!("发请求时断开了：{e}")))?;
    if let Some(body) = body {
        stream
            .write_all(body)
            .map_err(|e| LoginError::new(format!("发请求时断开了：{e}")))?;
    }
    stream
        .flush()
        .map_err(|e| LoginError::new(format!("发请求时断开了：{e}")))?;
    let (status, payload) = read_response(&mut stream)?;
    match status {
        200 => Ok(payload),
        404 | 410 => Err(LoginError::new(
            "二维码已过期或已经用过了。在出码的那台设备上重新生成一个",
        )),
        405 => Err(LoginError::new(
            "二维码方向和这次操作不一致（可能两端版本不同）",
        )),
        400 => Err(LoginError::new(
            "对方没能解开这次同步（二维码可能过期了）。重新生成一个再试",
        )),
        other => Err(LoginError::new(format!(
            "对方拒绝了这次同步（HTTP {other}）"
        ))),
    }
}

fn read_response(stream: &mut TcpStream) -> LResult<(u16, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = header_end(&buf) {
            break pos;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return Err(LoginError::new("对方返回的响应头不完整"));
        }
        let n = stream
            .read(&mut chunk)
            .map_err(|e| LoginError::new(format!("接收同步内容时断开了，请重新生成二维码再试（{e}）")))?;
        if n == 0 {
            return Err(LoginError::new("对方返回的响应不完整"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| LoginError::new("对方返回的响应看不懂"))?;
    let mut length: Option<usize> = None;
    for line in head.lines().skip(1) {
        let Some(colon) = line.find(':') else {
            continue;
        };
        if line[..colon].trim().eq_ignore_ascii_case("content-length") {
            length = line[colon + 1..].trim().parse::<usize>().ok();
        }
    }
    let mut body = buf[head_end..].to_vec();
    if let Some(length) = length {
        if length > protocol::MAX_SEALED_BYTES {
            return Err(LoginError::new("对方返回的同步内容太大，已拒绝"));
        }
        while body.len() < length {
            let n = stream
                .read(&mut chunk)
                .map_err(|e| LoginError::new(format!("接收同步内容时断开了，请重新生成二维码再试（{e}）")))?;
            if n == 0 {
                return Err(LoginError::new("接收同步内容时断开了，请重新生成二维码再试"));
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(length);
    } else {
        loop {
            let n = stream
                .read(&mut chunk)
                .map_err(|e| LoginError::new(format!("接收同步内容时断开了，请重新生成二维码再试（{e}）")))?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
            if body.len() > protocol::MAX_SEALED_BYTES {
                return Err(LoginError::new("对方返回的同步内容太大，已拒绝"));
            }
        }
    }
    if body.len() > protocol::MAX_SEALED_BYTES {
        return Err(LoginError::new("对方返回的同步内容太大，已拒绝"));
    }
    Ok((status, body))
}

// ===========================================================================
// 测试
// ===========================================================================

