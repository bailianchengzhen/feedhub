//! **TLS 指纹档案**：桌面和手机共用同一份。
//!
//! 目标是**现在安卓上 OkHttp 4.12 + Conscrypt 的形状**，由 `rust/net-fingerprint/golden/`
//! 里的抓包定死（`android-conscrypt/hello.json`）。Rust 侧用的是 **BoringSSL**（`btls`），
//! 和安卓 Conscrypt 是同一个家族，所以形状能逐字节对上；
//!
//! # 这些常量从哪来
//!
//! **全部由 golden 生成，不手写**。改指纹的唯一正规做法是**重新抓 golden**，再回来改这张表。
//!
//! | 项 | golden 里的值 |
//! |---|---|
//! | 密码套件 | 15 个，顺序照抓包（TLS1.3 三档 + ECDHE-ECDSA 三档 + ECDHE-RSA 三档 + CBC 两档 + RSA 四档） |
//! | 曲线 | `x25519` `secp256r1` `secp384r1` |
//! | 签名算法 | 9 条（含最后的 `rsa_pkcs1_sha1`，Conscrypt 还留着） |
//! | 扩展顺序 | `0 23 65281 10 11 35 16 5 13 51 45 43 21`（21 = padding，**必须在最后**） |
//! | KeyShare | 只有 `x25519` 一段 |
//! | ALPN | `h2` `http/1.1` |
//! | 版本 | TLS 1.2 ~ 1.3 |
//! | GREASE | 关（Conscrypt 不发） |
//! | OCSP stapling / session ticket | 开（golden 里有 `status_request` / `session_ticket` 扩展） |
//!
//! # 为什么 padding 不用我们管
//!
//! 上游 `boring` 把 ClientHello padding（RFC 7685，避开 F5 的老 bug）删了，`btls` 固定的
//! BoringSSL 还带着，而且是在扩展表最后**自动**加的（`ssl_add_clienthello_tlsext` 里那段
//! `padding_len = 0x200 - header_len`）。所以 `extension_permutation` 里给 `PADDING` 留的位置
//! 只是"顺序占位"，实际长度由 BoringSSL 按 512 字节算——**这正是我们要的形状**，
//! 而且 `pre_shared_key`（会话恢复时才有）会被排到 padding 之后（BoringSSL 的规则：PSK 永远最后）。
//!
//! # 会话恢复
//!
//! golden 的第二次连接（`android-conscrypt-resumed/second`）是**533 字节 + `pre_shared_key(41)`、
//! 没有 padding**。这一条由 `wreq` 的会话缓存（`tls_session_cache`）负责：BoringSSL 自己会把
//! PSK 塞进 CH，我们不用手写。缓存行为要和 OkHttp + Conscrypt 对齐（见 `client.rs` 的
//! `SESSION_CACHE_PER_HOST`）。

use wreq::tls::{AlpnProtocol, ExtensionType, KeyShare, TlsOptions, TlsVersion};

/// golden 的密码套件（**顺序**是形状的一部分）。
///
/// 注意顺序：TLS1.3 三档在最前（`1301 1302 1303`），然后才是 TLS1.2 的 `c02b c02c cca9 c02f c030 cca8
/// c013 c014 009c 009d 002f 0035`。
pub const CIPHERS: &str = "\
TLS_AES_128_GCM_SHA256:TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:\
TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256:TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384:\
TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256:\
TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256:TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384:\
TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256:\
TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA:TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA:\
TLS_RSA_WITH_AES_128_GCM_SHA256:TLS_RSA_WITH_AES_256_GCM_SHA384:\
TLS_RSA_WITH_AES_128_CBC_SHA:TLS_RSA_WITH_AES_256_CBC_SHA";

/// golden 的曲线：`x25519(001d) secp256r1(0017) secp384r1(0018)`。
pub const CURVES: &str = "X25519:P-256:P-384";

/// golden 的签名算法 9 条。
pub const SIGALGS: &str = "\
ecdsa_secp256r1_sha256:rsa_pss_rsae_sha256:rsa_pkcs1_sha256:\
ecdsa_secp384r1_sha384:rsa_pss_rsae_sha384:rsa_pkcs1_sha384:\
rsa_pss_rsae_sha512:rsa_pkcs1_sha512:rsa_pkcs1_sha1";

/// golden 的扩展顺序：`0 23 65281 10 11 35 16 5 13 51 45 43 21`。
///
/// 最后那个 `PADDING`(21) 是占位——BoringSSL 自己按 512 字节补，长度不由这张表决定；
/// 位置放在最后是因为它就是"最后一个扩展"的形状（会话恢复时 PSK 会被 BoringSSL 排到它后面，
/// 变成 `... 43 41`，和 golden 的恢复形状一致）。
pub const EXTENSION_ORDER: &[u16] = &[
    0,    // server_name
    23,   // extended_master_secret
    65281,// renegotiation_info
    10,   // supported_groups
    11,   // ec_point_formats
    35,   // session_ticket
    16,   // alpn
    5,    // status_request
    13,   // signature_algorithms
    51,   // key_share
    45,   // psk_key_exchange_modes
    43,   // supported_versions
    21,   // padding（BoringSSL 自动补；这里只定位置）
];

/// `EXTENSION_ORDER` 的类型化版本（wreq 的 builder 要 `ExtensionType`）。
pub fn extension_types() -> Vec<ExtensionType> {
    EXTENSION_ORDER.iter().copied().map(ExtensionType::from).collect()
}

/// 一份 TLS 指纹档案。
///
/// 现在只有一份（[`TlsProfile::okhttp4_12`]）——**桌面和手机用同一个**。
/// 留着这个类型是因为：① 以后真要分端再说，② 测试里要能"故意改一项"验证门是真的在比。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TlsProfile {
    /// 这份档案叫什么（写日志 / 测试失败信息用）。
    pub name: &'static str,
}

impl TlsProfile {
    /// 现在安卓上 OkHttp 4.12 + Conscrypt 的形状（golden `android-conscrypt`）。
    pub const fn okhttp4_12() -> Self {
        Self { name: "okhttp4.12-conscrypt" }
    }

    /// 生成 wreq 的 [`TlsOptions`]。
    ///
    /// - `session_resumption`：要不要用会话缓存（golden 的"恢复"形状）。**生产路径永远是 true**
    ///   （OkHttp 默认就恢复）；测试里关掉它来抓"首连"形状。
    /// - `cert_store`：信任库。生产是系统 CA（[`crate::client`] 建），测试是本机 CA。
    pub fn tls_options(
        &self,
        session_resumption: bool,
        cert_store: Option<wreq::tls::trust::CertStore>,
    ) -> TlsOptions {
        let b = TlsOptions::builder()
            .cipher_list(CIPHERS)
            // TLS1.3 的套件**顺序**也照 golden（BoringSSL 默认会把 1301 排到 1302 后面）
            .preserve_tls13_cipher_list(true)
            .curves_list(CURVES)
            .sigalgs_list(SIGALGS)
            .alpn_protocols([AlpnProtocol::HTTP2, AlpnProtocol::HTTP1])
            .min_tls_version(TlsVersion::TLS_1_2)
            .max_tls_version(TlsVersion::TLS_1_3)
            // Conscrypt 不发 GREASE（发了 JA3 / JA4 就变了）
            .grease_enabled(false)
            // golden 里有 session_ticket(35) / status_request(5) 两个扩展
            .session_ticket(true)
            .enable_ocsp_stapling(true)
            // golden 的 key_share 只有 x25519 一段
            .key_shares(vec![KeyShare::X25519])
            // 固定的扩展顺序（btls 的 BoringSSL 补丁：`SSL_CTX_set_extension_order`）
            .extension_permutation(extension_types())
            // 会话恢复：wreq 的 `pre_shared_key` 是"开不开 PSK 恢复"的总开关（默认 false）。
            // 关了就是"每次完整握手"。
            .pre_shared_key(session_resumption);
        // 信任库不在这里设（`ClientBuilder::tls_cert_store` 是它自己的字段）；参数留着是为了
        // 让调用方一眼看到"信任库和指纹是两件事"。
        let _ = cert_store;
        b.build()
    }
}

impl Default for TlsProfile {
    fn default() -> Self {
        Self::okhttp4_12()
    }
}

/// HTTP/2 的形状（golden 的 `frames.jsonl`）。
///
/// | 项 | OkHttp 4.12 的值 | 为什么 |
/// |---|---|---|
/// | SETTINGS 条目 | **只有** `INITIAL_WINDOW_SIZE=16777216` | golden 帧 1 就是这 6 字节 |
/// | 连接窗口 | 16777216 → `WINDOW_UPDATE` increment `16711681` | = 16777216 - 65535 |
/// | 首流号 | **3** | OkHttp 把流 1 留给"升级" |
/// | 伪头顺序 | `:method :path :authority :scheme` | golden 帧 3 的 HPACK 顺序 |
/// | push | 不发（等价于禁止） | OkHttp 默认 `ENABLE_PUSH` 不发 |
pub fn http2_options() -> wreq::http2::Http2Options {
    use wreq::http2::{PseudoId, PseudoOrder, SettingId, SettingsOrder};

    wreq::http2::Http2Options::builder()
        .settings_order(SettingsOrder::builder().extend([SettingId::InitialWindowSize]).build())
        .initial_window_size(16777216)
        .initial_connection_window_size(16777216)
        .initial_stream_id(3)
        .headers_pseudo_order(
            PseudoOrder::builder()
                .extend([PseudoId::Method, PseudoId::Path, PseudoId::Authority, PseudoId::Scheme])
                .build(),
        )
        .build()
}

