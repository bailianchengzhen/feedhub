//! **桌面播放引擎的 UniFFI 边界**：前缀 `FfiGst*`。
//!
//! 一个带状态的对象 [`FfiGstEngine`]（引擎）+ 一组纯函数（运行时探测 / 代理翻译的输入校验 /
//! 常量）。宿主（Kotlin 的 `RustGstPlayerEngine`）只做四件事：
//!
//! 1. **画**：从共享帧内存读 BGRA，造 Skia 的 `Image` 画进 Compose 的 Canvas；
//! 2. **把快照搬到 `PlayerState`**：`poll_snapshot` 返回的字段逐个赋值；
//! 3. 转发控制调用（`play` / `pause` / `seek_to` / `set_rate` …）；
//! 4. 定时器（每帧问一次 `frame_seq`、每 250ms 问一次 `poll_snapshot`）。
//!
//! ## 一帧的像素不过 FFI
//!
//! `surface_addr` / `surface_len` 在 `open` 之后给一次（`u64` 两个标量），之后每帧只有
//! `frame_seq` 一个整数变。布局见 `surface.rs` 的文件头。
//!
//! ## 回调
//!
//! 音画同步测量要"每帧 / 每段音频回调一次"。UniFFI 的 callback interface 可以做到，
//! 但那是**每次回调过一次 JNA 边界**——测量时一秒钟几十次，可以接受（它只在测试和
//! 检查工具里开）。生产路径一个回调都不装。

use std::sync::Arc;

use feedhub_core::play::VideoCodec;

use crate::engine::{
    AudioStamp, Engine, EngineConfig, EngineSnapshot, EngineStats, FrameStamp, LedgerHook,
    MediaSpec,
};
use crate::runtime;

/// 引擎推给界面的快照。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiGstSnapshot {
    pub playing: bool,
    pub buffering: bool,
    pub ended: bool,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub buffered_ms: i64,
    pub video_width: i32,
    pub video_height: i32,
    /// 实际在用的视频解码器元素名（如 `vah264dec`）；还不知道是 `None`。
    pub decoder: Option<String>,
    /// 硬解吗：`Some(true)` / `Some(false)` / `None` = 还不知道。
    pub hardware_decoding: Option<bool>,
    pub has_frame: bool,
    pub frames: u64,
    pub error: Option<String>,
    /// 刚发生了一次 flush seek、还没落地（宿主要不要接受"位置往回跳"看它）。
    pub seeking: bool,
}

impl From<EngineSnapshot> for FfiGstSnapshot {
    fn from(s: EngineSnapshot) -> Self {
        Self {
            playing: s.playing,
            buffering: s.buffering,
            ended: s.ended,
            position_ms: s.position_ms,
            duration_ms: s.duration_ms,
            buffered_ms: s.buffered_ms,
            video_width: s.video_width,
            video_height: s.video_height,
            decoder: s.decoder,
            hardware_decoding: s.hardware_decoding,
            has_frame: s.has_frame,
            frames: s.frames,
            error: s.error,
            seeking: s.seeking,
        }
    }
}

/// 一帧的记录（测量音画同步用）。
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct FfiGstFrameStamp {
    pub pts_ns: i64,
    pub running_ns: i64,
    pub luma: i32,
    pub width: i32,
    pub height: i32,
}

/// 一段音频的记录（测量音画同步用）。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiGstAudioStamp {
    pub pts_ns: i64,
    pub running_ns: i64,
    /// F32LE 交错的采样数据。
    pub data: Vec<u8>,
    pub rate: i32,
    pub channels: i32,
}

/// 引擎级统计。
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct FfiGstStats {
    pub flush_seeks: u64,
    pub instant_rate_changes: u64,
    pub frames_received: u64,
    pub software_fallbacks: u64,
    pub failovers: u64,
}

impl From<EngineStats> for FfiGstStats {
    fn from(s: EngineStats) -> Self {
        Self {
            flush_seeks: s.flush_seeks,
            instant_rate_changes: s.instant_rate_changes,
            frames_received: s.frames_received,
            software_fallbacks: s.software_fallbacks,
            failovers: s.failovers,
        }
    }
}

/// 共享帧内存的描述（宿主读一帧要用）。
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct FfiGstSurface {
    /// 块的基地址。
    pub addr: u64,
    /// 整块的字节数。
    pub len: u64,
    /// 每个槽的总跨度（头 + 像素区）。
    pub slot_stride: u64,
    /// 每个槽的像素区容量。
    pub slot_capacity: u64,
    /// 每次重建这块内存都会换一个新 id（宿主据此换掉自己的映射）。
    pub id: u64,
}

/// VA-API 探测结果。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiVaDevice {
    pub path: String,
    pub vendor: Option<String>,
    pub driver: String,
    pub va_version: String,
    /// profile id → 它的 entrypoint 列表。
    pub profiles: Vec<FfiVaProfile>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiVaProfile {
    pub id: i32,
    pub entrypoints: Vec<i32>,
}

impl From<&crate::vaapi::Device> for FfiVaDevice {
    fn from(d: &crate::vaapi::Device) -> Self {
        Self {
            path: d.path.clone(),
            vendor: d.vendor.clone(),
            driver: d.driver.clone(),
            va_version: d.va_version.clone(),
            profiles: d
                .profiles
                .iter()
                .map(|(id, eps)| FfiVaProfile {
                    id: *id,
                    entrypoints: eps.clone(),
                })
                .collect(),
        }
    }
}

/// 引擎的可调项（测量 / 测试用）。产品路径用 [`FfiGstConfig::product`]。
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct FfiGstConfig {
    pub hardware: bool,
    pub debug_sync: bool,
    /// 硬解输出宽度不取 64 的倍数、也不做"没有 PTS 就退回软解"的保护（还原修复前的行为）。
    pub align_width: bool,
    pub sink_max_buffers: u32,
    pub use_system_clock: bool,
    pub use_buffering: bool,
    /// 关掉即时变速，强制走 flush seek 回退路径（测试用）。
    pub allow_instant_rate: bool,
    /// 不出声。
    pub silent: bool,
    /// 用 appsink 当音频 sink（测量音画同步用）。
    pub debug_audio: bool,
    /// 量吞吐上限用：解码器后面直接接 `fakesink sync=false`。
    pub raw_sink: bool,
    /// 同上但保留 GPU 转换 + 缩放到画布尺寸（L2）。
    pub raw_sink_scaled: bool,
}

impl FfiGstConfig {
    /// 产品路径的默认值（和原来 Kotlin 的默认一致）。
    ///
    /// **不能当 `#[uniffi::constructor]`**：UniFFI 的构造器只能挂在 `#[uniffi::Object]` 上，
    /// 而 `FfiGstConfig` 是个 `Record`。所以宿主侧用的是 `FfiGstConfigBuilder`（Kotlin 那边的
    /// 小函数），默认值和这里对齐。
    pub fn product() -> Self {
        let d = EngineConfig::default();
        Self::from(&d)
    }
}

impl From<&EngineConfig> for FfiGstConfig {
    fn from(c: &EngineConfig) -> Self {
        Self {
            hardware: c.hardware,
            debug_sync: c.debug_sync,
            align_width: c.align_width,
            sink_max_buffers: c.sink_max_buffers,
            use_system_clock: c.use_system_clock,
            use_buffering: c.use_buffering,
            allow_instant_rate: c.allow_instant_rate,
            silent: c.silent,
            debug_audio: c.debug_audio,
            raw_sink: c.raw_sink,
            raw_sink_scaled: c.raw_sink_scaled,
        }
    }
}

impl From<FfiGstConfig> for EngineConfig {
    fn from(c: FfiGstConfig) -> Self {
        Self {
            hardware: c.hardware,
            debug_sync: c.debug_sync,
            align_width: c.align_width,
            sink_max_buffers: c.sink_max_buffers,
            use_system_clock: c.use_system_clock,
            use_buffering: c.use_buffering,
            allow_instant_rate: c.allow_instant_rate,
            silent: c.silent,
            debug_audio: c.debug_audio,
            raw_sink: c.raw_sink,
            raw_sink_scaled: c.raw_sink_scaled,
        }
    }
}

/// 视频编码（独立于 `feedhub_ffi`，免得两个 cdylib 互相绑死）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiGstCodec {
    Avc,
    Hevc,
    Av1,
    Other,
}

/// 帧节奏实测的账本写入端口（宿主桥接到 `RustDecodeLedger`）。
#[uniffi::export(with_foreign)]
pub trait FfiGstLedgerSink: Send + Sync {
    fn record_decode(&self, codec: FfiGstCodec, pixels_per_second: i64, smooth: bool, now_ms: i64);
}

/// 一次播放要放什么（宿主把 `MediaSource` 摊平）。**中转那一半不在这里**。
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiGstMedia {
    /// 视频地址（主 + 备用，按顺序试）。DASH 音画分离时这是**清单地址**。
    pub video_urls: Vec<String>,
    pub audio_urls: Vec<String>,
    /// 拉流必带的请求头，**顺序有意义**。
    pub headers: Vec<FfiGstHeader>,
    /// 源标的编码（没标就 `None`，引擎会从解码器元素名推）。
    pub codec: Option<FfiGstCodec>,
    pub duration_ms: Option<i64>,
    /// `souphttpsrc.proxy` 的值：`None` = 不设，`Some("")` = 显式直连，`Some("http://…")` = 走这个代理。
    /// **翻译在宿主**（`PlayerProxy.gstProxyUri`）。
    pub proxy_uri: Option<String>,
    /// 经中转的那一路（播放器只请求本机回环，请求头由中转的 OkHttp 加）。
    pub relayed: bool,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiGstHeader {
    pub name: String,
    pub value: String,
}

impl FfiGstMedia {
    fn to_spec(&self) -> MediaSpec {
        MediaSpec {
            video_urls: self.video_urls.clone(),
            audio_urls: self.audio_urls.clone(),
            headers: self
                .headers
                .iter()
                .map(|h| (h.name.clone(), h.value.clone()))
                .collect(),
            codec: self.codec.map(codec_to_core),
            duration_ms: self.duration_ms,
            proxy_uri: self.proxy_uri.clone(),
            relayed: self.relayed,
        }
    }
}

/// 测量用的回调（帧 / 音频）。**只在检查工具和对拍里装**。
#[uniffi::export(callback_interface)]
pub trait FfiGstFrameSink: Send + Sync {
    fn on_frame(&self, stamp: FfiGstFrameStamp);
}

#[uniffi::export(callback_interface)]
pub trait FfiGstAudioSink: Send + Sync {
    fn on_audio(&self, stamp: FfiGstAudioStamp);
}

fn codec_to_core(codec: FfiGstCodec) -> VideoCodec {
    match codec {
        FfiGstCodec::Avc => VideoCodec::Avc,
        FfiGstCodec::Hevc => VideoCodec::Hevc,
        FfiGstCodec::Av1 => VideoCodec::Av1,
        FfiGstCodec::Other => VideoCodec::Other,
    }
}

fn codec_to_ffi(codec: VideoCodec) -> FfiGstCodec {
    match codec {
        VideoCodec::Avc => FfiGstCodec::Avc,
        VideoCodec::Hevc => FfiGstCodec::Hevc,
        VideoCodec::Av1 => FfiGstCodec::Av1,
        VideoCodec::Other => FfiGstCodec::Other,
    }
}

/// **桌面播放引擎**（GStreamer 1.26 + VA-API 硬解）。
///
/// 一个实例一条播放（`open` 换一条管线）。所有方法在任意线程调都行；阻塞的 GStreamer 调用
/// 在 Rust 内部的控制线程上跑。
#[derive(uniffi::Object)]
pub struct FfiGstEngine {
    engine: Engine,
}

#[uniffi::export]
impl FfiGstEngine {
    /// 建一台引擎。**会触发 GStreamer 的进程级初始化 + VA-API 探测**（几百毫秒，一次）。
    /// 起不来时不抛异常——之后 `unavailable()` 会给原因，`open` 会立刻报错（和原来
    /// Kotlin 的那条"需要安装 GStreamer"提示一致）。
    #[uniffi::constructor]
    pub fn new(config: FfiGstConfig) -> Arc<Self> {
        Arc::new(Self {
            engine: Engine::new(config.into()),
        })
    }

    // ---------------- 能力 ----------------

    /// 这台机器没法播放的原因（`None` = 能用）。
    pub fn unavailable(&self) -> Option<String> {
        self.engine.unavailable()
    }

    /// 现在能硬解的编码（硬解关着时是空）。
    pub fn hardware_codecs(&self) -> Vec<FfiGstCodec> {
        self.engine.hardware_codecs().into_iter().map(codec_to_ffi).collect()
    }

    /// **解码环境指纹**：选定 VA 设备的 PCI `vendor:device` + libva 的驱动串 + GStreamer 版本。
    /// 账本拿它当落盘记录的失效判据；拿不到就 `None`（那时候不读也不写账本）。
    pub fn decode_environment_fingerprint(&self) -> Option<String> {
        runtime::decode_environment_fingerprint()
    }

    /// 和 [`FfiGstEngine::decode_environment_fingerprint`] 同义，起个全局名（宿主那边不需要句柄）。
    pub fn built_environment_fingerprint(&self) -> Option<String> {
        runtime::decode_environment_fingerprint()
    }

    // ---------------- 播放 ----------------

    /// 开始播放 [media]，从 [start_ms] 起；[auto_play] 为 false 时停在第一帧。
    pub fn open(&self, media: FfiGstMedia, start_ms: i64, auto_play: bool) {
        self.engine.open(media.to_spec(), start_ms, auto_play);
    }

    pub fn play(&self) {
        self.engine.play();
    }

    pub fn pause(&self) {
        self.engine.pause();
    }

    pub fn seek_to(&self, position_ms: i64) {
        self.engine.seek_to(position_ms);
    }

    pub fn set_volume(&self, percent: i32) {
        self.engine.set_volume(percent);
    }

    pub fn set_muted(&self, muted: bool) {
        self.engine.set_muted(muted);
    }

    pub fn set_looping(&self, looping: bool) {
        self.engine.set_looping(looping);
    }

    /// 设置播放速率（0.25～4，1.0 = 正常；变速不变调）。
    pub fn set_rate(&self, rate: f32) {
        self.engine.set_rate(rate);
    }

    /// 改视频 appsink 的 `max-buffers`（检查工具量"队列深度对出帧节奏的影响"用）。
    /// **只能在下一次 `open` 之前改**。
    pub fn set_sink_max_buffers(&self, n: u32) {
        self.engine.set_sink_max_buffers(n);
    }

    /// 画布变成了 `w×h` 像素（界面里是 Canvas 的尺寸回调）。
    pub fn set_surface_size(&self, w: i32, h: i32) {
        self.engine.set_surface_size(w, h);
    }

    /// 停止并释放。之后再调别的都是 no-op。
    pub fn release(&self) {
        self.engine.release();
    }

    // ---------------- 状态 ----------------

    /// 当前快照。
    pub fn snapshot(&self) -> FfiGstSnapshot {
        self.engine.snapshot().into()
    }

    /// 变了才返回（宿主每帧 / 每次 tick 调；`None` = 没变，不用写 Compose 状态）。
    pub fn poll_snapshot(&self) -> Option<FfiGstSnapshot> {
        self.engine.poll_snapshot().map(Into::into)
    }

    pub fn stats(&self) -> FfiGstStats {
        self.engine.stats().into()
    }

    /// 接上解码账本。**不传就只统计、不落盘。**
    pub fn attach_ledger(&self, ledger: Arc<dyn FfiGstLedgerSink>) {
        let hook: LedgerHook = Arc::new(move |codec, pps, smooth, now_ms| {
            ledger.record_decode(codec_to_ffi(codec), pps, smooth, now_ms);
        });
        self.engine.set_ledger(Some(hook));
    }

    // ---------------- 出帧 ----------------

    /// 共享帧内存的描述；还没有就 `None`（`open` 之前）。
    pub fn surface(&self) -> Option<FfiGstSurface> {
        self.engine.surface_info().map(|(addr, len, slot_stride, slot_capacity, id)| FfiGstSurface {
            addr,
            len,
            slot_stride,
            slot_capacity,
            id,
        })
    }

    /// 最新一帧的序号（宿主轮询它决定要不要重画：**只读这一个标量**）。
    pub fn frame_seq(&self) -> u64 {
        self.engine.frame_seq()
    }

    /// **读者占住当前这一帧**：返回槽号 / 宽 / 高 / 行距 / PTS，读完必须调
    /// [`FfiGstEngine::release_frame_slot`] 放开它（写者靠这个知道哪一槽不能覆写）。
    ///
    /// 宿主的用法（零拷贝）：`acquire` → 用 `surface().addr + slot*slotStride + slotHeaderBytes`
    /// 造 Skia 的 `Image` → `release`。像素一个字节都不过 FFI。
    pub fn acquire_frame(&self) -> Option<FfiGstFrameHeader> {
        self.engine
            .acquire_frame()
            .map(|(slot, w, h, stride, pts)| FfiGstFrameHeader {
                slot,
                width: w,
                height: h,
                stride,
                pts_ns: pts,
            })
    }

    /// 读完这一帧，放开槽。
    pub fn release_frame_slot(&self) {
        self.engine.release_frame_slot();
    }

    /// `(已写帧数, 因三槽占满跳过的帧数)`。正常跳过的恒为 0。
    pub fn frame_counters(&self) -> FfiGstFrameCounters {
        let (written, dropped) = self.engine.frame_counters();
        FfiGstFrameCounters { written, dropped }
    }

    /// 把当前这一帧编成 PNG（验证 / 调试用；不开窗、不依赖界面）。
    pub fn frame_as_png(&self) -> Option<Vec<u8>> {
        self.engine.frame_as_png()
    }

    // ---------------- 测量 / 验证 ----------------

    /// 装帧回调（音画同步测量用）。`None` = 摘掉。
    pub fn set_frame_sink(&self, sink: Option<Box<dyn FfiGstFrameSink>>) {
        let hook = sink.map(|s| {
            let s: Arc<dyn FfiGstFrameSink> = Arc::from(s);
            let hook: Arc<dyn Fn(FrameStamp) + Send + Sync> = Arc::new(move |stamp: FrameStamp| {
                s.on_frame(FfiGstFrameStamp {
                    pts_ns: stamp.pts_ns,
                    running_ns: stamp.running_ns,
                    luma: stamp.luma,
                    width: stamp.width,
                    height: stamp.height,
                })
            });
            hook
        });
        self.engine.set_frame_hook(hook);
    }

    /// 装音频回调（音画同步测量用，要在 `open` 之前设）。`None` = 摘掉。
    pub fn set_audio_sink(&self, sink: Option<Box<dyn FfiGstAudioSink>>) {
        let hook = sink.map(|s| {
            let s: Arc<dyn FfiGstAudioSink> = Arc::from(s);
            let hook: Arc<dyn Fn(AudioStamp) + Send + Sync> = Arc::new(move |stamp: AudioStamp| {
                s.on_audio(FfiGstAudioStamp {
                    pts_ns: stamp.pts_ns,
                    running_ns: stamp.running_ns,
                    data: stamp.data,
                    rate: stamp.rate,
                    channels: stamp.channels,
                })
            });
            hook
        });
        self.engine.set_audio_hook(hook);
    }

    /// 当前管线里所有元素的工厂名（验证用，如看有没有 scaletempo）。
    pub fn debug_elements(&self) -> Vec<String> {
        self.engine.debug_elements()
    }

    /// 音频 sink 收到的格式（验证用）：采样率和解码出来的一样，说明变速靠 scaletempo 拉伸、
    /// 没有靠改采样率。
    pub fn debug_audio_caps(&self) -> Option<String> {
        self.engine.debug_audio_caps()
    }

    /// 视频链路上各点的实际 caps（验证用）。
    pub fn debug_video_caps(&self) -> Option<String> {
        self.engine.debug_video_caps()
    }

    /// 管线里所有 `va*` 元素和它们的 `device-path`。
    pub fn debug_va_devices(&self) -> Vec<String> {
        self.engine.debug_va_devices()
    }

    /// 音频 sink 和视频 sink 各自此刻渲染到的媒体位置（毫秒）。
    pub fn debug_sink_positions_ms(&self) -> Option<FfiGstSinkPositions> {
        self.engine
            .debug_sink_positions_ms()
            .map(|(audio, video)| FfiGstSinkPositions { audio_ms: audio, video_ms: video })
    }

    /// 开 / 关 `uridecodebin3` 的 `use-buffering`（下一次 `open` 生效）。
    pub fn set_use_buffering(&self, on: bool) {
        self.engine.set_use_buffering(on);
    }

    /// 开 / 关即时变速（`false` = 换倍速强制走 flush seek 回退；验证回退路径用）。
    pub fn set_allow_instant_rate(&self, allowed: bool) {
        self.engine.set_allow_instant_rate(allowed);
    }

    /// 给管线里的 `dashdemux2` 设一个属性（检查工具压水位用）。元素还没建出来时 `false`。
    pub fn set_demux_property(&self, name: String, value: String) -> bool {
        self.engine.set_demux_property(&name, &value)
    }

    /// 每条 BUFFERING 报上来的百分比：`元素名=百分比`。
    pub fn debug_buffer_pct(&self) -> Vec<String> {
        self.engine.debug_buffer_pct()
    }

    /// 当前管线的时钟名（`"GstSystemClock"` = 系统时钟；声卡 sink 的名字 = 被声卡抢走了）。
    pub fn debug_clock_name(&self) -> Option<String> {
        self.engine.debug_clock_name()
    }

    /// 当前管线名（测量用）。
    pub fn debug_pipeline_name(&self) -> Option<String> {
        self.engine.debug_pipeline_name()
    }
}

/// 一帧的头部（读者拿到的"这一帧长什么样"）。
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct FfiGstFrameHeader {
    /// 这一帧在哪个槽。
    pub slot: u32,
    pub width: i32,
    pub height: i32,
    /// 每行的字节数（≥ 宽×4；GPU 下载出来的帧可能每行有对齐 padding）。
    pub stride: i32,
    /// 这一帧的 PTS（纳秒）；没有就是 -1。
    pub pts_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct FfiGstFrameCounters {
    pub written: u64,
    pub dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct FfiGstSinkPositions {
    pub audio_ms: i64,
    pub video_ms: i64,
}

// ---------------- 自由函数：运行时探测 ----------------

/// GStreamer 的版本串（`"1.26.2"`）；起不来就 `None`。
#[uniffi::export]
pub fn gst_version() -> Option<String> {
    runtime::gst_version_string()
}

/// GStreamer 初始化能不能用（`None` = 能用）。**会触发进程级初始化**。
#[uniffi::export]
pub fn gst_problem() -> Option<String> {
    runtime::runtime().problem.clone()
}

/// 选定 VA-API 设备路径（核显）；没有就 `None`。
#[uniffi::export]
pub fn gst_va_device_path() -> Option<String> {
    runtime::runtime().va_device_path.clone()
}

/// 选定设备的 PCI 厂商（`0x1002` = AMD / `0x8086` = Intel）。
#[uniffi::export]
pub fn gst_va_vendor() -> Option<String> {
    runtime::runtime().va_vendor.clone()
}

/// 选定设备上 libva 报的驱动串。
#[uniffi::export]
pub fn gst_va_driver() -> Option<String> {
    runtime::runtime().va_driver.clone()
}

/// GStreamer 的 va 元素**实际**解析到的设备（和 `gst_va_device_path` 不一致就是钉失败了）。
#[uniffi::export]
pub fn gst_va_resolved_device() -> Option<String> {
    runtime::runtime().va_resolved_device.clone()
}

/// 本机确认能硬解的编码。
#[uniffi::export]
pub fn gst_hardware_codecs() -> Vec<FfiGstCodec> {
    runtime::runtime().hardware_codecs.iter().copied().map(codec_to_ffi).collect()
}

/// 探测过、但设备侧没确认的编码 → 原因。
#[uniffi::export]
pub fn gst_rejected_codecs() -> Vec<FfiGstRejectedCodec> {
    runtime::runtime()
        .rejected_codecs
        .iter()
        .map(|(codec, why)| FfiGstRejectedCodec {
            codec: codec_to_ffi(*codec),
            reason: why.clone(),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiGstRejectedCodec {
    pub codec: FfiGstCodec,
    pub reason: String,
}

/// 能不能用 `INSTANT_RATE_CHANGE` 换倍速（GStreamer 1.18+）。
#[uniffi::export]
pub fn gst_instant_rate_change() -> bool {
    runtime::runtime().instant_rate_change()
}

/// **只测"这个 URL 拉得到吗"**（测试 / 检查工具用）：`souphttpsrc ! fakesink` 起到 EOS。
/// [proxy] 的语义和引擎里完全一致（`None` = 不设、`Some("")` = 显式直连、`Some("http://…")` = 走它）。
#[uniffi::export]
pub fn gst_probe_url(url: String, proxy: Option<String>, timeout_s: u32) -> bool {
    crate::engine::probe_url(&url, proxy.as_deref(), timeout_s)
}

/// `autoaudiosink` 内部实际选中的 sink 工厂名（`"pulsesink"` / `"fakesink"` …）；
/// `"fakesink"` 说明这台机器没有声卡。**会建一个临时 sink 起到 READY**。
#[uniffi::export]
pub fn gst_audio_sink_name() -> Option<String> {
    crate::engine::audio_sink_name()
}

/// 有没有 vapostproc（GPU 上转 BGRA 和缩放）。
#[uniffi::export]
pub fn gst_has_va_postproc() -> bool {
    runtime::runtime().has_va_postproc()
}

/// 开 / 关硬解：改 va 解码器的 rank（进程级，下一个建的管线生效）。
#[uniffi::export]
pub fn gst_set_hardware(enabled: bool) {
    runtime::runtime().set_hardware(enabled);
}

/// `/dev/dri/renderD128` → 它所在显卡的 PCI 厂商，如 `"0x1002"`。
#[uniffi::export]
pub fn gst_vendor_of(device_path: String) -> Option<String> {
    runtime::vendor_of(&device_path)
}

/// `/dev/dri/renderD128` → 这块卡的 PCI `vendor:device`（去掉 0x、小写，如 `1002:1234`）。
#[uniffi::export]
pub fn gst_pci_id_of(device_path: String) -> Option<String> {
    runtime::pci_id_of(&device_path)
}

/// 厂商 id → 人看得懂的名字。
#[uniffi::export]
pub fn gst_vendor_name(vendor: Option<String>) -> String {
    runtime::vendor_name(vendor.as_deref())
}

/// **解码环境指纹**（全局版，宿主不用建引擎就能问）：PCI `vendor:device` + libva 驱动串 +
/// GStreamer 版本。账本拿它当落盘记录的失效判据；拿不到就 `None`。
#[uniffi::export]
pub fn gst_decode_environment_fingerprint() -> Option<String> {
    runtime::decode_environment_fingerprint()
}

/// NVIDIA 的 PCI 厂商 id（宿主判断"这一块是不是独显"用，别在各处写死 `0x10de`）。
#[uniffi::export]
pub fn gst_nvidia_vendor() -> String {
    runtime::NVIDIA.to_string()
}

/// 让 GIO 的代理解析器不要代理回环（`no_proxy` / `NO_PROXY` 补上 127.0.0.1 / localhost / ::1）。
/// `GstRuntime` 初始化时已经调过一次；这个口子给宿主在"改了代理设置"之后重新调。
#[uniffi::export]
pub fn gst_ensure_loopback_not_proxied() {
    runtime::ensure_loopback_not_proxied();
}

// ---------------- 自由函数：VA-API 体检（检查工具用） ----------------

/// 扫 `/dev/dri/renderD*`，问每一个能不能 `vaInitialize`（**只查询、不解码**）。
///
/// 和引擎自己的探测不同：这个**会问 NVIDIA 那块**，
/// 生产路径不会碰独显。
#[uniffi::export]
pub fn va_scan_all() -> Vec<FfiVaDevice> {
    crate::vaapi::scan_all()
        .iter()
        .map(FfiVaDevice::from)
        .collect()
}

/// 问一块卡的 VA-API 能力；`vaInitialize` 失败就 `None`。
#[uniffi::export]
pub fn va_query(path: String) -> Option<FfiVaDevice> {
    crate::vaapi::query(&path).as_ref().map(FfiVaDevice::from)
}

/// 进程里有没有 libva。
#[uniffi::export]
pub fn va_available() -> bool {
    crate::vaapi::available()
}

// ---------------- 自由函数：口径常量（宿主和测试都要） ----------------

/// 硬解输出宽度的对齐单位（像素）。宿主不需要，但对拍要比这个值。
#[uniffi::export]
pub fn gst_align_px() -> i32 {
    crate::engine::ALIGN_PX
}

/// 硬解输出的最大宽度。
#[uniffi::export]
pub fn gst_max_frame_width() -> i32 {
    crate::engine::MAX_FRAME_WIDTH
}

/// 视频 appsink 的默认队列上限。
#[uniffi::export]
pub fn gst_sink_max_buffers() -> u32 {
    crate::engine::SINK_MAX_BUFFERS
}

/// 共享内存的槽数（宿主映射时要用）。
#[uniffi::export]
pub fn gst_surface_slots() -> u32 {
    crate::SURFACE_SLOTS as u32
}

/// 每个槽头部的字节数（宿主解析布局时要用）。
#[uniffi::export]
pub fn gst_slot_header_bytes() -> u32 {
    crate::surface::SLOT_HEADER as u32
}

/// 槽头里 `format` 字段的取值：BGRA8888。
#[uniffi::export]
pub fn gst_format_bgra() -> u32 {
    crate::surface::FORMAT_BGRA
}

