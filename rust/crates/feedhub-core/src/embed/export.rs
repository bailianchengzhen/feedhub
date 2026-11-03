//! 行为数据的**导出格式**（JSON Lines，一行一个对象，`table` 字段说明它属于哪张表）。
//!
//! `app/src/commonMain/kotlin/feedhub/app/export/BehaviorExport.kt` 的 Rust 版。
//! 行的顺序、字段的顺序、字段名、浮点写法**全部照 kotlinx.serialization 的输出**，这样同一个库导两次
//! （Rust 一份、老的 Kotlin 实现一份）能逐字节对上：
//!
//! | 规则 | 落点 |
//! |---|---|
//! | 对象键按"插入顺序"写（kotlinx 的 `JsonObject` 保序） | 下面每个 `add_*` 函数 |
//! | 字符串转义：`"` `\` `\n` `\r` `\t` `\b` `\f`，其余 < 0x20 写 `\u00xx`（小写），非 ASCII 原样 | [`crate::hosted::push_json_string`] |
//! | `null` 的字段**整个不出现**（`explicitNulls = false`） | 每个 `if let Some(...)` |
//! | `Float` 的写法 = Java `Float.toString`（`1.0` 而不是 `1`、`1.0E-4` 而不是 `0.0001`） | [`java_float_string`] |
//! | 向量是 **f16 小端原始字节的 base64**（标准 base64，带 `=` 补齐） | [`super::text::float_vec_to_half_bytes`] + [`encode_base64`] |
//!
//! **不含任何 Cookie / token / URL**：内容只导出"回放用得上的字段"（平台、id、类型、标题、摘要、作者名、
//! 圈子、时间、语言、18+），媒体直链 / 播放地址 / 外链一律不写；信号里只有 key 和枚举名。

use std::collections::{HashMap, HashSet};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use crate::hosted::push_json_string;

use super::item::EmbedItem;
use super::signal::{
    build_signals, BlockedAuthorRow, ExposureRow, FavoriteRow, HistoryRow, SeenRow, Snapshot, Surface,
};
use super::taste::build_profile;
use super::text::{float_vec_to_half_bytes, vec_from_half_bytes};

/// 导出文件的格式版本（`meta` 行里的 `schema`）；改了字段就加一。
pub const EXPORT_SCHEMA_VERSION: i32 = 2;

/// 导出时摘要最多留多少个字（UTF-16 单元；回放只要语义）。
pub const EXPORT_SUMMARY_MAX_CHARS: usize = 500;

/// 导出的模型标识（`meta` 行里的 `embedding_model`，是 CPU 那份的空间标识，和 Kotlin 常量一致）。
pub const EXPORT_EMBEDDING_MODEL: &str = super::text::EMBEDDING_MODEL_ID;

/// 一行 `seen_item`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSeen {
    pub item: EmbedItem,
    pub first_seen_at: i64,
    /// `FeedKind.key`（`recommend` / `following` / …）。
    pub feed_kind: String,
    pub batch_id: i64,
    pub platform_rank: i32,
}

/// 一行 `exposure`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportExposure {
    pub key: String,
    pub platform: String,
    /// `ExposureSurface.key`（`home` / `detail` / …）。
    pub surface: String,
    pub exposed_at: i64,
    pub last_seen_at: i64,
    pub dwell_ms: i64,
}

/// 一行 `history`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportHistory {
    pub item: EmbedItem,
    pub visited_at: i64,
    pub progress_ms: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// 一行 `favorite`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportFavorite {
    pub item: EmbedItem,
    pub folder: Option<String>,
    pub added_at: i64,
}

/// 一行 `item_embedding`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportEmbedding {
    pub key: String,
    pub model: String,
    pub dim: i32,
    /// f16 小端原始字节。
    pub vec: Vec<u8>,
    pub created_at: i64,
    pub source: String,
}

/// 一次导出的输入（宿主从库里读出来，按"最近的在前"给）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportInput {
    pub seen: Vec<ExportSeen>,
    pub exposures: Vec<ExportExposure>,
    pub history: Vec<ExportHistory>,
    pub favorites: Vec<ExportFavorite>,
    pub embeddings: Vec<ExportEmbedding>,
    /// 屏蔽的作者（`platform:id` → 屏蔽时刻）。
    pub blocked: Vec<BlockedAuthorRow>,
    /// 设置里的屏蔽词（**只导出"命中过"这件事，不导出词本身**）。
    pub block_words: Vec<String>,
    pub now: i64,
}

/// 导出结果：整份文件的行（**不带换行**，宿主按 `\n` 连起来写）+ 每张表几行 + 字节数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOutput {
    pub lines: Vec<String>,
    /// `(table, 行数)`，按第一次出现的顺序。
    pub tables: Vec<(String, i32)>,
    /// 写进文件的字节数（每行 UTF-8 字节数 + 1 个 `\n`）。
    pub bytes: i64,
}

/// 拼行的累加器（行 + 每张表几行，按第一次出现的顺序）。
struct Lines {
    out: Vec<String>,
    tables: Vec<(String, i32)>,
    at: HashMap<String, usize>,
}

impl Lines {
    fn new(capacity: usize) -> Self {
        Lines {
            out: Vec::with_capacity(capacity),
            tables: Vec::new(),
            at: HashMap::new(),
        }
    }

    fn add(&mut self, table: &str, body: impl FnOnce(&mut String)) {
        let mut line = format!("{{\"table\":\"{table}\"");
        body(&mut line);
        line.push('}');
        self.out.push(line);
        match self.at.get(table).copied() {
            Some(i) => self.tables[i].1 += 1,
            None => {
                self.at.insert(table.to_string(), self.tables.len());
                self.tables.push((table.to_string(), 1));
            }
        }
    }
}

/// 把库里这几张表拼成一个 JSON Lines 文件的**全部行**。
pub fn export(input: &ExportInput) -> ExportOutput {
    let mut lines = Lines::new(
        input.seen.len()
            + input.exposures.len()
            + input.history.len()
            + input.favorites.len()
            + input.embeddings.len()
            + 8,
    );

    // meta
    lines.add("meta", |line| {
        line.push_str(",\"schema\":");
        line.push_str(&EXPORT_SCHEMA_VERSION.to_string());
        line.push_str(",\"exported_at\":");
        line.push_str(&input.now.to_string());
        line.push_str(",\"embedding_model\":");
        push_json_string(line, EXPORT_EMBEDDING_MODEL);
        line.push_str(",\"app\":\"feedhub\"");
    });

    // 信号（和 App 里打分**同一套**纯函数算，回放才能复现同样的权重 / 衰减）
    let signals = build_signals(&Snapshot {
        favorites: input
            .favorites
            .iter()
            .map(|f| FavoriteRow {
                item: f.item.clone(),
                added_at: f.added_at,
            })
            .collect(),
        history: input
            .history
            .iter()
            .map(|h| HistoryRow {
                item: h.item.clone(),
                visited_at: h.visited_at,
                progress_ms: h.progress_ms,
                duration_ms: h.duration_ms,
            })
            .collect(),
        exposures: input
            .exposures
            .iter()
            .map(|e| ExposureRow {
                key: e.key.clone(),
                platform: e.platform.clone(),
                surface: Surface::from_key(&e.surface),
                exposed_at: e.exposed_at,
                last_seen_at: e.last_seen_at,
                dwell_ms: e.dwell_ms,
            })
            .collect(),
        seen: input
            .seen
            .iter()
            .map(|s| SeenRow {
                item: s.item.clone(),
                first_seen_at: s.first_seen_at,
            })
            .collect(),
        blocked_authors: input.blocked.clone(),
        block_words: input.block_words.clone(),
    });

    for row in &input.seen {
        lines.add("seen_item", |line| {
            line.push_str(",\"key\":");
            push_json_string(line, &row.item.key);
            line.push_str(",\"platform\":");
            push_json_string(line, &row.item.platform);
            line.push_str(",\"item\":");
            push_sanitized_item(line, &row.item);
            line.push_str(",\"first_seen_at\":");
            line.push_str(&row.first_seen_at.to_string());
            line.push_str(",\"feed_kind\":");
            push_json_string(line, &row.feed_kind);
            line.push_str(",\"batch_id\":");
            line.push_str(&row.batch_id.to_string());
            line.push_str(",\"platform_rank\":");
            line.push_str(&row.platform_rank.to_string());
        });
    }
    for row in &input.exposures {
        lines.add("exposure", |line| {
            line.push_str(",\"key\":");
            push_json_string(line, &row.key);
            line.push_str(",\"platform\":");
            push_json_string(line, &row.platform);
            line.push_str(",\"surface\":");
            push_json_string(line, &row.surface);
            line.push_str(",\"exposed_at\":");
            line.push_str(&row.exposed_at.to_string());
            line.push_str(",\"last_seen_at\":");
            line.push_str(&row.last_seen_at.to_string());
            line.push_str(",\"dwell_ms\":");
            line.push_str(&row.dwell_ms.to_string());
        });
    }
    for row in &input.history {
        lines.add("history", |line| {
            line.push_str(",\"key\":");
            push_json_string(line, &row.item.key);
            line.push_str(",\"platform\":");
            push_json_string(line, &row.item.platform);
            line.push_str(",\"item\":");
            push_sanitized_item(line, &row.item);
            line.push_str(",\"visited_at\":");
            line.push_str(&row.visited_at.to_string());
            if let Some(progress) = row.progress_ms {
                line.push_str(",\"progress_ms\":");
                line.push_str(&progress.to_string());
            }
            if let Some(duration) = row.duration_ms {
                line.push_str(",\"duration_ms\":");
                line.push_str(&duration.to_string());
            }
        });
    }
    for row in &input.favorites {
        lines.add("favorite", |line| {
            line.push_str(",\"key\":");
            push_json_string(line, &row.item.key);
            line.push_str(",\"platform\":");
            push_json_string(line, &row.item.platform);
            line.push_str(",\"item\":");
            push_sanitized_item(line, &row.item);
            line.push_str(",\"folder\":");
            match &row.folder {
                Some(folder) => push_json_string(line, folder),
                None => line.push_str("null"),
            }
            line.push_str(",\"added_at\":");
            line.push_str(&row.added_at.to_string());
        });
    }

    // 信号行：类型 / 条目 key / 时间 / 强度 / 这一刻的权重
    for sig in &signals {
        lines.add("signal", |line| {
            line.push_str(",\"key\":");
            push_json_string(line, &sig.key);
            line.push_str(",\"kind\":");
            push_json_string(line, sig.kind.key());
            if let Some(platform) = &sig.platform {
                line.push_str(",\"platform\":");
                push_json_string(line, platform);
            }
            line.push_str(",\"at\":");
            line.push_str(&sig.at.to_string());
            line.push_str(",\"strength\":");
            line.push_str(&java_float_string(sig.strength));
            line.push_str(",\"weight\":");
            line.push_str(&java_float_string(sig.effective_weight(input.now)));
        });
    }

    // 画像行（每个模型一份）：只用"有向量的信号"，f16 base64
    if !input.embeddings.is_empty() {
        let signal_keys: HashSet<&str> = signals.iter().map(|s| s.key.as_str()).collect();
        let mut models: Vec<&str> = Vec::new();
        let mut grouped: HashMap<&str, Vec<&ExportEmbedding>> = HashMap::new();
        for row in &input.embeddings {
            if !signal_keys.contains(row.key.as_str()) {
                continue;
            }
            if !grouped.contains_key(row.model.as_str()) {
                models.push(row.model.as_str());
            }
            grouped.entry(row.model.as_str()).or_default().push(row);
        }
        for model in models {
            let rows = &grouped[model];
            let mut vectors: HashMap<String, Vec<f32>> = HashMap::with_capacity(rows.len() * 2);
            for row in rows {
                if let Some(vec) = vec_from_half_bytes(&row.vec, row.dim.max(0) as usize) {
                    vectors.insert(row.key.clone(), vec);
                }
            }
            let profile = build_profile(&signals, model, &vectors, input.now);
            if !profile.usable() {
                continue;
            }
            let positive = profile.positive.as_ref().unwrap();
            lines.add("profile", |line| {
                line.push_str(",\"model\":");
                push_json_string(line, model);
                line.push_str(",\"dim\":");
                line.push_str(&positive.len().to_string());
                line.push_str(",\"positive\":");
                push_json_string(line, &encode_base64(&float_vec_to_half_bytes(positive)));
                if let Some(negative) = &profile.negative {
                    line.push_str(",\"negative\":");
                    push_json_string(line, &encode_base64(&float_vec_to_half_bytes(negative)));
                }
                line.push_str(",\"positive_signals\":");
                line.push_str(&profile.positive_signals.to_string());
                line.push_str(",\"negative_signals\":");
                line.push_str(&profile.negative_signals.to_string());
                line.push_str(",\"positive_weight\":");
                line.push_str(&java_float_string(profile.positive_weight));
                line.push_str(",\"negative_weight\":");
                line.push_str(&java_float_string(profile.negative_weight));
            });
        }
    }

    // 向量行
    for row in &input.embeddings {
        lines.add("item_embedding", |line| {
            line.push_str(",\"key\":");
            push_json_string(line, &row.key);
            line.push_str(",\"model\":");
            push_json_string(line, &row.model);
            line.push_str(",\"dim\":");
            line.push_str(&row.dim.to_string());
            line.push_str(",\"vec\":");
            push_json_string(line, &encode_base64(&row.vec));
            line.push_str(",\"created_at\":");
            line.push_str(&row.created_at.to_string());
            line.push_str(",\"source\":");
            push_json_string(line, &row.source);
        });
    }

    let mut bytes = 0i64;
    for line in &lines.out {
        bytes += line.len() as i64 + 1; // + '\n'
    }
    ExportOutput {
        lines: lines.out,
        tables: lines.tables,
        bytes,
    }
}

/// 导出用的内容：**只留文字和分类**，媒体 / 视频 / 外链地址全部丢掉。
pub fn push_sanitized_item(out: &mut String, item: &EmbedItem) {
    out.push_str("{\"platform\":");
    push_json_string(out, &item.platform);
    out.push_str(",\"id\":");
    push_json_string(out, &item.id);
    out.push_str(",\"kind\":");
    push_json_string(out, &item.kind);
    if let Some(title) = &item.title {
        out.push_str(",\"title\":");
        push_json_string(out, title);
    }
    if let Some(summary) = &item.summary {
        out.push_str(",\"summary\":");
        push_json_string(out, &super::text::truncate_utf16(summary, EXPORT_SUMMARY_MAX_CHARS));
    }
    if let Some(author) = &item.author_name {
        out.push_str(",\"author\":");
        push_json_string(out, author);
    }
    if let Some(at) = item.published_at {
        out.push_str(",\"published_at\":");
        out.push_str(&at.to_string());
    }
    if let Some(language) = &item.language {
        out.push_str(",\"language\":");
        push_json_string(out, language);
    }
    if let Some(community) = &item.community {
        out.push_str(",\"community\":");
        push_json_string(out, community);
    }
    if let Some(reason) = &item.reason {
        out.push_str(",\"reason\":");
        push_json_string(out, reason);
    }
    out.push_str(",\"adult\":");
    out.push_str(if item.adult { "true" } else { "false" });
    out.push('}');
}

/// 标准 base64（带 `=` 补齐）——导出的向量就是它。
pub fn encode_base64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// 一行 JSON 里出现的表名（`table` 是每行第一个键）。
pub fn table_of(line: &str) -> Option<&str> {
    let prefix = "{\"table\":\"";
    let rest = line.strip_prefix(prefix)?;
    let end = rest.find('"')?;
    if end == 0 {
        None
    } else {
        Some(&rest[..end])
    }
}

/// Java `Float.toString` 的等价物（kotlinx 写 `Float` 就是它的输出）。
///
/// 规则（`FloatingDecimal.toJavaFormatString`）：
/// - `NaN` / `Infinity` / `-Infinity`；`0.0` / `-0.0`；
/// - `1e-3 <= |v| < 1e7` 用普通写法，**小数点后至少一位**（`1.0`、`0.35`、`0.001`）；
/// - 其余用科学计数法：`1.0E-4`、`1.23E7`（指数不带 `+`）。
///
/// 数字本身用 Rust 的**最短往返**表示（和 Java 的 Ryū 同一个口径），所以两边打印出来是同一串数字。
pub fn java_float_string(value: f32) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let negative = value < 0.0;
    let abs = value.abs();
    // 最短往返：`{:e}` 给 "3.5e-1" 这种（尾数不带多余的 0）
    let sci = format!("{:e}", abs);
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };

    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if (-3..7).contains(&exp) {
        // 普通写法
        let n = digits.len() as i32;
        if exp >= 0 {
            let int_len = exp + 1;
            for i in 0..int_len {
                out.push(if i < n { digit(digits, i as usize) } else { '0' });
            }
            out.push('.');
            if n > int_len {
                for i in int_len..n {
                    out.push(digit(digits, i as usize));
                }
            } else {
                out.push('0');
            }
        } else {
            out.push_str("0.");
            for _ in 0..(-exp - 1) {
                out.push('0');
            }
            out.push_str(digits);
        }
    } else {
        out.push(digit(digits, 0));
        out.push('.');
        if digits.len() > 1 {
            out.push_str(&digits[1..]);
        } else {
            out.push('0');
        }
        out.push('E');
        out.push_str(&exp.to_string());
    }
    out
}

fn digit(digits: &str, i: usize) -> char {
    digits.as_bytes().get(i).map(|b| *b as char).unwrap_or('0')
}

