//! **资料库同步的 UniFFI 边界**。
//!
//! 纯函数 + 两个小对象，收发与落盘都不在这里：
//!
//! | Rust | Kotlin 门面 | 干什么 |
//! |---|---|---|
//! | [`FfiSyncSnapshot`] 一族 record | `core/sync/LibrarySyncModel.kt` 的数据类 | 过边界的同步模型 |
//! | [`ffi_sync_encode_snapshot`] / [`ffi_sync_decode_snapshot`] | `LibrarySnapshotCodec` | 快照 JSON 编解码 |
//! | [`ffi_sync_sanitize`] | `LibrarySnapshotSanitizer` | 坏条目清洗（"能不能解成 `FeedItem`"由宿主回调） |
//! | [`ffi_sync_merge`] | `LibraryMerge` | 合并引擎 |
//! | [`FfiSyncFileKeys`] 一族 + [`FfiSyncFile`] | `LibrarySyncFile` | 同步文件（v1/v2）读写 |
//! | [`FfiSyncDeviceKey`] | `DeviceKey` | 一把 X25519 设备密钥（私钥只在 Rust 里，**不过边界**） |
//! | [`ffi_sync_pairing_code`] | `PairingCode` | 6 位配对码 |
//! | `ffi_sync_offer_*` / `ffi_sync_pairing_offer_*` | `SyncOffer` / `PairingOffer` | 二维码 |
//! | [`FfiSyncPairingMessage`] + codec 函数 | `PairingCodec` | 配对报文 |
//!
//! ## 为什么私钥不过边界
//!
//! `DeviceKey` 的私钥只在 Rust 里（[`FfiSyncDeviceKey`] 持有着，`Option` 是"已销毁"）。
//! Kotlin 的 `DeviceKeyStore` 仍然负责**落盘**（600 权限 + 原子替换，那是平台的事），
//! 所以它要能读回私钥字节——那一步走 [`FfiSyncDeviceKey::private_key_bytes`]，
//! 调一次、写文件、马上把 `ByteArray` 填零（和 Kotlin 原实现"私钥字段只在本机文件里"一致）。
//!
//! ## 随机量从外面传
//!
//! 加密 / 配对的 nonce、salt、临时密钥都由 Kotlin 给（生产是 `SecureRandom`，测试是固定序列），
//! 所以这里的方法都带 `nonce` / `salt` / `random` 参数——两边拿同一批字节，对拍才逐字节可比。

use std::sync::Arc;

use feedhub_core::sync::crypto as core_crypto;
use feedhub_core::sync::file as core_file;
use feedhub_core::sync::flow as core_flow;
use feedhub_core::sync::json as core_json;
use feedhub_core::sync::merge as core_merge;
use feedhub_core::sync::offer as core_offer;
use feedhub_core::sync::pairing as core_pairing;
use feedhub_core::sync::snapshot as core_snapshot;

// ===========================================================================
// 错误
// ===========================================================================

/// 同步的错误。字段不叫 `message`。
///
/// 只有两种变体：`Format` = 可以直接给用户看的一句话（"不是同步文件" / "密码不对" / "不是发给这台设备的"），
/// `Internal` = 内部错误（参数组合不对）。**Kotlin 侧把 `Format` 映射成 `LibrarySyncFormatException`**。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiSyncError {
    #[error("{detail}")]
    Format { detail: String },

    #[error("{detail}")]
    Internal { detail: String },
}

impl From<core_snapshot::LibSyncError> for FfiSyncError {
    fn from(e: core_snapshot::LibSyncError) -> Self {
        FfiSyncError::Format {
            detail: e.to_string(),
        }
    }
}

impl FfiSyncError {
    fn internal(detail: impl Into<String>) -> Self {
        FfiSyncError::Internal {
            detail: detail.into(),
        }
    }
}

type FfiResult<T> = Result<T, FfiSyncError>;

// ===========================================================================
// record：同步模型（和 Kotlin 的数据类一一对应）
// ===========================================================================

/// 一个收藏夹（`SyncFolderRow`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncFolderRow {
    pub uid: String,
    pub name: String,
    pub created_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
}

/// 一条收藏（`SyncFavoriteRow`）。`item_json` 是透传的 JSON 原文（`None` = 没有）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncFavoriteRow {
    pub key: String,
    pub folder_uid: Option<String>,
    pub added_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
    pub item_json: Option<String>,
}

/// 一条历史（`SyncHistoryRow`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncHistoryRow {
    pub key: String,
    pub visited_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
    pub progress_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub progress_hlc: i64,
    pub progress_dev: String,
    pub item_json: Option<String>,
}

/// 一个屏蔽（`SyncBlockedRow`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncBlockedRow {
    pub key: String,
    pub user_json: Option<String>,
    pub blocked_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
}

/// 四张表（`SyncState`）。合并引擎只认它。
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct FfiSyncState {
    pub history_cleared_hlc: i64,
    pub folders: Vec<FfiSyncFolderRow>,
    pub favorites: Vec<FfiSyncFavoriteRow>,
    pub history: Vec<FfiSyncHistoryRow>,
    pub blocked: Vec<FfiSyncBlockedRow>,
}

/// 一份快照（`LibrarySnapshot`）：四张表 + 报文元数据。
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct FfiSyncSnapshot {
    pub magic: String,
    pub v: i64,
    pub schema: i64,
    pub device: String,
    pub seq: i64,
    pub generated_at: i64,
    pub history_cleared_hlc: i64,
    pub folders: Vec<FfiSyncFolderRow>,
    pub favorites: Vec<FfiSyncFavoriteRow>,
    pub history: Vec<FfiSyncHistoryRow>,
    pub blocked: Vec<FfiSyncBlockedRow>,
}

/// 清洗结果（`SanitizedSnapshot`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncSanitized {
    pub snapshot: FfiSyncSnapshot,
    pub skipped: u32,
}

/// 各类记录的增 / 改 / 删计数（`SyncCounts`）。
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct FfiSyncCounts {
    pub favorites_added: i32,
    pub favorites_updated: i32,
    pub favorites_deleted: i32,
    pub folders_added: i32,
    pub folders_updated: i32,
    pub folders_deleted: i32,
    pub history_added: i32,
    pub history_updated: i32,
    pub history_deleted: i32,
    pub blocked_added: i32,
    pub blocked_updated: i32,
    pub blocked_deleted: i32,
}

/// 本机收藏夹 uid → 合并后的 uid（同名认领）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncFolderAlias {
    pub old_uid: String,
    pub new_uid: String,
}

/// 合并结果（`MergeOutcome`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncMergeOutcome {
    pub state: FfiSyncState,
    pub counts: FfiSyncCounts,
    pub folder_aliases: Vec<FfiSyncFolderAlias>,
    pub folders_created: i32,
    pub history_cleared_by_remote: bool,
    pub cleared_rows: i32,
}

// ===========================================================================
// record：同步文件
// ===========================================================================

/// 文件头（`LibrarySyncFile.Header`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncFileHeader {
    pub version: u32,
    /// `true` = 按设备加密（收件人模式），`false` = 密码。
    pub recipients_mode: bool,
    /// 密码模式的 PBKDF2 迭代次数；按设备加密是 0。
    pub iterations: u32,
    pub device: String,
    pub generated_at: i64,
    pub seq: i64,
    pub recipients: Vec<String>,
}

/// 解密成功的内容（`LibrarySyncFile.Opened`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncOpened {
    pub header: FfiSyncFileHeader,
    /// 解压后的明文（快照 JSON 的字节）。
    pub plaintext: Vec<u8>,
    /// `true` = 用本机设备密钥解开的（没问密码）。
    pub unlocked_by_device_key: bool,
}

/// 一个收件设备（`LibrarySyncFile.Recipient`）：设备 id + X25519 公钥。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncRecipient {
    pub device: String,
    pub public_key: Vec<u8>,
}

// ===========================================================================
// core ↔ FFI
// ===========================================================================

fn folder_from_ffi(row: &FfiSyncFolderRow) -> core_snapshot::SyncFolderRow {
    core_snapshot::SyncFolderRow {
        uid: row.uid.clone(),
        name: row.name.clone(),
        created_at: row.created_at,
        rev_hlc: row.rev_hlc,
        rev_dev: row.rev_dev.clone(),
        deleted: row.deleted,
    }
}

fn folder_to_ffi(row: &core_snapshot::SyncFolderRow) -> FfiSyncFolderRow {
    FfiSyncFolderRow {
        uid: row.uid.clone(),
        name: row.name.clone(),
        created_at: row.created_at,
        rev_hlc: row.rev_hlc,
        rev_dev: row.rev_dev.clone(),
        deleted: row.deleted,
    }
}

fn favorite_from_ffi(row: &FfiSyncFavoriteRow) -> core_snapshot::SyncFavoriteRow {
    core_snapshot::SyncFavoriteRow {
        key: row.key.clone(),
        folder_uid: row.folder_uid.clone(),
        added_at: row.added_at,
        rev_hlc: row.rev_hlc,
        rev_dev: row.rev_dev.clone(),
        deleted: row.deleted,
        item: parse_item(row.item_json.as_deref()),
    }
}

fn favorite_to_ffi(row: &core_snapshot::SyncFavoriteRow) -> FfiSyncFavoriteRow {
    FfiSyncFavoriteRow {
        key: row.key.clone(),
        folder_uid: row.folder_uid.clone(),
        added_at: row.added_at,
        rev_hlc: row.rev_hlc,
        rev_dev: row.rev_dev.clone(),
        deleted: row.deleted,
        item_json: item_to_json(row.item.as_ref()),
    }
}

fn history_from_ffi(row: &FfiSyncHistoryRow) -> core_snapshot::SyncHistoryRow {
    core_snapshot::SyncHistoryRow {
        key: row.key.clone(),
        visited_at: row.visited_at,
        rev_hlc: row.rev_hlc,
        rev_dev: row.rev_dev.clone(),
        deleted: row.deleted,
        progress_ms: row.progress_ms,
        duration_ms: row.duration_ms,
        progress_hlc: row.progress_hlc,
        progress_dev: row.progress_dev.clone(),
        item: parse_item(row.item_json.as_deref()),
    }
}

fn history_to_ffi(row: &core_snapshot::SyncHistoryRow) -> FfiSyncHistoryRow {
    FfiSyncHistoryRow {
        key: row.key.clone(),
        visited_at: row.visited_at,
        rev_hlc: row.rev_hlc,
        rev_dev: row.rev_dev.clone(),
        deleted: row.deleted,
        progress_ms: row.progress_ms,
        duration_ms: row.duration_ms,
        progress_hlc: row.progress_hlc,
        progress_dev: row.progress_dev.clone(),
        item_json: item_to_json(row.item.as_ref()),
    }
}

fn blocked_from_ffi(row: &FfiSyncBlockedRow) -> core_snapshot::SyncBlockedRow {
    core_snapshot::SyncBlockedRow {
        key: row.key.clone(),
        user: parse_item(row.user_json.as_deref()),
        blocked_at: row.blocked_at,
        rev_hlc: row.rev_hlc,
        rev_dev: row.rev_dev.clone(),
        deleted: row.deleted,
    }
}

fn blocked_to_ffi(row: &core_snapshot::SyncBlockedRow) -> FfiSyncBlockedRow {
    FfiSyncBlockedRow {
        key: row.key.clone(),
        user_json: item_to_json(row.user.as_ref()),
        blocked_at: row.blocked_at,
        rev_hlc: row.rev_hlc,
        rev_dev: row.rev_dev.clone(),
        deleted: row.deleted,
    }
}

/// Kotlin 给的是 `item_json` 原文（`FeedJson.encodeToString(FeedItem.serializer(), item)`）。
/// 解不开就当成 `None`（"只有墓碑"那条分支），和 `RustLibrary.toFfi` 的 `runCatching` 一个口径。
fn parse_item(text: Option<&str>) -> Option<core_json::Value> {
    let text = text?;
    core_json::parse(text).ok()
}

/// 回到 Kotlin：**原样打印**解析时的键顺序和数字字面量（`JsonElement.toString()` 的口径）。
fn item_to_json(value: Option<&core_json::Value>) -> Option<String> {
    value.map(core_json::to_json_string)
}

fn state_from_ffi(state: &FfiSyncState) -> core_snapshot::SyncState {
    core_snapshot::SyncState {
        history_cleared_hlc: state.history_cleared_hlc,
        folders: state.folders.iter().map(folder_from_ffi).collect(),
        favorites: state.favorites.iter().map(favorite_from_ffi).collect(),
        history: state.history.iter().map(history_from_ffi).collect(),
        blocked: state.blocked.iter().map(blocked_from_ffi).collect(),
    }
}

fn state_to_ffi(state: &core_snapshot::SyncState) -> FfiSyncState {
    FfiSyncState {
        history_cleared_hlc: state.history_cleared_hlc,
        folders: state.folders.iter().map(folder_to_ffi).collect(),
        favorites: state.favorites.iter().map(favorite_to_ffi).collect(),
        history: state.history.iter().map(history_to_ffi).collect(),
        blocked: state.blocked.iter().map(blocked_to_ffi).collect(),
    }
}

fn snapshot_from_ffi(snapshot: &FfiSyncSnapshot) -> core_snapshot::LibrarySnapshot {
    core_snapshot::LibrarySnapshot {
        magic: snapshot.magic.clone(),
        v: snapshot.v,
        schema: snapshot.schema,
        device: snapshot.device.clone(),
        seq: snapshot.seq,
        generated_at: snapshot.generated_at,
        history_cleared_hlc: snapshot.history_cleared_hlc,
        state: core_snapshot::SyncState {
            history_cleared_hlc: snapshot.history_cleared_hlc,
            folders: snapshot.folders.iter().map(folder_from_ffi).collect(),
            favorites: snapshot.favorites.iter().map(favorite_from_ffi).collect(),
            history: snapshot.history.iter().map(history_from_ffi).collect(),
            blocked: snapshot.blocked.iter().map(blocked_from_ffi).collect(),
        },
    }
}

fn snapshot_to_ffi(snapshot: &core_snapshot::LibrarySnapshot) -> FfiSyncSnapshot {
    FfiSyncSnapshot {
        magic: snapshot.magic.clone(),
        v: snapshot.v,
        schema: snapshot.schema,
        device: snapshot.device.clone(),
        seq: snapshot.seq,
        generated_at: snapshot.generated_at,
        history_cleared_hlc: snapshot.history_cleared_hlc,
        folders: snapshot.state.folders.iter().map(folder_to_ffi).collect(),
        favorites: snapshot.state.favorites.iter().map(favorite_to_ffi).collect(),
        history: snapshot.state.history.iter().map(history_to_ffi).collect(),
        blocked: snapshot.state.blocked.iter().map(blocked_to_ffi).collect(),
    }
}

fn counts_to_ffi(counts: &core_merge::SyncCounts) -> FfiSyncCounts {
    FfiSyncCounts {
        favorites_added: counts.favorites_added,
        favorites_updated: counts.favorites_updated,
        favorites_deleted: counts.favorites_deleted,
        folders_added: counts.folders_added,
        folders_updated: counts.folders_updated,
        folders_deleted: counts.folders_deleted,
        history_added: counts.history_added,
        history_updated: counts.history_updated,
        history_deleted: counts.history_deleted,
        blocked_added: counts.blocked_added,
        blocked_updated: counts.blocked_updated,
        blocked_deleted: counts.blocked_deleted,
    }
}

fn header_to_ffi(header: &core_file::Header) -> FfiSyncFileHeader {
    FfiSyncFileHeader {
        version: header.version as u32,
        recipients_mode: header.mode == core_file::Mode::Recipients,
        iterations: header.iterations,
        device: header.device.clone(),
        generated_at: header.generated_at,
        seq: header.seq,
        recipients: header.recipients.clone(),
    }
}

// ===========================================================================
// 快照编解码 / 清洗 / 合并
// ===========================================================================

/// 快照 → JSON 字节（**和 `LibrarySnapshotCodec.encode` 逐字节相同**）。
#[uniffi::export]
pub fn ffi_sync_encode_snapshot(snapshot: FfiSyncSnapshot) -> Vec<u8> {
    core_snapshot::encode(&snapshot_from_ffi(&snapshot))
}

/// JSON 字节 → 快照（`LibrarySnapshotCodec.decode`：magic / 版本 / schema 任何一个不对就整份拒）。
#[uniffi::export]
pub fn ffi_sync_decode_snapshot(bytes: Vec<u8>) -> FfiResult<FfiSyncSnapshot> {
    core_snapshot::decode(&bytes)
        .map(|snapshot| snapshot_to_ffi(&snapshot))
        .map_err(FfiSyncError::from)
}

/// magic / 协议版本 / schema 的检查（`LibrarySnapshotCodec.checkHeader`）。
#[uniffi::export]
pub fn ffi_sync_check_header(magic: String, v: i64, schema: i64) -> FfiResult<()> {
    core_snapshot::check_header(&magic, v, schema).map_err(FfiSyncError::from)
}

/// 坏条目清洗的唯一出口（"能不能解成 `FeedItem` / `UserRef`"要在 Kotlin 里判，见
/// [`FfiSyncSanitizeHost`]）。**必须**走这个函数：`ffi_sync_sanitize_plain` 只查 JSON 对象 + 字节上限。
#[uniffi::export]
pub fn ffi_sync_sanitize(snapshot: FfiSyncSnapshot, host: Arc<dyn FfiSyncSanitizeHost>) -> FfiSyncSanitized {
    let inner = snapshot_from_ffi(&snapshot);
    let outcome = core_snapshot::sanitize_with(
        &inner,
        &|value| host.valid_item(core_json::to_json_string(value)),
        &|value| host.valid_user(core_json::to_json_string(value)),
    );
    FfiSyncSanitized {
        snapshot: snapshot_to_ffi(&outcome.snapshot),
        skipped: outcome.skipped as u32,
    }
}

/// 不带宿主回调的清洗：只查"是不是 JSON 对象 + 字节上限"。**只给对拍 / 排障用**，
/// 生产走 [`ffi_sync_sanitize`]（更严）。
#[uniffi::export]
pub fn ffi_sync_sanitize_plain(snapshot: FfiSyncSnapshot) -> FfiSyncSanitized {
    let inner = snapshot_from_ffi(&snapshot);
    let outcome = core_snapshot::sanitize(&inner);
    FfiSyncSanitized {
        snapshot: snapshot_to_ffi(&outcome.snapshot),
        skipped: outcome.skipped as u32,
    }
}

/// 宿主的"这条 item / user 能不能用"判定。
///
/// 参数是**那棵子树打印出来的 JSON 字符串**（Rust 侧和 Kotlin 侧写法一致，见 `sync::json`），
/// 实现里用 `FeedJson.decodeFromJsonElement` 试一次就行。**回调只做纯计算**。
#[uniffi::export(with_foreign)]
pub trait FfiSyncSanitizeHost: Send + Sync {
    fn valid_item(&self, item_json: String) -> bool;
    fn valid_user(&self, user_json: String) -> bool;
}

/// 合并引擎（`LibraryMerge.merge`）。
#[uniffi::export]
pub fn ffi_sync_merge(
    local: FfiSyncState,
    remote: FfiSyncState,
    retention_cutoff_ms: Option<i64>,
) -> FfiSyncMergeOutcome {
    let outcome = core_merge::merge(&state_from_ffi(&local), &state_from_ffi(&remote), retention_cutoff_ms);
    FfiSyncMergeOutcome {
        state: state_to_ffi(&outcome.state),
        counts: counts_to_ffi(&outcome.counts),
        folder_aliases: outcome
            .folder_aliases
            .iter()
            .map(|(old, new)| FfiSyncFolderAlias {
                old_uid: old.clone(),
                new_uid: new.clone(),
            })
            .collect(),
        folders_created: outcome.folders_created,
        history_cleared_by_remote: outcome.history_cleared_by_remote,
        cleared_rows: outcome.cleared_rows,
    }
}

/// 合并结果里最大的 HLC（调用方要把本机时钟抬到它之后）。
#[uniffi::export]
pub fn ffi_sync_max_hlc(state: FfiSyncState) -> i64 {
    core_merge::max_hlc(&state_from_ffi(&state))
}

// ===========================================================================
// 密码学（纯函数）
// ===========================================================================

/// AES-256-GCM 封装 → `ciphertext || tag`（**不带 nonce**）。
#[uniffi::export]
pub fn ffi_sync_gcm_seal(key: Vec<u8>, nonce: Vec<u8>, plaintext: Vec<u8>, aad: Vec<u8>) -> FfiResult<Vec<u8>> {
    if key.len() != core_crypto::KEY_BYTES || nonce.len() != core_crypto::NONCE_BYTES {
        return Err(FfiSyncError::internal("GCM 的密钥 / nonce 长度不对"));
    }
    Ok(core_crypto::gcm_seal(&key, &nonce, &plaintext, &aad))
}

/// 解开；**密钥不对 / 内容被改 / AAD 不匹配 / 长度不对一律 `None`**。
#[uniffi::export]
pub fn ffi_sync_gcm_open(key: Vec<u8>, nonce: Vec<u8>, sealed: Vec<u8>, aad: Vec<u8>) -> Option<Vec<u8>> {
    core_crypto::gcm_open(&key, &nonce, &sealed, &aad)
}

/// PBKDF2-HMAC-SHA256 → 32 字节（口令按 UTF-8 编）。
#[uniffi::export]
pub fn ffi_sync_password_key(password: Vec<u8>, salt: Vec<u8>, iterations: u32) -> Vec<u8> {
    // 口令过边界是**字节**（Kotlin 那边 `password.toByteArray(UTF_8)`）：`Char[]` → `Vec<u8>`
    // 的编码决定权交给调用方，Rust 不猜平台的默认编码。
    let text = String::from_utf8_lossy(&password).into_owned();
    core_crypto::password_key(&text, &salt, iterations).to_vec()
}

/// HKDF-SHA256（RFC 5869）。
#[uniffi::export]
pub fn ffi_sync_hkdf_sha256(ikm: Vec<u8>, salt: Vec<u8>, info: Vec<u8>, length: u32) -> Vec<u8> {
    core_crypto::hkdf_sha256(&ikm, &salt, &info, length as usize)
}

/// SHA-256。
#[uniffi::export]
pub fn ffi_sync_sha256(data: Vec<u8>) -> Vec<u8> {
    core_crypto::sha256(&data).to_vec()
}

/// X25519 共享秘密；对方公钥不合法（长度不对 / 低阶点）→ `None`。
#[uniffi::export]
pub fn ffi_sync_x25519_agree(secret: Vec<u8>, peer_public_key: Vec<u8>) -> Option<Vec<u8>> {
    let secret: [u8; core_crypto::X25519_BYTES] = secret.try_into().ok()?;
    core_crypto::x25519_agree(&secret, &peer_public_key).map(|s| s.to_vec())
}

/// 私钥 → 公钥。
#[uniffi::export]
pub fn ffi_sync_x25519_public_key(secret: Vec<u8>) -> Option<Vec<u8>> {
    let secret: [u8; core_crypto::X25519_BYTES] = secret.try_into().ok()?;
    Some(core_crypto::x25519_public_key(&secret).to_vec())
}

/// 6 位配对码（`PairingCode.of`）。两边设备 id 相同 / 公钥长度不对 → `None`。
#[uniffi::export]
pub fn ffi_sync_pairing_code(
    a_device: String,
    a_public_key: Vec<u8>,
    b_device: String,
    b_public_key: Vec<u8>,
) -> Option<String> {
    core_crypto::pairing_code(&a_device, &a_public_key, &b_device, &b_public_key)
}

/// base64url（无填充）编码。
#[uniffi::export]
pub fn ffi_sync_base64url_encode(bytes: Vec<u8>) -> String {
    feedhub_core::sync::b64::encode(&bytes)
}

/// base64url 解码（认 `=` 填充；口径同 `java.util.Base64.getUrlDecoder()`）。
#[uniffi::export]
pub fn ffi_sync_base64url_decode(text: String) -> Option<Vec<u8>> {
    feedhub_core::sync::b64::decode(&text)
}

/// HLC 的编解码（`Hlc.of` / `physical` / `counter` / `compareVersion` / `maxVersion`）。
///
/// 为什么过边界：HLC 的比较是**合并的正确性依赖**（平局按设备 id 定胜负），
/// 两边必须是同一份实现；而且它是纯函数，一进一出没有状态。
#[uniffi::export]
pub fn ffi_sync_hlc_of(physical_ms: i64, counter: i32) -> i64 {
    core_crypto::hlc::of(physical_ms, counter)
}

#[uniffi::export]
pub fn ffi_sync_hlc_physical(hlc: i64) -> i64 {
    core_crypto::hlc::physical(hlc)
}

#[uniffi::export]
pub fn ffi_sync_hlc_counter(hlc: i64) -> i32 {
    core_crypto::hlc::counter(hlc)
}

#[uniffi::export]
pub fn ffi_sync_hlc_compare_version(hlc_a: i64, dev_a: String, hlc_b: i64, dev_b: String) -> i32 {
    core_crypto::hlc::compare_version(hlc_a, &dev_a, hlc_b, &dev_b)
}

#[uniffi::export]
pub fn ffi_sync_hlc_max_version(hlc_a: i64, dev_a: String, hlc_b: i64, dev_b: String) -> i64 {
    core_crypto::hlc::max_version(hlc_a, &dev_a, hlc_b, &dev_b)
}

// ===========================================================================
// 设备密钥（对象：私钥留在 Rust 里）
// ===========================================================================

/// 一把 X25519 设备密钥。**私钥不出边界**（只有 [`FfiSyncDeviceKey::private_key_bytes`] 这一个口子，
/// 给 Kotlin 的 `DeviceKeyStore` 落盘用）。
#[derive(uniffi::Object)]
pub struct FfiSyncDeviceKey {
    device_id: String,
    secret: [u8; core_crypto::X25519_BYTES],
    public: [u8; core_crypto::X25519_BYTES],
}

#[uniffi::export]
impl FfiSyncDeviceKey {
    /// 从一把私钥恢复（公钥现算，不信调用方给的那份——和 `DeviceKey.of` 一样）。
    #[uniffi::constructor]
    pub fn from_private_key(device_id: String, private_key: Vec<u8>) -> FfiResult<Arc<Self>> {
        let secret: [u8; core_crypto::X25519_BYTES] = private_key
            .try_into()
            .map_err(|_| FfiSyncError::internal("设备私钥必须是 32 字节"))?;
        let public = core_crypto::x25519_public_key(&secret);
        Ok(Arc::new(FfiSyncDeviceKey {
            device_id,
            secret,
            public,
        }))
    }

    /// 生成一把新的（随机数从外面给：32 字节；`None` = 用系统的）。
    #[uniffi::constructor]
    pub fn generate(device_id: String, private_key: Option<Vec<u8>>) -> FfiResult<Arc<Self>> {
        let secret = match private_key {
            Some(bytes) => bytes
                .try_into()
                .map_err(|_| FfiSyncError::internal("设备私钥必须是 32 字节"))?,
            None => core_crypto::x25519_generate().0,
        };
        let public = core_crypto::x25519_public_key(&secret);
        Ok(Arc::new(FfiSyncDeviceKey {
            device_id,
            secret,
            public,
        }))
    }

    pub fn device_id(&self) -> String {
        self.device_id.clone()
    }

    /// 公钥（32 字节）。
    pub fn public_key(&self) -> Vec<u8> {
        self.public.to_vec()
    }

    /// 公钥的 base64url。
    pub fn public_key_base64(&self) -> String {
        feedhub_core::sync::b64::encode(&self.public)
    }

    /// 私钥字节——**只给 `DeviceKeyStore` 落盘**。调用方必须马上填零。
    pub fn private_key_bytes(&self) -> Vec<u8> {
        self.secret.to_vec()
    }

    /// 和对方公钥算共享秘密；公钥不合法 → `None`。
    pub fn agree(&self, peer_public_key: Vec<u8>) -> Option<Vec<u8>> {
        core_crypto::x25519_agree(&self.secret, &peer_public_key).map(|s| s.to_vec())
    }

    /// 私钥和公钥对不对得上（测试 / "密钥文件坏了"的判断用）。
    pub fn matches_public_key(&self) -> bool {
        core_crypto::x25519_public_key(&self.secret) == self.public
    }
}

impl std::fmt::Debug for FfiSyncDeviceKey {
    /// **日志里只有设备 id，永远不含密钥**（`DeviceKey.toString` 一个口径）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FfiSyncDeviceKey({})", self.device_id)
    }
}

// ===========================================================================
// 同步文件（密码 / 收件人）
// ===========================================================================

/// 用密码加密（`LibrarySyncFile.seal`）。`salt` / `nonce` 各 16 / 12 字节，由调用方给。
#[uniffi::export]
pub fn ffi_sync_file_seal_with_password(
    plaintext: Vec<u8>,
    password: Vec<u8>,
    device: String,
    generated_at: i64,
    seq: i64,
    iterations: u32,
    salt: Vec<u8>,
    nonce: Vec<u8>,
) -> FfiResult<Vec<u8>> {
    if salt.len() != 16 || nonce.len() != core_crypto::NONCE_BYTES {
        return Err(FfiSyncError::internal("salt 必须是 16 字节、nonce 必须是 12 字节"));
    }
    let text = String::from_utf8_lossy(&password).into_owned();
    let mut random = RandomChain::new(vec![salt, nonce]).into_source();
    core_file::seal(&plaintext, &text, &device, generated_at, seq, iterations, &mut random)
        .map_err(FfiSyncError::from)
}

/// 一个"按顺序吐出固定字节"的随机源（这一层不自己取随机数：所有 nonce / salt / 临时密钥都由调用方给）。
struct RandomChain {
    chunks: Vec<Vec<u8>>,
}

impl RandomChain {
    fn new(chunks: Vec<Vec<u8>>) -> Self {
        RandomChain { chunks }
    }

    fn into_source(mut self) -> core_file::RandomSource<'static> {
        if self.chunks.is_empty() {
            self.chunks.push(vec![0u8; 1]);
        }
        let chunks = std::mem::take(&mut self.chunks);
        // 用闭包按顺序取：每次取 n 字节就从当前块里切（块不够就循环用）
        let mut offset = 0usize;
        let mut chunk_index = 0usize;
        core_file::RandomSource::from_fn(move |n: usize| {
            let mut out = Vec::with_capacity(n);
            while out.len() < n {
                let chunk = &chunks[chunk_index % chunks.len()];
                if offset >= chunk.len() {
                    chunk_index += 1;
                    offset = 0;
                    continue;
                }
                out.push(chunk[offset]);
                offset += 1;
            }
            out
        })
    }
}

/// 按设备加密（`LibrarySyncFile.sealToRecipients`）。
///
/// `file_key` / `ephemeral_secret` / `nonce` / `wrap_nonces` 都由调用方给（生产是 `SecureRandom`，
/// 测试是固定值），所以同一批输入两边产出的字节**逐字节相同**（gzip 之后的部分除外，见 `sync/file.rs`）。
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn ffi_sync_file_seal_to_recipients(
    plaintext: Vec<u8>,
    device: String,
    generated_at: i64,
    seq: i64,
    recipients: Vec<FfiSyncRecipient>,
    file_key: Vec<u8>,
    ephemeral_secret: Vec<u8>,
    nonce: Vec<u8>,
    wrap_nonces: Vec<Vec<u8>>,
) -> FfiResult<Vec<u8>> {
    if file_key.len() != 32 {
        return Err(FfiSyncError::internal("文件密钥必须是 32 字节"));
    }
    if ephemeral_secret.len() != core_crypto::X25519_BYTES {
        return Err(FfiSyncError::internal("临时私钥必须是 32 字节"));
    }
    if nonce.len() != core_crypto::NONCE_BYTES {
        return Err(FfiSyncError::internal("nonce 必须是 12 字节"));
    }
    let mut parsed = Vec::with_capacity(recipients.len());
    for recipient in &recipients {
        parsed.push(core_file::Recipient::new(&recipient.device, &recipient.public_key).map_err(FfiSyncError::from)?);
    }
    let mut chunks = vec![file_key, ephemeral_secret, nonce];
    chunks.extend(wrap_nonces);
    let mut random = RandomChain::new(chunks).into_source();
    core_file::seal_to_recipients(&plaintext, &device, generated_at, seq, &parsed, &mut random)
        .map_err(FfiSyncError::from)
}

/// 用密码打开（v2 密码文件 / v1 旧文件）。
#[uniffi::export]
pub fn ffi_sync_file_open_with_password(bytes: Vec<u8>, password: Vec<u8>) -> FfiResult<FfiSyncOpened> {
    let text = String::from_utf8_lossy(&password).into_owned();
    core_file::open(&bytes, &text)
        .map(|opened| FfiSyncOpened {
            header: header_to_ffi(&opened.header),
            plaintext: opened.plaintext,
            unlocked_by_device_key: opened.unlock == core_file::Unlock::DeviceKey,
        })
        .map_err(FfiSyncError::from)
}

/// 用本机设备密钥打开（按设备加密的文件）。
///
/// `device_id` + `private_key` = 那把密钥；`None` 用 [`FfiSyncDeviceKey`] 里的那份。
#[uniffi::export]
pub fn ffi_sync_file_open_with_device_key(
    bytes: Vec<u8>,
    device_id: String,
    private_key: Vec<u8>,
) -> FfiResult<FfiSyncOpened> {
    core_file::open_with_device_key(&bytes, &device_id, &private_key)
        .map(|opened| FfiSyncOpened {
            header: header_to_ffi(&opened.header),
            plaintext: opened.plaintext,
            unlocked_by_device_key: opened.unlock == core_file::Unlock::DeviceKey,
        })
        .map_err(FfiSyncError::from)
}

/// 只读文件头（不解密）。
#[uniffi::export]
pub fn ffi_sync_file_read_header(bytes: Vec<u8>) -> FfiResult<FfiSyncFileHeader> {
    core_file::read_header(&bytes)
        .map(|header| header_to_ffi(&header))
        .map_err(FfiSyncError::from)
}

/// 这个文件有没有本机的收件条目（读头部，不解密）。
#[uniffi::export]
pub fn ffi_sync_file_has_recipient(bytes: Vec<u8>, device_id: String) -> bool {
    core_file::has_recipient(&bytes, &device_id)
}

/// 文件格式版本（`LibrarySyncFile.VERSION` / `VERSION_PASSWORD_ONLY`）。
#[uniffi::export]
pub fn ffi_sync_file_version() -> u32 {
    core_file::VERSION as u32
}

/// 最少的 PBKDF2 迭代次数（打开时低于它直接拒）。
#[uniffi::export]
pub fn ffi_sync_file_min_iterations() -> u32 {
    core_file::MIN_ITERATIONS
}

/// gzip（测试 / 排障用：确认两边的 gzip 互相能解）。
#[uniffi::export]
pub fn ffi_sync_gzip(data: Vec<u8>) -> Vec<u8> {
    core_file::gzip(&data)
}

/// gunzip（超上限直接拒）。
#[uniffi::export]
pub fn ffi_sync_gunzip(data: Vec<u8>) -> FfiResult<Vec<u8>> {
    core_file::gunzip(&data).map_err(FfiSyncError::from)
}

// ===========================================================================
// 二维码：登录同步 / 配对
// ===========================================================================

/// 登录互传的方向。由**出码方**选：
/// `Send` = 发出去（扫码方取走），`Receive` = 收进来（扫码方把密文推上来）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiSyncDirection {
    Send,
    Receive,
}

impl FfiSyncDirection {
    pub(crate) fn to_core(self) -> core_offer::Direction {
        match self {
            Self::Send => core_offer::Direction::Send,
            Self::Receive => core_offer::Direction::Receive,
        }
    }

    pub(crate) fn from_core(direction: core_offer::Direction) -> Self {
        match direction {
            core_offer::Direction::Send => Self::Send,
            core_offer::Direction::Receive => Self::Receive,
        }
    }
}

/// 登录同步二维码（`SyncOffer`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncOffer {
    pub host: String,
    pub port: u32,
    pub session_id: String,
    pub key: Vec<u8>,
    pub version: i32,
    /// 方向（v1 的码解析出来固定是 `Send`）。
    pub direction: FfiSyncDirection,
}

impl FfiSyncOffer {
    fn content(&self) -> String {
        // 走一遍真正的解析 / 格式化，保证和 Kotlin 的 `SyncOffer.content` 一样
        match self.to_core() {
            Some(offer) => offer.content(),
            None => String::new(),
        }
    }

    pub(crate) fn to_core(&self) -> Option<core_offer::SyncOffer> {
        let port: u16 = u16::try_from(self.port).ok()?;
        core_offer::SyncOffer::new(
            &self.host,
            port,
            &self.session_id,
            &self.key,
            self.version,
            self.direction.to_core(),
        )
    }
}

/// 登录同步二维码的字段（协议常量）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncProtocol {
    pub version: i32,
    /// v1（没有方向字段）的版本号：新客户端扫到老码时用。
    pub version_v1: i32,
    pub qr_prefix: String,
    pub path_prefix: String,
    /// v1 的路径前缀（兼容老码）。
    pub path_prefix_v1: String,
    pub ttl_ms: i64,
    pub key_bytes: u32,
    pub session_bytes: u32,
    pub max_sealed_bytes: u32,
    pub max_plain_bytes: u32,
    pub max_cookies: u32,
}

#[uniffi::export]
pub fn ffi_sync_protocol() -> FfiSyncProtocol {
    FfiSyncProtocol {
        version: core_offer::protocol::VERSION,
        version_v1: core_offer::protocol::VERSION_V1,
        qr_prefix: core_offer::protocol::QR_PREFIX.to_string(),
        path_prefix: core_offer::protocol::PATH_PREFIX.to_string(),
        path_prefix_v1: core_offer::protocol::PATH_PREFIX_V1.to_string(),
        ttl_ms: core_offer::protocol::TTL_MS,
        key_bytes: core_offer::protocol::KEY_BYTES as u32,
        session_bytes: core_offer::protocol::SESSION_BYTES as u32,
        max_sealed_bytes: core_offer::protocol::MAX_SEALED_BYTES as u32,
        max_plain_bytes: core_offer::protocol::MAX_PLAIN_BYTES as u32,
        max_cookies: core_offer::protocol::MAX_COOKIES as u32,
    }
}

/// GCM 的附加认证数据（`SyncProtocol.aad`）。v2 绑方向。
#[uniffi::export]
pub fn ffi_sync_protocol_aad(session_id: String, direction: FfiSyncDirection) -> Vec<u8> {
    core_offer::protocol::aad(&session_id, direction.to_core())
}

/// 登录同步二维码的内容（`SyncOffer.content`）。
#[uniffi::export]
pub fn ffi_sync_offer_content(offer: FfiSyncOffer) -> String {
    offer.content()
}

/// 登录同步二维码的路径 / URL / AAD。
#[uniffi::export]
pub fn ffi_sync_offer_path(offer: FfiSyncOffer) -> FfiResult<String> {
    let inner = offer_inner(&offer)?;
    Ok(inner.path())
}

#[uniffi::export]
pub fn ffi_sync_offer_url(offer: FfiSyncOffer) -> FfiResult<String> {
    let inner = offer_inner(&offer)?;
    Ok(inner.url())
}

#[uniffi::export]
pub fn ffi_sync_offer_aad(offer: FfiSyncOffer) -> FfiResult<Vec<u8>> {
    let inner = offer_inner(&offer)?;
    Ok(inner.aad())
}

fn offer_inner(offer: &FfiSyncOffer) -> FfiResult<core_offer::SyncOffer> {
    let port: u16 = u16::try_from(offer.port).map_err(|_| FfiSyncError::internal("端口不合法"))?;
    core_offer::SyncOffer::new(
        &offer.host,
        port,
        &offer.session_id,
        &offer.key,
        offer.version,
        offer.direction.to_core(),
    )
    .ok_or_else(|| FfiSyncError::Format {
        detail: "同步二维码无效，请重新生成".to_string(),
    })
}

/// 解析二维码内容（`SyncOffer.parse`）；任何一处不对都是 `None`。
#[uniffi::export]
pub fn ffi_sync_offer_parse(content: String) -> Option<FfiSyncOffer> {
    core_offer::SyncOffer::parse(&content).map(|offer| FfiSyncOffer {
        host: offer.host.clone(),
        port: offer.port as u32,
        session_id: offer.session_id.clone(),
        key: offer.key(),
        version: offer.version,
        direction: FfiSyncDirection::from_core(offer.direction),
    })
}

/// 这个字符串是不是我们这种二维码（别的二维码一律不理）。
#[uniffi::export]
pub fn ffi_sync_offer_looks_like(content: String) -> bool {
    core_offer::SyncOffer::looks_like_sync(&content)
}

/// 配对二维码（`PairingOffer`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncPairingOffer {
    pub host: String,
    pub port: u32,
    pub session_id: String,
    pub key: Vec<u8>,
    pub version: i32,
}

/// 配对协议的字段（`PairProtocol`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncPairingProtocol {
    pub version: i32,
    pub qr_prefix: String,
    pub path_prefix: String,
    pub ttl_ms: i64,
    pub key_bytes: u32,
    pub session_bytes: u32,
    pub nonce_bytes: u32,
    pub max_message_bytes: u32,
    pub max_body_bytes: u32,
    pub name_max: u32,
}

#[uniffi::export]
pub fn ffi_sync_pairing_protocol() -> FfiSyncPairingProtocol {
    FfiSyncPairingProtocol {
        version: core_pairing::protocol::VERSION,
        qr_prefix: core_pairing::protocol::QR_PREFIX.to_string(),
        path_prefix: core_pairing::protocol::PATH_PREFIX.to_string(),
        ttl_ms: core_pairing::protocol::TTL_MS,
        key_bytes: core_pairing::protocol::KEY_BYTES as u32,
        session_bytes: core_pairing::protocol::SESSION_BYTES as u32,
        nonce_bytes: core_pairing::protocol::NONCE_BYTES as u32,
        max_message_bytes: core_pairing::protocol::MAX_MESSAGE_BYTES as u32,
        max_body_bytes: core_pairing::protocol::MAX_BODY_BYTES as u32,
        name_max: core_pairing::protocol::NAME_MAX as u32,
    }
}

/// 配对二维码的 AAD。
#[uniffi::export]
pub fn ffi_sync_pairing_aad(session_id: String) -> Vec<u8> {
    core_pairing::protocol::aad(&session_id)
}

fn pairing_offer_inner(offer: &FfiSyncPairingOffer) -> FfiResult<core_pairing::PairingOffer> {
    let port: u16 = u16::try_from(offer.port).map_err(|_| FfiSyncError::internal("端口不合法"))?;
    core_pairing::pairing_offer(&offer.host, port, &offer.session_id, &offer.key)
        .ok_or_else(|| FfiSyncError::Format {
            detail: "配对二维码无效，请重新生成".to_string(),
        })
}

/// 配对二维码的内容。
#[uniffi::export]
pub fn ffi_sync_pairing_offer_content(offer: FfiSyncPairingOffer) -> FfiResult<String> {
    Ok(pairing_offer_inner(&offer)?.content())
}

#[uniffi::export]
pub fn ffi_sync_pairing_offer_path(offer: FfiSyncPairingOffer) -> FfiResult<String> {
    Ok(pairing_offer_inner(&offer)?.path())
}

#[uniffi::export]
pub fn ffi_sync_pairing_offer_url(offer: FfiSyncPairingOffer) -> FfiResult<String> {
    Ok(pairing_offer_inner(&offer)?.url())
}

#[uniffi::export]
pub fn ffi_sync_pairing_offer_aad(offer: FfiSyncPairingOffer) -> FfiResult<Vec<u8>> {
    Ok(pairing_offer_inner(&offer)?.aad())
}

/// 解析配对二维码；任何一处不对都是 `None`。
#[uniffi::export]
pub fn ffi_sync_pairing_offer_parse(content: String) -> Option<FfiSyncPairingOffer> {
    core_pairing::PairingOffer::parse(&content).map(|offer| FfiSyncPairingOffer {
        host: offer.host.clone(),
        port: offer.port as u32,
        session_id: offer.session_id.clone(),
        key: offer.key(),
        version: offer.version,
    })
}

/// 这个字符串是不是配对二维码。
#[uniffi::export]
pub fn ffi_sync_pairing_offer_looks_like(content: String) -> bool {
    core_pairing::PairingOffer::looks_like_pairing(&content)
}

/// 配对报文（`PairingMessage`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncPairingMessage {
    pub v: i32,
    pub device: String,
    pub name: String,
    pub public_key: String,
    pub nonce: String,
}

/// 配对结果（`PairingResult` + `PairingPeer`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncPairingResult {
    pub device: String,
    pub name: String,
    pub public_key: Vec<u8>,
    pub code: String,
}

#[uniffi::export]
pub fn ffi_sync_pairing_message_encode(message: FfiSyncPairingMessage) -> Vec<u8> {
    core_pairing::codec::encode(&pairing_message_from_ffi(&message))
}

#[uniffi::export]
pub fn ffi_sync_pairing_message_decode(bytes: Vec<u8>) -> Option<FfiSyncPairingMessage> {
    core_pairing::codec::decode(&bytes).map(|message| FfiSyncPairingMessage {
        v: message.v,
        device: message.device,
        name: message.name,
        public_key: message.public_key,
        nonce: message.nonce,
    })
}

/// 本机的一条报文（新随机数 `nonce` 由调用方给，16 字节）。
#[uniffi::export]
pub fn ffi_sync_pairing_message_own(
    device_id: String,
    name: String,
    public_key: Vec<u8>,
    nonce: Vec<u8>,
) -> FfiSyncPairingMessage {
    let message = core_pairing::codec::own(&device_id, &name, &public_key, &nonce);
    FfiSyncPairingMessage {
        v: message.v,
        device: message.device,
        name: message.name,
        public_key: message.public_key,
        nonce: message.nonce,
    }
}

/// 设备名清洗（`PairingCodec.cleanName`）。
#[uniffi::export]
pub fn ffi_sync_clean_device_name(name: String) -> String {
    core_pairing::codec::clean_name(&name)
}

/// 两台设备的配对结果（对端 + 6 位码）。设备 id 撞上 / 公钥长度不对 → `None`。
#[uniffi::export]
pub fn ffi_sync_pair_result(
    own: FfiSyncPairingMessage,
    peer: FfiSyncPairingMessage,
) -> Option<FfiSyncPairingResult> {
    let result = core_pairing::pair_result(&pairing_message_from_ffi(&own), &pairing_message_from_ffi(&peer))?;
    Some(FfiSyncPairingResult {
        device: result.peer.device,
        name: result.peer.name,
        public_key: result.peer.public_key,
        code: result.code,
    })
}

fn pairing_message_from_ffi(message: &FfiSyncPairingMessage) -> core_pairing::PairingMessage {
    core_pairing::PairingMessage {
        v: message.v,
        device: message.device.clone(),
        name: message.name.clone(),
        public_key: message.public_key.clone(),
        nonce: message.nonce.clone(),
    }
}

/// 只给测试用：`json` 的规范化打印（确认 `item_json` 过边界不掉信息）。
#[uniffi::export]
pub fn ffi_sync_normalize_json(text: String) -> FfiResult<String> {
    core_json::parse(&text)
        .map(|value| core_json::to_json_string(&value))
        .map_err(|detail| FfiSyncError::Format { detail })
}

// ===========================================================================
// 手动导出 / 导入的编排
// ===========================================================================

/// 一台"已经配对过"的设备（`SyncPeer` 里编排用得到的部分）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncPeer {
    pub device_id: String,
    /// 公钥（base64url，无填充）。
    pub public_key: String,
}

/// 导出计划：收件人（**本机在最前面**）+ 跳过了几台。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncPairedExportPlan {
    pub recipients: Vec<FfiSyncRecipient>,
    pub skipped_peers: u32,
}

/// **发给全部已配对设备**的导出计划（Kotlin `LibrarySync.exportToPairedDevices` 的前半）。
///
/// 失败时返回的字符串就是界面上那句话（和 Kotlin 原话一致）。
#[uniffi::export]
pub fn ffi_sync_plan_export_to_paired(
    self_id: String,
    self_public_key: String,
    peers: Vec<FfiSyncPeer>,
) -> FfiResult<FfiSyncPairedExportPlan> {
    let peers: Vec<core_flow::Peer> = peers
        .into_iter()
        .map(|p| core_flow::Peer {
            device_id: p.device_id,
            public_key: p.public_key,
        })
        .collect();
    core_flow::plan_export_to_paired(&self_id, &self_public_key, &peers)
        .map(|plan| FfiSyncPairedExportPlan {
            recipients: plan
                .recipients
                .into_iter()
                .map(|r| FfiSyncRecipient {
                    device: r.device,
                    public_key: r.public_key,
                })
                .collect(),
            skipped_peers: plan.skipped_peers as u32,
        })
        .map_err(FfiSyncError::internal)
}

/// 密码够不够长（Kotlin `LibrarySync.MIN_PASSWORD_LENGTH`）。
#[uniffi::export]
pub fn ffi_sync_check_password(password_len: u32) -> FfiResult<()> {
    core_flow::check_password(password_len as usize).map_err(FfiSyncError::internal)
}

/// 同步密码的最短长度。
#[uniffi::export]
pub fn ffi_sync_min_password_length() -> u32 {
    core_flow::MIN_PASSWORD_LENGTH as u32
}

/// 只看文件头（Kotlin `LibrarySync.inspect`）。`mode` 是 `"recipients"` / `"password"`。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncInspection {
    pub mode: String,
    pub recipients: Vec<String>,
    /// 文件里有本机的收件条目 → 可以用本机密钥直接解，不问密码。
    pub for_this_device: bool,
    /// 文件头都读不出来时的错误。
    pub error: Option<String>,
}

#[uniffi::export]
pub fn ffi_sync_inspect(bytes: Vec<u8>, self_id: Option<String>) -> FfiSyncInspection {
    let view = core_flow::inspect(&bytes, self_id.as_deref());
    FfiSyncInspection {
        mode: match view.mode {
            core_file::Mode::Recipients => "recipients".to_string(),
            core_file::Mode::Password => "password".to_string(),
        },
        recipients: view.recipients,
        for_this_device: view.for_this_device,
        error: view.error,
    }
}

/// 解开一份同步文件的结果（Kotlin `LibrarySync.preview`）。
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiSyncUnlockOutcome {
    /// 解开了。
    Opened {
        snapshot: FfiSyncSnapshot,
        /// `"device"` / `"password"`（怎么解开的）。
        unlock: String,
    },
    /// 这是密码文件：界面弹密码框，输完再 `preview(password)`。
    NeedsPassword,
    /// 打不开（文件坏了 / 不是发给这台设备的 / 密钥不在 / 头尾对不上）。
    Failed { message: String },
}

/// 解开一份同步文件（不要密码的按设备文件 / 密码文件给上密码）。
#[uniffi::export]
pub fn ffi_sync_preview(
    bytes: Vec<u8>,
    self_id: Option<String>,
    device_private_key: Option<Vec<u8>>,
    password: Option<String>,
) -> FfiSyncUnlockOutcome {
    match core_flow::preview(
        &bytes,
        self_id.as_deref(),
        device_private_key.as_deref(),
        password.as_deref(),
    ) {
        core_flow::UnlockOutcome::Opened { snapshot, unlock } => FfiSyncUnlockOutcome::Opened {
            snapshot: snapshot_to_ffi(&snapshot),
            unlock: match unlock {
                core_file::Unlock::DeviceKey => "device".to_string(),
                core_file::Unlock::Password => "password".to_string(),
            },
        },
        core_flow::UnlockOutcome::NeedsPassword => FfiSyncUnlockOutcome::NeedsPassword,
        core_flow::UnlockOutcome::Failed(message) => FfiSyncUnlockOutcome::Failed { message },
    }
}

/// 导出的默认文件名：`feedhub-yyyyMMdd-HHmm.fhshare`（本地时间的年月日时分由宿主算好传进来）。
#[uniffi::export]
pub fn ffi_sync_default_file_name(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> String {
    core_flow::default_file_name(year, month, day, hour, minute)
}

/// 本机"只保留最近 N 天"的截止时间；没开就是 `None`。
#[uniffi::export]
pub fn ffi_sync_retention_cutoff(history_limit_on: bool, history_days: i64, now_ms: i64) -> Option<i64> {
    core_flow::retention_cutoff(history_limit_on, history_days, now_ms)
}

