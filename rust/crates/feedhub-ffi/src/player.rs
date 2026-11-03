//! **播放器决策层**的 UniFFI 边界。
//!
//! Rust 侧是 [`feedhub_core::player`]（选流 / 解码账本 / 弹幕 / 清单改写 / 中转映射 / 播放状态机）；
//! 这里只有 record / enum、一层薄转发，和**几个"对象"**（有跨调用状态的那几件）。Kotlin 门面在
//! `rust-bridge/.../feedhub/player/`。
//!
//! 三条和别处一样的约定：
//!
//! 1. **一次调用换一整批**：一段弹幕的过滤 / 一整段排布表 / 一整份清单 / 一次选流各一次调用，
//!    不逐条、不逐帧；
//! 2. **拿不到就是 `None`**，不要编默认值；
//! 3. **不读时钟、不读存储**：`now_ms` 一律是参数，落盘由宿主照
//!    [`FfiPersist`] 做。
//!
//! 前缀：这一节用 `FfiPlayer*` / `FfiDanmaku*`。`FfiDanmaku`（一条弹幕）已经在
//! [`crate::play`] 里了（B站 播放那一批建的），这里**直接复用**，不再造第二份。

use std::sync::{Arc, Mutex, MutexGuard};

use feedhub_core::play::{Danmaku as CoreDanmaku, DanmakuMode, VideoCodec};
use feedhub_core::player::danmaku as core_danmaku;
use feedhub_core::player::ledger as core_ledger;
use feedhub_core::player::manifest as core_manifest;
use feedhub_core::player::relay as core_relay;
use feedhub_core::player::select as core_select;
use feedhub_core::player::session as core_session;

use crate::play::{to_ffi_danmaku, to_ffi_danmaku_mode, FfiDanmaku};

// ===========================================================================
// 错误
// ===========================================================================

/// 播放器层的错误。字段不叫 `message`。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiPlayerError {
    /// 清单 / 模板解析不出来（宿主按平台原来的报错处理：中转回 502 / 400）。
    #[error("{detail}")]
    Parse { detail: String },
    /// 参数对不上（占位符个数、索引越界、分 P 不存在）。
    #[error("{detail}")]
    BadInput { detail: String },
}

// ===========================================================================
// 枚举：和 Kotlin 一一对应
// ===========================================================================

/// 视频编码（Kotlin `VideoCodec`）：**直接用播放那一批已经定好的 [`FfiPlayCodec`]**，不再造第二份
/// （UniFFI 不导出 `type` 别名，所以这里只能 `use` 进来，别写 `pub type`）。
use crate::play::FfiPlayCodec as FfiPlayerCodec;

/// 设置里的编码偏好（Kotlin `CodecPreference`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCodecPreference {
    Auto,
    Avc,
    Hevc,
    Av1,
}

impl FfiCodecPreference {
    fn to_core(self) -> core_select::CodecPreference {
        match self {
            Self::Auto => core_select::CodecPreference::Auto,
            Self::Avc => core_select::CodecPreference::Avc,
            Self::Hevc => core_select::CodecPreference::Hevc,
            Self::Av1 => core_select::CodecPreference::Av1,
        }
    }
}

/// 设置里的默认清晰度（Kotlin `DefaultQuality`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDefaultQuality {
    Highest,
    P1080,
    P720,
}

impl FfiDefaultQuality {
    fn to_core(self) -> core_select::DefaultQuality {
        match self {
            Self::Highest => core_select::DefaultQuality::Highest,
            Self::P1080 => core_select::DefaultQuality::P1080,
            Self::P720 => core_select::DefaultQuality::P720,
        }
    }

    fn from_core(q: core_select::DefaultQuality) -> Self {
        match q {
            core_select::DefaultQuality::Highest => Self::Highest,
            core_select::DefaultQuality::P1080 => Self::P1080,
            core_select::DefaultQuality::P720 => Self::P720,
        }
    }
}

pub(crate) fn codec_to_core(codec: FfiPlayerCodec) -> VideoCodec {
    match codec {
        FfiPlayerCodec::Avc => VideoCodec::Avc,
        FfiPlayerCodec::Hevc => VideoCodec::Hevc,
        FfiPlayerCodec::Av1 => VideoCodec::Av1,
        FfiPlayerCodec::Other => VideoCodec::Other,
    }
}

pub(crate) fn codec_from_core(codec: VideoCodec) -> FfiPlayerCodec {
    crate::play::to_ffi_codec(codec)
}

// ===========================================================================
// 选流
// ===========================================================================

/// 本机的解码能力（Kotlin `DecodeCapability`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiDecodeCapability {
    /// 设备侧确认过有真硬解的编码。
    pub hardware: Vec<FfiPlayerCodec>,
    /// 实测"稳定实时"的像素率上限（每秒像素数）；不在表里 = 没测出问题。
    pub smooth_pixel_rate: Vec<FfiPixelRateEntry>,
}

/// 一条"这种编码在多少像素/秒以下能稳定实时"的记录。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPixelRateEntry {
    pub codec: FfiPlayerCodec,
    pub pixels_per_second: u64,
}

impl FfiDecodeCapability {
    pub(crate) fn to_core(&self) -> core_ledger::DecodeCapability {
        core_ledger::DecodeCapability {
            hardware: self.hardware.iter().copied().map(codec_to_core).collect(),
            smooth_pixel_rate: self
                .smooth_pixel_rate
                .iter()
                .map(|e| (codec_to_core(e.codec), e.pixels_per_second))
                .collect(),
        }
    }

    fn from_core(cap: &core_ledger::DecodeCapability) -> Self {
        Self {
            hardware: cap.hardware.iter().copied().map(codec_from_core).collect(),
            smooth_pixel_rate: cap
                .smooth_pixel_rate
                .iter()
                .map(|(c, v)| FfiPixelRateEntry {
                    codec: codec_from_core(*c),
                    pixels_per_second: *v,
                })
                .collect(),
        }
    }
}

/// 选出来的一路流（Kotlin `StreamChoice` 的下标版）：`video` / `audio` / `progressive` 都是
/// 那份 `PlayInfo` 里的**下标**，宿主拿它回自己的对象，不深拷贝。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiStreamChoice {
    pub quality: Option<i32>,
    pub video: Option<u32>,
    pub audio: Option<u32>,
    pub progressive: Option<u32>,
}

/// 清晰度菜单里的一项（Kotlin `QualityOption`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiQualityOption {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
}

/// 选流：按编码偏好 / 默认清晰度 / 解码能力从这份 `PlayInfo` 里挑一路。
///
/// `info_json` 是 `PlayInfo` 的 JSON（**只读**：选流只要视频轨的编码 / 码率 / 尺寸 / 帧率、
/// 音频轨的 codecs / 码率、音画一体那几条、清晰度列表——过一份 JSON 比过几十个 record 便宜）。
/// 口径见 `feedhub_core::player::select`。
#[uniffi::export]
pub fn player_select(
    info_json: String,
    preference: FfiCodecPreference,
    default_quality: FfiDefaultQuality,
    quality: Option<i32>,
    auto: Vec<FfiPlayerCodec>,
    capability: Option<FfiDecodeCapability>,
) -> Result<Option<FfiStreamChoice>, FfiPlayerError> {
    let info = parse_play_info(&info_json)?;
    let auto: Vec<VideoCodec> = auto.iter().copied().map(codec_to_core).collect();
    let auto = if auto.is_empty() {
        core_select::DESKTOP_AUTO.to_vec()
    } else {
        auto
    };
    let cap = capability.as_ref().map(|c| c.to_core());
    Ok(core_select::select(
        &info,
        preference.to_core(),
        default_quality.to_core(),
        quality,
        &auto,
        cap.as_ref(),
    )
    .map(|s| FfiStreamChoice {
        quality: s.quality,
        video: s.video,
        audio: s.audio,
        progressive: s.progressive,
    }))
}

/// 自动的编码顺序（Kotlin `StreamSelection.autoOrder`）。
#[uniffi::export]
pub fn player_auto_order(
    capability: FfiDecodeCapability,
    pixels_per_second: Option<u64>,
) -> Vec<FfiPlayerCodec> {
    core_select::auto_order(&capability.to_core(), pixels_per_second)
        .into_iter()
        .map(codec_from_core)
        .collect()
}

/// 编码优先级（`StreamSelection.codecOrder`）。
#[uniffi::export]
pub fn player_codec_order(
    preference: FfiCodecPreference,
    auto: Vec<FfiPlayerCodec>,
) -> Vec<FfiPlayerCodec> {
    let auto: Vec<VideoCodec> = auto.iter().copied().map(codec_to_core).collect();
    core_select::codec_order(preference.to_core(), &auto)
        .into_iter()
        .map(codec_from_core)
        .collect()
}

/// 默认清晰度想要哪一档（`StreamSelection.targetQuality`）。
#[uniffi::export]
pub fn player_target_quality(default_quality: FfiDefaultQuality, available: Vec<i32>) -> Option<i32> {
    core_select::target_quality(default_quality.to_core(), &available)
}

/// 在 `available` 里找最接近 `target` 的一档（`StreamSelection.pickQuality`）。
#[uniffi::export]
pub fn player_pick_quality(available: Vec<i32>, target: Option<i32>) -> Option<i32> {
    core_select::pick_quality(&available, target)
}

/// 挑一条音频轨，返回下标（`StreamSelection.pickAudio`）。
#[uniffi::export]
pub fn player_pick_audio(audio: Vec<FfiAudioForPick>) -> Option<u32> {
    let list: Vec<feedhub_core::play::AudioStream> = audio
        .into_iter()
        .map(|a| feedhub_core::play::AudioStream {
            id: a.id,
            codecs: a.codecs,
            url: String::new(),
            backup_urls: Vec::new(),
            bandwidth: a.bandwidth,
            segment_base: None,
        })
        .collect();
    core_select::pick_audio(&list)
}

/// 挑音频只要这两项（`codecs` 判 AAC、`bandwidth` 比码率）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiAudioForPick {
    pub id: i32,
    pub codecs: Option<String>,
    pub bandwidth: Option<i64>,
}

/// 清晰的菜单项（`StreamSelection.qualityOptions`）。
#[uniffi::export]
pub fn player_quality_options(
    info_json: String,
) -> Result<Vec<FfiQualityOption>, FfiPlayerError> {
    let info = parse_play_info(&info_json)?;
    Ok(core_select::quality_options(&info)
        .into_iter()
        .map(|o| FfiQualityOption {
            id: o.id,
            label: o.label,
            enabled: o.enabled,
        })
        .collect())
}

/// 界面标签（编码 / 硬解 / 那个小 chip）。
#[uniffi::export]
pub fn player_codec_label(codec: Option<FfiPlayerCodec>) -> String {
    core_select::codec_label(codec.map(codec_to_core))
}

/// `硬解` / `软解` / 空。
#[uniffi::export]
pub fn player_decode_label(hardware: Option<bool>) -> String {
    core_select::decode_label(hardware)
}

/// `AVC · 硬解` 这种。
#[uniffi::export]
pub fn player_chip_label(codec: Option<FfiPlayerCodec>, hardware: Option<bool>) -> String {
    core_select::chip_label(codec.map(codec_to_core), hardware)
}

/// 倍速的档位（`PlaybackSpeed.STEPS`）。
#[uniffi::export]
pub fn player_speed_steps() -> Vec<f32> {
    core_select::PlaybackSpeed::STEPS.to_vec()
}

/// `1.5x` 这种。
#[uniffi::export]
pub fn player_speed_label(speed: f32) -> String {
    core_select::PlaybackSpeed::label(speed)
}

/// 往快 / 慢走一档。
#[uniffi::export]
pub fn player_speed_step(current: f32, delta: i32) -> f32 {
    core_select::PlaybackSpeed::step(current, delta)
}

/// 默认清晰度被封顶之后是哪一档（`limitQuality`）。
#[uniffi::export]
pub fn player_limit_quality(
    setting: FfiDefaultQuality,
    limit: Option<FfiDefaultQuality>,
) -> FfiDefaultQuality {
    FfiDefaultQuality::from_core(core_select::limit_quality(
        setting.to_core(),
        limit.map(|l| l.to_core()),
    ))
}

/// 帧率文本 → 每秒帧数（`frameRateOf`）。
#[uniffi::export]
pub fn player_frame_rate(text: Option<String>) -> Option<f64> {
    core_ledger::frame_rate_of(text.as_deref())
}

/// 一路视频轨的负载（`pixelRateOf`）。
#[uniffi::export]
pub fn player_pixel_rate(
    width: Option<i32>,
    height: Option<i32>,
    frame_rate: Option<String>,
) -> Option<u64> {
    core_ledger::pixel_rate_of(width, height, frame_rate.as_deref())
}

/// 像素率上限 → `1080P60` 这种标签（`pixelRateLabel`）。
#[uniffi::export]
pub fn player_pixel_rate_label(rate: u64) -> String {
    core_ledger::pixel_rate_label(rate)
}

// ===========================================================================
// 解码能力账本
// ===========================================================================

/// 落盘要做的动作（Kotlin `store.put(KEY, …)` / `store.put(KEY, null)`）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPersist {
    /// 删掉这个键。
    Delete,
    /// 写这份内容。
    Write { text: String },
}

impl FfiPersist {
    fn from_core(p: core_ledger::Persist) -> Self {
        match p {
            core_ledger::Persist::Delete => Self::Delete,
            core_ledger::Persist::Write(text) => Self::Write { text },
        }
    }
}

/// 账本里一行（界面 / 检查工具用）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiLedgerEntry {
    pub codec: FfiPlayerCodec,
    pub cap: u64,
    /// 最后一次实测的时刻（epoch 毫秒）。
    pub at: i64,
}

/// **解码能力账本**（Kotlin `object DecodeLedger` 的替身）。
///
/// 一个进程一个实例（宿主在 `AppGraph` 里建一个、`GstPlayerEngine` 和设置页共用）。所有方法都是
/// `&self` + 内部 `Mutex`：播放引擎在播放线程上调、设置页在界面线程上调。
#[derive(uniffi::Object)]
pub struct FfiDecodeLedger {
    /// `Arc` 而不是裸 `Mutex`：**桌面播放引擎要拿同一份账本**记账
    /// ，所以得能共享。
    inner: Arc<Mutex<core_ledger::DecodeLedger>>,
}

#[uniffi::export]
impl FfiDecodeLedger {
    /// 新建一个空账本（还没接落盘）。
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Mutex::new(core_ledger::DecodeLedger::new())),
        })
    }

    /// 接上落盘：`stored` 是从 `KeyValueStore` 读出来的原文（没有就 `None`），`fingerprint` 是当前
    /// 环境指纹（拿不到就 `None` → 不读也不写旧记录）。
    pub fn attach(&self, stored: Option<String>, fingerprint: Option<String>) {
        self.lock().attach(stored.as_deref(), fingerprint.as_deref());
    }

    /// 把接进来的存档立刻解析掉（宿主想"读完马上清理过期记录 / 指纹对不上的记录"时用）。
    pub fn load_now(&self, now_ms: i64) {
        self.lock().load_now(now_ms);
    }

    /// 记一次实测（不稳就把上限压到负载的三分之一；稳的什么都不做）。
    pub fn record(
        &self,
        codec: Option<FfiPlayerCodec>,
        pixels_per_second: i64,
        smooth: bool,
        now_ms: i64,
    ) {
        // 负载 ≤ 0（拿不到尺寸 / 算错了）**不记**——`u64` 的话 `-1` 会变成一个大数，把上限写成"无穷大"
        self.lock().record(
            codec.map(codec_to_core),
            pixels_per_second.max(0) as u64,
            smooth,
            now_ms,
        );
    }

    /// 这种编码现在记下的上限（`None` = 不限制）。
    pub fn rate(&self, codec: FfiPlayerCodec, now_ms: i64) -> Option<u64> {
        self.lock().rate(codec_to_core(codec), now_ms)
    }

    /// 现在有效的全部上限（按 AVC / HEVC / AV1 的顺序）。
    pub fn snapshot(&self, now_ms: i64) -> Vec<FfiPixelRateEntry> {
        self.lock()
            .snapshot(now_ms)
            .into_iter()
            .map(|(c, v)| FfiPixelRateEntry {
                codec: codec_from_core(c),
                pixels_per_second: v,
            })
            .collect()
    }

    /// 现在有效的全部上限 + 调用方给的 `hardware`，包成一份 [`FfiDecodeCapability`]。
    pub fn capability(
        &self,
        hardware: Vec<FfiPlayerCodec>,
        now_ms: i64,
    ) -> FfiDecodeCapability {
        let hardware: Vec<VideoCodec> = hardware.iter().copied().map(codec_to_core).collect();
        let cap = self.lock().capability(&hardware, now_ms);
        FfiDecodeCapability::from_core(&cap)
    }

    /// 有没有测出过结果。
    pub fn is_empty(&self, now_ms: i64) -> bool {
        self.lock().is_empty(now_ms)
    }

    /// 界面那一行（`AV1 ≤ 1080P60`）。**日期串由宿主格式化**：传当前时刻和
    /// 每一行的实测时刻，宿主按自己的时区 / 地区拼字符串再交回来（`day_name(at_ms)` 由宿主实现，
    /// 这里只给结构化的行）。
    pub fn summary(&self, now_ms: i64) -> Vec<FfiLedgerSummaryRow> {
        let snapshot = self.lock().snapshot(now_ms);
        let mut out = Vec::with_capacity(snapshot.len());
        for (codec, cap) in snapshot {
            let at = self.lock().measured_at(codec).filter(|at| now_ms - *at <= core_ledger::TTL_MS);
            out.push(FfiLedgerSummaryRow {
                codec: codec_from_core(codec),
                cap,
                label: core_ledger::pixel_rate_label(cap),
                measured_at_ms: at,
            });
        }
        out
    }

    /// 「清除，重新测」：内存和落盘一起清掉。
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// 测试 / 检查工具：只清内存，不动落盘。
    pub fn reset(&self) {
        self.lock().reset();
    }

    /// 检查工具 / 截图：灌一份只在内存里的假记录（不落盘）。
    pub fn seed_for_demo(&self, codec: FfiPlayerCodec, cap: u64, at: i64) {
        self.lock()
            .seed_for_demo(codec_to_core(codec), cap, at);
    }

    /// 取走待落盘的动作（宿主每次调用完问一遍，有就照做）。
    pub fn take_persist(&self) -> Option<FfiPersist> {
        self.lock().take_persist().map(FfiPersist::from_core)
    }

    /// 落盘内容（不看"有没有待写"）：测试用。
    pub fn encoded(&self) -> String {
        self.lock().encode_now()
    }
}

impl FfiDecodeLedger {
    fn lock(&self) -> MutexGuard<'_, core_ledger::DecodeLedger> {
        // 宿主里可能有人 panic 在锁里（不该有），但别让整个 App 跟着挂
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 账本界面那一行的一格（日期串由宿主拼）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiLedgerSummaryRow {
    pub codec: FfiPlayerCodec,
    pub cap: u64,
    /// `1080P60` 这种档位标签。
    pub label: String,
    /// 最后一次实测的时刻（已经按 TTL 过滤过）；`None` = 没有时刻可显示。
    pub measured_at_ms: Option<i64>,
}

// ===========================================================================
// 弹幕：过滤 / 时间线
// ===========================================================================

/// 弹幕过滤设置（Kotlin `DanmakuFilter`）。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiDanmakuFilter {
    pub allow_scroll: bool,
    pub allow_top: bool,
    pub allow_bottom: bool,
    pub allow_colorful: bool,
    pub weight_level: i32,
    pub rules: Vec<String>,
    pub static_to_scroll: bool,
    pub merge: bool,
    pub merge_window_ms: i64,
    pub merge_threshold: i32,
}

impl Default for FfiDanmakuFilter {
    fn default() -> Self {
        let f = core_danmaku::Filter::default();
        Self {
            allow_scroll: f.allow_scroll,
            allow_top: f.allow_top,
            allow_bottom: f.allow_bottom,
            allow_colorful: f.allow_colorful,
            weight_level: f.weight_level,
            rules: f.rules,
            static_to_scroll: f.static_to_scroll,
            merge: f.merge,
            merge_window_ms: f.merge_window_ms,
            merge_threshold: f.merge_threshold,
        }
    }
}

impl FfiDanmakuFilter {
    pub(crate) fn to_core(&self) -> core_danmaku::Filter {
        core_danmaku::Filter {
            allow_scroll: self.allow_scroll,
            allow_top: self.allow_top,
            allow_bottom: self.allow_bottom,
            allow_colorful: self.allow_colorful,
            weight_level: self.weight_level,
            rules: self.rules.clone(),
            static_to_scroll: self.static_to_scroll,
            merge: self.merge,
            merge_window_ms: self.merge_window_ms,
            merge_threshold: self.merge_threshold,
        }
    }
}

pub(crate) fn danmaku_from_ffi(d: &FfiDanmaku) -> CoreDanmaku {
    CoreDanmaku {
        id: d.id.clone(),
        time_ms: d.time_ms,
        mode: mode_to_core(d.mode),
        text: d.text.clone(),
        color: d.color,
        font_size: d.font_size,
        weight: d.weight,
    }
}

fn mode_to_core(mode: crate::play::FfiDanmakuMode) -> DanmakuMode {
    match mode {
        crate::play::FfiDanmakuMode::Scroll => DanmakuMode::Scroll,
        crate::play::FfiDanmakuMode::Top => DanmakuMode::Top,
        crate::play::FfiDanmakuMode::Bottom => DanmakuMode::Bottom,
        crate::play::FfiDanmakuMode::Other => DanmakuMode::Other,
    }
}

fn mode_from_core(mode: DanmakuMode) -> crate::play::FfiDanmakuMode {
    to_ffi_danmaku_mode(mode)
}

/// 一段弹幕：按设置过滤、合并、按时间排好。**一段一次**。
#[uniffi::export]
pub fn danmaku_filter_segment(
    list: Vec<FfiDanmaku>,
    filter: FfiDanmakuFilter,
) -> Vec<FfiDanmaku> {
    let core_list: Vec<CoreDanmaku> = list.iter().map(danmaku_from_ffi).collect();
    let rules = core_danmaku::BlockRules::compile(&filter.rules);
    let out = filter.to_core().process(&core_list, &rules);
    to_ffi_danmaku(&out)
}

/// 一条弹幕会不会被这批屏蔽词挡住（`BlockRules.blocks`）。
///
/// 宿主那边每条弹幕都要问一次（`DanmakuScheduler` 排布前、`BlockRules.blocks`），所以单独一个口子：
/// 只搬"文字 + 规则"，不搬整条弹幕。
#[uniffi::export]
pub fn danmaku_blocked(text: String, rules: Vec<String>) -> bool {
    core_danmaku::BlockRules::compile(&rules).blocks(&text)
}

/// 一条屏蔽词算哪种（`Keyword` / `Regex` / `Invalid`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRuleKind {
    Keyword,
    Regex,
    Invalid,
}

/// 判一条屏蔽词的类型（设置面板的提示用）——**会真的编译一次正则**，但只是用户输入时调一次。
#[uniffi::export]
pub fn danmaku_rule_kind(rule: String) -> FfiRuleKind {
    match core_danmaku::BlockRules::kind_of(&rule) {
        core_danmaku::RuleKind::Keyword => FfiRuleKind::Keyword,
        core_danmaku::RuleKind::Regex => FfiRuleKind::Regex,
        core_danmaku::RuleKind::Invalid => FfiRuleKind::Invalid,
    }
}

/// 合并重复弹幕（单独一个口子：已经过滤好的列表想再合并时用）。
#[uniffi::export]
pub fn danmaku_merge(
    list: Vec<FfiDanmaku>,
    window_ms: i64,
    threshold: i32,
) -> Vec<FfiDanmaku> {
    let core_list: Vec<CoreDanmaku> = list.iter().map(danmaku_from_ffi).collect();
    let out = core_danmaku::merge_duplicates(&core_list, window_ms, threshold);
    to_ffi_danmaku(&out)
}

/// 一段处理好的弹幕（`set_filter` 重新处理之后交回宿主）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiDanmakuSegment {
    pub segment: i32,
    pub items: Vec<FfiDanmaku>,
}

/// 时间线的统计（检查工具 / 测试用）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiTimelineStats {
    pub loaded: u32,
    pub version: u32,
}

/// **弹幕时间线**（Kotlin `DanmakuTimeline` 的替身）：按段存已取回的弹幕、过滤好了之后再取。
#[derive(uniffi::Object)]
pub struct FfiDanmakuTimeline {
    inner: Mutex<core_danmaku::Timeline>,
    segment_ms: i64,
}

#[uniffi::export]
impl FfiDanmakuTimeline {
    /// `segment_ms`：一段多长（毫秒，B站 是 6 分钟）。
    #[uniffi::constructor]
    pub fn new(segment_ms: i64, filter: FfiDanmakuFilter) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(core_danmaku::Timeline::new(segment_ms, filter.to_core())),
            segment_ms,
        })
    }

    /// 放进一段：返回**处理好的**列表（宿主拿它当文字表；排布表里的 id 指的就是这个列表的下标）。
    pub fn put(&self, segment: i32, list: Vec<FfiDanmaku>) -> Vec<FfiDanmaku> {
        let core_list: Vec<CoreDanmaku> = list.iter().map(danmaku_from_ffi).collect();
        let out = self.lock().put(segment, core_list);
        to_ffi_danmaku(&out)
    }

    /// 塞进这一段每条的文字宽度（像素，**一段一次**，顺序和 [`put`](Self::put) 交回的列表一致）。
    pub fn set_widths(&self, segment: i32, widths: Vec<f32>) {
        self.lock().set_widths(segment, widths);
    }

    /// 换过滤规则；返回 `None` = 规则没变（什么都没做）。否则返回**重新处理过的每一段**
    /// `(段号, 列表)`——宿主拿它重建文字表、重新量宽度、重新排。
    pub fn set_filter(&self, filter: FfiDanmakuFilter) -> Option<Vec<FfiDanmakuSegment>> {
        self.lock()
            .set_filter_returning(filter.to_core())
            .map(|segments| {
                segments
                    .into_iter()
                    .map(|(segment, list)| FfiDanmakuSegment {
                        segment,
                        items: to_ffi_danmaku(&list),
                    })
                    .collect()
            })
    }

    /// 这一段取回来了没有。
    pub fn has(&self, segment: i32) -> bool {
        self.lock().has(segment)
    }

    /// 时间 `ms` 落在第几段。
    pub fn segment_of(&self, ms: i64) -> i32 {
        self.lock().segment_of(ms)
    }

    /// 一共几条（处理好的）。
    pub fn loaded_count(&self) -> u32 {
        self.lock().loaded_count() as u32
    }

    /// 过滤规则变过几次。
    pub fn version(&self) -> u32 {
        self.lock().version()
    }

    /// 段长（毫秒）。
    pub fn segment_ms(&self) -> i64 {
        self.segment_ms
    }

    /// (`from_exclusive`, `to_inclusive`] 里的弹幕**下标**（宿主回自己的列表取，不过字符串）。
    pub fn range_ids(&self, from_exclusive: i64, to_inclusive: i64) -> Vec<u64> {
        self.lock()
            .range(from_exclusive, to_inclusive)
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    /// 出现时间严格大于 `after_ms` 的最近一条弹幕的时刻（排布器定"下一次什么时候来问"用）。
    pub fn next_time_after(&self, after_ms: i64) -> Option<i64> {
        self.lock().next_time_after(after_ms)
    }
}

impl FfiDanmakuTimeline {
    fn lock(&self) -> MutexGuard<'_, core_danmaku::Timeline> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

// ===========================================================================
// 弹幕：装载策略
// ===========================================================================

/// **弹幕装载策略**（Kotlin `DanmakuLoader` 的决策部分）：只回答"现在该取哪几段"，
/// 宿主拿着去取、取完调 [`complete`](Self::complete)。
#[derive(uniffi::Object)]
pub struct FfiDanmakuLoader {
    inner: Mutex<core_danmaku::Loader>,
}

#[uniffi::export]
impl FfiDanmakuLoader {
    #[uniffi::constructor]
    pub fn new(segment_ms: i64, duration_ms: Option<i64>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(core_danmaku::Loader::new(segment_ms, duration_ms)),
        })
    }

    /// 这一刻该去取哪几段（已有的 / 在飞的 / 试够次数的 / 还没到重试时间的都不在里面）。
    pub fn ensure(&self, position_ms: i64, loaded: Vec<i32>, now_ms: i64) -> Vec<i32> {
        let is_loaded = move |seg: i32| loaded.contains(&seg);
        self.lock().ensure(position_ms, &is_loaded, now_ms)
    }

    /// 这一段取完了（成功 / 失败都算）。
    pub fn complete(&self, segment: i32) {
        self.lock().complete(segment);
    }

    /// 这一段试了几次。
    pub fn tries(&self, segment: i32) -> u32 {
        self.lock().tries(segment)
    }

    /// 现在有几段在飞。
    pub fn in_flight(&self) -> u32 {
        self.lock().in_flight() as u32
    }
}

impl FfiDanmakuLoader {
    fn lock(&self) -> MutexGuard<'_, core_danmaku::Loader> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

// ===========================================================================
// 弹幕：设置模型与排布
// ===========================================================================

/// 弹幕设置（Kotlin `DanmakuSettings`）。字段名和默认值一对一。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiDanmakuSettings {
    pub opacity: f32,
    pub font_scale: f32,
    pub font_weight: i32,
    pub stroke_width: f32,
    pub area: f32,
    pub line_height: f32,
    pub massive: bool,
    pub scroll_seconds: f32,
    pub fixed_velocity: bool,
    pub fixed_seconds: f32,
    pub static_to_scroll: bool,
    pub allow_scroll: bool,
    pub allow_top: bool,
    pub allow_bottom: bool,
    pub allow_colorful: bool,
    pub weight_level: i32,
    pub merge_duplicates: bool,
    pub merge_window_ms: i32,
    pub merge_threshold: i32,
    pub block_rules: Vec<String>,
}

impl Default for FfiDanmakuSettings {
    fn default() -> Self {
        to_ffi_settings(&core_danmaku::Settings::default())
    }
}

impl FfiDanmakuSettings {
    pub(crate) fn to_core(&self) -> core_danmaku::Settings {
        core_danmaku::Settings {
            opacity: self.opacity,
            font_scale: self.font_scale,
            font_weight: self.font_weight,
            stroke_width: self.stroke_width,
            area: self.area,
            line_height: self.line_height,
            massive: self.massive,
            scroll_seconds: self.scroll_seconds,
            fixed_velocity: self.fixed_velocity,
            fixed_seconds: self.fixed_seconds,
            static_to_scroll: self.static_to_scroll,
            allow_scroll: self.allow_scroll,
            allow_top: self.allow_top,
            allow_bottom: self.allow_bottom,
            allow_colorful: self.allow_colorful,
            weight_level: self.weight_level,
            merge_duplicates: self.merge_duplicates,
            merge_window_ms: self.merge_window_ms,
            merge_threshold: self.merge_threshold,
            block_rules: self.block_rules.clone(),
        }
    }

    fn from_core(s: &core_danmaku::Settings) -> Self {
        to_ffi_settings(s)
    }
}

fn to_ffi_settings(s: &core_danmaku::Settings) -> FfiDanmakuSettings {
    FfiDanmakuSettings {
        opacity: s.opacity,
        font_scale: s.font_scale,
        font_weight: s.font_weight,
        stroke_width: s.stroke_width,
        area: s.area,
        line_height: s.line_height,
        massive: s.massive,
        scroll_seconds: s.scroll_seconds,
        fixed_velocity: s.fixed_velocity,
        fixed_seconds: s.fixed_seconds,
        static_to_scroll: s.static_to_scroll,
        allow_scroll: s.allow_scroll,
        allow_top: s.allow_top,
        allow_bottom: s.allow_bottom,
        allow_colorful: s.allow_colorful,
        weight_level: s.weight_level,
        merge_duplicates: s.merge_duplicates,
        merge_window_ms: s.merge_window_ms,
        merge_threshold: s.merge_threshold,
        block_rules: s.block_rules.clone(),
    }
}

/// 排布要用的那几项，已经换算成像素（Kotlin `DanmakuLayout`）。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiDanmakuLayout {
    pub font_px: f32,
    pub font_weight: i32,
    pub lane_height: f32,
    pub area: f32,
    pub scroll_ms: i64,
    pub fixed_ms: i64,
    pub reserve_top: bool,
    pub reserve_bottom: bool,
}

impl FfiDanmakuLayout {
    pub(crate) fn to_core(&self) -> core_danmaku::Layout {
        core_danmaku::Layout {
            font_px: self.font_px,
            font_weight: self.font_weight,
            lane_height: self.lane_height,
            area: self.area,
            scroll_ms: self.scroll_ms,
            fixed_ms: self.fixed_ms,
            reserve_top: self.reserve_top,
            reserve_bottom: self.reserve_bottom,
        }
    }
}

/// 设置换算：在 `view_width × view_height` 的画面上排布要用的那几项（`DanmakuSettings.layout`）。
#[uniffi::export]
pub fn danmaku_layout(
    settings: FfiDanmakuSettings,
    view_width: f32,
    view_height: f32,
    density: f32,
) -> FfiDanmakuLayout {
    let l = settings.to_core().layout(view_width, view_height, density);
    FfiDanmakuLayout {
        font_px: l.font_px,
        font_weight: l.font_weight,
        lane_height: l.lane_height,
        area: l.area,
        scroll_ms: l.scroll_ms,
        fixed_ms: l.fixed_ms,
        reserve_top: l.reserve_top,
        reserve_bottom: l.reserve_bottom,
    }
}

/// 过滤用的那几项（`DanmakuSettings.filter`）。
#[uniffi::export]
pub fn danmaku_settings_filter(settings: FfiDanmakuSettings) -> FfiDanmakuFilter {
    let f = settings.to_core().filter();
    FfiDanmakuFilter {
        allow_scroll: f.allow_scroll,
        allow_top: f.allow_top,
        allow_bottom: f.allow_bottom,
        allow_colorful: f.allow_colorful,
        weight_level: f.weight_level,
        rules: f.rules,
        static_to_scroll: f.static_to_scroll,
        merge: f.merge,
        merge_window_ms: f.merge_window_ms,
        merge_threshold: f.merge_threshold,
    }
}

/// 把超出范围的值收回范围、屏蔽词去空白去重去过长（`DanmakuSettings.normalized`）。
#[uniffi::export]
pub fn danmaku_settings_normalized(settings: FfiDanmakuSettings) -> FfiDanmakuSettings {
    FfiDanmakuSettings::from_core(&settings.to_core().normalized())
}

/// 恢复默认（屏蔽词留着）。
#[uniffi::export]
pub fn danmaku_settings_reset(settings: FfiDanmakuSettings) -> FfiDanmakuSettings {
    FfiDanmakuSettings::from_core(&settings.to_core().reset())
}

/// 旧存档的"弹幕密度"（0~100）→ 新设置（`fromLegacyDensity`）。
#[uniffi::export]
pub fn danmaku_settings_from_legacy_density(density: i32) -> FfiDanmakuSettings {
    FfiDanmakuSettings::from_core(&core_danmaku::Settings::from_legacy_density(density))
}

/// 离 `value` 最近的一档显示区域（`nearestArea`）。
#[uniffi::export]
pub fn danmaku_nearest_area(value: f32) -> f32 {
    core_danmaku::nearest_area(value)
}

/// 显示区域的八档（`AREA_OPTIONS`）。
#[uniffi::export]
pub fn danmaku_area_options() -> Vec<f32> {
    core_danmaku::AREA_OPTIONS.to_vec()
}

/// 屏蔽词上限（`MAX_RULES`）。
#[uniffi::export]
pub fn danmaku_max_rules() -> u32 {
    core_danmaku::MAX_RULES as u32
}

/// 一条屏蔽词最长多少字（`MAX_RULE_LENGTH`）。
#[uniffi::export]
pub fn danmaku_max_rule_length() -> u32 {
    core_danmaku::MAX_RULE_LENGTH as u32
}

/// 海量模式下的行高（`massiveLineHeight`）。
#[uniffi::export]
pub fn danmaku_massive_line_height(line_height: f32) -> f32 {
    core_danmaku::massive_line_height(line_height)
}

/// 倍速下滚动弹幕走完全程的时间（`DanmakuDefaults.scrollMs`）。
#[uniffi::export]
pub fn danmaku_scroll_ms(speed: f32, base_ms: i64) -> i64 {
    core_danmaku::scroll_ms(speed, base_ms)
}

/// 固定像素速度的宽度系数（`fixedVelocityFactor`）。
#[uniffi::export]
pub fn danmaku_fixed_velocity_factor(width_dp: f32) -> f32 {
    core_danmaku::fixed_velocity_factor(width_dp)
}

/// 排布表里的一条（Kotlin 画每一帧要的就是它 + 宽度）。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiPlannedDanmaku {
    /// 弹幕 id（`danmaku_id(segment, index)`），宿主拿它回自己的文字 / 颜色表。
    pub id: u64,
    pub mode: crate::play::FfiDanmakuMode,
    pub lane: i32,
    pub start_ms: i64,
    pub duration_ms: i64,
    pub width: f32,
    pub view_width: f32,
}

/// 一次排布过 FFI 的整批：屏上的那些 + 这次用的轨道参数 + 下一次该来问的时刻。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiDanmakuPlan {
    pub items: Vec<FfiPlannedDanmaku>,
    pub view_width: f32,
    pub view_height: f32,
    pub lane_height: f32,
    pub usable_lanes: i32,
    pub scroll_lanes: i32,
    pub top_fixed_lanes: i32,
    pub bottom_fixed_lanes: i32,
    /// 下一次该来问的时刻（`None` = 后面什么都没有了）。
    pub next_ms: Option<i64>,
}

fn plan_to_ffi(plan: &core_danmaku::Plan) -> FfiDanmakuPlan {
    FfiDanmakuPlan {
        items: plan
            .items
            .iter()
            .map(|p| FfiPlannedDanmaku {
                id: p.id,
                mode: mode_from_core(p.mode),
                lane: p.lane,
                start_ms: p.start_ms,
                duration_ms: p.duration_ms,
                width: p.width,
                view_width: p.view_width,
            })
            .collect(),
        view_width: plan.view_width,
        view_height: plan.view_height,
        lane_height: plan.lane_height,
        usable_lanes: plan.usable_lanes,
        scroll_lanes: plan.scroll_lanes,
        top_fixed_lanes: plan.top_fixed_lanes,
        bottom_fixed_lanes: plan.bottom_fixed_lanes,
        next_ms: plan.next_ms,
    }
}

/// **弹幕排布器**（Kotlin `DanmakuScheduler` 的替身）。
///
/// 只在"有新的弹幕要出现 / 跳转 / 改设置 / 换倍速"时调 [`advance`](Self::advance) 或
/// [`reset`](Self::reset)，**不是每帧**；每帧的 x / y 由宿主拿 [`FfiDanmakuPlan`] 里的
/// `start_ms` / `duration_ms` / `lane` 自己算（纯算术）。
#[derive(uniffi::Object)]
pub struct FfiDanmakuScheduler {
    inner: Mutex<core_danmaku::Scheduler>,
}

#[uniffi::export]
impl FfiDanmakuScheduler {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(core_danmaku::Scheduler::new()),
        })
    }

    /// 视图尺寸 / 排布变了：重算轨道数。返回 `true` = 真的变了（宿主据此重新 [`reset`](Self::reset)）。
    pub fn configure(
        &self,
        view_width: f32,
        view_height: f32,
        layout: FfiDanmakuLayout,
    ) -> bool {
        self.lock().configure(view_width, view_height, layout.to_core())
    }

    /// 倍速（宿主每帧从 `PlayerState.speed` 推下来）：只影响**新排**的滚动弹幕。
    pub fn set_speed(&self, speed: f32) {
        self.lock().playback_speed = speed;
    }

    /// 从 `position_ms` 起重新对齐，返回此刻的排布。
    pub fn reset(&self, position_ms: i64, timeline: &FfiDanmakuTimeline) -> FfiDanmakuPlan {
        let plan = {
            let mut inner = self.lock();
            let tl = timeline.lock();
            inner.reset(position_ms, &tl);
            inner.plan(position_ms, &tl)
        };
        plan_to_ffi(&plan)
    }

    /// 推进到 `now_ms`（把到点的弹幕排上），返回此刻的排布 + 下一次该来问的时刻。
    pub fn advance(&self, now_ms: i64, timeline: &FfiDanmakuTimeline) -> FfiDanmakuPlan {
        let plan = {
            let mut inner = self.lock();
            let tl = timeline.lock();
            inner.advance(now_ms, &tl);
            inner.plan(now_ms, &tl)
        };
        plan_to_ffi(&plan)
    }

    /// 只问"下一次该什么时候来"（不推进）。
    pub fn next_ms(&self, now_ms: i64, timeline: &FfiDanmakuTimeline) -> Option<i64> {
        let inner = self.lock();
        let tl = timeline.lock();
        inner.next_cue(now_ms, &tl).next_ms
    }

    /// 屏上现在有几条。
    pub fn active_count(&self) -> u32 {
        self.lock().active_count() as u32
    }
}

impl FfiDanmakuScheduler {
    fn lock(&self) -> MutexGuard<'_, core_danmaku::Scheduler> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 弹幕上边缘的 y（宿主每帧算；**纯函数**，一次一批）。
#[uniffi::export]
pub fn danmaku_plan_y(plan: FfiDanmakuPlan, item_index: u32) -> Option<f32> {
    let item = plan.items.get(item_index as usize)?;
    let lane = item.lane;
    let mode = item.mode;
    Some(match mode {
        crate::play::FfiDanmakuMode::Bottom => {
            (plan.usable_lanes - lane - 1) as f32 * plan.lane_height
        }
        crate::play::FfiDanmakuMode::Scroll => (plan.top_fixed_lanes + lane) as f32 * plan.lane_height,
        _ => lane as f32 * plan.lane_height,
    })
}

/// 滚动弹幕此刻左边缘的 x（顶部 / 底部居中）——**纯算术**，宿主每帧也可以自己算。
#[uniffi::export]
pub fn planned_x(item: FfiPlannedDanmaku, now_ms: i64, view_width: f32) -> f32 {
    match item.mode {
        crate::play::FfiDanmakuMode::Scroll => {
            view_width
                - (view_width + item.width) * (now_ms - item.start_ms) as f32
                    / item.duration_ms as f32
        }
        _ => (view_width - item.width) / 2.0,
    }
}

// ===========================================================================
// 清单生成与改写
// ===========================================================================

/// 一条 DASH 轨的描述（Kotlin `DashTrack`）。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiDashTrack {
    pub initialization: String,
    pub index_range: String,
    pub codecs: Option<String>,
    pub bandwidth: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub frame_rate: Option<String>,
}

impl FfiDashTrack {
    fn to_core(&self) -> core_manifest::DashTrack {
        core_manifest::DashTrack {
            initialization: self.initialization.clone(),
            index_range: self.index_range.clone(),
            codecs: self.codecs.clone(),
            bandwidth: self.bandwidth,
            width: self.width,
            height: self.height,
            frame_rate: self.frame_rate.clone(),
        }
    }
}

/// 拼一份只含 `SegmentBase` 的静态 MPD（`DashManifest.build`）。
#[uniffi::export]
pub fn player_dash_manifest(
    video_url: String,
    audio_url: Option<String>,
    video: FfiDashTrack,
    audio: Option<FfiDashTrack>,
    duration_ms: Option<i64>,
) -> String {
    core_manifest::dash_manifest(
        &video_url,
        audio_url.as_deref(),
        &video.to_core(),
        audio.as_ref().map(|a| a.to_core()).as_ref(),
        duration_ms,
    )
}

/// 中转的登记项（[`FfiRelay`] 里的状态；宿主一般不直接读，检查工具要）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRelayEntry {
    Local { content_type: Option<String> },
    Remote { url: String, dash: bool },
    Resource { url: String },
    Template { remote: String, tokens: Vec<String> },
}

/// 宿主收到一个中转请求之后该干什么。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRelayRoute {
    /// 回 404。
    NotFound,
    /// 回 400。
    BadTemplate,
    /// 回这段正文。
    Body {
        body: String,
        content_type: Option<String>,
    },
    /// 去远端取这份清单再改写（`dash == true` 走 DASH 改写）。
    RemoteManifest { url: String, dash: bool },
    /// 把远端资源原样转发（带 `Range` / `If-Range`）。
    Upstream { url: String },
}

/// **中转地址映射**（Kotlin `VideoRelay` 的登记 / 路由那一半）。
///
/// `id` 由**宿主**生成（Kotlin 用 UUID）：URL 里的 id 要能对回远端地址，而"生成随机串"留在宿主，
/// 这样 Rust 这边"同一份清单改写出同样的结构"这句话才成立。
#[derive(uniffi::Object)]
pub struct FfiRelay {
    inner: Mutex<core_relay::Relay>,
}

#[uniffi::export]
impl FfiRelay {
    /// `base` 是播放器看到的基地址（`http://127.0.0.1:<port>`）。
    #[uniffi::constructor]
    pub fn new(base: String) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(core_relay::Relay::new(&base)),
        })
    }

    /// 登记一个远端资源，返回本机地址。
    pub fn resource(&self, remote_url: String, id: String) -> String {
        self.lock().register_resource(&remote_url, &id)
    }

    /// 登记一个远端清单 / 播放列表，返回本机地址。
    pub fn playlist(&self, remote_url: String, id: String) -> String {
        self.lock().register_playlist(&remote_url, &id)
    }

    /// 登记一个远端分片模板，返回带占位符参数名的本机模板地址。
    pub fn template(&self, remote_template: String, id: String) -> String {
        self.lock().register_template(&remote_template, &id)
    }

    /// 放一份我们自己生成、已经改写好的清单，返回本机地址。
    pub fn local(&self, body: String, content_type: Option<String>, id: String) -> String {
        self.lock()
            .register_local(&body, content_type.as_deref(), &id)
    }

    /// 播放器发来一个请求：`path` 是 `requestURI` 的原始路径，`raw_query` 是原始查询串。
    pub fn route(&self, path: String, raw_query: Option<String>) -> FfiRelayRoute {
        match self.lock().route(&path, raw_query.as_deref()) {
            core_relay::Route::NotFound => FfiRelayRoute::NotFound,
            core_relay::Route::BadTemplate => FfiRelayRoute::BadTemplate,
            core_relay::Route::Body { body, content_type } => {
                FfiRelayRoute::Body { body, content_type }
            }
            core_relay::Route::RemoteManifest { url, dash } => {
                FfiRelayRoute::RemoteManifest { url, dash }
            }
            core_relay::Route::Upstream { url } => FfiRelayRoute::Upstream { url },
        }
    }

    /// 登记了几条。
    pub fn len(&self) -> u32 {
        self.lock().len() as u32
    }

    /// 登记过的 id（**登记顺序**；检查工具 / 测试用）。
    pub fn registered_ids(&self) -> Vec<String> {
        self.lock().registered_ids()
    }

    /// 按 id 取条目（检查工具用）。
    pub fn entry(&self, id: String) -> Option<FfiRelayEntry> {
        self.lock().entry(&id).map(|e| match e {
            core_relay::Entry::Local { content_type, .. } => FfiRelayEntry::Local {
                content_type: content_type.clone(),
            },
            core_relay::Entry::Remote { url, dash } => FfiRelayEntry::Remote {
                url: url.clone(),
                dash: *dash,
            },
            core_relay::Entry::Resource { url } => FfiRelayEntry::Resource { url: url.clone() },
            core_relay::Entry::Template { remote, tokens } => FfiRelayEntry::Template {
                remote: remote.clone(),
                tokens: tokens.clone(),
            },
        })
    }
}

impl FfiRelay {
    fn lock(&self) -> MutexGuard<'_, core_relay::Relay> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// **改写一份 DASH 清单**：`relay` 里登记远端地址，返回改写后的正文。
#[uniffi::export]
pub fn player_rewrite_dash(
    mpd: String,
    base_url: Option<String>,
    relay: &FfiRelay,
) -> String {
    let mut guard = relay.lock();
    core_manifest::rewrite_dash(&mpd, base_url.as_deref(), &mut *guard)
}

/// `URI.resolve` 的口径（和清单改写用的是同一份实现）。
#[uniffi::export]
pub fn player_resolve_url(base: String, reference: String) -> String {
    core_manifest::resolve_url(&base, &reference)
}

/// **改写一份 HLS 播放列表**。
#[uniffi::export]
pub fn player_rewrite_hls(
    playlist_url: String,
    text: String,
    relay: &FfiRelay,
) -> String {
    let mut guard = relay.lock();
    core_manifest::rewrite_hls(&playlist_url, &text, &mut *guard)
}

/// `bytes=a-b` 解析成闭区间（`sliceFor`）。
#[uniffi::export]
pub fn player_slice_for(range: Option<String>, total: u32) -> Option<FfiByteSlice> {
    core_relay::slice_for(range.as_deref(), total as usize).map(|(from, to)| FfiByteSlice {
        from: from as u32,
        to: to as u32,
    })
}

/// 一段闭区间（`Range` 解析的结果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiByteSlice {
    pub from: u32,
    pub to: u32,
}

/// 远端地址的扩展名（`extOf`）。
#[uniffi::export]
pub fn player_ext_of(url: String) -> String {
    core_relay::ext_of(&url)
}

/// 主机是不是本机回环（中转出来的清单都不该被代理）。
#[uniffi::export]
pub fn player_is_loopback_url(url: String) -> bool {
    core_relay::is_loopback_url(&url)
}

/// 地址是不是 DASH 清单。
#[uniffi::export]
pub fn player_is_dash_url(url: String) -> bool {
    core_relay::is_dash_url(&url)
}

// ===========================================================================
// 播放状态机
// ===========================================================================

/// 播放器此刻的状态（宿主把 `engine.state` 同步进来）。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiPlaybackState {
    pub playing: bool,
    pub buffering: bool,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub ended: bool,
    pub speed: f32,
    pub muted: bool,
    pub volume: i32,
}

impl FfiPlaybackState {
    fn to_core(&self) -> core_session::PlaybackState {
        core_session::PlaybackState {
            playing: self.playing,
            buffering: self.buffering,
            position_ms: self.position_ms,
            duration_ms: self.duration_ms,
            ended: self.ended,
            speed: self.speed,
            muted: self.muted,
            volume: self.volume,
        }
    }
}

/// 宿主要做的一件事。
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiPlayEffect {
    Play,
    Pause,
    Replay,
    Seek { position_ms: i64 },
    SetRate { rate: f32 },
    SaveProgress {
        position_ms: i64,
        duration_ms: Option<i64>,
    },
    SavePart { part_id: Option<String> },
}

impl FfiPlayEffect {
    fn from_core(e: core_session::Effect) -> Self {
        match e {
            core_session::Effect::Play => Self::Play,
            core_session::Effect::Pause => Self::Pause,
            core_session::Effect::Replay => Self::Replay,
            core_session::Effect::Seek(ms) => Self::Seek { position_ms: ms },
            core_session::Effect::SetRate(rate) => Self::SetRate { rate },
            core_session::Effect::SaveProgress {
                position_ms,
                duration_ms,
            } => Self::SaveProgress {
                position_ms,
                duration_ms,
            },
            core_session::Effect::SavePart(part) => Self::SavePart { part_id: part },
        }
    }
}

/// 取流失败的三种。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPlayFailure {
    NotLoggedIn,
    RiskControl,
    Other { message: String },
}

/// 取流失败要显示的文案。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiFailureText {
    pub needs_login: bool,
    pub error: String,
    pub hint: Option<String>,
}

/// **播放状态机**（Kotlin `PlaybackSession` 里"不是界面也不是引擎"的那一半）。
#[derive(uniffi::Object)]
pub struct FfiPlaybackSession {
    inner: Mutex<core_session::Session>,
}

#[uniffi::export]
impl FfiPlaybackSession {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(core_session::Session::new()),
        })
    }

    /// 播放 / 暂停（播完时点播放 = 从头重播）。
    pub fn toggle_play(&self, state: FfiPlaybackState) -> FfiPlayEffect {
        FfiPlayEffect::from_core(self.lock().toggle_play(&state.to_core()))
    }

    /// 切后台 / 锁屏。
    pub fn pause_for_background(&self, state: FfiPlaybackState) -> FfiPlayEffect {
        FfiPlayEffect::from_core(self.lock().pause_for_background(&state.to_core()))
    }

    /// 回前台（`None` = 什么都不用做）。
    pub fn resume_from_background(&self, state: FfiPlaybackState) -> Option<FfiPlayEffect> {
        self.lock()
            .resume_from_background(&state.to_core())
            .map(FfiPlayEffect::from_core)
    }

    /// 快进 / 快退。
    pub fn seek_by(&self, state: FfiPlaybackState, delta_ms: i64) -> FfiPlayEffect {
        FfiPlayEffect::from_core(self.lock().seek_by(&state.to_core(), delta_ms))
    }

    /// 用户自己设置倍速（取消临时加速、解除锁定）。
    pub fn set_speed(&self, rate: f32) -> FfiPlayEffect {
        FfiPlayEffect::from_core(self.lock().set_speed(rate))
    }

    /// 临时倍速（按下）。
    pub fn hold_speed(&self, state: FfiPlaybackState, rate: f32) -> FfiPlayEffect {
        FfiPlayEffect::from_core(self.lock().hold_speed(&state.to_core(), rate))
    }

    /// 临时倍速（松手）。
    pub fn release_speed(&self) -> Option<FfiPlayEffect> {
        self.lock().release_speed().map(FfiPlayEffect::from_core)
    }

    /// 长按按下。
    pub fn long_press_start(&self, state: FfiPlaybackState) -> FfiPlayEffect {
        FfiPlayEffect::from_core(self.lock().long_press_start(&state.to_core()))
    }

    /// 长按上滑（返回切换后是不是锁着）。
    pub fn long_press_swipe_up(&self) -> bool {
        self.lock().long_press_swipe_up()
    }

    /// 长按松手。
    pub fn long_press_end(&self) -> Option<FfiPlayEffect> {
        self.lock().long_press_end().map(FfiPlayEffect::from_core)
    }

    /// 快一档 / 慢一档。
    pub fn step_speed(&self, state: FfiPlaybackState, delta: i32) -> FfiPlayEffect {
        FfiPlayEffect::from_core(self.lock().step_speed(&state.to_core(), delta))
    }

    /// 该不该存进度（返回要做的事，可能是空表）。
    pub fn save_progress(
        &self,
        part_index: i32,
        position_ms: i64,
        duration_ms: i64,
        part_id: Option<String>,
    ) -> Vec<FfiPlayEffect> {
        self.lock()
            .save_progress(part_index, position_ms, duration_ms, part_id.as_deref())
            .into_iter()
            .map(FfiPlayEffect::from_core)
            .collect()
    }

    /// 现在锁着没有。
    pub fn speed_locked(&self) -> bool {
        self.lock().speed_locked()
    }

    /// 长按的手指还按着没有。
    pub fn speed_pressing(&self) -> bool {
        self.lock().speed_pressing()
    }

    /// 底座倍速（提示里显示"恢复成几倍"用）：临时加速中是**加速前**那个，否则就是 `current`。
    ///
    /// 这一格在 WIP 里漏了：`PlaybackSession.baseSpeed` 一度退化成 `state.speed`（长按加速时提示会显示
    /// "恢复成 3 倍"而不是原来的 1 倍）。
    pub fn base_speed(&self, current: f32) -> f32 {
        self.lock().base_speed(current)
    }

    /// 取流失败要显示的文案（风控红线：不重试、不换参数，只说话）。
    pub fn play_failure(
        &self,
        failure: FfiPlayFailure,
        platform_label: String,
        proxied: bool,
    ) -> FfiFailureText {
        let f = match failure {
            FfiPlayFailure::NotLoggedIn => core_session::Failure::NotLoggedIn,
            FfiPlayFailure::RiskControl => core_session::Failure::RiskControl,
            FfiPlayFailure::Other { message } => core_session::Failure::Other(message),
        };
        let t = self.lock().play_failure(f, &platform_label, proxied);
        FfiFailureText {
            needs_login: t.needs_login,
            error: t.error,
            hint: t.hint,
        }
    }
}

impl FfiPlaybackSession {
    fn lock(&self) -> MutexGuard<'_, core_session::Session> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 续播位置（`Session.startPosition`）。
#[uniffi::export]
pub fn player_start_position(
    resume: bool,
    saved_ms: Option<i64>,
    start_ms: i64,
    duration_ms: i64,
) -> i64 {
    core_session::Session::start_position(resume, saved_ms, start_ms, duration_ms)
}

/// 首次打开该播第几 P（`Session.initialPartIndex`）。
#[uniffi::export]
pub fn player_initial_part_index(parts: Vec<String>, saved: Option<String>) -> u32 {
    core_session::Session::initial_part_index(&parts, saved.as_deref()) as u32
}

/// 时长：播放地址给的优先，没有就用分 P 自带的。
#[uniffi::export]
pub fn player_duration_of(info_json: String, part_duration_sec: Option<i32>) -> i64 {
    let info = serde_json::from_str::<serde_json::Value>(&info_json)
        .ok()
        .and_then(|v| v.get("durationMs").and_then(|d| d.as_i64()));
    match info {
        Some(ms) => ms,
        None => part_duration_sec.map(|s| s as i64 * 1000).unwrap_or(0),
    }
}

/// "没有可播放的流"的文案。
#[uniffi::export]
pub fn player_no_stream_text() -> String {
    core_session::Session::new().no_stream_text()
}

/// "这个视频没有可播放的内容"的文案。
#[uniffi::export]
pub fn player_no_part_text() -> String {
    core_session::Session::new().no_part_text()
}

/// 倍速夹到引擎认的范围（0.25~4，`NaN` → 1）。
#[uniffi::export]
pub fn player_clamp_rate(rate: f32) -> f32 {
    core_session::clamp_rate(rate)
}

// ===========================================================================
// 解析 `PlayInfo`（选流要读的那几项）
// ===========================================================================

/// 从 `PlayInfo` 的 JSON 里还原 Rust 的模型。
///
/// **只读**：宿主传进来的是它自己的 `PlayInfo` 序列化结果（`FeedJson`），这里只取选流用得到的那几项
/// （视频轨 / 音频轨 / 音画一体 / 清晰度列表），别的字段忽略。
fn parse_play_info(
    info_json: &str,
) -> Result<feedhub_core::play::PlayInfo, FfiPlayerError> {
    use feedhub_core::play as core_play;
    let value: serde_json::Value = serde_json::from_str(info_json).map_err(|e| FfiPlayerError::Parse {
        detail: format!("PlayInfo 不是合法 JSON：{e}"),
    })?;
    let obj = value.as_object().ok_or_else(|| FfiPlayerError::Parse {
        detail: "PlayInfo 不是对象".to_string(),
    })?;
    let arr = |key: &str| obj.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let opt_i32 = |v: &serde_json::Value, key: &str| v.get(key).and_then(|x| x.as_i64()).map(|x| x as i32);
    let opt_i64 = |v: &serde_json::Value, key: &str| v.get(key).and_then(|x| x.as_i64());
    let opt_str = |v: &serde_json::Value, key: &str| v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string());
    let strs = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let codec = |v: &serde_json::Value| -> core_play::VideoCodec {
        match v.get("codec").and_then(|x| x.as_str()) {
            Some("AVC") => core_play::VideoCodec::Avc,
            Some("HEVC") => core_play::VideoCodec::Hevc,
            Some("AV1") => core_play::VideoCodec::Av1,
            _ => core_play::VideoCodec::Other,
        }
    };
    let segment_base = |v: &serde_json::Value| -> Option<core_play::SegmentBase> {
        let sb = v.get("segmentBase")?;
        Some(core_play::SegmentBase {
            initialization: sb.get("initialization").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
            index_range: sb.get("indexRange").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        })
    };
    let video = arr("video")
        .iter()
        .map(|v| core_play::VideoStream {
            quality: opt_i32(v, "quality").unwrap_or(0),
            codec: codec(v),
            codecs: opt_str(v, "codecs"),
            url: opt_str(v, "url").unwrap_or_default(),
            backup_urls: strs(v, "backupUrls"),
            bandwidth: opt_i64(v, "bandwidth"),
            width: opt_i32(v, "width"),
            height: opt_i32(v, "height"),
            frame_rate: opt_str(v, "frameRate"),
            segment_base: segment_base(v),
        })
        .collect();
    let audio = arr("audio")
        .iter()
        .map(|v| core_play::AudioStream {
            id: opt_i32(v, "id").unwrap_or(0),
            codecs: opt_str(v, "codecs"),
            url: opt_str(v, "url").unwrap_or_default(),
            backup_urls: strs(v, "backupUrls"),
            bandwidth: opt_i64(v, "bandwidth"),
            segment_base: segment_base(v),
        })
        .collect();
    let progressive = arr("progressive")
        .iter()
        .map(|v| core_play::ProgressiveStream {
            quality: opt_i32(v, "quality"),
            url: opt_str(v, "url").unwrap_or_default(),
            backup_urls: strs(v, "backupUrls"),
            size_bytes: opt_i64(v, "sizeBytes"),
            width: opt_i32(v, "width"),
            height: opt_i32(v, "height"),
            codec: v.get("codec").and_then(|x| x.as_str()).map(|_| codec(v)),
            bandwidth: opt_i64(v, "bandwidth"),
        })
        .collect();
    let qualities = arr("qualities")
        .iter()
        .map(|v| core_play::Quality {
            id: opt_i32(v, "id").unwrap_or(0),
            label: opt_str(v, "label").unwrap_or_default(),
            available: v.get("available").and_then(|x| x.as_bool()).unwrap_or(true),
        })
        .collect();
    // headers 是有序的：`Map` 在 JSON 里是对象，顺序就是出现顺序（serde_json 的 preserve_order 没开，
    // 但选流不用它——播放那一侧走 `FfiPlayInfo`）
    let headers = Vec::new();
    Ok(core_play::PlayInfo {
        duration_ms: obj.get("durationMs").and_then(|x| x.as_i64()),
        quality: obj.get("quality").and_then(|x| x.as_i64()).map(|x| x as i32),
        qualities,
        video,
        audio,
        progressive,
        headers,
    })
}

