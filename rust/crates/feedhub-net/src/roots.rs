//! **系统根证书**（安卓专用）。
//!
//! wreq 在没给 [`CertStore`]、又没开 `webpki-roots` 时走 BoringSSL 的 `set_default_verify_paths`，
//! 也就是编死的 `/etc/ssl/certs` / `/etc/ssl/cert.pem`。桌面 Linux 有这两个路径；**安卓没有 `/etc/ssl`**，
//! 结果是信任库是空的、每个 HTTPS 握手都校验失败。
//!
//! 安卓的系统根证书在 Conscrypt 读的那两个目录里，每张一个文件（`<subject hash>.0`，内容是一段 PEM
//! 后面跟着 `openssl x509 -text` 的文字）：
//! - `/apex/com.android.conscrypt/cacerts`：Android 14 起可以随 Conscrypt 模块更新，**有就以它为准**；
//! - `/system/etc/security/cacerts`：老位置。
//!
//! 用户自装的 CA（`/data/misc/user/0/cacerts-added`）**不加**：OkHttp 在 targetSdk ≥ 24 时默认也不信
//! （要 `network_security_config` 显式打开），这里和它保持一致。
//!
//! 整个进程只读一次（[`OnceLock`]），之后所有客户端共用同一份 `Arc<X509Store>`。

use std::path::Path;
use std::sync::OnceLock;

use wreq::tls::trust::CertStore;

/// 安卓上按顺序找的系统根证书目录：**第一个读出证书的那个就用它**，不合并
/// （apex 那份是系统那份的新版本，合并会把 apex 里已经撤掉的旧根又加回来）。
#[cfg(target_os = "android")]
const ANDROID_CA_DIRS: [&str; 2] = ["/apex/com.android.conscrypt/cacerts", "/system/etc/security/cacerts"];

/// 生产客户端在没显式给信任库时用的那一份。
///
/// - 安卓：读系统根证书目录（见模块说明）；一张都读不到时返回 `None`（退回 BoringSSL 默认路径，
///   握手照样会失败，但不在这里 panic：联网层本身还要能起来，错误由请求带回去）。
/// - 其它系统：`None`，交给 BoringSSL 的默认路径（桌面 Linux 的 `/etc/ssl/certs`）。
pub fn system_cert_store() -> Option<CertStore> {
    static STORE: OnceLock<Option<CertStore>> = OnceLock::new();
    STORE.get_or_init(load_platform_store).clone()
}

#[cfg(target_os = "android")]
fn load_platform_store() -> Option<CertStore> {
    ANDROID_CA_DIRS.iter().find_map(|dir| load_pem_dir(Path::new(dir)))
}

#[cfg(not(target_os = "android"))]
fn load_platform_store() -> Option<CertStore> {
    None
}

/// 把一个目录里每个文件当成一张 PEM 证书读进来（文件里 PEM 后面的说明文字不影响）。
///
/// 读不了的文件、解析不了的证书**跳过**（一张坏证书不该让整个信任库失效）。
/// 一张都没读到就是 `None`。
#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
pub(crate) fn load_pem_dir(dir: &Path) -> Option<CertStore> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut pems: Vec<Vec<u8>> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter(|bytes| bytes.windows(27).any(|w| w == b"-----BEGIN CERTIFICATE-----"))
        .collect();
    if pems.is_empty() {
        return None;
    }
    // 顺序和信任结果无关；排一下只是让同一个目录每次建出来的库一模一样，排查时好对照
    pems.sort();
    CertStore::builder().add_pem_certs(&pems).build().ok()
}

