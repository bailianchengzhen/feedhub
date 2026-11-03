//! 建表与迁移：**和 `SqliteLibrary.migrate()` 一字不差**（表名 / 列名 / 索引 / 版本号 / 建表语句原文都是数据契约）。
//!
//! 三件事必须逐字节对上，不然"用户已有的库原地打开就能用"就不成立：
//!
//! 1. **版本号**（`PRAGMA user_version`，现在 9）：新版本号 > 9 时拒绝打开（不猜、不改人家的库）；
//! 2. **语句顺序**：v1 建四张表 + 索引，v2 曝光表，v3 两列 18+ 标记（并按 JSON 回填），v4 同步四张表的
//!    rev / 墓碑 / 收藏夹 uid（可重入），v5 配对信息，v6 `seen_item`，v7 `item_embedding`，v8 向量来源列，
//!    v9 `item_embedding` 主键改成 `(key, model)`（封面向量和纯文字向量并存，可重入）；
//! 3. **建表语句的原文**：SQLite 把 `CREATE TABLE` 的文字原样存进 `sqlite_master.sql`，`.dump` / 排障时会
//!    原样打印出来。Kotlin 版用的是三引号（换行 + 24 个空格缩进），这里**照抄同样的空白**，
//!    所以下面那组常量是贴着左边写的——**别重排、别让 rustfmt 动它们**。
//!
//! 随机量（收藏夹的 `sync_uid`）也是从外面给的：`uid_seed` 是 Kotlin 传进来的 32 字节随机种子，
//! 按 SplitMix64 展开成 UUIDv4 形状的字符串（`.so` 里不取随机数，测试才能逐字节比两份库）。

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::{hlc_of, LibError, UidMinter};

/// 表结构版本（和 Kotlin `SqliteLibrary.SCHEMA_VERSION` 一致；`core` 的 `LIBRARY_SYNC_SCHEMA` 也看这个数）。
pub const SCHEMA_VERSION: i32 = 9;

/// 本机设备标识在 `sync_meta` 里的键（v4 起的四项）。
const META_DEVICE: &str = "device_id";

pub fn migrate(conn: &mut Connection, device_hint: &str, uid_seed: &[u8]) -> Result<(), LibError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let version = version as i32;
    if version > SCHEMA_VERSION {
        return Err(LibError::Unsupported {
            version,
            supported: SCHEMA_VERSION,
        });
    }
    if version < 1 {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(SQL_V1_FOLDER)?;
        tx.execute_batch(SQL_V1_FAVORITE)?;
        tx.execute_batch("CREATE INDEX favorite_added ON favorite(added_at DESC)")?;
        tx.execute_batch("CREATE INDEX favorite_folder_idx ON favorite(folder_id)")?;
        tx.execute_batch(SQL_V1_HISTORY)?;
        tx.execute_batch("CREATE INDEX history_visited ON history(visited_at DESC)")?;
        tx.execute_batch("CREATE INDEX history_platform_visited ON history(platform, visited_at DESC)")?;
        tx.execute_batch(SQL_V1_BLOCKED)?;
        tx.execute_batch("PRAGMA user_version = 1")?;
        tx.commit()?;
    }
    if version < 2 {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 行为数据（曝光 / 停留）：一个 "key + surface" 可以有多行，隔得久了就是新的一次曝光，窗口内的合并进同一行。
        tx.execute_batch(SQL_V2_EXPOSURE)?;
        tx.execute_batch("CREATE INDEX exposure_key_seen ON exposure(key, surface, last_seen_at DESC)")?;
        tx.execute_batch("CREATE INDEX exposure_exposed_at ON exposure(exposed_at DESC)")?;
        tx.execute_batch("PRAGMA user_version = 2")?;
        tx.commit()?;
    }
    if version < 3 {
        // 平台标成 18+ 的条目整条隐藏：is_adult 落成列，读取时在 SQL 里直接滤掉。
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("ALTER TABLE favorite ADD COLUMN is_adult INTEGER NOT NULL DEFAULT 0")?;
        tx.execute_batch("ALTER TABLE history ADD COLUMN is_adult INTEGER NOT NULL DEFAULT 0")?;
        // 老库里的行按 JSON 里的 adult 回填一次（加这个字段之前的 JSON 一律读到 false，回填只是保险）
        for table in ["favorite", "history"] {
            let mut stmt = tx.prepare(&format!("SELECT key, item_json FROM {table}"))?;
            let hidden = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter(|(_, json)| json_adult(json))
                .map(|(key, _)| key)
                .collect::<Vec<_>>();
            for key in hidden {
                tx.execute(&format!("UPDATE {table} SET is_adult = 1 WHERE key = ?1"), [key])?;
            }
        }
        tx.execute_batch("PRAGMA user_version = 3")?;
        tx.commit()?;
    }
    if version < 4 {
        migrate_v4(conn, device_hint, uid_seed)?;
    }
    if version < 5 {
        // 设备配对：sync_peer 多了对方的名字 / 公钥 / 配对时间。
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        add_column(&tx, "sync_peer", "name", "TEXT")?;
        add_column(&tx, "sync_peer", "public_key", "TEXT")?;
        add_column(&tx, "sync_peer", "paired_at", "INTEGER NOT NULL DEFAULT 0")?;
        tx.execute_batch("PRAGMA user_version = 5")?;
        tx.commit()?;
    }
    if version < 6 {
        // 曝光过的卡片内容：`exposure` 只存 key，没点开过的卡片在 `history` 里没有 item_json。
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(SQL_V6_SEEN_ITEM)?;
        tx.execute_batch("CREATE INDEX IF NOT EXISTS seen_item_first_seen ON seen_item(first_seen_at DESC)")?;
        tx.execute_batch("PRAGMA user_version = 6")?;
        tx.commit()?;
    }
    if version < 7 {
        // 卡片向量：key → 一条 256 维 float16 向量。
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(SQL_V7_EMBEDDING)?;
        tx.execute_batch("CREATE INDEX IF NOT EXISTS item_embedding_created ON item_embedding(created_at DESC)")?;
        tx.execute_batch("PRAGMA user_version = 7")?;
        tx.commit()?;
    }
    if version < 8 {
        // 向量来源：feed / favorite / history 补算。老行没有来源，一律当 feed。
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        add_column(&tx, "item_embedding", "source", "TEXT NOT NULL DEFAULT 'feed'")?;
        tx.execute_batch("PRAGMA user_version = 8")?;
        tx.commit()?;
    }
    if version < 9 {
        migrate_v9(conn)?;
    }
    Ok(())
}

/// v9：`item_embedding` 的主键从 `key` 改成 `(key, model)`。
///
/// 封面向量（文字 + 封面图）和纯文字向量是**两个 model**，要**并存、不替换**。v7 建表时一个 key 只有
/// 一行，封面向量写进去会把纯文字那条顶掉。SQLite 不能 ALTER 主键，只能建新表 → 拷数据 → 删旧表 →
/// 改名 → 重建 `created_at` 索引；老行一条不丢，**不需要重算库里已有的向量**。
///
/// **可重入**（和 Kotlin 版一字不差）：先读 `PRAGMA table_info` 认主键，已经是 `(key, model)` 就只把
/// 版本号推上去、一条 DDL 都不发——迁移做到一半或者有人把 `user_version` 调回 8 都不会打不开。
fn migrate_v9(conn: &mut Connection) -> Result<(), LibError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // 主键是哪几列、按主键里的顺序（`pk` 是 1..n 的序号，非主键列是 0）
    let pk: Vec<String> = {
        let mut stmt = tx.prepare("PRAGMA table_info(item_embedding)")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(5)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut keyed: Vec<(String, i64)> = rows.into_iter().filter(|(_, at)| *at > 0).collect();
        keyed.sort_by_key(|(_, at)| *at);
        keyed.into_iter().map(|(name, _)| name).collect()
    };
    if pk.as_slice() != ["key", "model"] {
        tx.execute_batch(SQL_V9_EMBEDDING)?;
        tx.execute_batch(
            "INSERT OR REPLACE INTO item_embedding_v9(key, model, dim, vec, created_at, source) \
             SELECT key, model, dim, vec, created_at, source FROM item_embedding",
        )?;
        tx.execute_batch("DROP TABLE item_embedding")?;
        tx.execute_batch("ALTER TABLE item_embedding_v9 RENAME TO item_embedding")?;
        tx.execute_batch("CREATE INDEX IF NOT EXISTS item_embedding_created ON item_embedding(created_at DESC)")?;
    }
    tx.execute_batch("PRAGMA user_version = 9")?;
    tx.commit()?;
    Ok(())
}

/// v4：四张表各加"改动时间 + 设备 + 墓碑"，收藏夹加跨设备稳定 uid，历史加单独的进度时间；
/// 再建 `sync_meta` / `sync_peer`。老行按自己的时间回填 `rev_hlc`（设备 = 本机），`deleted` 一律 0。
///
/// **可重入**（和 Kotlin 版一致）：`IF NOT EXISTS` / 先查列再加 / 只回填还是 0 的行——迁移做到一半、
/// 或者有人把 `user_version` 调回旧值（测试里就这么模拟 v1 / v2 老库），再打开也不会打不开。
fn migrate_v4(conn: &mut Connection, device_hint: &str, uid_seed: &[u8]) -> Result<(), LibError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS sync_meta(k TEXT PRIMARY KEY, v TEXT NOT NULL)")?;
    tx.execute_batch(SQL_V4_SYNC_PEER)?;

    let device: Option<String> = tx
        .query_row("SELECT v FROM sync_meta WHERE k = ?1", [META_DEVICE], |r| r.get(0))
        .optional()?;
    let device = match device {
        Some(d) if !d.is_empty() => d,
        // Kotlin 那边是 `SyncDevice.random()`：8 字节 base64url。这里用宿主给的随机 id（同一种取值方式）。
        _ => {
            let d = device_hint.to_string();
            tx.execute(
                "INSERT INTO sync_meta(k, v) VALUES(?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                rusqlite::params![META_DEVICE, d],
            )?;
            d
        }
    };

    add_column(&tx, "favorite_folder", "sync_uid", "TEXT")?;
    add_column(&tx, "favorite_folder", "rev_hlc", "INTEGER NOT NULL DEFAULT 0")?;
    add_column(&tx, "favorite_folder", "rev_dev", "TEXT")?;
    add_column(&tx, "favorite_folder", "deleted", "INTEGER NOT NULL DEFAULT 0")?;
    tx.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS favorite_folder_uid ON favorite_folder(sync_uid)")?;

    add_column(&tx, "favorite", "rev_hlc", "INTEGER NOT NULL DEFAULT 0")?;
    add_column(&tx, "favorite", "rev_dev", "TEXT")?;
    add_column(&tx, "favorite", "deleted", "INTEGER NOT NULL DEFAULT 0")?;
    add_column(&tx, "favorite", "folder_uid", "TEXT")?;

    add_column(&tx, "history", "rev_hlc", "INTEGER NOT NULL DEFAULT 0")?;
    add_column(&tx, "history", "rev_dev", "TEXT")?;
    add_column(&tx, "history", "deleted", "INTEGER NOT NULL DEFAULT 0")?;
    add_column(&tx, "history", "progress_hlc", "INTEGER NOT NULL DEFAULT 0")?;
    add_column(&tx, "history", "progress_dev", "TEXT")?;

    add_column(&tx, "blocked_user", "rev_hlc", "INTEGER NOT NULL DEFAULT 0")?;
    add_column(&tx, "blocked_user", "rev_dev", "TEXT")?;
    add_column(&tx, "blocked_user", "deleted", "INTEGER NOT NULL DEFAULT 0")?;

    // 收藏夹：没有 uid 的补一个 uuid，rev 还是 0 的按 created_at 回填
    let mut minter = UidMinter::new(uid_seed);
    let folders: Vec<(i64, i64, Option<String>, i64)> = {
        let mut stmt = tx.prepare("SELECT id, created_at, sync_uid, rev_hlc FROM favorite_folder")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (id, created, uid, rev) in folders {
        if uid.as_deref().map_or(true, |u| u.is_empty()) || rev == 0 {
            let uid = uid.filter(|u| !u.is_empty()).unwrap_or_else(|| minter.next());
            let rev = if rev == 0 { hlc_of(created) } else { rev };
            tx.execute(
                "UPDATE favorite_folder SET sync_uid = ?1, rev_hlc = ?2, rev_dev = ?3 WHERE id = ?4",
                rusqlite::params![uid, rev, device, id],
            )?;
        }
    }
    // 收藏：rev = added_at，folder_uid 从收藏夹 uid 查
    let favorites: Vec<(String, i64, i64)> = {
        let mut stmt = tx.prepare("SELECT key, added_at, rev_hlc FROM favorite")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (key, added, rev) in favorites {
        if rev == 0 {
            tx.execute(
                "UPDATE favorite SET rev_hlc = ?1, rev_dev = ?2, deleted = 0 WHERE key = ?3",
                rusqlite::params![hlc_of(added), device, key],
            )?;
        }
    }
    tx.execute_batch(
        "UPDATE favorite SET folder_uid = (SELECT f.sync_uid FROM favorite_folder f WHERE f.id = favorite.folder_id) \
         WHERE folder_id IS NOT NULL AND (folder_uid IS NULL OR folder_uid = '')",
    )?;
    // 历史：rev = visited_at；进度按"和访问同一时刻"回填（没有进度也是这个时间，比较时进度那边自然不赢）
    let history: Vec<(String, i64, i64, i64)> = {
        let mut stmt = tx.prepare("SELECT key, visited_at, rev_hlc, progress_hlc FROM history")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (key, visited, rev, progress_hlc) in history {
        if rev == 0 || progress_hlc == 0 {
            let rev = if rev == 0 { hlc_of(visited) } else { rev };
            let progress_hlc = if progress_hlc == 0 { rev } else { progress_hlc };
            tx.execute(
                "UPDATE history SET rev_hlc = ?1, rev_dev = ?2, progress_hlc = ?3, progress_dev = ?4, deleted = 0 \
                 WHERE key = ?5",
                rusqlite::params![rev, device, progress_hlc, device, key],
            )?;
        }
    }
    let blocked: Vec<(String, i64, i64)> = {
        let mut stmt = tx.prepare("SELECT key, blocked_at, rev_hlc FROM blocked_user")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (key, at, rev) in blocked {
        if rev == 0 {
            tx.execute(
                "UPDATE blocked_user SET rev_hlc = ?1, rev_dev = ?2, deleted = 0 WHERE key = ?3",
                rusqlite::params![hlc_of(at), device, key],
            )?;
        }
    }
    tx.execute_batch("PRAGMA user_version = 4")?;
    tx.commit()?;
    Ok(())
}

/// 迁移用：这一列在不在（表名 / 列名都是代码里写死的常量，不拼用户输入）。
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, LibError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 迁移用：没有这一列才加（可重入，见 v4 的注释）。
fn add_column(conn: &Connection, table: &str, column: &str, ty: &str) -> Result<(), LibError> {
    if !column_exists(conn, table, column)? {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"))?;
    }
    Ok(())
}

/// v3 的回填：`item_json` 里 `adult` 为 true 的行（Kotlin 是 `decode(json)?.adult == true`）。
fn json_adult(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("adult").and_then(|a| a.as_bool()))
        .unwrap_or(false)
}

// ===========================================================================
// 建表语句原文（**空白是数据契约**：SQLite 原样存进 sqlite_master.sql，`.dump` 会打印出来）
// ===========================================================================

const SQL_V1_FOLDER: &str = r#"CREATE TABLE favorite_folder (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        name TEXT NOT NULL,
                        created_at INTEGER NOT NULL)"#;

const SQL_V1_FAVORITE: &str = r#"CREATE TABLE favorite (
                        key TEXT PRIMARY KEY,
                        folder_id INTEGER,
                        platform TEXT NOT NULL,
                        is_video INTEGER NOT NULL,
                        search_text TEXT NOT NULL,
                        item_json TEXT NOT NULL,
                        added_at INTEGER NOT NULL)"#;

const SQL_V1_HISTORY: &str = r#"CREATE TABLE history (
                        key TEXT PRIMARY KEY,
                        platform TEXT NOT NULL,
                        search_text TEXT NOT NULL,
                        item_json TEXT NOT NULL,
                        visited_at INTEGER NOT NULL,
                        progress_ms INTEGER,
                        duration_ms INTEGER)"#;

const SQL_V1_BLOCKED: &str = r#"CREATE TABLE blocked_user (
                        key TEXT PRIMARY KEY,
                        user_json TEXT NOT NULL,
                        blocked_at INTEGER NOT NULL)"#;

const SQL_V2_EXPOSURE: &str = r#"CREATE TABLE exposure (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        key TEXT NOT NULL,
                        platform TEXT NOT NULL,
                        surface TEXT NOT NULL,
                        exposed_at INTEGER NOT NULL,
                        last_seen_at INTEGER NOT NULL,
                        dwell_ms INTEGER NOT NULL)"#;

const SQL_V4_SYNC_PEER: &str = r#"CREATE TABLE IF NOT EXISTS sync_peer(
                        peer TEXT PRIMARY KEY,
                        last_seq INTEGER NOT NULL DEFAULT 0,
                        last_seen_hlc INTEGER NOT NULL DEFAULT 0,
                        last_generated_at INTEGER NOT NULL DEFAULT 0)"#;

const SQL_V6_SEEN_ITEM: &str = r#"CREATE TABLE IF NOT EXISTS seen_item (
                        key TEXT PRIMARY KEY,
                        platform TEXT NOT NULL,
                        item_json TEXT NOT NULL,
                        first_seen_at INTEGER NOT NULL,
                        feed_kind TEXT NOT NULL,
                        batch_id INTEGER NOT NULL,
                        platform_rank INTEGER NOT NULL)"#;

const SQL_V7_EMBEDDING: &str = r#"CREATE TABLE IF NOT EXISTS item_embedding (
                        key TEXT PRIMARY KEY,
                        model TEXT NOT NULL,
                        dim INTEGER NOT NULL,
                        vec BLOB NOT NULL,
                        created_at INTEGER NOT NULL)"#;

/// v9 的新表：主键 `(key, model)`。**空白和 Kotlin 版一样（28 个空格）**——迁移完之后 SQLite 会把这条
/// `CREATE TABLE` 改名成 `CREATE TABLE "item_embedding" (...)`，原文进 `sqlite_master.sql`，对拍要逐字节比。
const SQL_V9_EMBEDDING: &str = r#"CREATE TABLE item_embedding_v9 (
                            key TEXT NOT NULL,
                            model TEXT NOT NULL,
                            dim INTEGER NOT NULL,
                            vec BLOB NOT NULL,
                            created_at INTEGER NOT NULL,
                            source TEXT NOT NULL DEFAULT 'feed',
                            PRIMARY KEY (key, model))"#;

