//! **解码能力账本**（`app/.../video/DecodeCapability.kt` + `DecodeLedger.kt` 的 Rust 版）。
//!
//! 为什么要它：单看"有没有硬解"会得出"AV1 最好"（码率最低、CPU 也不高），但这台机器（AMD 核显）在 4K 下
//! AV1 解不稳（4K60 出帧间隔 p95 冲到 30~42ms，HEVC / AVC 都是 17ms）。所以让**实际播放**当测量台：
//! 引擎在播放中盯着出帧节奏，发现某个编码在某个负载下持续迟到就 [`DecodeLedger::record`] 一下，
//! 下一次"自动"选编码就会避开它（见 [`crate::player::select::auto_order`]）。
//!
//! 落盘的失效判据（缺一个旧记录就会在换卡 / 升级驱动之后变成错的）：
//! - **环境指纹**（宿主给，桌面是 `decodeEnvironmentFingerprint()`：VA 设备的 PCI `vendor:device` +
//!   libva 报的驱动串 + GStreamer 版本）：任何一项变了，整份旧记录作废——换显卡 / 换 eGPU /
//!   升级 Mesa / 升级 GStreamer 之后都要重新学；
//! - **过期**（[`TTL_MS`]，30 天）：没有新实测就作废重测，免得一次偶发卡顿（当时机器在忙别的事）
//!   永久把一种编码打入冷宫。
//!
//! 仍然**只降不升**：一次表现好不能证明高负载也行，跨进程也不抬高已经记下的上限（[`record`] 取 min）。
//!
//! ## 落盘格式（**一个字节都不能变**）
//!
//! Kotlin 侧是 `FeedJson.encodeToString(DecodeLedger.File.serializer(), ...)`（`ignoreUnknownKeys` /
//! `explicitNulls = false` / `coerceInputValues`），也就是：
//!
//! ```json
//! {"fingerprint":"1002:1234|Mesa 25.0.7|GStreamer 1.26.2",
//!  "records":{"AV1":{"cap":166094438400,"at":1760000000000}}}
//! ```
//!
//! 三条口径（和 `xhs/feeditem_json.rs` 一样的做法：手写读写器，不给全局模型加 serde derive）：
//! 1. **字段顺序**按 Kotlin 的声明顺序（`fingerprint` 先、`records` 后；每条记录 `cap` 先、`at` 后）；
//! 2. **默认值也写**（`fingerprint = ""` 照样写出来）；`records` 空表写 `{}`；
//! 3. 数字都是十进制整数，不加空格。
//!
//! 读的时候**宽容**（和 `FeedJson` 的配置对齐）：未知字段忽略、`records` 里认不出的编码名丢掉、
//! 缺字段用默认值、坏 JSON 整份当没有。
//!
//! ## 宿主给什么、不给什么
//!
//! 这一层**不读时钟、不读存储**：`now_ms` 是参数，落盘是宿主拿 [`DecodeLedger::encode`] 的字符串去写
//! （`KeyValueStore` 的 `app.decodeLedger` 键）。[`DecodeLedger::attach`] 的语义和 Kotlin 一样：
//! 接一份已经读出来的存档 + 一个"现算指纹"的回调；**拿不到指纹就不读也不写**旧记录
//! （宁可不落盘，也不拿一份不知道对谁有效的记录去选流）。

use crate::play::VideoCodec;

/// 记下的上限多久没有新的实测就作废（毫秒）：30 天。
///
/// 理由（照 Kotlin 的注释）：`GstPlayerEngine.trackPace` 的判据（120 帧窗口里迟到 > 5%）只可能误伤
/// "偶发卡"的编码，方向安全但会留下错记录；30 天大约是一个发行版驱动 / GStreamer 小版本的间隔，
/// 也是"用户还会再看视频"的时间尺度——比"永久"好，又不会每次启动都重测。
pub const TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// 落盘用的键（放进 App 的 `KeyValueStore`；桌面是 `~/.config/feedhub/state.json`）。
pub const KEY: &str = "app.decodeLedger";

/// 这台机器各编码的解码能力（Kotlin `DecodeCapability`）。
///
/// 两个维度都是**实测**出来的，不是写死的"不要 AV1"：
/// - [`hardware`](Self::hardware)：设备侧确认过有真硬解（VA-API 的那个 profile 有 VLD 入口点、元素也在）；
/// - [`smooth_pixel_rate`](Self::smooth_pixel_rate)：播放过程中量出来的"这种编码在这个负载以下能稳定
///   实时出帧"的上限，单位是每秒像素数（宽 × 高 × 帧率）。只在**实测到跟不上**的时候才写进去，
///   所以空表 = 还没测出问题。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecodeCapability {
    /// 确认有真硬解的编码（"真" = 设备侧确认，不是只看元素存不存在）。
    pub hardware: Vec<VideoCodec>,
    /// 实测"稳定实时"的像素率上限（每秒像素数）；不在这里 = 没测出问题，不限制。
    pub smooth_pixel_rate: Vec<(VideoCodec, u64)>,
}

impl DecodeCapability {
    /// 只知道"能硬解哪些"，没有实测负载数据（安卓的 MediaCodec 探测、测试里的假引擎用这个）。
    pub fn of_hardware(hardware: &[VideoCodec]) -> Self {
        Self {
            hardware: hardware.to_vec(),
            smooth_pixel_rate: Vec::new(),
        }
    }

    /// 这个像素率下这种编码能不能实时：没测出上限就不限制；负载未知（`None`）也不限制。
    pub fn smooth_at(&self, codec: VideoCodec, pixels_per_second: Option<u64>) -> bool {
        let Some(pps) = pixels_per_second else {
            return true;
        };
        match self
            .smooth_pixel_rate
            .iter()
            .find(|(c, _)| *c == codec)
            .map(|(_, v)| *v)
        {
            None => true,
            Some(limit) => pps <= limit,
        }
    }

    /// 这种编码记下的上限（没记过就是 `None`）。
    pub fn rate(&self, codec: VideoCodec) -> Option<u64> {
        self.smooth_pixel_rate
            .iter()
            .find(|(c, _)| *c == codec)
            .map(|(_, v)| *v)
    }
}

/// 账本里一条编码的记录：`cap` = 能稳定实时的像素率上限，`at` = 最后一次实测到的时刻（epoch 毫秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerEntry {
    pub cap: u64,
    pub at: i64,
}

/// 落盘里的整份账本（Kotlin `DecodeLedger.File`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerFile {
    pub fingerprint: String,
    /// 按**编码名**（`AV1` / `HEVC` / `AVC`）存，和 Kotlin 的 `Map<String, Record>` 一样。
    pub records: Vec<(String, LedgerEntry)>,
}

impl LedgerFile {
    /// 编码名（Kotlin `VideoCodec.name`：`AVC` / `HEVC` / `AV1` / `OTHER`）。
    pub fn codec_name(codec: VideoCodec) -> &'static str {
        match codec {
            VideoCodec::Avc => "AVC",
            VideoCodec::Hevc => "HEVC",
            VideoCodec::Av1 => "AV1",
            VideoCodec::Other => "OTHER",
        }
    }

    /// 编码名 → 编码（认不出就是 `None`）。
    pub fn codec_from_name(name: &str) -> Option<VideoCodec> {
        match name {
            "AVC" => Some(VideoCodec::Avc),
            "HEVC" => Some(VideoCodec::Hevc),
            "AV1" => Some(VideoCodec::Av1),
            "OTHER" => Some(VideoCodec::Other),
            _ => None,
        }
    }
}

/// 落盘要做的动作（Kotlin 的 `persistNow()` / `clear()` 那一对写入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Persist {
    /// 删掉这个键（Kotlin `store.put(KEY, null)`）。
    Delete,
    /// 写这份内容（`app.decodeLedger`）。
    Write(String),
}

/// 解码能力账本（Kotlin 的 `object DecodeLedger`）。
///
/// 一个进程一份；宿主（`feedhub.app.video.DecodeLedger` 的 Kotlin 门面）持有一个
/// `Arc<Mutex<DecodeLedger>>`，所有方法都在锁里跑（播放引擎和选流会在不同线程上调）。
///
/// **不读时钟、不写存储**：`now_ms` 是参数，落盘由宿主照 [`Persist`] 去做
/// （`store.put("app.decodeLedger", …)` / `store.put(…, null)`）。
#[derive(Debug, Default)]
pub struct DecodeLedger {
    smooth_rate: Vec<(VideoCodec, u64)>,
    measured_at: Vec<(VideoCodec, i64)>,
    /// 接进来的存档原文（还没解释；第一次用到才解析，见 [`ensure_loaded`](DecodeLedger::ensure_loaded)）。
    stored: Option<String>,
    /// 环境指纹（`attach` 给的；`None` = 拿不到，不读也不写）。
    fingerprint: Option<String>,
    /// 检查工具 / 截图灌了假数据之后不再落盘，绝不动用户的数据。
    persist: bool,
    /// 是不是接过落盘（`attach` 过）。没接过的都不写（Kotlin 那边 `store == null`）。
    attached: bool,
    /// 待宿主去做的落盘动作（`take_persist` 取走）。
    pending: Option<Persist>,
}

impl DecodeLedger {
    /// 新建一个**空**账本（没接落盘：`record` 只记在内存里，`take_persist` 返回 `None`）。
    pub fn new() -> Self {
        Self {
            smooth_rate: Vec::new(),
            measured_at: Vec::new(),
            stored: None,
            fingerprint: None,
            persist: true,
            attached: false,
            pending: None,
        }
    }

    /// 接上落盘（桌面组装时调一次；**拿不到指纹也要调**，那样才会"不读也不写"）。
    ///
    /// - `stored`：宿主从 `KeyValueStore.get("app.decodeLedger")` 读出来的字符串（没有就是 `None`）；
    /// - `fingerprint`：当前环境指纹（拿不到就 `None` → **不读也不写**旧记录）。
    ///
    /// 语义照 Kotlin 的 `attach`：清掉内存里的记录，重新按存档装（惰性——第一次用到才解析）。
    pub fn attach(&mut self, stored: Option<&str>, fingerprint: Option<&str>) {
        self.smooth_rate.clear();
        self.measured_at.clear();
        self.stored = stored.map(|s| s.to_string());
        self.fingerprint = fingerprint.map(|s| s.to_string());
        self.persist = true;
        self.attached = true;
        self.pending = None;
    }

    /// 记一次实测：`codec` 在 `pixels_per_second` 这个负载下稳不稳。
    ///
    /// 不稳就把上限压到它的**三分之一**；
    /// 稳的样本**什么都不做**（Kotlin 里那句 `if (smooth) return` 在刷新时刻之前）。
    /// 三分之一的出处：4K60（498 Mpx/s）实测迟到 → 上限 166 Mpx/s，于是 1080P60（124）继续用 AV1，
    /// 1440P60（221）和 4K 都换 HEVC / AVC——1440P60 的 AV1 实测已经开始偶发迟到（p95 22ms / 最大 27ms）。
    pub fn record(
        &mut self,
        codec: Option<VideoCodec>,
        pixels_per_second: u64,
        smooth: bool,
        now_ms: i64,
    ) {
        let Some(codec) = codec else { return };
        if codec == VideoCodec::Other || pixels_per_second == 0 {
            return;
        }
        self.ensure_loaded(now_ms);
        if smooth {
            return;
        }
        let cap = pixels_per_second / 3;
        match self.smooth_rate.iter_mut().find(|(c, _)| *c == codec) {
            Some((_, old)) => {
                if *old > cap {
                    *old = cap;
                }
            }
            None => self.smooth_rate.push((codec, cap)),
        }
        match self.measured_at.iter_mut().find(|(c, _)| *c == codec) {
            Some((_, at)) => {
                if *at < now_ms {
                    *at = now_ms;
                }
            }
            None => self.measured_at.push((codec, now_ms)),
        }
        self.persist_now();
    }

    /// 这种编码现在记下的上限；没记过（或已过期作废）返回 `None` = 不限制。
    pub fn rate(&mut self, codec: VideoCodec, now_ms: i64) -> Option<u64> {
        self.ensure_loaded(now_ms);
        self.alive(codec, now_ms)
    }

    /// 现在有效的全部上限（按 AVC / HEVC / AV1 的顺序）。
    pub fn snapshot(&mut self, now_ms: i64) -> Vec<(VideoCodec, u64)> {
        self.ensure_loaded(now_ms);
        let mut out = Vec::new();
        for c in [
            VideoCodec::Avc,
            VideoCodec::Hevc,
            VideoCodec::Av1,
            VideoCodec::Other,
        ] {
            if let Some(cap) = self.alive(c, now_ms) {
                out.push((c, cap));
            }
        }
        out
    }

    /// 现在有效的全部上限，包成一份 [`DecodeCapability`]（`hardware` 由调用方给）。
    pub fn capability(&mut self, hardware: &[VideoCodec], now_ms: i64) -> DecodeCapability {
        DecodeCapability {
            hardware: hardware.to_vec(),
            smooth_pixel_rate: self.snapshot(now_ms),
        }
    }

    /// 这种编码最后一次实测的时刻（没记过就是 `None`）。界面那一行的""用它。
    pub fn measured_at(&mut self, codec: VideoCodec) -> Option<i64> {
        self.measured_at
            .iter()
            .find(|(c, _)| *c == codec)
            .map(|(_, at)| *at)
    }

    /// 有没有测出过结果。
    pub fn is_empty(&mut self, now_ms: i64) -> bool {
        self.snapshot(now_ms).is_empty()
    }

    /// 界面那一行要显示的内容（每种有记录的编码一句），如 `AV1 ≤ 1080P60`；没有就 `None`。
    ///
    /// 日期串由**宿主**给（`day_of` 把时刻格式化成 `yyyy-MM-dd`：那要用系统时区 / 地区，是操作系统的事）。
    pub fn summary_text<F>(&mut self, now_ms: i64, day_of: F) -> Option<String>
    where
        F: Fn(i64) -> String,
    {
        let snapshot = self.snapshot(now_ms);
        if snapshot.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        for (codec, cap) in snapshot {
            let head = format!("{} ≤ {}", codec_label(codec), pixel_rate_label(cap));
            let at = self
                .measured_at
                .iter()
                .find(|(c, _)| *c == codec)
                .map(|(_, at)| *at)
                .filter(|at| now_ms - *at <= TTL_MS);
            match at {
                None => parts.push(head),
                Some(at) => parts.push(format!("{head}（{} 实测）", day_of(at))),
            }
        }
        Some(parts.join("；"))
    }

    /// 「清除，重新测」：内存和落盘一起清掉，下一次播放重新学。
    ///
    /// 之后 [`take_persist`](Self::take_persist) 会给一个 [`Persist::Delete`]（Kotlin
    /// `store.put(KEY, null)`）——不清的话下一次读又会把旧的装回来。
    pub fn clear(&mut self) {
        self.smooth_rate.clear();
        self.measured_at.clear();
        self.stored = None; // 别让下一次读又把刚清掉的旧记录装回来
        if self.attached && self.fingerprint.is_some() {
            self.pending = Some(Persist::Delete);
        }
    }

    /// `attach` 之后立刻把存档解析掉（宿主想"读完马上清理过期记录"时用）。
    /// 之后 [`take_persist`](Self::take_persist) 会给要做的动作。
    pub fn load_now(&mut self, now_ms: i64) {
        self.ensure_loaded(now_ms);
    }

    /// 测试用：只清内存，不动落盘；也把接上的存储断掉。
    pub fn reset(&mut self) {
        self.smooth_rate.clear();
        self.measured_at.clear();
        self.stored = None;
        self.fingerprint = None;
        self.persist = true;
        self.attached = false;
        self.pending = None;
    }

    /// 检查工具 / 截图 / 演示用：换成一份**只在内存里**的假记录（不落盘，绝不改用户的数据）。
    pub fn seed_for_demo(&mut self, codec: VideoCodec, cap: u64, at: i64) {
        self.smooth_rate.clear();
        self.measured_at.clear();
        self.smooth_rate.push((codec, cap));
        self.measured_at.push((codec, at));
        self.persist = false;
        self.stored = None;
        self.pending = None;
    }

    /// 取走待落盘的动作（宿主每次调用完都问一遍，有就照做）。
    pub fn take_persist(&mut self) -> Option<Persist> {
        self.pending.take()
    }

    /// 落盘内容（不看"有没有待写"）：调试 / 测试用。
    pub fn encode_now(&self) -> String {
        let fingerprint = self.fingerprint.clone().unwrap_or_default();
        let mut entries: Vec<(String, LedgerEntry)> = Vec::new();
        for (codec, cap) in &self.smooth_rate {
            let name = LedgerFile::codec_name(*codec).to_string();
            let at = self
                .measured_at
                .iter()
                .find(|(c, _)| c == codec)
                .map(|(_, at)| *at)
                .unwrap_or(0);
            match entries.iter_mut().find(|(n, _)| *n == name) {
                Some((_, e)) => *e = LedgerEntry { cap: *cap, at },
                None => entries.push((name, LedgerEntry { cap: *cap, at })),
            }
        }
        encode_file(&LedgerFile {
            fingerprint,
            records: entries,
        })
    }

    /// 这个编码的记录还活着就返回上限，过期了就顺手扔掉。
    fn alive(&mut self, codec: VideoCodec, now_ms: i64) -> Option<u64> {
        let cap = self
            .smooth_rate
            .iter()
            .find(|(c, _)| *c == codec)
            .map(|(_, v)| *v)?;
        let Some(at) = self
            .measured_at
            .iter()
            .find(|(c, _)| *c == codec)
            .map(|(_, v)| *v)
        else {
            return Some(cap);
        };
        if now_ms - at <= TTL_MS {
            return Some(cap);
        }
        self.smooth_rate.retain(|(c, _)| *c != codec);
        self.measured_at.retain(|(c, _)| *c != codec);
        self.persist_now();
        None
    }

    /// 第一次真正用到账本时（选流 / 播放中记账）才解析那份存档。
    ///
    /// 三条判据（和 Kotlin `ensureLoaded` 一样）：**拿不到当前指纹就什么都不做**（既不读也不动存储）、
    /// **指纹对不上整份作废**、**过期的条目扔掉**；后两种都要顺手把存储重写一遍。
    fn ensure_loaded(&mut self, now_ms: i64) {
        let Some(stored) = self.stored.take() else {
            return;
        };
        if !self.persist {
            return;
        }
        let Some(file) = decode_file(&stored) else {
            return;
        };
        let Some(current) = self.fingerprint.clone() else {
            // 拿不到当前环境指纹就不信任旧记录（也不动存储：可能只是这次没 GStreamer）
            return;
        };
        if file.fingerprint != current {
            // 换卡 / 升级驱动 / 升级 GStreamer：旧记录整份作废，顺手把存储也清干净
            self.persist_now();
            return;
        }
        let mut dropped = false;
        for (name, rec) in &file.records {
            let Some(codec) = LedgerFile::codec_from_name(name) else {
                dropped = true;
                continue;
            };
            if rec.cap == 0 || now_ms - rec.at > TTL_MS {
                dropped = true;
                continue;
            }
            self.smooth_rate.push((codec, rec.cap));
            self.measured_at.push((codec, rec.at));
        }
        if dropped {
            self.persist_now();
        }
    }

    fn persist_now(&mut self) {
        if !self.persist || !self.attached || self.fingerprint.is_none() {
            return;
        }
        self.pending = Some(Persist::Write(self.encode_now()));
    }
}

/// 一个整数在 JSON 里的写法（十进制，不补零）。
fn push_i64(out: &mut String, value: i64) {
    out.push_str(&value.to_string());
}

/// 一份账本 → JSON（**逐字节和 Kotlin `FeedJson` 的输出一致**）。
pub fn encode_file(file: &LedgerFile) -> String {
    let mut out = String::with_capacity(128);
    out.push_str("{\"fingerprint\":");
    push_json_string(&mut out, &file.fingerprint);
    out.push_str(",\"records\":{");
    for (i, (name, entry)) in file.records.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_json_string(&mut out, name);
        out.push_str(":{\"cap\":");
        push_i64(&mut out, entry.cap as i64);
        out.push_str(",\"at\":");
        push_i64(&mut out, entry.at);
        out.push('}');
    }
    out.push_str("}}");
    out
}

/// JSON → 一份账本（宽松：未知字段忽略、认不出的编码名丢掉、坏 JSON 就是 `None`）。
pub fn decode_file(text: &str) -> Option<LedgerFile> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = value.as_object()?;
    let fingerprint = obj
        .get("fingerprint")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let mut records: Vec<(String, LedgerEntry)> = Vec::new();
    if let Some(map) = obj.get("records").and_then(|v| v.as_object()) {
        for (name, rec) in map {
            if LedgerFile::codec_from_name(name).is_none() {
                continue;
            }
            let Some(rec) = rec.as_object() else { continue };
            let cap = rec.get("cap").and_then(json_u64).unwrap_or(0);
            let at = rec.get("at").and_then(json_i64).unwrap_or(0);
            records.push((name.clone(), LedgerEntry { cap, at }));
        }
    }
    Some(LedgerFile {
        fingerprint,
        records,
    })
}

fn json_u64(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => n.as_u64().or_else(|| n.as_i64().map(|i| i.max(0) as u64)),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn json_i64(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_u64().map(|u| u as i64)),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// JSON 字符串字面量（**和 Kotlin 的 `Json.encodeToString(String)` 逐字节一致**）。
fn push_json_string(out: &mut String, value: &str) {
    crate::hosted::push_json_string(out, value);
}

/// 像素率上限 → "这一档以下还能实时"的标签：取不超过上限的最大标准档位；比最小档还小就说
/// "低于 480P30"。
pub fn pixel_rate_label(rate: u64) -> String {
    let steps = pixel_rate_steps();
    match steps.iter().rev().find(|(_, v)| *v <= rate) {
        Some((name, _)) => (*name).to_string(),
        None => format!("低于 {}", steps[0].0),
    }
}

/// 已知的"清晰度 + 帧率"档位负载（像素 / 秒），**按像素率从小到大**（1440P30 比 1080P60 小，别按名字排）。
pub fn pixel_rate_steps() -> Vec<(&'static str, u64)> {
    vec![
        ("480P30", 640 * 480 * 30),
        ("720P30", 1280 * 720 * 30),
        ("720P60", 1280 * 720 * 60),
        ("1080P30", 1920 * 1080 * 30),
        ("1440P30", 2560 * 1440 * 30),
        ("1080P60", 1920 * 1080 * 60),
        ("1440P60", 2560 * 1440 * 60),
        ("4K30", 3840 * 2160 * 30),
        ("4K60", 3840 * 2160 * 60),
        ("8K30", 7680 * 4320 * 30),
        ("8K60", 7680 * 4320 * 60),
    ]
}

/// 编码在界面上的名字（账本那一行用）。
fn codec_label(codec: VideoCodec) -> String {
    crate::player::select::codec_label(Some(codec))
}

/// [`VideoStream.frame_rate`](crate::play::VideoStream::frame_rate) 里的帧率文本（`30`、`60/1`、
/// `30000/1001`）→ 每秒帧数；认不出就 `None`。
///
/// **分母认不出时按 1 算**（Kotlin 是 `parts.getOrNull(1)?.toDoubleOrNull() ?: 1.0`）：`"30/"` 这种尾随斜杠
/// 的写法（有些平台就是这么给的）Kotlin 读出来是 30 fps，早先这里 `?` 掉了，对拍当场抓出来。
/// 分子认不出、分母是 0、算出来不是正数 → `None`，这几条和 Kotlin 一致。
pub fn frame_rate_of(text: Option<&str>) -> Option<f64> {
    let text = text?;
    if crate::hosted::is_blank(text) {
        return None;
    }
    let trimmed = crate::hosted::kotlin_trim(text);
    let mut parts = trimmed.split('/');
    let num = parts.next()?.parse::<f64>().ok()?;
    let den = match parts.next() {
        None => 1.0,
        Some(d) => d.parse::<f64>().unwrap_or(1.0),
    };
    if den == 0.0 {
        return None;
    }
    let fps = num / den;
    if fps > 0.0 {
        Some(fps)
    } else {
        None
    }
}

/// 一路视频轨的负载（每秒像素数）；缺宽 / 高 / 帧率就 `None`（那就按"不限制"处理）。
///
/// Kotlin 是 `(w.toLong() * h * fps).toLong()`——**先算出浮点再截断**（`59.94` 这类分数帧率会
/// 落成 `4293853184` 这种数），这里照样走 `f64` 再截断，保证两边数字一模一样。
///
/// **饱和到 `i64::MAX` 而不是 `u64::MAX`**：`f64 as u64` 在 Rust 里是把超范围的值饱和到 `u64::MAX`
/// （`18446744073709551615`），过到 Kotlin 的 `Long` 就变成 **`-1`**；而 Kotlin 的 `Double.toLong()`
/// 饱和到 `Long.MAX_VALUE`。畸形的帧率文本（`"999999999999999999999"`）会让两边差一整圈，对拍抓出来了。
/// 这个上限本身在现实里不会出现（4K60 只有 4.98 亿），但口径必须对齐。
pub fn pixel_rate_of(width: Option<i32>, height: Option<i32>, frame_rate: Option<&str>) -> Option<u64> {
    let w = width.filter(|v| *v > 0)? as f64;
    let h = height.filter(|v| *v > 0)? as f64;
    let fps = frame_rate_of(frame_rate)?;
    let rate = w * h * fps;
    if !rate.is_finite() || rate < 0.0 {
        return None;
    }
    Some((rate as u64).min(i64::MAX as u64))
}

