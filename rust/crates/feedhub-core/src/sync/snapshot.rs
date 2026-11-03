//! **快照模型 + 编解码 + 坏条目清洗**（`core/sync/LibrarySyncModel.kt` 的同步四张表 +
//! `LibrarySnapshotCodec.kt`）。
//!
//! ## 编码口径（这是「两边编码逐字节相同」那道门）
//!
//! Kotlin 的 `LibrarySnapshotCodec` 用的是
//! `Json { ignoreUnknownKeys = true; encodeDefaults = false; explicitNulls = false }`，
//! 所以：
//!
//! - 字段顺序 = `LibrarySnapshot` / `Sync*Row` 的**声明顺序**（kotlinx 按声明顺序写）；
//! - 等于默认值的字段**不写**（`magic` / `v` / `schema` / `deleted = false` / `progressHlc = 0`……）；
//! - `null` 字段**不写**（`folderUid` / `item` / `user` / `progressMs` / `durationMs`）；
//! - 四个 list 字段**总是写**（空就是 `[]`；它们的默认值虽然是 `emptyList()`，但 kotlinx 只对
//!   **标量**默认值省略……不对，`encodeDefaults = false` 对集合也省略。所以 `folders` 等为空时**不写**，
//!   和 Kotlin 一致，见下面的 `field_list_non_empty`）；
//! - 字符串按 [`super::json::push_quoted`] 的转义（`\u00xx` 小写、非 ASCII 原样）；
//! - `item` / `user` 是**透传的 JSON 子树**，按解析时的键顺序 + 原始数字字面量写回。
//!
//! ## 解码口径
//!
//! 版本 / magic / schema 任何一个不对就**整份拒**（`LibSyncError::Format`），未知字段忽略，
//! 尺寸上限见 [`LIBRARY_SYNC_MAX_ENTRIES`] / [`LIBRARY_SYNC_MAX_ITEM_BYTES`]。
//! 缺字段用 Kotlin 的默认值补（`deleted = false`、`progressHlc = 0` 等）。

use super::json::{self, ObjectWriter, Value};

/// 文件 / 报文的开头（`LIBRARY_SYNC_MAGIC`）。
pub const LIBRARY_SYNC_MAGIC: &str = "feedhub-sync";
/// 协议版本（`LIBRARY_SYNC_PROTOCOL`）。**不动**：老版本 App 要能读新版本导出的文件。
pub const LIBRARY_SYNC_PROTOCOL: i64 = 1;
/// 本地库表结构的版本（`LIBRARY_SYNC_SCHEMA`，和 `SqliteLibrary.SCHEMA_VERSION` 一致）。
pub const LIBRARY_SYNC_SCHEMA: i64 = 4;
/// 一份快照里最多多少条记录（收藏 + 历史 + 收藏夹 + 屏蔽）。
pub const LIBRARY_SYNC_MAX_ENTRIES: usize = 50_000;
/// 单条 `item` 的 JSON 上限（**字节**）；超出的这一条跳过并计数。
pub const LIBRARY_SYNC_MAX_ITEM_BYTES: usize = 64 * 1024;

/// 一个收藏夹。`uid` 跨设备稳定（v4 迁移时新建的 UUID 字符串），本地整数 id 不进快照。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncFolderRow {
    pub uid: String,
    pub name: String,
    pub created_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
}

/// 一条收藏。`folder_uid = None` = 默认收藏夹。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncFavoriteRow {
    pub key: String,
    pub folder_uid: Option<String>,
    pub added_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
    /// 透传的 `item` JSON 子树（没有就是 `None`）。
    pub item: Option<Value>,
}

/// 一条历史。访问时间和播放进度**分开定序**。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncHistoryRow {
    pub key: String,
    pub visited_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
    pub progress_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub progress_hlc: i64,
    pub progress_dev: String,
    pub item: Option<Value>,
}

/// 一个屏蔽的用户。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncBlockedRow {
    pub key: String,
    pub user: Option<Value>,
    pub blocked_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
}

/// 合并用的四张表（含墓碑）：没有报文元数据，纯数据（`SyncState`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncState {
    pub history_cleared_hlc: i64,
    pub folders: Vec<SyncFolderRow>,
    pub favorites: Vec<SyncFavoriteRow>,
    pub history: Vec<SyncHistoryRow>,
    pub blocked: Vec<SyncBlockedRow>,
}

impl SyncState {
    /// 四张表加起来多少条（`LibrarySnapshot.entryCount` 的同名口径）。
    pub fn entry_count(&self) -> usize {
        self.folders.len() + self.favorites.len() + self.history.len() + self.blocked.len()
    }
}

/// 一份快照（`LibrarySnapshot`）：四张表 + 报文元数据。
#[derive(Debug, Clone, PartialEq)]
pub struct LibrarySnapshot {
    pub magic: String,
    pub v: i64,
    pub schema: i64,
    /// 生成这份快照的设备 id（8 字节 base64url）。
    pub device: String,
    /// 这台设备单调递增的快照序号（防"把网盘里的旧文件放回来"）。
    pub seq: i64,
    pub generated_at: i64,
    pub history_cleared_hlc: i64,
    pub state: SyncState,
}

impl Default for LibrarySnapshot {
    fn default() -> Self {
        LibrarySnapshot {
            magic: LIBRARY_SYNC_MAGIC.to_string(),
            v: LIBRARY_SYNC_PROTOCOL,
            schema: LIBRARY_SYNC_SCHEMA,
            device: String::new(),
            seq: 0,
            generated_at: 0,
            history_cleared_hlc: 0,
            state: SyncState::default(),
        }
    }
}

impl LibrarySnapshot {
    /// 只取可以合并的那部分（不含 magic / v / schema / seq 这些报文元数据）。
    pub fn to_state(&self) -> SyncState {
        SyncState {
            history_cleared_hlc: self.history_cleared_hlc,
            ..self.state.clone()
        }
    }

    pub fn from_state(state: SyncState, device: &str, seq: i64, generated_at: i64) -> Self {
        LibrarySnapshot {
            magic: LIBRARY_SYNC_MAGIC.to_string(),
            v: LIBRARY_SYNC_PROTOCOL,
            schema: LIBRARY_SYNC_SCHEMA,
            device: device.to_string(),
            seq,
            generated_at,
            history_cleared_hlc: state.history_cleared_hlc,
            state,
        }
    }
}

/// 同步层的错误。**消息可以直接给用户看**（不带条目内容 / 密钥 / Cookie）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LibSyncError {
    /// 文件 / 报文格式不对（不是同步文件、版本不认、太大……）。
    #[error("{0}")]
    Format(String),
}

pub type Result<T> = std::result::Result<T, LibSyncError>;

fn format_err<T>(message: impl Into<String>) -> Result<T> {
    Err(LibSyncError::Format(message.into()))
}

// ===========================================================================
// 编码
// ===========================================================================

/// 把快照编成 UTF-8 的 JSON 字节。**和 `LibrarySnapshotCodec.encode` 逐字节相同**。
pub fn encode(snapshot: &LibrarySnapshot) -> Vec<u8> {
    let mut w = ObjectWriter::new();
    // 声明顺序：magic, v, schema, device, seq, generatedAt, historyClearedHlc, folders, favorites, history, blocked
    w.field_str_non_default("magic", &snapshot.magic, LIBRARY_SYNC_MAGIC);
    w.field_i64_non_default("v", snapshot.v, LIBRARY_SYNC_PROTOCOL);
    w.field_i64_non_default("schema", snapshot.schema, LIBRARY_SYNC_SCHEMA);
    // device / seq / generatedAt / historyClearedHlc 的默认值是 "" / 0 / 0 / 0
    w.field_str_non_default("device", &snapshot.device, "");
    w.field_i64_non_default("seq", snapshot.seq, 0);
    w.field_i64_non_default("generatedAt", snapshot.generated_at, 0);
    w.field_i64_non_default("historyClearedHlc", snapshot.history_cleared_hlc, 0);
    field_list(&mut w, "folders", &snapshot.state.folders, encode_folder);
    field_list(&mut w, "favorites", &snapshot.state.favorites, encode_favorite);
    field_list(&mut w, "history", &snapshot.state.history, encode_history);
    field_list(&mut w, "blocked", &snapshot.state.blocked, encode_blocked);
    w.finish().into_bytes()
}

/// 一个 list 字段：空就**不写**（`encodeDefaults = false` 对集合也生效），非空写成数组。
fn field_list<T>(w: &mut ObjectWriter, name: &str, items: &[T], encode_one: fn(&T) -> String) {
    if items.is_empty() {
        return;
    }
    let encoded: Vec<String> = items.iter().map(encode_one).collect();
    w.field_array(name, &encoded);
}

fn encode_folder(row: &SyncFolderRow) -> String {
    let mut w = ObjectWriter::new();
    // uid, name, createdAt, revHlc, revDev, deleted
    w.field_str("uid", &row.uid);
    w.field_str("name", &row.name);
    w.field_i64_non_default("createdAt", row.created_at, 0);
    w.field_i64_non_default("revHlc", row.rev_hlc, 0);
    w.field_str("revDev", &row.rev_dev);
    w.field_bool_non_default("deleted", row.deleted, false);
    w.finish()
}

fn encode_favorite(row: &SyncFavoriteRow) -> String {
    let mut w = ObjectWriter::new();
    // key, folderUid, addedAt, revHlc, revDev, deleted, item
    w.field_str("key", &row.key);
    w.field_opt_str("folderUid", row.folder_uid.as_deref());
    w.field_i64_non_default("addedAt", row.added_at, 0);
    w.field_i64_non_default("revHlc", row.rev_hlc, 0);
    w.field_str("revDev", &row.rev_dev);
    w.field_bool_non_default("deleted", row.deleted, false);
    if let Some(item) = &row.item {
        w.field_raw("item", &json::to_json_string(item));
    }
    w.finish()
}

fn encode_history(row: &SyncHistoryRow) -> String {
    let mut w = ObjectWriter::new();
    // key, visitedAt, revHlc, revDev, deleted, progressMs, durationMs, progressHlc, progressDev, item
    w.field_str("key", &row.key);
    w.field_i64_non_default("visitedAt", row.visited_at, 0);
    w.field_i64_non_default("revHlc", row.rev_hlc, 0);
    w.field_str("revDev", &row.rev_dev);
    w.field_bool_non_default("deleted", row.deleted, false);
    w.field_opt_i64("progressMs", row.progress_ms);
    w.field_opt_i64("durationMs", row.duration_ms);
    w.field_i64_non_default("progressHlc", row.progress_hlc, 0);
    w.field_str_non_default("progressDev", &row.progress_dev, "");
    if let Some(item) = &row.item {
        w.field_raw("item", &json::to_json_string(item));
    }
    w.finish()
}

fn encode_blocked(row: &SyncBlockedRow) -> String {
    let mut w = ObjectWriter::new();
    // key, user, blockedAt, revHlc, revDev, deleted
    w.field_str("key", &row.key);
    if let Some(user) = &row.user {
        w.field_raw("user", &json::to_json_string(user));
    }
    w.field_i64_non_default("blockedAt", row.blocked_at, 0);
    w.field_i64_non_default("revHlc", row.rev_hlc, 0);
    w.field_str("revDev", &row.rev_dev);
    w.field_bool_non_default("deleted", row.deleted, false);
    w.finish()
}

// ===========================================================================
// 解码
// ===========================================================================

/// 解一份快照的字节（`LibrarySnapshotCodec.decode`）。**宁严勿松**：magic / 协议版本 / 资料库版本
/// 任何一个不对就整份拒；未知字段忽略。
pub fn decode(bytes: &[u8]) -> Result<LibrarySnapshot> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| LibSyncError::Format("文件内容不是有效的同步快照（不是 UTF-8）".to_string()))?;
    let root = json::parse(text)
        .map_err(|e| LibSyncError::Format(format!("文件内容不是有效的同步快照（{e}）")))?;
    let snapshot = decode_snapshot(&root)?;
    check_header(&snapshot.magic, snapshot.v, snapshot.schema)?;
    if snapshot.state.entry_count() > LIBRARY_SYNC_MAX_ENTRIES {
        return format_err(format!(
            "文件里的条目太多（{} 条，最多 {} 条）",
            snapshot.state.entry_count(),
            LIBRARY_SYNC_MAX_ENTRIES
        ));
    }
    if is_blank(&snapshot.device) {
        return format_err("快照没有设备标识");
    }
    Ok(snapshot)
}

fn decode_snapshot(root: &Value) -> Result<LibrarySnapshot> {
    let Value::Object(_) = root else {
        return format_err("文件内容不是有效的同步快照（不是 JSON 对象）");
    };
    let mut snapshot = LibrarySnapshot {
        magic: str_or(root, "magic", LIBRARY_SYNC_MAGIC),
        v: i64_or(root, "v", LIBRARY_SYNC_PROTOCOL),
        schema: i64_or(root, "schema", LIBRARY_SYNC_SCHEMA),
        device: str_or(root, "device", ""),
        seq: i64_or(root, "seq", 0),
        generated_at: i64_or(root, "generatedAt", 0),
        history_cleared_hlc: i64_or(root, "historyClearedHlc", 0),
        state: SyncState::default(),
    };
    snapshot.state.history_cleared_hlc = snapshot.history_cleared_hlc;
    snapshot.state.folders = decode_list(root, "folders", decode_folder)?;
    snapshot.state.favorites = decode_list(root, "favorites", decode_favorite)?;
    snapshot.state.history = decode_list(root, "history", decode_history)?;
    snapshot.state.blocked = decode_list(root, "blocked", decode_blocked)?;
    Ok(snapshot)
}

fn decode_list<T>(root: &Value, name: &str, decode_one: fn(&Value) -> Result<T>) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for item in root.array_field(name) {
        out.push(decode_one(item)?);
    }
    Ok(out)
}

fn decode_folder(v: &Value) -> Result<SyncFolderRow> {
    require_object(v, "收藏夹")?;
    Ok(SyncFolderRow {
        uid: str_or(v, "uid", ""),
        name: str_or(v, "name", ""),
        created_at: i64_or(v, "createdAt", 0),
        rev_hlc: i64_or(v, "revHlc", 0),
        rev_dev: str_or(v, "revDev", ""),
        deleted: bool_or(v, "deleted", false),
    })
}

fn decode_favorite(v: &Value) -> Result<SyncFavoriteRow> {
    require_object(v, "收藏")?;
    Ok(SyncFavoriteRow {
        key: str_or(v, "key", ""),
        folder_uid: opt_str(v, "folderUid"),
        added_at: i64_or(v, "addedAt", 0),
        rev_hlc: i64_or(v, "revHlc", 0),
        rev_dev: str_or(v, "revDev", ""),
        deleted: bool_or(v, "deleted", false),
        item: opt_value(v, "item"),
    })
}

fn decode_history(v: &Value) -> Result<SyncHistoryRow> {
    require_object(v, "历史")?;
    Ok(SyncHistoryRow {
        key: str_or(v, "key", ""),
        visited_at: i64_or(v, "visitedAt", 0),
        rev_hlc: i64_or(v, "revHlc", 0),
        rev_dev: str_or(v, "revDev", ""),
        deleted: bool_or(v, "deleted", false),
        progress_ms: opt_i64(v, "progressMs"),
        duration_ms: opt_i64(v, "durationMs"),
        progress_hlc: i64_or(v, "progressHlc", 0),
        progress_dev: str_or(v, "progressDev", ""),
        item: opt_value(v, "item"),
    })
}

fn decode_blocked(v: &Value) -> Result<SyncBlockedRow> {
    require_object(v, "屏蔽")?;
    Ok(SyncBlockedRow {
        key: str_or(v, "key", ""),
        user: opt_value(v, "user"),
        blocked_at: i64_or(v, "blockedAt", 0),
        rev_hlc: i64_or(v, "revHlc", 0),
        rev_dev: str_or(v, "revDev", ""),
        deleted: bool_or(v, "deleted", false),
    })
}

/// kotlinx 的类型错误是"整份解不开"（`SerializationException` → `LibrarySyncFormatException`），
/// 所以字段类型不对也拒整份。
fn require_object(v: &Value, what: &str) -> Result<()> {
    match v {
        Value::Object(_) => Ok(()),
        _ => format_err(format!("文件内容不是有效的同步快照（{what}那一条不是对象）")),
    }
}

fn str_or(v: &Value, key: &str, default: &str) -> String {
    v.str_field(key).map(|s| s.to_string()).unwrap_or_else(|| default.to_string())
}

fn i64_or(v: &Value, key: &str, default: i64) -> i64 {
    v.i64_field(key).unwrap_or(default)
}

fn bool_or(v: &Value, key: &str, default: bool) -> bool {
    v.bool_field(key).unwrap_or(default)
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|f| f.as_str()).map(|s| s.to_string())
}

fn opt_i64(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|f| f.as_i64())
}

/// 取一个透传的 JSON 子树；缺字段 / 显式 `null` 都是 `None`。
fn opt_value(v: &Value, key: &str) -> Option<Value> {
    v.get(key).filter(|f| !f.is_null()).cloned()
}

/// magic / 协议版本 / 资料库版本（`LibrarySnapshotCodec.checkHeader`）。
pub fn check_header(magic: &str, v: i64, schema: i64) -> Result<()> {
    if magic != LIBRARY_SYNC_MAGIC {
        return format_err("不是 feedhub 同步文件");
    }
    if v != LIBRARY_SYNC_PROTOCOL {
        return format_err(format!(
            "同步文件的协议版本是 {v}，这个程序只认 {LIBRARY_SYNC_PROTOCOL}（请升级 App）"
        ));
    }
    if schema != LIBRARY_SYNC_SCHEMA {
        return format_err(format!(
            "同步文件来自资料库版本 {schema}，这个程序只认 {LIBRARY_SYNC_SCHEMA}"
        ));
    }
    Ok(())
}

// ===========================================================================
// 坏条目清洗（LibrarySnapshotSanitizer）
// ===========================================================================

/// 清洗结果：干净的快照 + 跳过了多少条。
#[derive(Debug, Clone, PartialEq)]
pub struct SanitizedSnapshot {
    pub snapshot: LibrarySnapshot,
    pub skipped: usize,
}

/// 把远端快照里"塞不进去 / 解不开"的条目剔掉，**只影响这一条**：
///
/// - 收藏夹：`uid` / `name` / `revDev` 有一个空 → 跳过；
/// - 收藏 / 历史：`key` / `revDev` 空，或者"活着但 `item` 不是一个能用的 JSON 对象"→ 跳过；
/// - 屏蔽：`key` / `revDev` 空，或者"活着但 `user` 不是一个 JSON 对象"→ 跳过。
///
/// **同一个 key / uid 出现两次**时后一个覆盖前一个（Kotlin 用的是 `LinkedHashMap`：`put` 已存在的键
/// 不改变迭代位置）。清洗完还超上限就整份拒。
///
/// 和 Kotlin 唯一的差别：Kotlin 还要把 `item` 解成 `FeedItem` / `user` 解成 `UserRef`（那是
/// `kotlinx.serialization` 的事，Rust 里没有 `FeedItem` 的派生解码器）。所以 Rust 这层只查
/// **"是不是 JSON 对象" + 字节上限**，更严的那一步由宿主的 `FfiSyncSanitizeHost` 回调补上——
/// 一侧的判断只可能更严，不可能更松，对拍里逐条对得上（`RustSyncDualRunTest`）。
pub fn sanitize(snapshot: &LibrarySnapshot) -> SanitizedSnapshot {
    sanitize_with(snapshot, &|_| true, &|_| true)
}

/// 带宿主回调的清洗：`valid_item` / `valid_user` 用来看"这个子树能不能解成 `FeedItem` / `UserRef`"。
/// 两个回调**只在**"行是活的、且是 JSON 对象、且没超字节上限"之后才会被问（和 Kotlin 的判断顺序一致）。
pub fn sanitize_with(
    snapshot: &LibrarySnapshot,
    valid_item: &dyn Fn(&Value) -> bool,
    valid_user: &dyn Fn(&Value) -> bool,
) -> SanitizedSnapshot {
    let mut skipped = 0usize;

    // 用一个"保持首次出现位置、后值覆盖"的表（= Kotlin 的 LinkedHashMap）
    let mut folders: Vec<SyncFolderRow> = Vec::new();
    for row in &snapshot.state.folders {
        if super::text::is_blank(&row.uid) || super::text::is_blank(&row.name) || super::text::is_blank(&row.rev_dev) {
            skipped += 1;
            continue;
        }
        upsert(&mut folders, row, |r| r.uid.as_str(), |r| r.uid.clone());
    }

    let mut favorites: Vec<SyncFavoriteRow> = Vec::new();
    for row in &snapshot.state.favorites {
        if super::text::is_blank(&row.key)
            || super::text::is_blank(&row.rev_dev)
            || (!row.deleted && !item_ok(&row.item, valid_item))
        {
            skipped += 1;
            continue;
        }
        upsert(&mut favorites, row, |r| r.key.as_str(), |r| r.key.clone());
    }

    let mut history: Vec<SyncHistoryRow> = Vec::new();
    for row in &snapshot.state.history {
        if super::text::is_blank(&row.key)
            || super::text::is_blank(&row.rev_dev)
            || (!row.deleted && !item_ok(&row.item, valid_item))
        {
            skipped += 1;
            continue;
        }
        upsert(&mut history, row, |r| r.key.as_str(), |r| r.key.clone());
    }

    let mut blocked: Vec<SyncBlockedRow> = Vec::new();
    for row in &snapshot.state.blocked {
        if super::text::is_blank(&row.key)
            || super::text::is_blank(&row.rev_dev)
            || (!row.deleted && !user_ok(&row.user, valid_user))
        {
            skipped += 1;
            continue;
        }
        upsert(&mut blocked, row, |r| r.key.as_str(), |r| r.key.clone());
    }

    let mut clean = snapshot.clone();
    clean.state.folders = folders;
    clean.state.favorites = favorites;
    clean.state.history = history;
    clean.state.blocked = blocked;
    SanitizedSnapshot {
        snapshot: clean,
        skipped,
    }
}

/// 按 key 覆盖 / 追加：已有同 key 的那一行换成新的（位置不变）。
fn upsert<T: Clone>(
    rows: &mut Vec<T>,
    row: &T,
    key_of: impl Fn(&T) -> &str,
    own_key: impl Fn(&T) -> String,
) {
    let key = own_key(row);
    // 先按 key 找（不能借用 rows 的字段，因为紧接着要可变借用）
    let index = rows.iter().position(|r| key_of(r) == key);
    match index {
        Some(i) => rows[i] = row.clone(),
        None => rows.push(row.clone()),
    }
}

/// `item` 能不能要：必须是 JSON 对象，序列化后不超过 [`LIBRARY_SYNC_MAX_ITEM_BYTES`] 字节，
/// 再交给宿主回调（"能不能解成 `FeedItem`"）。
fn item_ok(item: &Option<Value>, valid: &dyn Fn(&Value) -> bool) -> bool {
    let Some(value) = item else { return false };
    if !matches!(value, Value::Object(_)) {
        return false;
    }
    if json::to_json_string(value).len() > LIBRARY_SYNC_MAX_ITEM_BYTES {
        return false;
    }
    valid(value)
}

fn user_ok(user: &Option<Value>, valid: &dyn Fn(&Value) -> bool) -> bool {
    let Some(value) = user else { return false };
    if !matches!(value, Value::Object(_)) {
        return false;
    }
    valid(value)
}

// ===========================================================================
// 小工具（text 的转发，避免上层多 import 一个模块）
// ===========================================================================

use super::text::is_blank;

