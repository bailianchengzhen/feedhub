//! **登录互传的 UniFFI 边界**。
//!
//! 纯逻辑全在 `feedhub-core/src/sync/login.rs`（打包 / 解包 / 校验 / 先清后写 / 一次性服务 /
//! 局域网取与推）。这里只做四件事：
//!
//! 1. 把 Kotlin 的包 / 平台范围 / 结果 record 翻成 core 的模型；
//! 2. 把 [`CookieJar`] 句柄交给 core；
//! 3. 起 / 停一次性服务，查询状态；
//! 4. 把错误折成 [`FfiSyncError::Format`]（消息里只有平台 id / 个数，没有 Cookie 值）。
//!
//! **收发不经过 OkHttp / HostHttp**：服务端和扫码方都是 `std::net` 的局域网直连，
//! 所以 `.so` 里没有引入任何网络栈依赖；`127.0.0.1` 给桌面验收 / 模拟器 e2e 用。

use std::sync::{Arc, Mutex};

use feedhub_core::sync::login as core_login;

use crate::cookies::{CookieJar, FfiCookieEntry};
use crate::library_sync::{FfiSyncDirection, FfiSyncError, FfiSyncOffer};

type FfiResult<T> = Result<T, FfiSyncError>;

impl From<core_login::LoginError> for FfiSyncError {
    fn from(e: core_login::LoginError) -> Self {
        FfiSyncError::Format { detail: e.message }
    }
}

// ===========================================================================
// record：包 / 范围 / 结果
// ===========================================================================

/// 平台 → Cookie 列表（`SyncPlatform`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLoginPlatform {
    pub id: String,
    pub cookies: Vec<FfiCookieEntry>,
}

/// 明文报文（`SyncPackage`）。只在内存里、只在 GCM 里面出现。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLoginPackage {
    pub v: i32,
    pub sent_at_ms: i64,
    pub platforms: Vec<FfiLoginPlatform>,
}

impl FfiLoginPackage {
    pub(crate) fn to_core(&self) -> core_login::LoginPackage {
        core_login::LoginPackage {
            v: self.v,
            sent_at_ms: self.sent_at_ms,
            platforms: self
                .platforms
                .iter()
                .map(|p| core_login::LoginPlatform {
                    id: p.id.clone(),
                    cookies: p.cookies.iter().map(|c| c.to_core()).collect(),
                })
                .collect(),
        }
    }

    pub(crate) fn from_core(package: &core_login::LoginPackage) -> Self {
        Self {
            v: package.v,
            sent_at_ms: package.sent_at_ms,
            platforms: package
                .platforms
                .iter()
                .map(|p| FfiLoginPlatform {
                    id: p.id.clone(),
                    cookies: p.cookies.iter().map(FfiCookieEntry::from_core).collect(),
                })
                .collect(),
        }
    }
}

/// 一个平台的 Cookie 地盘（插件描述的 `login.chromeImport` / `login.web.cookieDomain`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLoginScope {
    pub platform_id: String,
    pub clear_domains: Vec<String>,
    pub suffixes: Vec<String>,
    pub exact_hosts: Vec<String>,
}

impl FfiLoginScope {
    fn to_core(&self) -> core_login::PlatformScope {
        core_login::PlatformScope {
            platform_id: self.platform_id.clone(),
            clear_domains: self.clear_domains.clone(),
            suffixes: self.suffixes.clone(),
            exact_hosts: self.exact_hosts.clone(),
        }
    }
}

fn scopes_to_core(scopes: &[FfiLoginScope]) -> Vec<core_login::PlatformScope> {
    scopes.iter().map(FfiLoginScope::to_core).collect()
}

/// 一个平台带了几条 / 写了几条。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLoginCount {
    pub platform_id: String,
    pub count: u32,
}

/// 某个平台没带 / 没写的原因（只有平台 id + 一句短话）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLoginSkip {
    pub platform_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLoginApplyResult {
    pub applied: Vec<FfiLoginCount>,
    pub skipped: Vec<FfiLoginSkip>,
}

impl FfiLoginApplyResult {
    fn from_core(outcome: core_login::ApplyOutcome) -> Self {
        Self {
            applied: outcome
                .applied
                .into_iter()
                .map(|(platform_id, count)| FfiLoginCount { platform_id, count })
                .collect(),
            skipped: outcome
                .skipped
                .into_iter()
                .map(|(platform_id, reason)| FfiLoginSkip {
                    platform_id,
                    reason,
                })
                .collect(),
        }
    }
}

/// 打包结果（发送 / 推送那一侧显示"会传哪些平台、各几条、跳过谁"）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLoginBuild {
    pub counts: Vec<FfiLoginCount>,
    pub skipped: Vec<String>,
}

impl FfiLoginBuild {
    fn from_core(outcome: &core_login::BuildOutcome) -> Self {
        Self {
            counts: outcome
                .counts
                .iter()
                .map(|(platform_id, count)| FfiLoginCount {
                    platform_id: platform_id.clone(),
                    count: *count,
                })
                .collect(),
            skipped: outcome.skipped.clone(),
        }
    }
}

/// 打包 + 明文包（宿主 `SyncCodec.encode` / 测试用）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLoginBuildResult {
    pub package: FfiLoginPackage,
    pub counts: Vec<FfiLoginCount>,
    pub skipped: Vec<String>,
}

/// 服务状态（和 Kotlin `SyncServer.State` 对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiLoginPhase {
    Idle,
    Listening,
    Delivered,
    Received,
    Expired,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLoginState {
    pub phase: FfiLoginPhase,
    pub bound_port: u32,
    pub expires_at_ms: i64,
    pub failure: Option<String>,
}

// ===========================================================================
// 纯函数：包编解码 / 打包 / 写入 / 默认勾选
// ===========================================================================

/// 明文包 → JSON 字节（和 Kotlin kotlinx.serialization 逐字节一致，v1 / v2 都认）。
#[uniffi::export]
pub fn ffi_sync_login_package_encode(package: FfiLoginPackage) -> FfiResult<Vec<u8>> {
    core_login::encode(&package.to_core()).map_err(Into::into)
}

/// JSON 字节 → 明文包（解密之后校验；v1 / v2 都认）。
#[uniffi::export]
pub fn ffi_sync_login_package_decode(bytes: Vec<u8>) -> FfiResult<FfiLoginPackage> {
    core_login::decode(&bytes)
        .map(|p| FfiLoginPackage::from_core(&p))
        .map_err(Into::into)
}

/// 发送方：按选中平台从本机罐子里打包（只挑、不写）。
#[uniffi::export]
pub fn ffi_sync_login_build(
    jar: Arc<CookieJar>,
    platforms: Vec<String>,
    scopes: Vec<FfiLoginScope>,
    now_ms: i64,
) -> FfiLoginBuildResult {
    let scopes = scopes_to_core(&scopes);
    let built = core_login::build(&jar.shared(), &platforms, &scopes, now_ms);
    FfiLoginBuildResult {
        package: FfiLoginPackage::from_core(&built.package),
        counts: built
            .counts
            .iter()
            .map(|(platform_id, count)| FfiLoginCount {
                platform_id: platform_id.clone(),
                count: *count,
            })
            .collect(),
        skipped: built.skipped.clone(),
    }
}

/// 接收方：按平台各自的域**先清后写**（真正写的是 Rust 的 Cookie 罐）。
#[uniffi::export]
pub fn ffi_sync_login_apply(
    jar: Arc<CookieJar>,
    package: FfiLoginPackage,
    scopes: Vec<FfiLoginScope>,
    now_ms: i64,
) -> FfiResult<FfiLoginApplyResult> {
    let scopes = scopes_to_core(&scopes);
    core_login::apply(&jar.shared(), &package.to_core(), &scopes, now_ms)
        .map(FfiLoginApplyResult::from_core)
        .map_err(Into::into)
}

/// 出码方默认勾哪些平台（本机有该平台域下未过期 Cookie 的那些，按 `scopes` 顺序）。
#[uniffi::export]
pub fn ffi_sync_login_default_platforms(
    jar: Arc<CookieJar>,
    scopes: Vec<FfiLoginScope>,
    now_ms: i64,
) -> Vec<String> {
    core_login::default_platforms(&jar.shared(), &scopes_to_core(&scopes), now_ms)
}

// ===========================================================================
// 一次性小服务（出码方）
// ===========================================================================

/// 出码方的一次性服务。**`stop()` 之后后台线程最多 20ms 内退出**。
#[derive(uniffi::Object)]
pub struct FfiLoginServer {
    inner: Arc<core_login::LoginServer>,
}

#[uniffi::export]
impl FfiLoginServer {
    /// 二维码里装的东西（含实际绑定端口）。
    pub fn offer(&self) -> FfiSyncOffer {
        let offer = self.inner.offer();
        FfiSyncOffer {
            host: offer.host.clone(),
            port: offer.port as u32,
            session_id: offer.session_id.clone(),
            key: offer.key(),
            version: offer.version,
            direction: FfiSyncDirection::from_core(offer.direction),
        }
    }

    pub fn state(&self) -> FfiLoginState {
        let snapshot = self.inner.snapshot();
        FfiLoginState {
            phase: match snapshot.phase {
                core_login::Phase::Idle => FfiLoginPhase::Idle,
                core_login::Phase::Listening => FfiLoginPhase::Listening,
                core_login::Phase::Delivered => FfiLoginPhase::Delivered,
                core_login::Phase::Received => FfiLoginPhase::Received,
                core_login::Phase::Expired => FfiLoginPhase::Expired,
                core_login::Phase::Stopped => FfiLoginPhase::Stopped,
                core_login::Phase::Failed => FfiLoginPhase::Failed,
            },
            bound_port: snapshot.bound_port as u32,
            expires_at_ms: snapshot.expires_at_ms,
            failure: snapshot.failure,
        }
    }

    pub fn stop(&self) {
        self.inner.stop();
    }

    /// 收进来那一侧：已经收到的平台 id + 条数（`None` = 还没收到 / 这是发出去的模式）。
    pub fn pending_platforms(&self) -> Option<Vec<FfiLoginCount>> {
        self.inner.pending_platforms().map(|list| {
            list.into_iter()
                .map(|(platform_id, count)| FfiLoginCount { platform_id, count })
                .collect()
        })
    }

    /// 收进来那一侧：把收到的包写进本机罐子（**只能成功一次**）。
    pub fn apply(
        &self,
        jar: Arc<CookieJar>,
        scopes: Vec<FfiLoginScope>,
        now_ms: i64,
    ) -> FfiResult<FfiLoginApplyResult> {
        self.inner
            .apply_pending(&jar.shared(), &scopes_to_core(&scopes), now_ms)
            .map(FfiLoginApplyResult::from_core)
            .map_err(Into::into)
    }
}

/// 起一个一次性服务（发出去 / 收进来）。`platforms` 只在 `Send` 时用。
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn ffi_sync_login_start_server(
    jar: Arc<CookieJar>,
    bind_host: String,
    advertise_host: String,
    direction: FfiSyncDirection,
    platforms: Vec<String>,
    scopes: Vec<FfiLoginScope>,
    now_ms: i64,
    ttl_ms: i64,
) -> FfiResult<Arc<FfiLoginServer>> {
    let server = core_login::LoginServer::start_new(
        &jar.shared(),
        &bind_host,
        &advertise_host,
        direction.to_core(),
        &platforms,
        &scopes_to_core(&scopes),
        now_ms,
        ttl_ms,
    )?;
    Ok(Arc::new(FfiLoginServer {
        inner: Arc::new(server),
    }))
}

// ===========================================================================
// 扫码方：取 / 推
// ===========================================================================

/// 扫码方取回来的、已经解密校验过的包。**写罐子之前先让宿主播确认框**。
#[derive(uniffi::Object)]
pub struct FfiLoginPending {
    package: Mutex<Option<core_login::LoginPackage>>,
}

#[uniffi::export]
impl FfiLoginPending {
    /// 包里有哪些平台、各多少条（确认框用）。
    pub fn platforms(&self) -> Vec<FfiLoginCount> {
        self.package
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|p| {
                p.platforms
                    .iter()
                    .map(|entry| FfiLoginCount {
                        platform_id: entry.id.clone(),
                        count: entry.cookies.len() as u32,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 写进本机罐子（**只能成功一次**）；调用方已经确认过。
    pub fn apply(
        &self,
        jar: Arc<CookieJar>,
        scopes: Vec<FfiLoginScope>,
        now_ms: i64,
    ) -> FfiResult<FfiLoginApplyResult> {
        let package = self
            .package
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| FfiSyncError::Format {
                detail: "这次同步已经用过了，请重新扫码".to_string(),
            })?;
        core_login::apply(&jar.shared(), &package, &scopes_to_core(&scopes), now_ms)
            .map(FfiLoginApplyResult::from_core)
            .map_err(Into::into)
    }
}

/// 扫码方：从「发出去」的二维码取回、解密、校验。**不改本机罐子**。
#[uniffi::export]
pub fn ffi_sync_login_fetch(offer: FfiSyncOffer) -> FfiResult<Arc<FfiLoginPending>> {
    let inner = offer.to_core().ok_or_else(|| FfiSyncError::Format {
        detail: "同步二维码无效，请重新生成".to_string(),
    })?;
    let package = core_login::fetch(&inner)?;
    Ok(Arc::new(FfiLoginPending {
        package: Mutex::new(Some(package)),
    }))
}

/// 扫码方：把本机选中的平台推给「收进来」的二维码。返回本机打包的个数（对方应用的成功与否不在这里）。
#[uniffi::export]
pub fn ffi_sync_login_push(
    offer: FfiSyncOffer,
    jar: Arc<CookieJar>,
    platforms: Vec<String>,
    scopes: Vec<FfiLoginScope>,
    now_ms: i64,
) -> FfiResult<FfiLoginBuild> {
    let inner = offer.to_core().ok_or_else(|| FfiSyncError::Format {
        detail: "同步二维码无效，请重新生成".to_string(),
    })?;
    let built = core_login::push(
        &inner,
        &jar.shared(),
        &platforms,
        &scopes_to_core(&scopes),
        now_ms,
    )?;
    Ok(FfiLoginBuild::from_core(&built))
}
