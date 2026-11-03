//! 本地资料库的 UniFFI 边界：`FfiLibrary` + 过边界的 record + 合并回调。
//!
//! 原则和平台层一样：
//!
//! - **一次调用换一整批结果**（JVM ↔ Rust 过一次 JNA 不便宜）：列表查询一次拿回整页，批写一次提交一批；
//! - **JSON 留在 Kotlin**：`item_json` / `user_json` 到这里已经是字符串，Rust 只负责落列；
//! - **列值由 Kotlin 算**（[`FfiLibItemColumns`]）：`platform` 的老 enum 名映射、`search_text` 的
//!   `take(120)` 口径都在 Kotlin，Rust 不重写一份；
//! - **错误带类型**：[`FfiLibError`] 分 `Sql` / `Unsupported` / `Other`，Kotlin 侧只有 `Unsupported`
//!   会往上抛（库是更新版本的程序建的），其余照旧打日志 + 用内存里那份。
//!
//! **合并回调**（[`FfiLibMergeHost`]）：导入必须在"写锁 + 事务"里读本机状态、合并、写回，而合并引擎
//! （`LibraryMerge`，含 `JsonElement` 的相等比较）留在 Kotlin。所以 Rust 在写锁里同步回调它一次，
//! 拿到合并结果再写。回调里**不许**再调资料库自己的方法（会把写锁等死）。

use std::sync::{Arc, Mutex, PoisonError};

use feedhub_core::library as core;

// ===========================================================================
// 错误
// ===========================================================================

/// 资料库的错误。字段不叫 `message`。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiLibError {
    /// SQLite 自己的错误：打日志、用内存里那份状态（不让界面跟着崩）。
    #[error("{detail}")]
    Sql { detail: String },

    /// 库是更新版本的程序建的：**不打开、不改它**（Kotlin 侧抛 `IllegalStateException`，App 退回内存实现）。
    #[error("{detail}")]
    Unsupported { detail: String, version: i32, supported: i32 },

    /// 别的（参数不对 / 已经关了 / 内容解不开）。
    #[error("{detail}")]
    Other { detail: String },
}

impl From<core::LibError> for FfiLibError {
    fn from(e: core::LibError) -> Self {
        match e {
            core::LibError::Sql(detail) => FfiLibError::Sql { detail },
            core::LibError::Unsupported { version, supported } => FfiLibError::Unsupported {
                detail: e.to_string(),
                version,
                supported,
            },
            core::LibError::Other(detail) => FfiLibError::Other { detail },
        }
    }
}

// ===========================================================================
// record
// ===========================================================================

/// 一个收藏夹。`uid` 跨设备稳定（v4 起）；`id` 是本机的整数主键。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibFolder {
    pub id: i64,
    pub name: String,
    pub uid: Option<String>,
}

/// 内存镜像要的收藏行（不读 JSON）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibFavoriteRef {
    pub key: String,
    pub folder_id: Option<i64>,
    pub adult: bool,
}

/// 一条屏蔽（`user_json` 由 Kotlin 解码）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibBlocked {
    pub user_json: String,
    pub blocked_at: i64,
}

/// 一条收藏的读取结果。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibFavorite {
    pub item_json: String,
    pub folder_id: Option<i64>,
    pub added_at: i64,
}

/// 一条历史。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibHistory {
    pub item_json: String,
    pub visited_at: i64,
    pub progress_ms: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// 一行曝光（导出用）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibExposureRow {
    pub key: String,
    pub platform: String,
    pub surface: String,
    pub exposed_at: i64,
    pub last_seen_at: i64,
    pub dwell_ms: i64,
}

/// 一段时间的曝光统计。
#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiLibExposureStats {
    pub exposures: i32,
    pub clicked: i32,
    pub dwell_ms: i64,
}

/// 一条"曝光过的卡片内容"。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibSeenItem {
    pub item_json: String,
    pub first_seen_at: i64,
    pub feed_kind: String,
    pub batch_id: i64,
    pub platform_rank: i32,
}

/// 一条向量（`vec` 是 float16 的原始字节）。表主键是 `(key, model)`（v9）：同一个 `key` 的不同
/// `model`（纯文字 / 封面）是**两行**，过边界时各自一条 record。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibEmbedding {
    pub key: String,
    pub model: String,
    pub dim: i32,
    pub vec: Vec<u8>,
    pub created_at: i64,
    pub source: String,
}

/// `(model, source)` 下的向量条数。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibEmbeddingStat {
    pub model: String,
    pub source: String,
    pub count: i32,
}

/// 一条补算候选。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibBackfill {
    pub key: String,
    pub item_json: String,
    pub at: i64,
    pub source: String,
}

/// 一台已配对设备。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibPeer {
    pub device_id: String,
    pub name: String,
    pub public_key: String,
    pub paired_at: i64,
}

/// 从 `item_json` 解出来的那四列（**由 Kotlin 算**，理由见 `feedhub_core::library::ItemColumns`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibItemColumns {
    pub platform: String,
    pub is_video: bool,
    pub search_text: String,
    pub adult: bool,
}

/// 快照里的一本收藏夹（含墓碑）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibSyncFolderRow {
    pub uid: String,
    pub name: String,
    pub created_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
}

/// 快照里的一条收藏（含墓碑）。
///
/// `item_json` 是**要写回**的 JSON 原文（读出来时一定有）；`columns` 是 Kotlin 把 `item_json` 解成
/// `FeedItem` 之后算的列。两个都是 `None` = 这一行只处理墓碑（JSON 解不开，和 Kotlin 版的 `decodeElement`
/// 返回 null 一个意思）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibSyncFavoriteRow {
    pub key: String,
    pub folder_uid: Option<String>,
    pub added_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
    pub item_json: Option<String>,
    pub columns: Option<FfiLibItemColumns>,
}

/// 快照里的一条历史（访问信息和播放进度分开定序，含墓碑）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibSyncHistoryRow {
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
    pub columns: Option<FfiLibItemColumns>,
}

/// 快照里的一条屏蔽（含墓碑）。`user_json` 为 `None` = 这一行只处理墓碑。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibSyncBlockedRow {
    pub key: String,
    pub user_json: Option<String>,
    pub blocked_at: i64,
    pub rev_hlc: i64,
    pub rev_dev: String,
    pub deleted: bool,
}

/// 全量（含墓碑）：导出和合并都用它。
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct FfiLibSyncState {
    pub history_cleared_hlc: i64,
    pub folders: Vec<FfiLibSyncFolderRow>,
    pub favorites: Vec<FfiLibSyncFavoriteRow>,
    pub history: Vec<FfiLibSyncHistoryRow>,
    pub blocked: Vec<FfiLibSyncBlockedRow>,
}

/// 本机收藏夹 uid → 合并后的 uid（同名认领）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibFolderAlias {
    pub old_uid: String,
    pub new_uid: String,
}

/// `LibraryMerge.merge` 的结果。Rust 只需要 `state` / `aliases`；其余的计数原样还给 Kotlin 拼 `ImportResult`。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibMergeOutcome {
    pub state: FfiLibSyncState,
    pub aliases: Vec<FfiLibFolderAlias>,
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
    pub folders_created: i32,
    pub folders_claimed: i32,
    pub history_cleared_by_remote: bool,
    pub cleared_rows: i32,
}

/// 远端快照的报文元数据（写进 `sync_peer`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibRemoteMeta {
    pub device: String,
    pub seq: i64,
    pub generated_at: i64,
}

/// `apply_import` 的结果：合并结果原样带回 + 远端最大的 HLC（调用方要把本机时钟抬到它之后）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLibApplyResult {
    pub outcome: FfiLibMergeOutcome,
    pub remote_max_hlc: i64,
}

/// 收藏夹筛选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiLibFolderFilter {
    All,
    Default,
    Folder { id: i64 },
}

/// 收藏的"类型"筛选：混合 = 不筛，视频 = 视频 + 直播，图文 = 其他。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiLibContentType {
    Mixed,
    Video,
    Text,
}

// ===========================================================================
// 合并回调
// ===========================================================================

/// 宿主的合并引擎。**在 Rust 的写锁里被同步调用一次**，参数是刚读出来的本机状态和调用方给的远端状态；
/// 实现里只做纯计算（`LibraryMerge.merge`），**不要再调资料库**。
#[uniffi::export(with_foreign)]
pub trait FfiLibMergeHost: Send + Sync {
    fn merge(
        &self,
        local: FfiLibSyncState,
        remote: FfiLibSyncState,
        retention_cutoff_ms: Option<i64>,
    ) -> FfiLibMergeOutcome;
}

/// 把宿主的合并回调适配成 core 的 [`core::MergeHost`]，顺手把整份结果留下来还给外层。
struct MergeAdapter {
    host: Arc<dyn FfiLibMergeHost>,
    last: Mutex<Option<FfiLibMergeOutcome>>,
}

impl core::MergeHost for MergeAdapter {
    fn merge(&self, local: &core::SyncState, remote: &core::SyncState, cutoff: Option<i64>) -> core::MergedState {
        let outcome = self
            .host
            .merge(state_to_ffi(local), state_to_ffi(remote), cutoff);
        let merged = core::MergedState {
            state: state_from_ffi(&outcome.state),
            aliases: outcome
                .aliases
                .iter()
                .map(|a| (a.old_uid.clone(), a.new_uid.clone()))
                .collect(),
        };
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
        merged
    }
}

// ===========================================================================
// 平台对象
// ===========================================================================

/// 本地资料库（一个数据库文件）。**线程安全**：内部两个连接各一把锁，Kotlin 可以在任意线程上同步调。
#[derive(uniffi::Object)]
pub struct FfiLibrary {
    inner: core::Library,
    merge_host: Arc<dyn FfiLibMergeHost>,
}

#[uniffi::export]
impl FfiLibrary {
    /// 打开（不存在就建）[path]。`device_hint` = fresh 库要的 8 字节 base64url 设备 id（宿主生成），
    /// `uid_seed` = 迁移 v4 给老收藏夹补 uid 的随机种子（宿主生成 32 字节）。
    #[uniffi::constructor]
    pub fn new(
        path: String,
        device_hint: String,
        uid_seed: Vec<u8>,
        merge_host: Arc<dyn FfiLibMergeHost>,
    ) -> Result<Arc<Self>, FfiLibError> {
        Ok(Arc::new(FfiLibrary {
            inner: core::Library::open(&path, &device_hint, &uid_seed)?,
            merge_host,
        }))
    }

    /// 本机设备标识（8 字节 base64url，v4 起每个库一份）。
    pub fn device_id(&self) -> String {
        self.inner.device_id().to_string()
    }

    /// 关掉（先 checkpoint）。App 退出 / 测试收尾时调；之后再调别的方法会报错，不会崩。
    ///
    /// 名字不叫 `close`：UniFFI 生成的 Kotlin 类本身带一个 `AutoCloseable.close()`（释放这个对象的指针），
    /// 同名会 "Conflicting overloads" 编不过。
    pub fn shutdown(&self) {
        self.inner.close();
    }

    /// 本机 HLC 的起点（`last_hlc` 和四张表里最大的 rev 取最大）。
    pub fn last_hlc(&self) -> Result<i64, FfiLibError> {
        Ok(self.inner.last_hlc()?)
    }

    // ---------------------------------------------------------------- 打开时的一次性读取

    pub fn folders(&self) -> Result<Vec<FfiLibFolder>, FfiLibError> {
        Ok(self
            .inner
            .folders()?
            .into_iter()
            .map(|f| FfiLibFolder {
                id: f.id,
                name: f.name,
                uid: f.uid,
            })
            .collect())
    }

    pub fn favorite_refs(&self) -> Result<Vec<FfiLibFavoriteRef>, FfiLibError> {
        Ok(self
            .inner
            .favorite_refs()?
            .into_iter()
            .map(|f| FfiLibFavoriteRef {
                key: f.key,
                folder_id: f.folder_id,
                adult: f.adult,
            })
            .collect())
    }

    pub fn blocked_rows(&self) -> Result<Vec<FfiLibBlocked>, FfiLibError> {
        Ok(self
            .inner
            .blocked_rows()?
            .into_iter()
            .map(|b| FfiLibBlocked {
                user_json: b.user_json,
                blocked_at: b.blocked_at,
            })
            .collect())
    }

    // ---------------------------------------------------------------- 收藏

    #[allow(clippy::too_many_arguments)]
    pub fn add_favorite(
        &self,
        key: String,
        folder_id: Option<i64>,
        platform: String,
        is_video: bool,
        search_text: String,
        item_json: String,
        added_at: i64,
        adult: bool,
        rev: i64,
    ) -> Result<(), FfiLibError> {
        Ok(self.inner.add_favorite(
            &key,
            folder_id,
            &platform,
            is_video,
            &search_text,
            &item_json,
            added_at,
            adult,
            rev,
        )?)
    }

    pub fn remove_favorite(&self, key: String, rev: i64) -> Result<(), FfiLibError> {
        Ok(self.inner.remove_favorite(&key, rev)?)
    }

    pub fn create_folder(&self, name: String, uid: String, created_at: i64, rev: i64) -> Result<i64, FfiLibError> {
        Ok(self.inner.create_folder(&name, &uid, created_at, rev)?)
    }

    pub fn rename_folder(&self, id: i64, name: String, rev: i64) -> Result<(), FfiLibError> {
        Ok(self.inner.rename_folder(id, &name, rev)?)
    }

    pub fn delete_folder(&self, id: Option<i64>, delete_items: bool, rev: i64) -> Result<(), FfiLibError> {
        Ok(self.inner.delete_folder(id, delete_items, rev)?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn favorites(
        &self,
        folder: FfiLibFolderFilter,
        platform: Option<String>,
        content_type: FfiLibContentType,
        search: String,
        limit: i64,
    ) -> Result<Vec<FfiLibFavorite>, FfiLibError> {
        let folder = match folder {
            FfiLibFolderFilter::All => core::FolderFilter::All,
            FfiLibFolderFilter::Default => core::FolderFilter::Default,
            FfiLibFolderFilter::Folder { id } => core::FolderFilter::Folder(id),
        };
        let ty = match content_type {
            FfiLibContentType::Mixed => core::ContentType::Mixed,
            FfiLibContentType::Video => core::ContentType::Video,
            FfiLibContentType::Text => core::ContentType::Text,
        };
        Ok(self
            .inner
            .favorites(folder, platform.as_deref(), ty, &search, limit)?
            .into_iter()
            .map(|f| FfiLibFavorite {
                item_json: f.item_json,
                folder_id: f.folder_id,
                added_at: f.added_at,
            })
            .collect())
    }

    // ---------------------------------------------------------------- 历史

    #[allow(clippy::too_many_arguments)]
    pub fn record_visit(
        &self,
        key: String,
        platform: String,
        search_text: String,
        item_json: String,
        visited_at: i64,
        adult: bool,
        rev: i64,
    ) -> Result<(), FfiLibError> {
        Ok(self
            .inner
            .record_visit(&key, &platform, &search_text, &item_json, visited_at, adult, rev)?)
    }

    pub fn save_progress(
        &self,
        key: String,
        position_ms: i64,
        duration_ms: Option<i64>,
        rev: i64,
    ) -> Result<i32, FfiLibError> {
        Ok(self.inner.save_progress(&key, position_ms, duration_ms, rev)? as i32)
    }

    pub fn progress(&self, key: String) -> Result<Option<i64>, FfiLibError> {
        Ok(self.inner.progress(&key)?)
    }

    pub fn history(
        &self,
        platform: Option<String>,
        search: String,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FfiLibHistory>, FfiLibError> {
        Ok(self
            .inner
            .history(platform.as_deref(), &search, limit, offset)?
            .into_iter()
            .map(|h| FfiLibHistory {
                item_json: h.item_json,
                visited_at: h.visited_at,
                progress_ms: h.progress_ms,
                duration_ms: h.duration_ms,
            })
            .collect())
    }

    pub fn history_count(&self, platform: Option<String>, search: String) -> Result<i32, FfiLibError> {
        Ok(self.inner.history_count(platform.as_deref(), &search)? as i32)
    }

    pub fn remove_history(&self, key: String, rev: i64) -> Result<(), FfiLibError> {
        Ok(self.inner.remove_history(&key, rev)?)
    }

    pub fn clear_history(&self, rev: i64) -> Result<(), FfiLibError> {
        Ok(self.inner.clear_history(rev)?)
    }

    pub fn prune_history(&self, older_than_ms: i64) -> Result<i32, FfiLibError> {
        Ok(self.inner.prune_history(older_than_ms)? as i32)
    }

    // ---------------------------------------------------------------- 屏蔽

    pub fn block(&self, key: String, user_json: String, blocked_at: i64, rev: i64) -> Result<(), FfiLibError> {
        Ok(self.inner.block(&key, &user_json, blocked_at, rev)?)
    }

    pub fn unblock(&self, key: String, rev: i64) -> Result<(), FfiLibError> {
        Ok(self.inner.unblock(&key, rev)?)
    }

    // ---------------------------------------------------------------- 行为数据

    #[allow(clippy::too_many_arguments)]
    pub fn record_exposure(
        &self,
        key: String,
        platform: String,
        at: i64,
        dwell_ms: i64,
        surface: String,
        last_seen_at: i64,
        merge_window_ms: i64,
    ) -> Result<(), FfiLibError> {
        Ok(self.inner.record_exposure(
            &key,
            &platform,
            at,
            dwell_ms,
            &surface,
            last_seen_at,
            merge_window_ms,
        )?)
    }

    pub fn exposure_count(&self) -> Result<i32, FfiLibError> {
        Ok(self.inner.exposure_count()? as i32)
    }

    pub fn exposure_stats(
        &self,
        from_ms: i64,
        to_ms: i64,
        platform: Option<String>,
        surface: Option<String>,
    ) -> Result<FfiLibExposureStats, FfiLibError> {
        let stats = self
            .inner
            .exposure_stats(from_ms, to_ms, platform.as_deref(), surface.as_deref())?;
        Ok(FfiLibExposureStats {
            exposures: stats.exposures as i32,
            clicked: stats.clicked as i32,
            dwell_ms: stats.dwell_ms,
        })
    }

    pub fn exposure_rows(&self, limit: i64) -> Result<Vec<FfiLibExposureRow>, FfiLibError> {
        Ok(self
            .inner
            .exposure_rows(limit)?
            .into_iter()
            .map(|r| FfiLibExposureRow {
                key: r.key,
                platform: r.platform,
                surface: r.surface,
                exposed_at: r.exposed_at,
                last_seen_at: r.last_seen_at,
                dwell_ms: r.dwell_ms,
            })
            .collect())
    }

    pub fn prune_exposure(&self, older_than_ms: i64) -> Result<i32, FfiLibError> {
        Ok(self.inner.prune_exposure(older_than_ms)? as i32)
    }

    pub fn clear_exposure(&self) -> Result<(), FfiLibError> {
        Ok(self.inner.clear_exposure()?)
    }

    // ---------------------------------------------------------------- 曝光过的卡片内容

    #[allow(clippy::too_many_arguments)]
    pub fn record_seen_item(
        &self,
        key: String,
        platform: String,
        item_json: String,
        at: i64,
        feed_kind: String,
        batch_id: i64,
        platform_rank: i32,
    ) -> Result<(), FfiLibError> {
        Ok(self.inner.record_seen_item(
            &key,
            &platform,
            &item_json,
            at,
            &feed_kind,
            batch_id,
            platform_rank as i64,
        )?)
    }

    pub fn seen_item_count(&self) -> Result<i32, FfiLibError> {
        Ok(self.inner.seen_item_count()? as i32)
    }

    pub fn seen_items(&self, limit: i64) -> Result<Vec<FfiLibSeenItem>, FfiLibError> {
        Ok(self
            .inner
            .seen_items(limit)?
            .into_iter()
            .map(|r| FfiLibSeenItem {
                item_json: r.item_json,
                first_seen_at: r.first_seen_at,
                feed_kind: r.feed_kind,
                batch_id: r.batch_id,
                platform_rank: r.platform_rank as i32,
            })
            .collect())
    }

    pub fn prune_seen_items(&self, older_than_ms: i64) -> Result<i32, FfiLibError> {
        Ok(self.inner.prune_seen_items(older_than_ms)? as i32)
    }

    // ---------------------------------------------------------------- 向量

    pub fn insert_embeddings(&self, rows: Vec<FfiLibEmbedding>) -> Result<i32, FfiLibError> {
        let rows: Vec<core::EmbeddingRow> = rows
            .into_iter()
            .map(|r| core::EmbeddingRow {
                key: r.key,
                model: r.model,
                dim: r.dim as i64,
                vec: r.vec,
                created_at: r.created_at,
                source: r.source,
            })
            .collect();
        Ok(self.inner.insert_embeddings(&rows)? as i32)
    }

    pub fn embedding_keys(&self, keys: Vec<String>, model: String) -> Result<Vec<String>, FfiLibError> {
        Ok(self.inner.embedding_keys(&keys, &model)?)
    }

    pub fn embedding_count(&self) -> Result<i32, FfiLibError> {
        Ok(self.inner.embedding_count()? as i32)
    }

    pub fn embedding_stats(&self) -> Result<Vec<FfiLibEmbeddingStat>, FfiLibError> {
        Ok(self
            .inner
            .embedding_stats()?
            .into_iter()
            .map(|s| FfiLibEmbeddingStat {
                model: s.model,
                source: s.source,
                count: s.count as i32,
            })
            .collect())
    }

    pub fn embedding_backfill(&self, model: String, since_ms: i64, limit: i64) -> Result<Vec<FfiLibBackfill>, FfiLibError> {
        Ok(self
            .inner
            .embedding_backfill(&model, since_ms, limit)?
            .into_iter()
            .map(|r| FfiLibBackfill {
                key: r.key,
                item_json: r.item_json,
                at: r.at,
                source: r.source,
            })
            .collect())
    }

    pub fn prune_embeddings(&self, older_than_ms: i64) -> Result<i32, FfiLibError> {
        Ok(self.inner.prune_embeddings(older_than_ms)? as i32)
    }

    pub fn embeddings(&self, limit: i64) -> Result<Vec<FfiLibEmbedding>, FfiLibError> {
        Ok(self
            .inner
            .embeddings(limit)?
            .into_iter()
            .map(|r| FfiLibEmbedding {
                key: r.key,
                model: r.model,
                dim: r.dim as i32,
                vec: r.vec,
                created_at: r.created_at,
                source: r.source,
            })
            .collect())
    }

    pub fn clear_embeddings(&self) -> Result<(), FfiLibError> {
        Ok(self.inner.clear_embeddings()?)
    }

    // ---------------------------------------------------------------- 跨设备同步

    /// 快照序号 +1，返回新序号。
    pub fn bump_snapshot_seq(&self) -> Result<i64, FfiLibError> {
        Ok(self.inner.bump_snapshot_seq()?)
    }

    pub fn read_sync_state(&self) -> Result<FfiLibSyncState, FfiLibError> {
        Ok(state_to_ffi(&self.inner.read_sync_state()?))
    }

    pub fn last_imported_seq(&self, device: String) -> Result<i64, FfiLibError> {
        Ok(self.inner.last_imported_seq(&device)?)
    }

    pub fn sync_peers(&self) -> Result<Vec<FfiLibPeer>, FfiLibError> {
        Ok(self
            .inner
            .sync_peers()?
            .into_iter()
            .map(|p| FfiLibPeer {
                device_id: p.device_id,
                name: p.name,
                public_key: p.public_key,
                paired_at: p.paired_at,
            })
            .collect())
    }

    pub fn upsert_sync_peer(
        &self,
        device: String,
        name: String,
        public_key: String,
        paired_at: i64,
    ) -> Result<(), FfiLibError> {
        Ok(self
            .inner
            .upsert_sync_peer(&device, &name, &public_key, paired_at)?)
    }

    pub fn remove_sync_peer(&self, device: String) -> Result<bool, FfiLibError> {
        Ok(self.inner.remove_sync_peer(&device)?)
    }

    /// 导入：**在写锁 + 事务里**读本机状态 → 回调 `LibraryMerge` → 写回合并结果。
    pub fn apply_import(
        &self,
        remote: FfiLibSyncState,
        remote_meta: FfiLibRemoteMeta,
        retention_cutoff_ms: Option<i64>,
        hlc_last: i64,
    ) -> Result<FfiLibApplyResult, FfiLibError> {
        let adapter = MergeAdapter {
            host: self.merge_host.clone(),
            last: Mutex::new(None),
        };
        let remote_max_hlc = self.inner.apply_import(
            &state_from_ffi(&remote),
            &core::RemoteMeta {
                device: remote_meta.device,
                seq: remote_meta.seq,
                generated_at: remote_meta.generated_at,
            },
            retention_cutoff_ms,
            hlc_last,
            &adapter,
        )?;
        let outcome = adapter
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or_else(|| FfiLibError::Other {
                detail: "合并回调没有被调用（内部错误）".to_string(),
            })?;
        Ok(FfiLibApplyResult {
            outcome,
            remote_max_hlc,
        })
    }
}

// ===========================================================================
// core ↔ FFI
// ===========================================================================

fn state_to_ffi(state: &core::SyncState) -> FfiLibSyncState {
    FfiLibSyncState {
        history_cleared_hlc: state.history_cleared_hlc,
        folders: state
            .folders
            .iter()
            .map(|f| FfiLibSyncFolderRow {
                uid: f.uid.clone(),
                name: f.name.clone(),
                created_at: f.created_at,
                rev_hlc: f.rev_hlc,
                rev_dev: f.rev_dev.clone(),
                deleted: f.deleted,
            })
            .collect(),
        favorites: state
            .favorites
            .iter()
            .map(|f| FfiLibSyncFavoriteRow {
                key: f.key.clone(),
                folder_uid: f.folder_uid.clone(),
                added_at: f.added_at,
                rev_hlc: f.rev_hlc,
                rev_dev: f.rev_dev.clone(),
                deleted: f.deleted,
                item_json: f.item_json.clone(),
                columns: f.columns.as_ref().map(columns_to_ffi),
            })
            .collect(),
        history: state
            .history
            .iter()
            .map(|h| FfiLibSyncHistoryRow {
                key: h.key.clone(),
                visited_at: h.visited_at,
                rev_hlc: h.rev_hlc,
                rev_dev: h.rev_dev.clone(),
                deleted: h.deleted,
                progress_ms: h.progress_ms,
                duration_ms: h.duration_ms,
                progress_hlc: h.progress_hlc,
                progress_dev: h.progress_dev.clone(),
                item_json: h.item_json.clone(),
                columns: h.columns.as_ref().map(columns_to_ffi),
            })
            .collect(),
        blocked: state
            .blocked
            .iter()
            .map(|b| FfiLibSyncBlockedRow {
                key: b.key.clone(),
                user_json: b.user_json.clone(),
                blocked_at: b.blocked_at,
                rev_hlc: b.rev_hlc,
                rev_dev: b.rev_dev.clone(),
                deleted: b.deleted,
            })
            .collect(),
    }
}

fn state_from_ffi(state: &FfiLibSyncState) -> core::SyncState {
    core::SyncState {
        history_cleared_hlc: state.history_cleared_hlc,
        folders: state
            .folders
            .iter()
            .map(|f| core::SyncFolderRow {
                uid: f.uid.clone(),
                name: f.name.clone(),
                created_at: f.created_at,
                rev_hlc: f.rev_hlc,
                rev_dev: f.rev_dev.clone(),
                deleted: f.deleted,
            })
            .collect(),
        favorites: state
            .favorites
            .iter()
            .map(|f| core::SyncFavoriteRow {
                key: f.key.clone(),
                folder_uid: f.folder_uid.clone(),
                added_at: f.added_at,
                rev_hlc: f.rev_hlc,
                rev_dev: f.rev_dev.clone(),
                deleted: f.deleted,
                item_json: f.item_json.clone(),
                columns: f.columns.as_ref().map(columns_from_ffi),
            })
            .collect(),
        history: state
            .history
            .iter()
            .map(|h| core::SyncHistoryRow {
                key: h.key.clone(),
                visited_at: h.visited_at,
                rev_hlc: h.rev_hlc,
                rev_dev: h.rev_dev.clone(),
                deleted: h.deleted,
                progress_ms: h.progress_ms,
                duration_ms: h.duration_ms,
                progress_hlc: h.progress_hlc,
                progress_dev: h.progress_dev.clone(),
                item_json: h.item_json.clone(),
                columns: h.columns.as_ref().map(columns_from_ffi),
            })
            .collect(),
        blocked: state
            .blocked
            .iter()
            .map(|b| core::SyncBlockedRow {
                key: b.key.clone(),
                user_json: b.user_json.clone(),
                blocked_at: b.blocked_at,
                rev_hlc: b.rev_hlc,
                rev_dev: b.rev_dev.clone(),
                deleted: b.deleted,
            })
            .collect(),
    }
}

fn columns_to_ffi(c: &core::ItemColumns) -> FfiLibItemColumns {
    FfiLibItemColumns {
        platform: c.platform.clone(),
        is_video: c.is_video,
        search_text: c.search_text.clone(),
        adult: c.adult,
    }
}

fn columns_from_ffi(c: &FfiLibItemColumns) -> core::ItemColumns {
    core::ItemColumns {
        platform: c.platform.clone(),
        is_video: c.is_video,
        search_text: c.search_text.clone(),
        adult: c.adult,
    }
}
