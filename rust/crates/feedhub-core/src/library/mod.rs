//! 本地资料库（收藏 / 历史 / 屏蔽 / 行为数据 / 向量 / 跨设备同步的四张表）：Rust + SQLite。
//!
//! **同一个数据库文件、同一套表结构**：桌面是 `~/.config/feedhub/library.db`，安卓是
//! `<filesDir>/library/library.db`。表结构、列名、索引、`PRAGMA user_version` 和 Kotlin 版
//! `SqliteLibrary` 完全一致（见 `schema.rs`），所以用户已有的库**原地打开就能用**，不迁移不丢数据；
//! cli 的只读实现（`cli/LibraryRead.kt`，自己拼 SQL 读同一个文件）也照旧能读。
//!
//! ## 边界和口径
//!
//! - **JSON 留在 Kotlin**：`item_json` / `user_json` 从这里过边界时是**字符串**，由 Kotlin 的
//!   `FeedJson` 编解码。好处是"存进去的字节"和以前一模一样（`.dump` 逐行相同），也不用把
//!   `FeedItem` 的序列化在 Rust 里再写一遍（那是两个实现、两份漂移风险）。
//! - **列值由调用方算好**：`platform` / `is_video` / `search_text` / `is_adult` 是 Kotlin 按
//!   `FeedItem` 算出来再传的（`Platform.key` / `isVideoLike()` / `searchText()`）。Rust 只负责落列，
//!   不猜怎么从 JSON 里推——推错一列，搜索和 18+ 过滤就会漂。
//! - **时间和随机量从外面给**：`now_ms` / `rev`（HLC）/ 新建收藏夹的 uid / 迁移用的随机种子都由宿主传，
//!   `.so` 里不读时钟、不取随机数。所以同一个操作序列在两份库上逐字节相同。
//! - **错误一律带上下文回去**：`LibError` 过 UniFFI 变成 `FfiLibError`，Kotlin 门面照旧"打日志 + 用内存里
//!   那份状态"（磁盘出问题不该让界面崩）。
//!
//! ## 线程模型（第二道验收：并发访问）
//!
//! 两个连接 + 各一把 `Mutex`（**写连接 / 读连接**），WAL 模式：
//!
//! - 写连接：所有 `INSERT` / `UPDATE` / `DELETE`，一行的提交是微秒级（WAL 下不 fsync），和以前一样在
//!   调用线程里同步做；`BEGIN IMMEDIATE` 显式开事务（迁移 / 多语句写 / 导入）。
//! - 读连接：列表查询（收藏 / 历史 / 曝光行 / 导出），**不挡写**，也拿不到写事务里没提交的中间状态——
//!   和 Kotlin 版两个连接的行为一致。列表查询本来就是 `suspend`（宿主的 `Dispatchers.IO` 上调进来）。
//! - 两把锁不会互等：调用顺序只有"写→读"（`apply_import` 在写锁里读一次本机状态），没有反过来的路径。
//! - `.so` 里**没有后台线程、没有定时器**：闲置零唤醒（和平台层一样）。
//! - `close()` 先 `wal_checkpoint(TRUNCATE)` 再关两个连接（和 Kotlin 版一样，关完主库文件才是完整的）。

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, TransactionBehavior};

mod schema;

pub use schema::SCHEMA_VERSION;

/// 资料库的错误。过 UniFFI 之后 Kotlin 只做一件事：打日志 + 退化到内存里那份状态（见 `RustLibrary` 的 `guarded`）。
#[derive(Debug, thiserror::Error)]
pub enum LibError {
    /// SQLite 自己的错误（磁盘满 / 库被删 / 违反了约束……）。
    #[error("数据库错误：{0}")]
    Sql(String),

    /// 库是更新版本的程序建的：**不打开、不改它**（Kotlin 侧抛 `IllegalStateException`，App 退回内存实现）。
    #[error("资料库是更新版本的程序建的（版本 {version}），这个程序只认到 {supported}")]
    Unsupported { version: i32, supported: i32 },

    /// 别的（参数不对 / 已经关了 / 内容解不开）。
    #[error("{0}")]
    Other(String),
}

impl From<rusqlite::Error> for LibError {
    fn from(e: rusqlite::Error) -> Self {
        LibError::Sql(e.to_string())
    }
}

// ===========================================================================
// 过边界用的行（字段和 SQL 的列一一对应，顺序照 SELECT）
// ===========================================================================

/// 一个收藏夹（`deleted = 0`）。
pub struct FolderRow {
    pub id: i64,
    pub name: String,
    /// 跨设备 uid（v4 起每本夹都有一个；理论上不会是 NULL，老库半截迁移时可能是）。
    pub uid: Option<String>,
}

/// 收藏在内存里要的那几列（`isFavorite` / `favoriteCount` 用，不读 JSON）。
pub struct FavoriteRefRow {
    pub key: String,
    pub folder_id: Option<i64>,
    pub adult: bool,
}

/// 一条屏蔽（`deleted = 0`）。
pub struct BlockedRow {
    pub user_json: String,
    pub blocked_at: i64,
}

/// 一条收藏的读取结果（`item_json` 由 Kotlin 解码）。
pub struct FavoriteRow {
    pub item_json: String,
    pub folder_id: Option<i64>,
    pub added_at: i64,
}

/// 一条历史。
pub struct HistoryRow {
    pub item_json: String,
    pub visited_at: i64,
    pub progress_ms: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// 一行曝光（导出用）。
pub struct ExposureRow {
    pub key: String,
    pub platform: String,
    pub surface: String,
    pub exposed_at: i64,
    pub last_seen_at: i64,
    pub dwell_ms: i64,
}

/// 一段时间的曝光统计（`clicked` 是"曝光之后在历史里被打开过"的条数）。
pub struct ExposureStats {
    pub exposures: i64,
    pub clicked: i64,
    pub dwell_ms: i64,
}

/// 一条"曝光过的卡片内容"。
pub struct SeenItemRow {
    pub item_json: String,
    pub first_seen_at: i64,
    pub feed_kind: String,
    pub batch_id: i64,
    pub platform_rank: i64,
}

/// 一条向量（`vec` 是 float16 的原始字节）。主键是 `(key, model)`（v9），同一个 key 的不同 model 各一行。
pub struct EmbeddingRow {
    pub key: String,
    pub model: String,
    pub dim: i64,
    pub vec: Vec<u8>,
    pub created_at: i64,
    pub source: String,
}

/// `(model, source)` 下的向量条数。
pub struct EmbeddingStat {
    pub model: String,
    pub source: String,
    pub count: i64,
}

/// 一条补算候选（收藏 / 历史里还没有这个模型向量的）。`item_json` 由 Kotlin 解码。
pub struct BackfillRow {
    pub key: String,
    pub item_json: String,
    pub at: i64,
    pub source: String,
}

/// 一台已配对设备。
pub struct PeerRow {
    pub device_id: String,
    pub name: String,
    pub public_key: String,
    pub paired_at: i64,
}

/// 快照里的一本收藏夹（含墓碑）。
pub struct SyncFolderRow {
    pub uid: String,
    pub name: String,
    pub created_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
}

/// 快照里的一条收藏（含墓碑）。`item_json` 是 `item_json` 列的原文（Kotlin 侧解析成 JSON 树）。
pub struct SyncFavoriteRow {
    pub key: String,
    pub folder_uid: Option<String>,
    pub added_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
    /// 要写回的 JSON 原文（读出来时一定在；写的时候 `None` = 只看墓碑）。
    pub item_json: Option<String>,
    /// Kotlin 把 `item_json` 解成 `FeedItem` 之后算好的那几列；解不出来是 `None`（那一行只处理墓碑）。
    pub columns: Option<ItemColumns>,
}

/// 快照里的一条历史（访问信息和播放进度分开定序，含墓碑）。
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
    /// 同 [`SyncFavoriteRow::item_json`]。
    pub item_json: Option<String>,
    /// 同 [`SyncFavoriteRow::columns`]。
    pub columns: Option<ItemColumns>,
}

/// 快照里的一条屏蔽（含墓碑）。
pub struct SyncBlockedRow {
    pub key: String,
    /// 要写回的 `UserRef` JSON（读出来时在；写的时候 `None` = 只看墓碑）。
    pub user_json: Option<String>,
    pub blocked_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
}

/// 全量（含墓碑）：导出和合并都用它。`item_json` / `user_json` 是**列原文**（Kotlin 负责解析）。
#[derive(Default)]
pub struct SyncState {
    pub history_cleared_hlc: i64,
    pub folders: Vec<SyncFolderRow>,
    pub favorites: Vec<SyncFavoriteRow>,
    pub history: Vec<SyncHistoryRow>,
    pub blocked: Vec<SyncBlockedRow>,
}

/// 合并结果（`LibraryMerge.merge` 在 Kotlin 里算，经过 [`MergeHost`] 回调进来）。
pub struct MergedState {
    pub state: SyncState,
    /// 本机收藏夹 uid → 合并后的 uid（同名认领）；正常情况下是空的。
    pub aliases: Vec<(String, String)>,
}

/// 远端快照的报文元数据（`apply_import` 要用 `sync_peer` 记下来）。
pub struct RemoteMeta {
    pub device: String,
    pub seq: i64,
    pub generated_at: i64,
}

/// 合并回调：**在写锁里**被调用，参数是刚读出来的本机状态和调用方给的远端状态。
///
/// 为什么要有这个回调（而不是让 Kotlin 先读本机状态、合并完再写回来）：合并必须和写入在同一个写锁 /
/// 同一个事务里，否则中间一次"存播放进度"就会被合并结果覆盖掉（Kotlin 版是 `synchronized(writeLock)`
/// 里 `readState()` + `merge` + `transaction { applyMerged }`）。实现方**不许**回调资料库自己的方法。
pub trait MergeHost: Send + Sync {
    fn merge(&self, local: &SyncState, remote: &SyncState, retention_cutoff_ms: Option<i64>) -> MergedState;
}

// ===========================================================================
// 打开 / 关闭
// ===========================================================================

pub struct Library {
    writer: Mutex<Option<Connection>>,
    reader: Mutex<Option<Connection>>,
    device_id: String,
}

impl Library {
    /// 打开（不存在就建）[path] 的资料库并跑到最新 schema。
    ///
    /// [device_hint]：fresh 库（v4 迁移）要一个 8 字节 base64url 设备 id，由宿主生成好传进来。
    /// [uid_seed]：v4 回填收藏夹 `sync_uid` 用的随机种子（宿主给 32 字节）。
    pub fn open(path: &str, device_hint: &str, uid_seed: &[u8]) -> Result<Self, LibError> {
        let mut writer = Connection::open(Path::new(path))?;
        let reader = Connection::open(Path::new(path))?;
        // WAL：列表查询（读连接）不挡写。synchronous=NORMAL：WAL 下不每次 fsync，和以前一样快。
        writer.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))?;
        writer.pragma_update(None, "synchronous", "NORMAL")?;
        writer.busy_timeout(Duration::from_millis(5000))?;
        reader.busy_timeout(Duration::from_millis(5000))?;
        schema::migrate(&mut writer, device_hint, uid_seed)?;
        let device_id = meta(&writer, "device_id")?
            .ok_or_else(|| LibError::Other("资料库缺少 device_id".to_string()))?;
        Ok(Library {
            writer: Mutex::new(Some(writer)),
            reader: Mutex::new(Some(reader)),
            device_id,
        })
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// 关掉：先 checkpoint（把 WAL 合回主库，之后主库文件才是完整的），再关两个连接。
    pub fn close(&self) {
        {
            let mut guard = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(conn) = guard.as_mut() {
                let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
            }
            let _ = guard.take();
        }
        let mut guard = self.reader.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = guard.take();
    }

    // ---------------------------------------------------------------- 打开时的一次性读取

    /// 本机 HLC 的起点：`sync_meta.last_hlc` 和四张表里最大的 rev 取最大（上次可能没来得及落盘）。
    pub fn last_hlc(&self) -> Result<i64, LibError> {
        self.with_writer(|conn| {
            let mut last = meta(conn, META_LAST_HLC)?
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            for sql in [
                "SELECT COALESCE(MAX(rev_hlc), 0) FROM favorite",
                "SELECT COALESCE(MAX(rev_hlc), 0) FROM favorite_folder",
                "SELECT COALESCE(MAX(MAX(rev_hlc), MAX(progress_hlc)), 0) FROM history",
                "SELECT COALESCE(MAX(rev_hlc), 0) FROM blocked_user",
            ] {
                last = last.max(conn.query_row(sql, [], |r| r.get(0))?);
            }
            Ok(last)
        })
    }

    /// 收藏夹（`deleted = 0`，按建立时间排）。默认收藏夹不在这里（它的 id 是 null）。
    pub fn folders(&self) -> Result<Vec<FolderRow>, LibError> {
        self.with_reader(|conn| {
            let mut stmt =
                conn.prepare("SELECT id, name, sync_uid FROM favorite_folder WHERE deleted = 0 ORDER BY created_at, id")?;
            let rows = stmt.query_map([], |r| {
                Ok(FolderRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    uid: r.get(2)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// 内存镜像用的收藏行（key / 收藏夹 / 是否 18+）。
    pub fn favorite_refs(&self) -> Result<Vec<FavoriteRefRow>, LibError> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare("SELECT key, folder_id, is_adult FROM favorite WHERE deleted = 0")?;
            let rows = stmt.query_map([], |r| {
                Ok(FavoriteRefRow {
                    key: r.get(0)?,
                    folder_id: r.get(1)?,
                    adult: r.get::<_, i64>(2)? != 0,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// 内存镜像用的屏蔽行（`user_json` 由 Kotlin 解码，解不开的跳过）。
    pub fn blocked_rows(&self) -> Result<Vec<BlockedRow>, LibError> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare("SELECT user_json, blocked_at FROM blocked_user WHERE deleted = 0")?;
            let rows = stmt.query_map([], |r| {
                Ok(BlockedRow {
                    user_json: r.get(0)?,
                    blocked_at: r.get(1)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    // ---------------------------------------------------------------- 收藏

    /// 收藏（或在收藏夹之间移动；已收藏就只换收藏夹，收藏时间不变）。
    #[allow(clippy::too_many_arguments)]
    pub fn add_favorite(
        &self,
        key: &str,
        folder_id: Option<i64>,
        platform: &str,
        is_video: bool,
        search_text: &str,
        item_json: &str,
        added_at: i64,
        adult: bool,
        rev: i64,
    ) -> Result<(), LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            persist_hlc(&tx, rev)?;
            let folder_uid: Option<String> = match folder_id {
                Some(id) => tx
                    .query_row("SELECT sync_uid FROM favorite_folder WHERE id = ?1", [id], |r| r.get(0))
                    .optional()?
                    .flatten(),
                None => None,
            };
            tx.execute(
                "INSERT INTO favorite(key, folder_id, folder_uid, platform, is_video, search_text, item_json, added_at,
                       is_adult, rev_hlc, rev_dev, deleted)
                   VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)
                   ON CONFLICT(key) DO UPDATE SET folder_id = excluded.folder_id, folder_uid = excluded.folder_uid,
                       item_json = excluded.item_json, search_text = excluded.search_text,
                       is_adult = excluded.is_adult, rev_hlc = excluded.rev_hlc, rev_dev = excluded.rev_dev,
                       deleted = 0",
                params![
                    key,
                    folder_id,
                    folder_uid,
                    platform,
                    is_video as i64,
                    search_text,
                    item_json,
                    added_at,
                    adult as i64,
                    rev,
                    self.device_id
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    /// 取消收藏：留墓碑（跨设备要传播删除），界面上一律当它不存在。
    pub fn remove_favorite(&self, key: &str, rev: i64) -> Result<(), LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            persist_hlc(&tx, rev)?;
            tx.execute(
                "UPDATE favorite SET deleted = 1, rev_hlc = ?1, rev_dev = ?2 WHERE key = ?3",
                params![rev, self.device_id, key],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_folder(&self, name: &str, uid: &str, created_at: i64, rev: i64) -> Result<i64, LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            persist_hlc(&tx, rev)?;
            tx.execute(
                "INSERT INTO favorite_folder(name, created_at, sync_uid, rev_hlc, rev_dev, deleted) VALUES(?1, ?2, ?3, ?4, ?5, 0)",
                params![name, created_at, uid, rev, self.device_id],
            )?;
            let id = tx.last_insert_rowid();
            tx.commit()?;
            Ok(id)
        })
    }

    pub fn rename_folder(&self, id: i64, name: &str, rev: i64) -> Result<(), LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            persist_hlc(&tx, rev)?;
            tx.execute(
                "UPDATE favorite_folder SET name = ?1, rev_hlc = ?2, rev_dev = ?3 WHERE id = ?4",
                params![name, rev, self.device_id, id],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    /// 删收藏夹。[delete_items] 为 false 时里面的收藏移回默认收藏夹（folder_id / folder_uid 都清空），
    /// true 时连收藏一起删（墓碑）。收藏夹本身留墓碑（同名认领 / 删除传播都要它的 uid）。
    pub fn delete_folder(&self, id: Option<i64>, delete_items: bool, rev: i64) -> Result<(), LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            persist_hlc(&tx, rev)?;
            if delete_items {
                tx.execute(
                    "UPDATE favorite SET deleted = 1, rev_hlc = ?1, rev_dev = ?2 WHERE folder_id = ?3",
                    params![rev, self.device_id, id],
                )?;
            } else {
                tx.execute(
                    "UPDATE favorite SET folder_id = NULL, folder_uid = NULL, rev_hlc = ?1, rev_dev = ?2 WHERE folder_id = ?3",
                    params![rev, self.device_id, id],
                )?;
            }
            tx.execute(
                "UPDATE favorite_folder SET deleted = 1, rev_hlc = ?1, rev_dev = ?2 WHERE id = ?3",
                params![rev, self.device_id, id],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    /// 按条件查收藏，新收藏的在前。排序里的 `rowid` 让同一毫秒的两次收藏也稳定。
    pub fn favorites(
        &self,
        folder: FolderFilter,
        platform: Option<&str>,
        ty: ContentType,
        search: &str,
        limit: i64,
    ) -> Result<Vec<FavoriteRow>, LibError> {
        self.with_reader(|conn| {
            let mut wh = Where::default();
            match folder {
                FolderFilter::All => {}
                FolderFilter::Default => wh.add("folder_id IS NULL", vec![]),
                FolderFilter::Folder(id) => wh.add("folder_id = ?", vec![Value::Integer(id)]),
            }
            wh.platform(platform);
            wh.ty(ty);
            wh.search(search);
            wh.visible();
            let sql = format!(
                "SELECT item_json, folder_id, added_at FROM favorite {} ORDER BY added_at DESC, rowid DESC LIMIT ?",
                wh.sql()
            );
            let mut args = wh.args;
            args.push(Value::Integer(limit));
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(args), |r| {
                Ok(FavoriteRow {
                    item_json: r.get(0)?,
                    folder_id: r.get(1)?,
                    added_at: r.get(2)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    // ---------------------------------------------------------------- 历史

    /// 打开详情时记一条（同一条内容只保留最新一次；删过又打开一次的话进度从零开始）。
    #[allow(clippy::too_many_arguments)]
    pub fn record_visit(
        &self,
        key: &str,
        platform: &str,
        search_text: &str,
        item_json: &str,
        visited_at: i64,
        adult: bool,
        rev: i64,
    ) -> Result<(), LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            persist_hlc(&tx, rev)?;
            tx.execute(
                "INSERT INTO history(key, platform, search_text, item_json, visited_at, is_adult, rev_hlc, rev_dev, deleted)
                   VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)
                   ON CONFLICT(key) DO UPDATE SET visited_at = excluded.visited_at, item_json = excluded.item_json,
                       search_text = excluded.search_text, is_adult = excluded.is_adult,
                       rev_hlc = excluded.rev_hlc, rev_dev = excluded.rev_dev,
                       -- 墓碑复活（删过历史又打开一次）从零开始，不把删掉之前的进度带回来
                       progress_ms = CASE WHEN deleted = 1 THEN NULL ELSE progress_ms END,
                       duration_ms = CASE WHEN deleted = 1 THEN NULL ELSE duration_ms END,
                       progress_hlc = CASE WHEN deleted = 1 THEN 0 ELSE progress_hlc END,
                       progress_dev = CASE WHEN deleted = 1 THEN '' ELSE progress_dev END,
                       deleted = 0",
                params![key, platform, search_text, item_json, visited_at, adult as i64, rev, self.device_id],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    /// 存播放进度，返回改了几行（0 = 这条历史不在 / 是墓碑，调用方据此别在内存里留假进度）。
    pub fn save_progress(
        &self,
        key: &str,
        position_ms: i64,
        duration_ms: Option<i64>,
        rev: i64,
    ) -> Result<i64, LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            persist_hlc(&tx, rev)?;
            let changed = tx.execute(
                "UPDATE history SET progress_ms = ?1, duration_ms = COALESCE(?2, duration_ms), progress_hlc = ?3, \
                 progress_dev = ?4 WHERE key = ?5 AND deleted = 0",
                params![position_ms, duration_ms, rev, self.device_id, key],
            )?;
            tx.commit()?;
            Ok(changed as i64)
        })
    }

    pub fn progress(&self, key: &str) -> Result<Option<i64>, LibError> {
        self.with_reader(|conn| {
            Ok(conn
                .query_row(
                    "SELECT progress_ms FROM history WHERE key = ?1 AND deleted = 0 AND progress_ms IS NOT NULL",
                    [key],
                    |r| r.get(0),
                )
                .optional()?)
        })
    }

    pub fn history(
        &self,
        platform: Option<&str>,
        search: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<HistoryRow>, LibError> {
        self.with_reader(|conn| {
            let mut wh = Where::default();
            wh.platform(platform);
            wh.search(search);
            wh.visible();
            let sql = format!(
                "SELECT item_json, visited_at, progress_ms, duration_ms FROM history {} \
                 ORDER BY visited_at DESC, rowid DESC LIMIT ? OFFSET ?",
                wh.sql()
            );
            let mut args = wh.args;
            args.push(Value::Integer(limit));
            args.push(Value::Integer(offset));
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(args), |r| {
                Ok(HistoryRow {
                    item_json: r.get(0)?,
                    visited_at: r.get(1)?,
                    progress_ms: r.get(2)?,
                    duration_ms: r.get(3)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn history_count(&self, platform: Option<&str>, search: &str) -> Result<i64, LibError> {
        self.with_reader(|conn| {
            let mut wh = Where::default();
            wh.platform(platform);
            wh.search(search);
            wh.visible();
            let sql = format!("SELECT count(*) FROM history {}", wh.sql());
            Ok(conn.query_row(&sql, params_from_iter(wh.args), |r| r.get(0))?)
        })
    }

    pub fn remove_history(&self, key: &str, rev: i64) -> Result<(), LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            persist_hlc(&tx, rev)?;
            // 墓碑不留进度（和合并引擎一致：删了之后复活从零开始）
            tx.execute(
                "UPDATE history SET deleted = 1, rev_hlc = ?1, rev_dev = ?2, progress_ms = NULL, duration_ms = NULL, \
                 progress_hlc = 0, progress_dev = '' WHERE key = ?3",
                params![rev, self.device_id, key],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    /// 清空历史：本地直接删行（不留一万条墓碑），别的设备靠 `history_cleared_hlc` 这个全局标记清到同一时刻。
    pub fn clear_history(&self, rev: i64) -> Result<(), LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            persist_hlc(&tx, rev)?;
            tx.execute("DELETE FROM history", [])?;
            let cleared = meta(&tx, META_CLEARED)?
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0)
                .max(rev);
            set_meta(&tx, META_CLEARED, &cleared.to_string())?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn prune_history(&self, older_than_ms: i64) -> Result<i64, LibError> {
        self.with_writer(|conn| {
            Ok(conn.execute("DELETE FROM history WHERE visited_at < ?1", [older_than_ms])? as i64)
        })
    }

    // ---------------------------------------------------------------- 屏蔽

    pub fn block(&self, key: &str, user_json: &str, blocked_at: i64, rev: i64) -> Result<(), LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            persist_hlc(&tx, rev)?;
            tx.execute(
                "INSERT INTO blocked_user(key, user_json, blocked_at, rev_hlc, rev_dev, deleted) VALUES(?1, ?2, ?3, ?4, ?5, 0)
                   ON CONFLICT(key) DO UPDATE SET user_json = excluded.user_json, blocked_at = excluded.blocked_at,
                       rev_hlc = excluded.rev_hlc, rev_dev = excluded.rev_dev, deleted = 0",
                params![key, user_json, blocked_at, rev, self.device_id],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn unblock(&self, key: &str, rev: i64) -> Result<(), LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            persist_hlc(&tx, rev)?;
            tx.execute(
                "UPDATE blocked_user SET deleted = 1, rev_hlc = ?1, rev_dev = ?2 WHERE key = ?3",
                params![rev, self.device_id, key],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    // ---------------------------------------------------------------- 行为数据（曝光 / 停留）

    /// 记一次曝光：同一个 key + surface 最近一行在合并窗口内就累加停留、推后"最近看到"，否则新起一行。
    #[allow(clippy::too_many_arguments)]
    pub fn record_exposure(
        &self,
        key: &str,
        platform: &str,
        at: i64,
        dwell_ms: i64,
        surface: &str,
        last_seen_at: i64,
        merge_window_ms: i64,
    ) -> Result<(), LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let last: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT id, last_seen_at FROM exposure WHERE key = ?1 AND surface = ?2 \
                     ORDER BY last_seen_at DESC, id DESC LIMIT 1",
                    params![key, surface],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match last {
                Some((id, seen)) if at - seen <= merge_window_ms => {
                    tx.execute(
                        "UPDATE exposure SET dwell_ms = dwell_ms + ?1, last_seen_at = MAX(last_seen_at, ?2) WHERE id = ?3",
                        params![dwell_ms, last_seen_at, id],
                    )?;
                }
                _ => {
                    tx.execute(
                        "INSERT INTO exposure(key, platform, surface, exposed_at, last_seen_at, dwell_ms) \
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                        params![key, platform, surface, at, last_seen_at, dwell_ms],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn exposure_count(&self) -> Result<i64, LibError> {
        self.with_reader(|conn| Ok(conn.query_row("SELECT count(*) FROM exposure", [], |r| r.get(0))?))
    }

    /// 一段时间的曝光统计（按 `last_seen_at` 落在 `[from, to)`，可按平台 / 面过滤）。
    pub fn exposure_stats(
        &self,
        from_ms: i64,
        to_ms: i64,
        platform: Option<&str>,
        surface: Option<&str>,
    ) -> Result<ExposureStats, LibError> {
        self.with_reader(|conn| {
            let mut wh = Where::default();
            wh.add("e.last_seen_at >= ?", vec![Value::Integer(from_ms)]);
            wh.add("e.last_seen_at < ?", vec![Value::Integer(to_ms)]);
            if let Some(p) = platform {
                wh.add("e.platform = ?", vec![Value::Text(p.to_string())]);
            }
            if let Some(s) = surface {
                wh.add("e.surface = ?", vec![Value::Text(s.to_string())]);
            }
            let sql = format!(
                "SELECT count(*), COALESCE(sum(e.dwell_ms), 0),
                          COALESCE(sum(CASE WHEN EXISTS(
                              SELECT 1 FROM history h WHERE h.key = e.key AND h.visited_at >= e.exposed_at AND h.deleted = 0
                          ) THEN 1 ELSE 0 END), 0)
                   FROM exposure e {}",
                wh.sql()
            );
            let mut stmt = conn.prepare(&sql)?;
            let row = stmt.query_row(params_from_iter(wh.args), |r| {
                Ok(ExposureStats {
                    exposures: r.get(0)?,
                    dwell_ms: r.get(1)?,
                    clicked: r.get(2)?,
                })
            })?;
            Ok(row)
        })
    }

    /// 全部曝光行（导出用，新的在前）。
    pub fn exposure_rows(&self, limit: i64) -> Result<Vec<ExposureRow>, LibError> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT key, platform, surface, exposed_at, last_seen_at, dwell_ms FROM exposure \
                 ORDER BY last_seen_at DESC, id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit], |r| {
                Ok(ExposureRow {
                    key: r.get(0)?,
                    platform: r.get(1)?,
                    surface: r.get(2)?,
                    exposed_at: r.get(3)?,
                    last_seen_at: r.get(4)?,
                    dwell_ms: r.get(5)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn prune_exposure(&self, older_than_ms: i64) -> Result<i64, LibError> {
        self.with_writer(|conn| {
            Ok(conn.execute("DELETE FROM exposure WHERE last_seen_at < ?1", [older_than_ms])? as i64)
        })
    }

    /// 清空全部行为数据：曝光 + 曝光过的卡片内容 + 向量（用户点一次"清除行为数据"就都该没了）。
    pub fn clear_exposure(&self) -> Result<(), LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute("DELETE FROM exposure", [])?;
            tx.execute("DELETE FROM seen_item", [])?;
            tx.execute("DELETE FROM item_embedding", [])?;
            tx.commit()?;
            Ok(())
        })
    }

    // ---------------------------------------------------------------- 曝光过的卡片内容

    /// 同一个 key 只写第一次（重复曝光不覆盖 item_json / 批 / 序号）。
    #[allow(clippy::too_many_arguments)]
    pub fn record_seen_item(
        &self,
        key: &str,
        platform: &str,
        item_json: &str,
        at: i64,
        feed_kind: &str,
        batch_id: i64,
        platform_rank: i64,
    ) -> Result<(), LibError> {
        self.with_writer(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO seen_item(key, platform, item_json, first_seen_at, feed_kind, batch_id, platform_rank)
                   VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![key, platform, item_json, at, feed_kind, batch_id, platform_rank],
            )?;
            Ok(())
        })
    }

    pub fn seen_item_count(&self) -> Result<i64, LibError> {
        self.with_reader(|conn| Ok(conn.query_row("SELECT count(*) FROM seen_item", [], |r| r.get(0))?))
    }

    pub fn seen_items(&self, limit: i64) -> Result<Vec<SeenItemRow>, LibError> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT item_json, first_seen_at, feed_kind, batch_id, platform_rank FROM seen_item \
                 ORDER BY first_seen_at DESC, rowid DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit], |r| {
                Ok(SeenItemRow {
                    item_json: r.get(0)?,
                    first_seen_at: r.get(1)?,
                    feed_kind: r.get(2)?,
                    batch_id: r.get(3)?,
                    platform_rank: r.get(4)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn prune_seen_items(&self, older_than_ms: i64) -> Result<i64, LibError> {
        self.with_writer(|conn| {
            Ok(conn.execute("DELETE FROM seen_item WHERE first_seen_at < ?1", [older_than_ms])? as i64)
        })
    }

    // ---------------------------------------------------------------- 卡片向量

    /// 一批向量：一个事务里 `INSERT OR REPLACE`，返回写成功的行数。
    ///
    /// 表的主键是 **(key, model)**：**同一个 key + 同一个 model** 再来一条
    /// 就整行替换（重算时不用先删），**换 model 是另一行**——同一条内容可以同时有纯文字向量
    /// （`…-htp-l128`）和封面向量（`…-htp-l128-img8x8`），互不覆盖。
    pub fn insert_embeddings(&self, rows: &[EmbeddingRow]) -> Result<i64, LibError> {
        if rows.is_empty() {
            return Ok(0);
        }
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut n = 0i64;
            for row in rows {
                n += tx.execute(
                    "INSERT OR REPLACE INTO item_embedding(key, model, dim, vec, created_at, source) \
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![row.key, row.model, row.dim, row.vec, row.created_at, row.source],
                )? as i64;
            }
            tx.commit()?;
            Ok(n)
        })
    }

    /// 这批 key 里已经有向量的那些（同一个 model 下）。分批查（和 Kotlin 版一样一批 400 个，别撞参数上限）。
    pub fn embedding_keys(&self, keys: &[String], model: &str) -> Result<Vec<String>, LibError> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        self.with_reader(|conn| {
            let mut out = Vec::new();
            for chunk in keys.chunks(400) {
                let marks = vec!["?"; chunk.len()].join(",");
                let sql = format!("SELECT key FROM item_embedding WHERE model = ? AND key IN ({marks})");
                let mut args = Vec::with_capacity(chunk.len() + 1);
                args.push(Value::Text(model.to_string()));
                args.extend(chunk.iter().map(|k| Value::Text(k.clone())));
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(params_from_iter(args), |r| r.get::<_, String>(0))?;
                for row in rows {
                    out.push(row?);
                }
            }
            Ok(out)
        })
    }

    pub fn embedding_count(&self) -> Result<i64, LibError> {
        self.with_reader(|conn| Ok(conn.query_row("SELECT count(*) FROM item_embedding", [], |r| r.get(0))?))
    }

    pub fn embedding_stats(&self) -> Result<Vec<EmbeddingStat>, LibError> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT model, source, count(*) FROM item_embedding GROUP BY model, source ORDER BY model, source",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(EmbeddingStat {
                    model: r.get(0)?,
                    source: r.get(1)?,
                    count: r.get(2)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// 收藏 / 历史里还没有这个模型向量的条目，最近的在前（一条 SQL 把两边 UNION 起来按时间倒序）。
    pub fn embedding_backfill(
        &self,
        model: &str,
        since_ms: i64,
        limit: i64,
    ) -> Result<Vec<BackfillRow>, LibError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        self.with_reader(|conn| {
            // 先多取一些（同一条可能两边都有，去重后会少），调用方再在内存里截到 limit
            let mut stmt = conn.prepare(
                "SELECT key, item_json, at, src FROM (\
                    SELECT key, item_json, added_at AS at, 'favorite' AS src \
                    FROM favorite WHERE is_adult = 0 AND deleted = 0 AND added_at >= ? \
                    AND key NOT IN (SELECT key FROM item_embedding WHERE model = ?) \
                    UNION ALL \
                    SELECT key, item_json, visited_at AS at, 'history' AS src \
                    FROM history WHERE is_adult = 0 AND deleted = 0 AND visited_at >= ? \
                    AND key NOT IN (SELECT key FROM item_embedding WHERE model = ?)\
                 ) ORDER BY at DESC LIMIT ?",
            )?;
            let rows = stmt.query_map(params![since_ms, model, since_ms, model, limit * 2], |r| {
                Ok(BackfillRow {
                    key: r.get(0)?,
                    item_json: r.get(1)?,
                    at: r.get(2)?,
                    source: r.get(3)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn prune_embeddings(&self, older_than_ms: i64) -> Result<i64, LibError> {
        self.with_writer(|conn| {
            Ok(conn.execute("DELETE FROM item_embedding WHERE created_at < ?1", [older_than_ms])? as i64)
        })
    }

    pub fn embeddings(&self, limit: i64) -> Result<Vec<EmbeddingRow>, LibError> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT key, model, dim, vec, created_at, source FROM item_embedding \
                 ORDER BY created_at DESC, rowid DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit], |r| {
                Ok(EmbeddingRow {
                    key: r.get(0)?,
                    model: r.get(1)?,
                    dim: r.get(2)?,
                    vec: r.get(3)?,
                    created_at: r.get(4)?,
                    source: r.get(5)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn clear_embeddings(&self) -> Result<(), LibError> {
        self.with_writer(|conn| {
            conn.execute("DELETE FROM item_embedding", [])?;
            Ok(())
        })
    }

    // ---------------------------------------------------------------- 跨设备同步

    /// 快照序号 +1（导出一次加一次：导入端拿它挡"把网盘里的旧文件放回来"）。
    pub fn bump_snapshot_seq(&self) -> Result<i64, LibError> {
        self.with_writer(|conn| {
            let next = meta(conn, META_SEQ)?
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0)
                + 1;
            set_meta(conn, META_SEQ, &next.to_string())?;
            Ok(next)
        })
    }

    /// 读全量（含墓碑），导出和合并都用它。
    pub fn read_sync_state(&self) -> Result<SyncState, LibError> {
        self.with_reader(|conn| read_state(conn))
    }

    pub fn last_imported_seq(&self, device: &str) -> Result<i64, LibError> {
        self.with_reader(|conn| {
            Ok(conn
                .query_row("SELECT last_seq FROM sync_peer WHERE peer = ?1", [device], |r| r.get(0))
                .optional()?
                .unwrap_or(0))
        })
    }

    /// 已配对的设备（有公钥的行），按配对时间从新到旧。
    pub fn sync_peers(&self) -> Result<Vec<PeerRow>, LibError> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT peer, COALESCE(name, ''), public_key, paired_at FROM sync_peer \
                 WHERE public_key IS NOT NULL AND public_key <> '' ORDER BY paired_at DESC, peer",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(PeerRow {
                    device_id: r.get(0)?,
                    name: r.get(1)?,
                    public_key: r.get(2)?,
                    paired_at: r.get(3)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn upsert_sync_peer(
        &self,
        device: &str,
        name: &str,
        public_key: &str,
        paired_at: i64,
    ) -> Result<(), LibError> {
        self.with_writer(|conn| {
            conn.execute(
                "INSERT INTO sync_peer(peer, name, public_key, paired_at, last_seq, last_seen_hlc, last_generated_at)
                   VALUES(?1, ?2, ?3, ?4, 0, 0, 0)
                   ON CONFLICT(peer) DO UPDATE SET
                     name = excluded.name, public_key = excluded.public_key, paired_at = excluded.paired_at",
                params![device, name, public_key, paired_at],
            )?;
            Ok(())
        })
    }

    pub fn remove_sync_peer(&self, device: &str) -> Result<bool, LibError> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let existed = tx
                .query_row("SELECT 1 FROM sync_peer WHERE peer = ?1", [device], |_| Ok(()))
                .optional()?
                .is_some();
            tx.execute("DELETE FROM sync_peer WHERE peer = ?1", [device])?;
            tx.commit()?;
            Ok(existed)
        })
    }

    /// 导入：**在写锁里**读本机状态 → 回调宿主合并 → 同一个事务里写回合并结果。
    ///
    /// 返回合并结果里远端最大的 HLC（调用方据此把本机时钟抬到它之后，见 `HlcClock.observe`）。
    /// `sync_peer` 里记下这台设备的快照序号和生成时间（LWW 之后本机知道"已经看到这一步了"）。
    pub fn apply_import(
        &self,
        remote: &SyncState,
        remote_meta: &RemoteMeta,
        retention_cutoff_ms: Option<i64>,
        hlc_last: i64,
        host: &dyn MergeHost,
    ) -> Result<i64, LibError> {
        self.with_writer(|conn| {
            // 本机状态在写锁里读，和后面的写入是一个一致快照（播放进度这类后台写也走这把锁）
            let local = read_state(conn)?;
            let merged = host.merge(&local, remote, retention_cutoff_ms);
            let remote_max_hlc = max_hlc(&merged.state);
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            apply_merged(&tx, &merged, remote_meta, remote_max_hlc, hlc_last)?;
            tx.commit()?;
            Ok(remote_max_hlc)
        })
    }

    // ---------------------------------------------------------------- 内部

    fn with_writer<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T, LibError>) -> Result<T, LibError> {
        let mut guard: MutexGuard<Option<Connection>> =
            self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.as_mut() {
            Some(conn) => f(conn),
            None => Err(LibError::Other("资料库已经关掉了".to_string())),
        }
    }

    fn with_reader<T>(&self, f: impl FnOnce(&Connection) -> Result<T, LibError>) -> Result<T, LibError> {
        let guard: MutexGuard<Option<Connection>> = self.reader.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.as_ref() {
            Some(conn) => f(conn),
            None => Err(LibError::Other("资料库已经关掉了".to_string())),
        }
    }
}

// ===========================================================================
// SQL 小工具（和 Kotlin 版的 `Where` / `meta` / `persistHlc` 一一对应）
// ===========================================================================

const META_SEQ: &str = "snapshot_seq";
const META_LAST_HLC: &str = "last_hlc";
const META_CLEARED: &str = "history_cleared_hlc";

/// 拼 WHERE：条件和参数一一对应（顺序就是 Kotlin 版 `Where` 的追加顺序——`LIMIT` / `OFFSET` 在最后）。
#[derive(Default)]
struct Where {
    parts: Vec<String>,
    args: Vec<Value>,
}

impl Where {
    fn add(&mut self, sql: &str, args: Vec<Value>) {
        self.parts.push(sql.to_string());
        self.args.extend(args);
    }

    fn platform(&mut self, p: Option<&str>) {
        if let Some(p) = p {
            self.add("platform = ?", vec![Value::Text(p.to_string())]);
        }
    }

    fn ty(&mut self, t: ContentType) {
        match t {
            ContentType::Mixed => {}
            ContentType::Video => self.add("is_video = 1", vec![]),
            ContentType::Text => self.add("is_video = 0", vec![]),
        }
    }

    /// 标题 / 摘要 / 作者 / 圈子里的子串（不分大小写，ASCII；`%` `_` `\` 转义）。
    fn search(&mut self, q: &str) {
        let s = q.trim();
        if !s.is_empty() {
            let escaped = s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            self.add(
                "search_text LIKE ? ESCAPE '\\'",
                vec![Value::Text(format!("%{escaped}%"))],
            );
        }
    }

    /// 平台标成 18+ 的整条隐藏 + 软删除的墓碑：界面上的一律当它不存在。
    fn visible(&mut self) {
        self.add("deleted = 0", vec![]);
        self.add("is_adult = 0", vec![]);
    }

    fn sql(&self) -> String {
        if self.parts.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", self.parts.join(" AND "))
        }
    }
}

fn meta(conn: &Connection, key: &str) -> Result<Option<String>, LibError> {
    Ok(conn
        .query_row("SELECT v FROM sync_meta WHERE k = ?1", [key], |r| r.get(0))
        .optional()?)
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<(), LibError> {
    conn.execute(
        "INSERT INTO sync_meta(k, v) VALUES(?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![key, value],
    )?;
    Ok(())
}

/// 把 HLC 的最后值落盘；并发写的时候只往前走（CAST 比较，不会回退）。
fn persist_hlc(conn: &Connection, value: i64) -> Result<(), LibError> {
    conn.execute(
        "INSERT INTO sync_meta(k, v) VALUES(?1, ?2) ON CONFLICT(k) DO UPDATE SET v = \
         CASE WHEN CAST(excluded.v AS INTEGER) > CAST(v AS INTEGER) THEN excluded.v ELSE v END",
        params![META_LAST_HLC, value.to_string()],
    )?;
    Ok(())
}

fn read_state(conn: &Connection) -> Result<SyncState, LibError> {
    let history_cleared_hlc = meta(conn, META_CLEARED)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let folders = {
        let mut stmt = conn.prepare(
            "SELECT sync_uid, name, created_at, rev_hlc, rev_dev, deleted FROM favorite_folder WHERE sync_uid IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SyncFolderRow {
                uid: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                rev_hlc: r.get(3)?,
                rev_dev: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                deleted: r.get::<_, i64>(5)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let favorites = {
        let mut stmt = conn.prepare(
            "SELECT key, folder_uid, added_at, rev_hlc, rev_dev, deleted, item_json FROM favorite",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SyncFavoriteRow {
                key: r.get(0)?,
                folder_uid: r.get(1)?,
                added_at: r.get(2)?,
                rev_hlc: r.get(3)?,
                rev_dev: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                deleted: r.get::<_, i64>(5)? != 0,
                item_json: Some(r.get(6)?),
                columns: None,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let history = {
        let mut stmt = conn.prepare(
            "SELECT key, visited_at, rev_hlc, rev_dev, deleted, progress_ms, duration_ms, progress_hlc, progress_dev, item_json \
             FROM history",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SyncHistoryRow {
                key: r.get(0)?,
                visited_at: r.get(1)?,
                rev_hlc: r.get(2)?,
                rev_dev: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                deleted: r.get::<_, i64>(4)? != 0,
                progress_ms: r.get(5)?,
                duration_ms: r.get(6)?,
                progress_hlc: r.get(7)?,
                progress_dev: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                item_json: Some(r.get(9)?),
                columns: None,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let blocked = {
        let mut stmt = conn.prepare("SELECT key, user_json, blocked_at, rev_hlc, rev_dev, deleted FROM blocked_user")?;
        let rows = stmt.query_map([], |r| {
            Ok(SyncBlockedRow {
                key: r.get(0)?,
                user_json: Some(r.get(1)?),
                blocked_at: r.get(2)?,
                rev_hlc: r.get(3)?,
                rev_dev: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                deleted: r.get::<_, i64>(5)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(SyncState {
        history_cleared_hlc,
        folders,
        favorites,
        history,
        blocked,
    })
}

fn max_hlc(state: &SyncState) -> i64 {
    let mut max = state.history_cleared_hlc;
    for f in &state.folders {
        max = max.max(f.rev_hlc);
    }
    for f in &state.favorites {
        max = max.max(f.rev_hlc);
    }
    for h in &state.history {
        max = max.max(h.rev_hlc).max(h.progress_hlc);
    }
    for b in &state.blocked {
        max = max.max(b.rev_hlc);
    }
    max
}

/// 把合并结果写进库（在调用方的事务里）。**只写合并结果里的那一份**（LWW 已经算好了）：
/// 收藏 / 历史 / 屏蔽整行 upsert，被清空标记去掉的本机历史行删掉；收藏夹先做同名认领再逐 uid upsert。
fn apply_merged(
    tx: &Connection,
    merged: &MergedState,
    remote: &RemoteMeta,
    remote_max_hlc: i64,
    hlc_last: i64,
) -> Result<(), LibError> {
    let state = &merged.state;
    // 1. 同名认领：本机旧收藏夹改成合并后的 uid；如果本机已经有那本夹，把条目接过去、旧行删掉
    for (old_uid, new_uid) in &merged.aliases {
        let old_id: Option<i64> = tx
            .query_row("SELECT id FROM favorite_folder WHERE sync_uid = ?1", [old_uid], |r| r.get(0))
            .optional()?;
        let Some(old_id) = old_id else { continue };
        let new_id: Option<i64> = tx
            .query_row("SELECT id FROM favorite_folder WHERE sync_uid = ?1", [new_uid], |r| r.get(0))
            .optional()?;
        match new_id {
            None => {
                tx.execute(
                    "UPDATE favorite_folder SET sync_uid = ?1 WHERE id = ?2",
                    params![new_uid, old_id],
                )?;
            }
            Some(new_id) => {
                tx.execute(
                    "UPDATE favorite SET folder_id = ?1, folder_uid = ?2 WHERE folder_id = ?3",
                    params![new_id, new_uid, old_id],
                )?;
                tx.execute("DELETE FROM favorite_folder WHERE id = ?1", [old_id])?;
            }
        }
    }

    // 2. 收藏夹：逐 uid upsert
    for row in &state.folders {
        let id: Option<i64> = tx
            .query_row("SELECT id FROM favorite_folder WHERE sync_uid = ?1", [&row.uid], |r| r.get(0))
            .optional()?;
        match id {
            None => {
                tx.execute(
                    "INSERT INTO favorite_folder(name, created_at, sync_uid, rev_hlc, rev_dev, deleted) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![row.name, row.created_at, row.uid, row.rev_hlc, row.rev_dev, row.deleted as i64],
                )?;
            }
            Some(id) => {
                tx.execute(
                    "UPDATE favorite_folder SET name = ?1, created_at = ?2, rev_hlc = ?3, rev_dev = ?4, deleted = ?5 WHERE id = ?6",
                    params![row.name, row.created_at, row.rev_hlc, row.rev_dev, row.deleted as i64, id],
                )?;
            }
        }
    }
    // uid → (本地 id, 是否已删)
    let folder_info: std::collections::HashMap<String, (i64, bool)> = {
        let mut stmt = tx.prepare("SELECT id, sync_uid, deleted FROM favorite_folder WHERE sync_uid IS NOT NULL")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                (r.get::<_, i64>(0)?, r.get::<_, i64>(2)? != 0),
            ))
        })?;
        rows.collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()?
    };

    // 3. 收藏
    for row in &state.favorites {
        let (Some(item), Some(item_json)) = (row.columns.as_ref(), row.item_json.as_ref()) else {
            if row.deleted {
                tx.execute(
                    "UPDATE favorite SET deleted = 1, rev_hlc = ?1, rev_dev = ?2 WHERE key = ?3",
                    params![row.rev_hlc, row.rev_dev, row.key],
                )?;
            }
            continue;
        };
        let folder = row
            .folder_uid
            .as_ref()
            .and_then(|uid| folder_info.get(uid))
            .filter(|(_, deleted)| !*deleted);
        tx.execute(
            "INSERT INTO favorite(key, folder_id, folder_uid, platform, is_video, search_text, item_json, added_at,
                   is_adult, rev_hlc, rev_dev, deleted)
               VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
               ON CONFLICT(key) DO UPDATE SET folder_id = excluded.folder_id, folder_uid = excluded.folder_uid,
                   platform = excluded.platform, is_video = excluded.is_video, search_text = excluded.search_text,
                   item_json = excluded.item_json, added_at = excluded.added_at, is_adult = excluded.is_adult,
                   rev_hlc = excluded.rev_hlc, rev_dev = excluded.rev_dev, deleted = excluded.deleted",
            params![
                row.key,
                folder.map(|(id, _)| *id),
                row.folder_uid,
                item.platform,
                item.is_video as i64,
                item.search_text,
                item_json,
                row.added_at,
                item.adult as i64,
                row.rev_hlc,
                row.rev_dev,
                row.deleted as i64
            ],
        )?;
    }

    // 4. 历史（访问信息 + 进度分开写）
    for row in &state.history {
        let (Some(item), Some(item_json)) = (row.columns.as_ref(), row.item_json.as_ref()) else {
            if row.deleted {
                tx.execute(
                    "UPDATE history SET deleted = 1, progress_ms = NULL, duration_ms = NULL, progress_hlc = 0, \
                     progress_dev = '', rev_hlc = ?1, rev_dev = ?2 WHERE key = ?3",
                    params![row.rev_hlc, row.rev_dev, row.key],
                )?;
            }
            continue;
        };
        tx.execute(
            "INSERT INTO history(key, platform, search_text, item_json, visited_at, progress_ms, duration_ms, is_adult,
                   rev_hlc, rev_dev, deleted, progress_hlc, progress_dev)
               VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
               ON CONFLICT(key) DO UPDATE SET platform = excluded.platform, search_text = excluded.search_text,
                   item_json = excluded.item_json, visited_at = excluded.visited_at, progress_ms = excluded.progress_ms,
                   duration_ms = excluded.duration_ms, is_adult = excluded.is_adult, rev_hlc = excluded.rev_hlc,
                   rev_dev = excluded.rev_dev, deleted = excluded.deleted, progress_hlc = excluded.progress_hlc,
                   progress_dev = excluded.progress_dev",
            params![
                row.key,
                item.platform,
                item.search_text,
                item_json,
                row.visited_at,
                row.progress_ms,
                row.duration_ms,
                item.adult as i64,
                row.rev_hlc,
                row.rev_dev,
                row.deleted as i64,
                row.progress_hlc,
                row.progress_dev
            ],
        )?;
    }
    // 被"清空历史"标记去掉的本机行：硬删（别的设备靠标记清，不靠一万条墓碑）
    let keep: std::collections::HashSet<&str> = state.history.iter().map(|h| h.key.as_str()).collect();
    let existing: Vec<String> = {
        let mut stmt = tx.prepare("SELECT key FROM history")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for key in existing {
        if !keep.contains(key.as_str()) {
            tx.execute("DELETE FROM history WHERE key = ?1", [&key])?;
        }
    }

    // 5. 屏蔽
    for row in &state.blocked {
        let Some(user_json) = row.user_json.as_ref() else {
            if row.deleted {
                tx.execute(
                    "UPDATE blocked_user SET deleted = 1, rev_hlc = ?1, rev_dev = ?2 WHERE key = ?3",
                    params![row.rev_hlc, row.rev_dev, row.key],
                )?;
            }
            continue;
        };
        tx.execute(
            "INSERT INTO blocked_user(key, user_json, blocked_at, rev_hlc, rev_dev, deleted) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
               ON CONFLICT(key) DO UPDATE SET user_json = excluded.user_json, blocked_at = excluded.blocked_at,
                   rev_hlc = excluded.rev_hlc, rev_dev = excluded.rev_dev, deleted = excluded.deleted",
            params![
                row.key,
                user_json,
                row.blocked_at,
                row.rev_hlc,
                row.rev_dev,
                row.deleted as i64
            ],
        )?;
    }

    // 6. 清空标记 + 本机时钟抬高 + 记住这台设备的快照序号
    let cleared = meta(tx, META_CLEARED)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        .max(state.history_cleared_hlc);
    set_meta(tx, META_CLEARED, &cleared.to_string())?;
    set_meta(tx, META_LAST_HLC, &hlc_last.max(remote_max_hlc).to_string())?;
    let previous_seq: i64 = tx
        .query_row("SELECT last_seq FROM sync_peer WHERE peer = ?1", [&remote.device], |r| r.get(0))
        .optional()?
        .unwrap_or(0);
    tx.execute(
        "INSERT INTO sync_peer(peer, last_seq, last_seen_hlc, last_generated_at) VALUES(?1, ?2, ?3, ?4)
           ON CONFLICT(peer) DO UPDATE SET last_seq = excluded.last_seq, last_seen_hlc = excluded.last_seen_hlc,
               last_generated_at = excluded.last_generated_at",
        params![
            remote.device,
            previous_seq.max(remote.seq),
            remote_max_hlc,
            remote.generated_at
        ],
    )?;
    Ok(())
}

/// 导入时从 `item_json` 里算出来的那四列。
///
/// **为什么由 Kotlin 传**：`platform` 要按 `Platform.toStored` / `fromStored` 的规则认老 enum 名
/// （`"BILIBILI"` → `bili`、`"YOUTUBE"` → `yt`……），`search_text` 是 `FeedItem.searchText()`
/// （`take(120)` 按 UTF-16 码元、空摘要也会留一个空串），`is_video` / `adult` 也一样。
/// 在 Rust 里重写一份只会漂——漂一列，搜索和 18+ 过滤就跟着漂。
pub struct ItemColumns {
    pub platform: String,
    pub is_video: bool,
    pub search_text: String,
    pub adult: bool,
}

/// `Hlc.of(physicalMs, 0)`：物理毫秒 << 16。
pub fn hlc_of(physical_ms: i64) -> i64 {
    physical_ms << 16
}

/// `Hlc.physical` / `Hlc.counter` 的对照实现（只给测试和排障用）。
pub fn hlc_physical(hlc: i64) -> i64 {
    hlc >> 16
}

/// 收藏夹筛选。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderFilter {
    All,
    Default,
    Folder(i64),
}

/// "视频" = 视频 + 直播，其余算图文（和信息流卡片的分法一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Mixed,
    Video,
    Text,
}

/// 从宿主给的种子展开 UUIDv4 形状的字符串（迁移 v4 给老收藏夹补 `sync_uid` 用）。
///
/// Rust 里不取随机数：种子是 Kotlin 的 `SecureRandom` 给的 32 字节，这里用 SplitMix64 按需展开。
pub struct UidMinter {
    state: u64,
}

impl UidMinter {
    pub fn new(seed: &[u8]) -> Self {
        // FNV-1a 把种子折成一个非零状态（全零的 SplitMix 状态也能跑，但没必要留这个坑）
        let mut state: u64 = 0xcbf2_9ce4_8422_2325;
        for b in seed {
            state ^= *b as u64;
            state = state.wrapping_mul(0x0000_0100_0000_01b3);
        }
        if state == 0 {
            state = 0x9e37_79b9_7f4a_7c15;
        }
        UidMinter { state }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn next(&mut self) -> String {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&self.next_u64().to_be_bytes());
        bytes[8..].copy_from_slice(&self.next_u64().to_be_bytes());
        bytes[6] = (bytes[6] & 0x0f) | 0x40; // 版本 4
        bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9],
            bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        )
    }
}

