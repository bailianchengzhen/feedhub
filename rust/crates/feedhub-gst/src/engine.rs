//! 引擎本体：建管线、总线消息、状态机、seek / 倍速 / 循环 / 缓冲 / 硬解退软解 / 备用地址。
//!
//! 逐条对照原 Kotlin `GstPlayerEngine.kt` 写的，每个"为什么"都保留在对应函数的注释里
//! （那些注释是踩出来的，不是装饰）。
//!
//! ## 线程模型
//!
//! 和原来一样：**所有可能阻塞的 GStreamer 调用排在同一条控制线程上**（`gst-control`），
//! 界面线程只改几个原子标志再往队列里塞一个任务。理由：`set_state` / `seek` 会等网络、
//! 等解码线程，在界面线程上做就是卡界面。总线消息（在 GStreamer 自己的流线程上回调）也一律
//! 转投到控制线程。
//!
//! appsink 的回调在 GStreamer 的流线程上跑，那里只做"拷一帧进共享内存 + 记账"，
//! **不做任何会阻塞的事**。
//!
//! ## 所有权
//!
//! 全部状态在 [`EngineInner`] 里，引擎句柄 [`Engine`] 只是 `Arc<EngineInner>` 的一层壳。
//! 两条线程、appsink 回调、总线回调都各持一份 `Arc`，**没有裸指针、没有 self 引用**。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use feedhub_core::play::VideoCodec;

use super::runtime::{self, Runtime};
use super::surface::FrameShm;

// ---------------- 常量（全部照原 Kotlin 的值，改之前先看那边的注释） ----------------

/// 硬解时输出宽度的上限：比这更宽的源会被缩小。
pub const MAX_FRAME_WIDTH: i32 = 1920;
/// 硬解输出宽度的对齐单位（像素）：×4 字节 = 256。AMD 的 VA 表面行距按 256 字节对齐；
/// Intel 的更小，64 的倍数也满足。
pub const ALIGN_PX: i32 = 64;
/// 硬解时连续这么多帧没有 PTS，就认为这台机器的 VA 输出还是走了丢时间戳的路径，退回软解。
pub const NO_PTS_LIMIT: u64 = 5;
/// 视频 appsink 内部队列上限：2 帧够"边等时钟边收下一帧"，再多会多占一个 VA 表面、
/// 也可能多延迟一帧。
pub const SINK_MAX_BUFFERS: u32 = 2;
/// 节奏结算窗口（帧）：每这么多帧看一次"迟到比例"。实测 4K60 的 AV1 约 5%~6% 的间隔迟到，
/// HEVC/AVC 是 0。取 120 帧 = 4K60 下 2 秒、1080p30 下 4 秒。
pub const PACE_WINDOW: u64 = 120;
/// 窗口里迟到超过这么多帧就认为这种编码在这个负载下跟不上（迟到 = 间隔 > 1.5× 标称；6/120 = 5%）。
pub const PACE_MAX_LATE: u64 = 6;
/// 打开超时。
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// 预卷完了之后最多等视频支路接上这么久（源其实没有视频轨时别卡住）。
pub const VIDEO_WAIT: Duration = Duration::from_secs(2);
/// 缓冲多久没进展就认为卡死。
pub const STALL_TIMEOUT: Duration = Duration::from_secs(45);
/// 位置 / 缓冲的快照节流：界面自己按插值走，引擎这边 250ms 报一次就够
/// （和原 Kotlin 的 `ticker.scheduleWithFixedDelay(250ms)` 对齐）。
pub const TICK_INTERVAL: Duration = Duration::from_millis(250);
/// 停下来之后 ticker 还跑几拍（4 × 250ms = 1 秒）。
const SETTLE_TICKS: u32 = 4;

/// 一次播放用的流。原来 Kotlin 的 `MediaSource`——但**中转那一半不在这里**
/// 引擎只管"拿到的地址"（回环 URL 或直链），
#[derive(Debug, Clone, Default)]
pub struct MediaSpec {
    /// 视频地址（主 + 备用，按顺序试）。DASH 音画分离时这是**清单地址**。
    pub video_urls: Vec<String>,
    /// 音频地址（音画分离时）。备用地址跟着视频那条走（越界就取最后一条）。
    pub audio_urls: Vec<String>,
    /// 拉流必带的请求头（B站是 Referer 和 User-Agent）。
    pub headers: Vec<(String, String)>,
    /// 编码（源标了的那个；没标就从实际的解码器元素名推）。
    pub codec: Option<VideoCodec>,
    /// 已知总时长（毫秒）。
    pub duration_ms: Option<i64>,
    /// 代理的 `souphttpsrc` 属性值：`None` = 不设（走 GIO 默认代理解析器），
    /// `Some("")` = 显式直连（回环地址），`Some("http://host:port")` = 走这个代理。
    ///
    /// **翻译在宿主**（`PlayerProxy.gstProxyUri`）：它取决于 `ProxyMode` 的语义
    /// （哪些能表达成 `souphttpsrc.proxy`、哪些只能走中转），那是"框架接口能不能表达"的问题。
    /// 引擎只负责把值设下去。
    pub proxy_uri: Option<String>,
    /// 经中转的那一路：播放器只请求本机回环，请求头已经由中转的 OkHttp 加上，
    /// 这里不用再带。
    pub relayed: bool,
}

/// 引擎推给界面的快照（宿主只把字段赋给 `PlayerState`）。
#[derive(Debug, Clone, PartialEq)]
pub struct EngineSnapshot {
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
    /// 出过第一帧没有（行内视频据此把封面收起来）。
    pub has_frame: bool,
    /// 出过多少帧。
    pub frames: u64,
    pub error: Option<String>,
    /// 刚发生了一次 flush seek、还没落地（宿主据此决定要不要接受"位置往回跳"）。
    pub seeking: bool,
}

impl EngineSnapshot {
    pub(crate) fn initial() -> Self {
        Self {
            playing: false,
            buffering: true,
            ended: false,
            position_ms: 0,
            duration_ms: 0,
            buffered_ms: 0,
            video_width: 0,
            video_height: 0,
            decoder: None,
            hardware_decoding: None,
            has_frame: false,
            frames: 0,
            error: None,
            seeking: false,
        }
    }
}

/// 引擎的可调项（测量 / 测试用；产品路径全用默认值）。
///
/// 原 Kotlin 的那些 `debugXxx` 字段一个不少地搬过来，`dev/` 下的探针和对拍靠它们。
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// 不用硬解（对比测试用）。`FEEDHUB_HWDEC=0` 同效。
    pub hardware: bool,
    /// 检查工具用：保留音频 / 视频 sink 的引用，能分别问它们"此刻渲染到媒体的哪个位置"。
    pub debug_sync: bool,
    /// 硬解输出宽度不取 64 的倍数、也不做"没有 PTS 就退回软解"的保护（还原修复前的行为）。
    pub align_width: bool,
    /// 视频 appsink 的 `max-buffers`。
    pub sink_max_buffers: u32,
    /// 用系统时钟、不让声卡 sink 提供时钟。
    pub use_system_clock: bool,
    /// `uridecodebin3` 的 `use-buffering`。
    pub use_buffering: bool,
    /// 关掉即时变速，强制走"带新速率的 flush seek"回退路径（测试用）。
    /// 运行时可以用 [`Engine::set_allow_instant_rate`] 改（那是个原子开关）。
    pub allow_instant_rate: bool,
    /// 不出声（无窗口 / `FEEDHUB_AUDIO=none`）。
    pub silent: bool,
    /// 用 appsink 当音频 sink（测量音画同步用）：每个缓冲连同到达时刻回调一次。
    pub debug_audio: bool,
    /// **量吞吐上限用**：视频链停在解码器后面直接接 `fakesink sync=false`，不转 BGRA、不缩放、
    /// 不等时钟。产品路径永远是 `false`。
    pub raw_sink: bool,
    /// 同 [`Self::raw_sink`]，但保留 `vapostproc ! capsfilter(BGRA,WxH)` 那一段再下载（L2）。
    pub raw_sink_scaled: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            hardware: true,
            debug_sync: false,
            align_width: true,
            sink_max_buffers: SINK_MAX_BUFFERS,
            use_system_clock: true,
            use_buffering: true,
            allow_instant_rate: true,
            silent: false,
            debug_audio: false,
            raw_sink: false,
            raw_sink_scaled: false,
        }
    }
}

/// 一帧送到 appsink（`sync=true`，到点才送）时的记录。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameStamp {
    pub pts_ns: i64,
    /// 管线运行时间（时钟 - base_time）。
    pub running_ns: i64,
    /// 画面 3×3 个点亮度的最小值（0~255）。
    pub luma: i32,
    pub width: i32,
    pub height: i32,
}

/// 一段音频送到音频 sink（`sync=true`，到点才送）时的记录。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioStamp {
    pub pts_ns: i64,
    pub running_ns: i64,
    /// F32LE 交错的采样数据。
    pub data: Vec<u8>,
    pub rate: i32,
    pub channels: i32,
}

/// 引擎级统计（验证 / 性能测量用）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EngineStats {
    /// 发出过几次 **flush** seek（拖动 / 循环 / 回退的变速都算）。
    pub flush_seeks: u64,
    /// 成功走上即时变速（`INSTANT_RATE_CHANGE`）的次数。
    pub instant_rate_changes: u64,
    /// 收到的帧数。
    pub frames_received: u64,
    /// 退回软解的次数。
    pub software_fallbacks: u64,
    /// 换备用地址的次数。
    pub failovers: u64,
}

type FrameHook = Arc<dyn Fn(FrameStamp) + Send + Sync>;
type AudioHook = Arc<dyn Fn(AudioStamp) + Send + Sync>;

/// 一条管线的全部状态。控制线程通过队列串行改它；appsink / 总线线程只用原子字段。
struct Run {
    spec: MediaSpec,
    use_hardware: bool,
    start_ms: i64,
    want_playing: AtomicBool,
    pipeline: gst::Pipeline,
    /// 管线的总线。
    ///
    /// `pipeline.bus()` 每次都返回新的包装，而 `timed_pop` / `set_flushing` 会在那一个上
    /// 现建一对 GLib 唤醒管道（2 个 fd），只有那个 `GstBus` finalize 时才关。缓存一份，
    /// 一轮播放就只可能有一对。
    bus: Option<gst::Bus>,
    started_at: Instant,
    prerolled: AtomicBool,
    seeking: AtomicBool,
    /// 这次 seek 是**为了换倍速**发的（界面要装成"什么都没发生"）。
    seek_quiet: AtomicBool,
    /// 预卷完成后要先跳到的位置；-1 = 不用跳。
    seek_after_preroll: AtomicI64,
    rate: Mutex<f32>,
    closed: AtomicBool,
    target_state: Mutex<gst::State>,
    buffer_pct: Mutex<HashMap<String, i32>>,
    buffering_since: Mutex<Option<Instant>>,
    video_linked: AtomicBool,
    /// 视频支路已经接好（元素进了管线、sink 也同步过状态了）。
    video_ready: AtomicBool,
    /// 管线已经预卷完、但在等视频支路接上的开始时刻。
    waiting_video_since: Mutex<Option<Instant>>,
    video_wait_timeout_logged: AtomicBool,
    audio_linked: AtomicBool,
    volume: Mutex<Option<gst::Element>>,
    video_filter: Mutex<Option<gst::Element>>,
    video_sink: Mutex<Option<gst_app::AppSink>>,
    audio_sink: Mutex<Option<gst::Element>>,
    video_sink_pad: Mutex<Option<gst::Pad>>,
    /// 信号处理器的 id。
    ///
    /// **为什么必须断**：`source-setup` / `pad-added` 的闭包抓着 `Arc<Run>` 和
    /// `Arc<EngineInner>`，而它们挂在 `uridecodebin3` 上，`uridecodebin3` 又在 `Run.pipeline`
    /// 里——`Run → pipeline → element → closure → Arc<Run>` 是一个**引用环**，`Arc` 永远不归零，
    /// 管线就永远不 finalize，它 bus 背后那对 `socketpair`（2 个 fd）也就永远不关。
    /// 实测：每开一次播放漏 2 个 fd，200 次漏 400 个。
    signal_handlers: Mutex<Vec<(gst::Element, glib::SignalHandlerId)>>,
    source_w: AtomicI64,
    source_h: AtomicI64,
    target_w: AtomicI64,
    target_h: AtomicI64,
    got_frame: AtomicBool,
    no_pts_frames: AtomicU64,
    frame_ns: AtomicI64,
    last_frame_ns: AtomicI64,
    pace_frames: AtomicU64,
    pace_late: AtomicU64,
    pace_codec: Mutex<Option<VideoCodec>>,
    pace_codec_resolved: AtomicBool,
    frame_hook: Mutex<Option<FrameHook>>,
    audio_hook: Mutex<Option<AudioHook>>,
    /// 表面尺寸（画布像素），用来让 vapostproc 缩放。
    surface_size: Mutex<(i32, i32)>,
    shm: Arc<FrameShm>,
}

impl Run {
    fn buffering(&self) -> bool {
        self.buffer_pct.lock().unwrap().values().any(|v| *v < 100)
    }
}

struct Job(Box<dyn FnOnce(&Arc<EngineInner>) + Send>);

pub type LedgerHook = Arc<dyn Fn(VideoCodec, i64, bool, i64) + Send + Sync>;

/// 引擎的全部状态。
struct EngineInner {
    cfg: EngineConfig,
    runtime_missing: Option<String>,
    rt: &'static Runtime,
    snapshot: Mutex<EngineSnapshot>,
    stats: Mutex<EngineStats>,
    want_playing: AtomicBool,
    current: Mutex<Option<MediaSpec>>,
    attempt: AtomicU64,
    software_fallback: AtomicBool,
    pending_start_ms: AtomicI64,
    auto_play: AtomicBool,
    looping: AtomicBool,
    run: Mutex<Option<Arc<Run>>>,
    /// 控制线程的任务队列 + 唤醒。
    jobs: Mutex<Vec<Job>>,
    jobs_cv: Condvar,
    /// 统计线程的条件等待（暂停 / 空闲时挂起，不再每 250ms 唤醒）。
    stats_wake: (Mutex<()>, Condvar),
    /// 暂停 / 预卷完 / seek 完之后 ticker 再跑几拍（每拍 250ms）：把位置刷到停下来那一刻的真值再挂起，
    /// 否则界面停在上一拍的位置（最多差 250ms，`GstAvSyncTest` 量的就是这个）。
    settle_ticks: AtomicU32,
    stopping: AtomicBool,
    released: AtomicBool,
    volume: AtomicI64,
    muted: AtomicBool,
    last_pushed: Mutex<EngineSnapshot>,
    shm: Mutex<Option<Arc<FrameShm>>>,
    /// 帧序号（Kotlin 轮询它决定要不要重画）。
    frame_seq: Arc<AtomicU64>,
    /// 记账用的账本回调（宿主注入）。
    ledger: Mutex<Option<LedgerHook>>,
    /// `open` 之前设的回调也要生效：先存这里，建 Run 时带过去。
    /// 下一次 `open` 要用的视频 appsink 队列上限。
    pending_sink_buffers: Mutex<u32>,
    /// 宿主报的画布尺寸（引擎像素）。**开播之前报的也要记住**：界面里 Canvas 的尺寸回调
    /// 常常比 `open` 还早，而 `Run::surface_size` 是建管线那一刻才有的。
    pending_surface: Mutex<(i32, i32)>,
    /// 有没有装音频回调。装了就必须用 appsink 当音频 sink（否则回调永远收不到东西），
    /// 所以 `set_audio_hook` 会把它置真——**不用调用方再去 `EngineConfig` 里改一项**。
    audio_hook_active: AtomicBool,
    /// 即时变速开着吗（`set_allow_instant_rate` 改，`instant_rate_change` 读）。
    allow_instant_rate: AtomicBool,
    /// `uridecodebin3` 的 `use-buffering`（`set_use_buffering` 改，建管线时读）。
    use_buffering: AtomicBool,
    pending_frame_hook: Mutex<Option<FrameHook>>,
    pending_audio_hook: Mutex<Option<AudioHook>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

/// 引擎句柄：`Arc<EngineInner>` 的一层薄壳。所有方法在任意线程调都行。
#[derive(Clone)]
pub struct Engine {
    inner: Arc<EngineInner>,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Self {
        let allow_instant = cfg.allow_instant_rate;
        let cfg_use_buffering = cfg.use_buffering;
        let rt = runtime::runtime();
        let inner = Arc::new(EngineInner {
            runtime_missing: rt.problem.clone(),
            cfg,
            rt,
            snapshot: Mutex::new(EngineSnapshot::initial()),
            stats: Mutex::new(EngineStats::default()),
            want_playing: AtomicBool::new(false),
            current: Mutex::new(None),
            attempt: AtomicU64::new(0),
            software_fallback: AtomicBool::new(false),
            pending_start_ms: AtomicI64::new(0),
            auto_play: AtomicBool::new(true),
            looping: AtomicBool::new(false),
            run: Mutex::new(None),
            jobs: Mutex::new(Vec::new()),
            jobs_cv: Condvar::new(),
            stats_wake: (Mutex::new(()), Condvar::new()),
            settle_ticks: AtomicU32::new(0),
            stopping: AtomicBool::new(false),
            released: AtomicBool::new(false),
            volume: AtomicI64::new(100),
            muted: AtomicBool::new(false),
            last_pushed: Mutex::new(EngineSnapshot::initial()),
            shm: Mutex::new(None),
            frame_seq: Arc::new(AtomicU64::new(0)),
            ledger: Mutex::new(None),
            pending_sink_buffers: Mutex::new(SINK_MAX_BUFFERS),
            pending_surface: Mutex::new((0, 0)),
            audio_hook_active: AtomicBool::new(false),
            allow_instant_rate: AtomicBool::new(allow_instant),
            use_buffering: AtomicBool::new(cfg_use_buffering),
            pending_frame_hook: Mutex::new(None),
            pending_audio_hook: Mutex::new(None),
            threads: Mutex::new(Vec::new()),
        });
        inner.clone().start_threads();
        Self { inner }
    }

    /// 这台机器能不能用（`Some(原因)` = 不能用）。
    pub fn unavailable(&self) -> Option<String> {
        self.inner.runtime_missing.clone()
    }

    /// 现在能硬解的编码。硬解关着时是空。
    pub fn hardware_codecs(&self) -> Vec<VideoCodec> {
        if !self.inner.hardware_enabled() {
            return Vec::new();
        }
        self.inner.rt.hardware_codecs.clone()
    }

    /// 当前快照（界面读它）。
    ///
    /// **锁被毒掉也要能读**：这个函数每帧都从宿主线程（JNA）进来，里面再 panic 一次就是
    /// "Rust 的 panic 穿过 FFI 边界"，实测会把整条测试 JVM 带走。锁被毒只说明**别处**先出过问题，
    /// 这里照读最后一份写好的快照（写者都是完整写完才放锁的）。
    pub fn snapshot(&self) -> EngineSnapshot {
        self.inner
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 变了才返回（宿主每帧 / 每次 tick 调；没变就不用写 Compose 状态，避免白出帧）。
    pub fn poll_snapshot(&self) -> Option<EngineSnapshot> {
        let snap = self.snapshot();
        let mut last = self.inner.last_pushed.lock().unwrap();
        if *last == snap {
            return None;
        }
        *last = snap.clone();
        Some(snap)
    }

    pub fn stats(&self) -> EngineStats {
        *self.inner.stats.lock().unwrap()
    }

    /// 共享帧内存的 `(地址, 总长, 槽跨度, 槽容量, id)`；还没有就 `None`。
    pub fn surface_info(&self) -> Option<(u64, u64, u64, u64, u64)> {
        let guard = self.inner.shm.lock().unwrap();
        guard
            .as_ref()
            .map(|s| (s.addr(), s.byte_len(), s.slot_stride(), s.slot_capacity(), s.id()))
    }

    /// 最新一帧的序号（Kotlin 轮询这个决定要不要重画）。
    pub fn frame_seq(&self) -> u64 {
        self.inner.frame_seq.load(Ordering::Acquire)
    }

    /// `(已写帧数, 因三槽占满跳过的帧数)`。
    pub fn frame_counters(&self) -> (u64, u64) {
        let guard = self.inner.shm.lock().unwrap();
        match guard.as_ref() {
            Some(s) => (s.frames_written(), s.frames_dropped()),
            None => (0, 0),
        }
    }

    /// 读者"占住"当前这一帧：返回 `(槽号, 宽, 高, stride, pts)`，读完要调 [`Engine::release_frame_slot`]。
    ///
    /// 这是共享内存协议里读者那一半：占住期间写者不会覆写这一槽（三槽轮转保证），
    /// 宿主可以在里面慢慢造 Skia 的 `Image`（零拷贝，不拷像素）。
    pub fn acquire_frame(&self) -> Option<(u32, i32, i32, i32, i64)> {
        let guard = self.inner.shm.lock().unwrap();
        let shm = guard.as_ref()?;
        let slot = shm.acquire()?;
        let (w, h, stride, pts) = match shm.slot_header(slot) {
            Some(v) => v,
            None => {
                shm.release_slot();
                return None;
            }
        };
        Some((slot, w as i32, h as i32, stride as i32, pts))
    }

    /// 读完这一帧，放开槽（写者可以覆写它了）。
    pub fn release_frame_slot(&self) {
        let guard = self.inner.shm.lock().unwrap();
        if let Some(shm) = guard.as_ref() {
            shm.release_slot();
        }
    }

    /// 把当前这一帧编成 PNG（验证 / 调试用；不开窗、不依赖界面）。还没有帧就 `None`。
    pub fn frame_as_png(&self) -> Option<Vec<u8>> {
        let (w, h, stride, pixels) = {
            let guard = self.inner.shm.lock().unwrap();
            let shm = guard.as_ref()?;
            let slot = shm.acquire()?;
            let (w, h, stride, _pts) = shm.slot_header(slot)?;
            let pixels = super::surface::copy_slot_pixels(shm, slot);
            shm.release_slot();
            (w, h, stride as usize, pixels)
        };
        encode_png(w, h, stride, &pixels)
    }

    // ---------------- 控制 ----------------

    pub fn open(&self, spec: MediaSpec, start_ms: i64, auto_play: bool) {
        let dur = spec.duration_ms.unwrap_or(0);
        *self.inner.current.lock().unwrap() = Some(spec);
        self.inner.attempt.store(0, Ordering::Release);
        self.inner.software_fallback.store(false, Ordering::Release);
        self.inner.pending_start_ms.store(start_ms, Ordering::Release);
        self.inner.auto_play.store(auto_play, Ordering::Release);
        self.inner.want_playing.store(auto_play, Ordering::Release);
        {
            let mut snap = self.inner.snapshot.lock().unwrap();
            snap.error = None;
            snap.ended = false;
            snap.playing = auto_play;
            snap.buffering = true;
            snap.duration_ms = dur;
            snap.position_ms = start_ms;
            snap.buffered_ms = start_ms;
            snap.video_width = 0;
            snap.video_height = 0;
            snap.decoder = None;
            snap.hardware_decoding = None;
        }
        self.inner.exec(|inner| inner.start_current());
    }

    pub fn play(&self) {
        let ended = self.inner.snapshot.lock().unwrap().ended;
        if ended {
            let spec = self.inner.current.lock().unwrap().clone();
            if let Some(spec) = spec {
                self.open(spec, 0, true);
            }
            return;
        }
        self.inner.want_playing.store(true, Ordering::Release);
        self.inner.snapshot.lock().unwrap().playing = true;
        self.inner.exec(|inner| {
            // **先把 guard 丢掉再往下调**（`if let Some(x) = m.lock().unwrap().clone()` 的
            // 临时 `MutexGuard` 会活到整个 `if` 块的末尾——`apply_target` 自己也要锁 `run`，
            // std 的 Mutex 不可重入，同一个线程当场自己等自己）。
            let run = inner.run.lock().unwrap().clone();
            if let Some(run) = run {
                run.want_playing.store(true, Ordering::Release);
                inner.apply_target(&run);
            }
        });
    }

    pub fn pause(&self) {
        self.inner.snapshot.lock().unwrap().playing = false;
        self.inner.want_playing.store(false, Ordering::Release);
        self.inner.exec(|inner| {
            let run = inner.run.lock().unwrap().clone();
            if let Some(run) = run {
                run.want_playing.store(false, Ordering::Release);
                inner.apply_target(&run);
            }
            inner.settle();
        });
    }

    pub fn seek_to(&self, position_ms: i64) {
        let dur = self.inner.snapshot.lock().unwrap().duration_ms;
        let limit = if dur > 0 { dur } else { i64::MAX };
        let target = position_ms.clamp(0, limit);
        if self.inner.snapshot.lock().unwrap().ended {
            if let Some(spec) = self.inner.current.lock().unwrap().clone() {
                self.open(spec, target, true);
            }
            return;
        }
        {
            let mut snap = self.inner.snapshot.lock().unwrap();
            snap.position_ms = target;
            snap.buffered_ms = target;
        }
        self.inner.exec(move |inner| {
            let run = inner.run.lock().unwrap().clone();
            if let Some(run) = run {
                if !run.prerolled.load(Ordering::Acquire) {
                    run.seek_after_preroll.store(target, Ordering::Release);
                } else {
                    inner.do_seek(&run, target, false);
                }
            }
        });
    }

    pub fn set_volume(&self, percent: i32) {
        let p = percent.clamp(0, 100);
        self.inner.volume.store(p as i64, Ordering::Release);
        self.inner.exec(move |inner| {
            let run = inner.run.lock().unwrap().clone();
            if let Some(run) = run {
                let volume = run.volume.lock().unwrap().clone();
                if let Some(v) = volume {
                    v.set_property("volume", p as f64 / 100.0);
                }
            }
        });
    }

    pub fn set_muted(&self, muted: bool) {
        self.inner.muted.store(muted, Ordering::Release);
        self.inner.exec(move |inner| {
            let run = inner.run.lock().unwrap().clone();
            if let Some(run) = run {
                let volume = run.volume.lock().unwrap().clone();
                if let Some(v) = volume {
                    v.set_property("mute", muted);
                }
            }
        });
    }

    pub fn set_looping(&self, looping: bool) {
        self.inner.looping.store(looping, Ordering::Release);
    }

    /// 设置播放速率（0.25～4，1.0 = 正常；变速不变调）。
    pub fn set_rate(&self, rate: f32) {
        let v = rate.clamp(0.25, 4.0);
        self.inner.exec(move |inner| {
            // 同上：`let ... else` 的 scrutinee 临时值也会活到块末尾，先绑定再判断
            let run_opt = inner.run.lock().unwrap().clone();
            let Some(run) = run_opt else { return };
            if (*run.rate.lock().unwrap() - v).abs() < 1e-6 {
                return;
            }
            *run.rate.lock().unwrap() = v;
            if run.prerolled.load(Ordering::Acquire) {
                // 先试即时变速（只改速率、不清管线）：不缓冲、不断音、位置不跳
                if inner.instant_rate_change(&run, v).is_some() {
                    inner.stats.lock().unwrap().instant_rate_changes += 1;
                    return;
                }
                // **回退 seek 的位置取"界面上此刻报的那个"，不重新问管线**：
                // `query_position` 回来的是 GStreamer 的播放位置，它比 `state.report` 收到的
                // 那一份晚一个 tick，而且 appsink 的队列里还有压缩好的帧——拿它当 seek 目标会比
                // 界面已经显示的位置**往回退几百毫秒**，用户看到进度条倒着跳。
                // 原来 Kotlin 引擎回退时用的就是 `state.positionMs`（界面上那个值），这里照它。
                let pos = inner.snapshot.lock().unwrap().position_ms;
                // 回退也装成"什么都没发生"：不转圈、不冻进度、不停弹幕。
                //
                // `tick` 在 `seeking` 为真期间会**冻住位置**（不更新 `position_ms`），
                // 所以界面这一侧看到的就是 seek 目标那一点，直到 ASYNC_DONE 之后管线
                // 真的从那儿接着走。seek 目标 = 界面此刻报的位置（`pos` 就是它），
                // 所以进度条一格都不动。
                inner.do_seek(&run, pos, true);
            } else if run.seek_after_preroll.load(Ordering::Acquire) < 0 {
                run.seek_after_preroll.store(run.start_ms.max(0), Ordering::Release);
            }
        });
    }

    /// 开 / 关 `uridecodebin3` 的 `use-buffering`（下一次 `open` 生效）。
    pub fn set_use_buffering(&self, on: bool) {
        self.inner.use_buffering.store(on, Ordering::Release);
    }

    /// 开 / 关即时变速（`false` = 换倍速强制走 flush seek 回退；验证回退路径用）。
    /// 改的是引擎的可变开关，下一个 `set_rate` 就生效。
    pub fn set_allow_instant_rate(&self, allowed: bool) {
        self.inner.allow_instant_rate.store(allowed, Ordering::Release);
    }

    /// 给管线里的 `dashdemux2` 设一个属性（`RelayStallProbeTest` 用来压低水位 / 抬高缓冲上限，
    /// 复现"卡住"）。**元素还没建出来时返回 `false`**（调用方轮询重试）。
    ///
    /// 检查工具用；产品路径不改 demux 的属性。
    pub fn set_demux_property(&self, name: &str, value: &str) -> bool {
        let Some(run) = self.inner.run.lock().unwrap().clone() else { return false };
        for el in iterate_elements(&run.pipeline) {
            let Some(f) = el.factory() else { continue };
            if f.name().as_str() != "dashdemux2" {
                continue;
            }
            // GObject 的属性不能按"字符串"一把设（`high-watermark-time` 是 guint64、
            // `presentation-delay` 是 guint）；这里只处理探针要用的两种类型。
            // `set_property` / `set_property_from_str` 返回 `bool`；类型不对时 GLib 会打 CRITICAL 但值不变。
            let ok = if let Ok(v) = value.parse::<u64>() {
                el.set_property(name, v);
                true
            } else {
                // `set_property_from_str` 没有返回值；能不能设由 `find_property` 先判一道
                if el.find_property(name).is_some() {
                    el.set_property_from_str(name, value);
                    true
                } else {
                    false
                }
            };
            if ok {
                return true;
            }
        }
        false
    }

    /// 改视频 appsink 的 `max-buffers`（检查工具量"队列深度对出帧节奏的影响"用）。
    /// **只能在下一次 `open` 之前改**（已经建好的管线不动）。
    pub fn set_sink_max_buffers(&self, n: u32) {
        *self.inner.pending_sink_buffers.lock().unwrap() = n.max(1);
    }

    /// 画布变成了 `w×h` 像素（界面里是 Canvas 的尺寸回调）。
    pub fn set_surface_size(&self, w: i32, h: i32) {
        self.inner.exec(move |inner| {
            // **先记下来**：`open` 之前报的画布尺寸（界面里 Canvas 的回调常常比 open 早）
            // 以前会直接丢掉 → 竖屏 + 硬解那条用例缩不到画布上（输出 704x1250 而不是 384 宽）。
            *inner.pending_surface.lock().unwrap() = (w, h);
            let run = inner.run.lock().unwrap().clone();
            if let Some(run) = run {
                *run.surface_size.lock().unwrap() = (w, h);
                inner.apply_target_size(&run);
            }
        });
    }

    /// 记账用的账本（宿主注入）。
    pub fn set_ledger(&self, ledger: Option<LedgerHook>) {
        *self.inner.ledger.lock().unwrap() = ledger;
    }

    /// 装帧回调（测量音画同步用）。
    pub fn set_frame_hook(&self, hook: Option<FrameHook>) {
        *self.inner.pending_frame_hook.lock().unwrap() = hook.clone();
        let run = self.inner.run.lock().unwrap().clone();
        if let Some(run) = run {
            *run.frame_hook.lock().unwrap() = hook;
        }
    }

    pub fn set_audio_hook(&self, hook: Option<AudioHook>) {
        self.inner.audio_hook_active.store(hook.is_some(), Ordering::Release);
        *self.inner.pending_audio_hook.lock().unwrap() = hook.clone();
        let run = self.inner.run.lock().unwrap().clone();
        if let Some(run) = run {
            *run.audio_hook.lock().unwrap() = hook;
        }
    }

    /// 释放整个引擎（之后再调别的都是 no-op）。
    pub fn release(&self) {
        if self.inner.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.inner.stopping.store(true, Ordering::Release);
        // 排一个收尾任务，然后等它跑完
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        {
            let d = done.clone();
            self.inner.push_job(Box::new(move |inner: &Arc<EngineInner>| {
                inner.teardown(true);
                let (m, cv) = &*d;
                *m.lock().unwrap() = true;
                cv.notify_all();
            }));
        }
        let (m, cv) = &*done;
        {
            let guard = m.lock().unwrap();
            let (guard, _) = cv.wait_timeout_while(guard, Duration::from_secs(5), |done| !*done).unwrap();
            drop(guard);
        }
        // 唤醒控制线程与统计线程让它们立刻退出，然后 join
        {
            // 持 jobs 锁再叫：控制线程在锁里查完 `stopping` 才睡（没有超时），不持锁可能漏掉这一下
            let _q = self.inner.jobs.lock().unwrap();
            self.inner.jobs_cv.notify_all();
        }
        self.inner.kick_ticker();
        self.inner.wake_bus();
        let handles: Vec<_> = self.inner.threads.lock().unwrap().drain(..).collect();
        for h in handles {
            let _ = h.join();
        }
    }

    // ---------------- 调试 / 验证 ----------------

    /// 每条 BUFFERING 报上来的百分比：`元素名=百分比`（检查工具查"卡在谁身上"用）。
    pub fn debug_buffer_pct(&self) -> Vec<String> {
        let run = self.inner.run.lock().unwrap().clone();
        let Some(run) = run else { return Vec::new() };
        let pct: Vec<String> = run
            .buffer_pct
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        pct
    }

    /// 每条 BUFFERING 的 (元素名, 百分比) 原始数据（检查工具用）。
    pub fn debug_buffer_pairs(&self) -> Vec<(String, i32)> {
        let run = self.inner.run.lock().unwrap().clone();
        let Some(run) = run else { return Vec::new() };
        let out: Vec<(String, i32)> = run.buffer_pct.lock().unwrap().iter().map(|(k, v)| (k.clone(), *v)).collect();
        out
    }

    pub fn debug_elements(&self) -> Vec<String> {
        let Some(run) = self.inner.run.lock().unwrap().clone() else { return Vec::new() };
        iterate_elements(&run.pipeline)
            .into_iter()
            .filter_map(|e| e.factory().map(|f| f.name().to_string()))
            .collect()
    }

    /// 音频 sink 收到的格式（验证用）：采样率和解码出来的一样，说明变速靠 scaletempo 拉伸、
    /// 没有靠改采样率（那样会变调）。
    pub fn debug_audio_caps(&self) -> Option<String> {
        let run = self.inner.run.lock().unwrap().clone()?;
        let sink = run.audio_sink.lock().unwrap().clone()?;
        sink.static_pad("sink")?.current_caps().map(|c| c.to_string())
    }

    /// 视频链路上各点的实际 caps（验证用）：解码器输出（进 vapostproc 之前）、vapostproc 输出、
    /// appsink 收到。解码器到 vapostproc 之间应该是 `video/x-raw(memory:VAMemory)`；
    /// 如果那里就是普通 `video/x-raw`，说明解码出来就先拷了一份，多花一次全帧拷贝。
    pub fn debug_video_caps(&self) -> Option<String> {
        let run = self.inner.run.lock().unwrap().clone()?;
        let dec_out = run
            .video_sink_pad
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|p| p.current_caps())
            .map(|c| c.to_string())
            .unwrap_or_default();
        let conv_out = run
            .video_filter
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|e| e.static_pad("src"))
            .and_then(|p| p.current_caps())
            .map(|c| c.to_string())
            .unwrap_or_default();
        let sink_in = run
            .video_sink
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.static_pad("sink"))
            .and_then(|p| p.current_caps())
            .map(|c| c.to_string())
            .unwrap_or_default();
        Some(format!(
            "解码器→vapostproc=[{dec_out}] vapostproc→capsfilter=[{conv_out}] →appsink=[{sink_in}]"
        ))
    }

    /// 管线里所有 `va*` 元素和它们的 `device-path`（验证硬件解码一定在选定的那块核显上）。
    pub fn debug_va_devices(&self) -> Vec<String> {
        let Some(run) = self.inner.run.lock().unwrap().clone() else { return Vec::new() };
        iterate_elements(&run.pipeline)
            .into_iter()
            .filter_map(|e| {
                let f = e.factory()?;
                let name = f.name().to_string();
                if !name.starts_with("va") {
                    return None;
                }
                // `device-path` 是只读的属性；老版本可能没有它
                let path: Option<String> = e.property::<Option<String>>("device-path");
                Some(format!("{name}={}", path.unwrap_or_default()))
            })
            .collect()
    }

    /// 音频 sink 和视频 sink 各自此刻渲染到的媒体位置（毫秒）；差越小音画越同步。
    pub fn debug_sink_positions_ms(&self) -> Option<(i64, i64)> {
        let run = self.inner.run.lock().unwrap().clone()?;
        let a = run.audio_sink.lock().unwrap().as_ref()?.query_position::<gst::ClockTime>()?;
        let v = run.video_sink.lock().unwrap().as_ref()?.query_position::<gst::ClockTime>()?;
        Some((a.mseconds() as i64, v.mseconds() as i64))
    }

    /// 当前管线的时钟名（`"GstSystemClock"` / 声卡 sink 的名字）。检查工具验
    /// "设了 no-audio-clock 之后管线不再被声卡抢走时钟"用。
    pub fn debug_clock_name(&self) -> Option<String> {
        let run = self.inner.run.lock().unwrap().clone()?;
        run.pipeline.clock().map(|c| c.name().to_string())
    }

    /// 当前管线名（测量用：查位置、时钟、延迟）。
    pub fn debug_pipeline_name(&self) -> Option<String> {
        self.inner.run.lock().unwrap().clone().map(|r| r.pipeline.name().to_string())
    }

    /// 引擎内部的那几个标志（排障用）。
    ///
    /// **只读原子 / 普通 Mutex，不调任何 GStreamer 查询**：`Element::current_state()` 这一类
    /// 在管线正被拆的时候会等 GStreamer 的流锁，从别的线程调就可能挂住。
    pub fn debug_flags(&self) -> Option<EngineFlags> {
        let run = self.inner.run.lock().unwrap().clone()?;
        let flags = EngineFlags {
            want_playing: run.want_playing.load(Ordering::Acquire),
            prerolled: run.prerolled.load(Ordering::Acquire),
            seeking: run.seeking.load(Ordering::Acquire),
            buffering: run.buffering(),
            buffer_pct: run
                .buffer_pct
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            closed: run.closed.load(Ordering::Acquire),
            target_state: *run.target_state.lock().unwrap(),
            got_frame: run.got_frame.load(Ordering::Acquire),
        };
        Some(flags)
    }
}

/// 引擎内部标志的快照（排障用，见 [`Engine::debug_flags`]）。
#[derive(Debug, Clone, PartialEq)]
pub struct EngineFlags {
    pub want_playing: bool,
    pub prerolled: bool,
    pub seeking: bool,
    pub buffering: bool,
    pub buffer_pct: Vec<(String, i32)>,
    pub closed: bool,
    pub target_state: gst::State,
    pub got_frame: bool,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // 只在最后一份句柄销毁时真收尾
        if Arc::strong_count(&self.inner) <= 2 {
            self.release();
        }
    }
}

impl EngineInner {
    fn hardware_enabled(&self) -> bool {
        self.cfg.hardware && std::env::var("FEEDHUB_HWDEC").as_deref() != Ok("0")
    }

    fn exec<F: FnOnce(&Arc<EngineInner>) + Send + 'static>(self: &Arc<Self>, job: F) {
        if self.released.load(Ordering::Acquire) {
            return;
        }
        self.push_job(Box::new(job));
    }

    fn push_job(&self, job: Box<dyn FnOnce(&Arc<EngineInner>) + Send>) {
        let mut q = self.jobs.lock().unwrap();
        q.push(Job(job));
        drop(q);
        self.jobs_cv.notify_one();
        self.kick_ticker();
        self.wake_bus();
    }

    /// 叫醒 ticker 重新看 [`Self::should_tick`]。**持锁再 notify**：ticker 在锁里查完条件才睡，
    /// 不持锁的话"它刚查完、还没睡"那一下的通知会丢，播放起来以后位置永远不再更新。
    fn kick_ticker(&self) {
        let _guard = self.stats_wake.0.lock().unwrap();
        self.stats_wake.1.notify_all();
    }

    /// 往当前管线总线投一条空唤醒消息，让 `bus.timed_pop` 立刻返回去取 `jobs`（避免 10ms 轮询）。
    fn wake_bus(&self) {
        let run = self.run.lock().unwrap().clone();
        if let Some(run) = run {
            if let Some(bus) = run.bus.as_ref() {
                let msg = gst::message::Application::new(gst::Structure::new_empty("feedhub-wake"));
                let _ = bus.post(msg);
            }
        }
    }

    /// 当前是否需要每 250ms 推进位置 / 缓冲 / 打开与卡死超时检查。
    /// 没有管线，或者已预卷完毕且处于暂停、无 seek、无缓冲时完全挂起，零唤醒。
    fn should_tick(&self) -> bool {
        let Some(run) = self.run.lock().unwrap().clone() else {
            return false;
        };
        if self.released.load(Ordering::Acquire) || run.closed.load(Ordering::Acquire) {
            return false;
        }
        run.want_playing.load(Ordering::Acquire)
            || !run.prerolled.load(Ordering::Acquire)
            || run.seeking.load(Ordering::Acquire)
            || run.buffering()
            || self.settle_ticks.load(Ordering::Acquire) > 0
    }

    /// 让 ticker 再跑 [`SETTLE_TICKS`] 拍（见 `settle_ticks`）。
    fn settle(&self) {
        self.settle_ticks.store(SETTLE_TICKS, Ordering::Release);
        self.kick_ticker();
    }

    fn start_threads(self: Arc<Self>) {
        // 控制线程：串行执行所有可能阻塞的 GStreamer 调用
        {
            let inner = self.clone();
            let h = std::thread::Builder::new()
                .name("gst-control".into())
                .spawn(move || loop {
                    let stopped = inner.stopping.load(Ordering::Acquire);
                    let job = {
                        let mut q = inner.jobs.lock().unwrap();
                        q.pop()
                    };
                    if let Some(Job(f)) = job {
                        f(&inner);
                        // 一个任务跑完就顺手把总线上的消息清一遍（很多消息正是这个任务引起的）
                        inner.drain_bus();
                        // ticker 判断的标志（want_playing / seeking / prerolled / 缓冲）都在本线程改，改完叫它看一眼
                        inner.kick_ticker();
                        continue;
                    }
                    if stopped {
                        // 退出前把剩下的消息和任务都处理掉，别丢 ERROR / EOS
                        inner.drain_bus();
                        let more = inner.jobs.lock().unwrap().pop();
                        if let Some(Job(f)) = more {
                            f(&inner);
                            continue;
                        }
                        break;
                    }
                    // 空闲：无管线时在 Condvar 上挂起（零唤醒）；有管线时等总线消息或 wake_bus 唤醒
                    inner.wait_and_drain();
                })
                .expect("起不了 gst-control 线程");
            self.threads.lock().unwrap().push(h);
        }
        // ticker：只在播放 / 预卷 / 缓冲 / seek 期间每 250ms 报一次位置 / 缓冲，暂停和空闲时挂起
        {
            let inner = self.clone();
            let h = std::thread::Builder::new()
                .name("gst-stats".into())
                .spawn(move || {
                    while !inner.stopping.load(Ordering::Acquire) {
                        if !inner.should_tick() {
                            let guard = inner.stats_wake.0.lock().unwrap();
                            let guard = inner
                                .stats_wake
                                .1
                                .wait_while(guard, |_| {
                                    !inner.stopping.load(Ordering::Acquire) && !inner.should_tick()
                                })
                                .unwrap();
                            drop(guard);
                            continue;
                        }
                        {
                            let guard = inner.stats_wake.0.lock().unwrap();
                            // 别的通知（每个任务跑完都会 kick）不缩短间隔，只有停机才提前醒
                            let (guard, _) = inner
                                .stats_wake
                                .1
                                .wait_timeout_while(guard, TICK_INTERVAL, |_| !inner.stopping.load(Ordering::Acquire))
                                .unwrap();
                            drop(guard);
                        }
                        if inner.stopping.load(Ordering::Acquire) {
                            break;
                        }
                        if !inner.should_tick() {
                            continue;
                        }
                        // **只排队，不在本线程碰管线**：
                        // 原 Kotlin 的 ticker 是 `ticker.scheduleWithFixedDelay { tick() }`，而
                        // `tick()` 里有一半是 GStreamer 查询（`query_position` / `query_duration` /
                        // `set_state`）。这些东西**必须和控制线程串行**——两边同时查询 / 设状态，
                        // GStreamer 的流锁和我们的 `run` / `snapshot` 锁会形成环，实测两个线程
                        // 一起卡在 futex 上、进程不退出也不报错。所以 ticker 只负责"到点了排一个
                        // 任务"，真正干活的是控制线程（和其余所有 GStreamer 调用同一条线）。
                        let _ = inner.settle_ticks.fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_sub(1));
                        inner.exec(|inner: &Arc<EngineInner>| inner.tick());
                    }
                })
                .expect("起不了 gst-stats 线程");
            self.threads.lock().unwrap().push(h);
        }
    }

    // ---------------- open / 建管线 ----------------

    fn start_current(self: &Arc<Self>) {
        let Some(spec) = self.current.lock().unwrap().clone() else { return };
        if self.released.load(Ordering::Acquire) {
            return;
        }
        self.teardown(false);
        let attempt = self.attempt.load(Ordering::Acquire) as usize;
        let Some(v) = spec.video_urls.get(attempt).cloned() else { return };
        let a = spec
            .audio_urls
            .get(attempt)
            .or_else(|| spec.audio_urls.last())
            .cloned();
        let use_hw = self.hardware_enabled() && !self.software_fallback.load(Ordering::Acquire);
        self.rt.set_hardware(use_hw);

        // 共享帧内存：没有就按 1080p 先建一块；第一帧到了发现不够会重建
        let shm = {
            let mut guard = self.shm.lock().unwrap();
            if guard.is_none() {
                *guard = FrameShm::new(1920, 1080).ok();
            }
            guard.clone()
        };
        let Some(shm) = shm else {
            self.set_error("播放失败：分配不到帧缓冲".to_string());
            return;
        };

        let start_ms = self.pending_start_ms.load(Ordering::Acquire);
        let pipeline = gst::Pipeline::with_name(&format!(
            "feedhub-{}",
            PIPELINE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let bus = pipeline.bus();
        let run = Arc::new(Run {
            spec: spec.clone(),
            use_hardware: use_hw,
            start_ms,
            want_playing: AtomicBool::new(self.auto_play.load(Ordering::Acquire)),
            pipeline,
            bus,
            started_at: Instant::now(),
            prerolled: AtomicBool::new(false),
            seeking: AtomicBool::new(false),
            seek_quiet: AtomicBool::new(false),
            // 预卷完成后要先跳到的位置（续播 / 预卷期间用户拖了进度条 / 不是 1 倍速）；-1 = 不用跳
            seek_after_preroll: AtomicI64::new(if start_ms > 1000 { start_ms } else { -1 }),
            rate: Mutex::new(1.0),
            closed: AtomicBool::new(false),
            target_state: Mutex::new(gst::State::Null),
            buffer_pct: Mutex::new(HashMap::new()),
            buffering_since: Mutex::new(None),
            video_linked: AtomicBool::new(false),
            video_ready: AtomicBool::new(false),
            waiting_video_since: Mutex::new(None),
            video_wait_timeout_logged: AtomicBool::new(false),
            audio_linked: AtomicBool::new(false),
            volume: Mutex::new(None),
            video_filter: Mutex::new(None),
            video_sink: Mutex::new(None),
            audio_sink: Mutex::new(None),
            video_sink_pad: Mutex::new(None),
            signal_handlers: Mutex::new(Vec::new()),
            source_w: AtomicI64::new(0),
            source_h: AtomicI64::new(0),
            target_w: AtomicI64::new(0),
            target_h: AtomicI64::new(0),
            got_frame: AtomicBool::new(false),
            no_pts_frames: AtomicU64::new(0),
            frame_ns: AtomicI64::new(0),
            last_frame_ns: AtomicI64::new(0),
            pace_frames: AtomicU64::new(0),
            pace_late: AtomicU64::new(0),
            pace_codec: Mutex::new(None),
            pace_codec_resolved: AtomicBool::new(false),
            frame_hook: Mutex::new(self.pending_frame_hook.lock().unwrap().clone()),
            audio_hook: Mutex::new(self.pending_audio_hook.lock().unwrap().clone()),
            surface_size: Mutex::new(*self.pending_surface.lock().unwrap()),
            shm,
        });
        *self.run.lock().unwrap() = Some(run.clone());
        self.kick_ticker();

        if let Err(e) = self.build_pipeline(&run, &spec, &v, a.as_deref()) {
            eprintln!("gst: 建管线失败：{e}");
            self.fail_over(&run, format!("建管线失败：{e}"));
            return;
        }
        *run.target_state.lock().unwrap() = gst::State::Paused;
        let _ = run.pipeline.set_state(gst::State::Paused);
    }

    fn build_pipeline(
        self: &Arc<Self>,
        run: &Arc<Run>,
        spec: &MediaSpec,
        video_uri: &str,
        audio_uri: Option<&str>,
    ) -> Result<(), String> {
        // 总线：**不打 watch、不挂 sync handler**，由控制线程自己 `timed_pop` 取出消息
        // （见 [`EngineInner::drain_bus`]）。
        //
        // 两条路都不能走：
        // - `add_watch_local` 要一个 GLib 线程默认 `MainContext` 在跑主循环，而且返回的
        //   `BusWatchGuard` **必须在创建它的线程上 drop**；我们是在控制线程建、在 `release()`
        //   里 drop，GStreamer 当场 `thread caused non-unwinding panic`，整个进程 SIGABRT。
        // - `set_sync_handler` 没有 guard 问题，但它是**从 `set_state` 的调用栈里同步重入**的
        //   （GStreamer 在设状态时直接投递消息），而我们正在 `apply_target` 里持着
        //   `target_state` 锁调 `set_state`——于是控制线程自己重入 `apply_target`，
        //   自己等自己的锁，**所有线程一起睡死**。
        //
        // 引擎本来就没有 GLib 主循环（控制线程就是循环），所以直接把消息从总线取出来处理
        // 才是对的结构：不重入、不要主循环、也不依赖"drop 在哪个线程"。
        if run.bus.is_none() {
            return Err("管线没有总线".to_string());
        }

        let mut uris: Vec<&str> = vec![video_uri];
        if let Some(a) = audio_uri {
            uris.push(a);
        }
        for uri in uris {
            let d = gst::ElementFactory::make("uridecodebin3")
                .build()
                .map_err(|e| format!("建不出 uridecodebin3：{e}"))?;
            d.set_property("uri", uri);
            d.set_property("use-buffering", self.use_buffering.load(Ordering::Acquire));
            // source-setup：给 souphttpsrc 设请求头 / 代理
            {
                let inner = self.clone();
                let spec_c = spec.clone();
                let uri_c = uri.to_string();
                let id = d.connect("source-setup", false, move |values| {
                    if let Some(src) = values[1].get::<gst::Element>().ok() {
                        inner.setup_source(&spec_c, &src, &uri_c);
                    }
                    None
                });
                run.signal_handlers.lock().unwrap().push((d.clone(), id));
            }
            // pad-added：接视频 / 音频链
            {
                let inner = self.clone();
                let run_c = run.clone();
                let id = d.connect_pad_added(move |_, pad| {
                    inner.on_pad_added(&run_c, pad);
                });
                run.signal_handlers.lock().unwrap().push((d.clone(), id));
            }
            run.pipeline
                .add(&d)
                .map_err(|e| format!("把 uridecodebin3 加进管线失败：{e}"))?;
        }
        Ok(())
    }

    /// 给 uridecodebin3 里**直接取远端**的那个 souphttpsrc 带请求头（B站要 Referer 和 UA）
    /// 和这个平台的代理。
    ///
    /// DASH / HLS 不用这条：它们的清单和分片都经中转走 OkHttp，`source-setup` 拿到的是取
    /// **本机清单**的 souphttpsrc，请求头由中转加（`spec.relayed`）。
    fn setup_source(&self, spec: &MediaSpec, source: &gst::Element, _uri: &str) {
        if source.factory().map(|f| f.name().to_string()).as_deref() != Some("souphttpsrc") {
            return;
        }
        let headers: &[(String, String)] = if spec.relayed { &[] } else { &spec.headers };
        if let Some((_, ua)) = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("User-Agent")) {
            source.set_property("user-agent", ua);
        }
        let extra: Vec<&(String, String)> = headers
            .iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("User-Agent"))
            .collect();
        if !extra.is_empty() {
            // extra-headers 是 GstStructure：名字 `extra-headers` + 每个头一个 `(string)"值"`
            let text = format!(
                "extra-headers, {}",
                extra
                    .iter()
                    .map(|(k, v)| format!("{k}=(string)\"{}\"", escape_structure_value(v)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            match text.parse::<gst::Structure>() {
                Ok(s) => source.set_property("extra-headers", s),
                Err(e) => eprintln!("gst: extra-headers 拼不出来（{text}）：{e}"),
            }
        }
        source.set_property("timeout", 15u32);
        // 视频流也走这个平台的代理（按平台指定代理）：本机回环的地址设空串直连，
        // 指定代理没填全给必然连不上的替身（fail-closed）。
        // 「直连」和 SOCKS5 在宿主那边就改走中转了，到不了这里。
        if let Some(proxy) = &spec.proxy_uri {
            // 很老的 souphttpsrc 没有这个属性：有才设
            if source.find_property("proxy").is_some() {
                source.set_property("proxy", proxy.as_str());
            } else {
                eprintln!("gst: 这个 souphttpsrc 没有 proxy 属性，视频流这一路没法走代理");
            }
        }
    }

    // ---------------- 管线接线 ----------------

    fn on_pad_added(self: &Arc<Self>, run: &Arc<Run>, pad: &gst::Pad) {
        if run.closed.load(Ordering::Acquire) {
            return;
        }
        let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None)));
        let Some(caps) = caps else { return };
        let Some(st) = caps.structure(0) else { return };
        let name = st.name().to_string();
        if name.starts_with("video/") {
            self.build_video_chain(run, pad);
        } else if name.starts_with("audio/") {
            self.build_audio_chain(run, pad);
        }
    }

    fn build_video_chain(self: &Arc<Self>, run: &Arc<Run>, pad: &gst::Pad) {
        if run.video_linked.swap(true, Ordering::AcqRel) {
            return;
        }
        // 这是检查工具专用的一条，产品路径 `raw_sink` / `raw_sink_scaled` 恒为 false。
        if self.cfg.raw_sink || self.cfg.raw_sink_scaled {
            self.build_video_chain_raw(run, pad);
            return;
        }
        let use_va = run.use_hardware && self.rt.has_va_postproc();
        let conv_name = if use_va {
            "vapostproc"
        } else if runtime::find_factory("videoconvertscale").is_some() {
            "videoconvertscale"
        } else {
            "videoconvert"
        };
        let conv = match gst::ElementFactory::make(conv_name).build() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("gst: 建不出 {conv_name}：{e}");
                return;
            }
        };
        let filter = match gst::ElementFactory::make("capsfilter").build() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("gst: 建不出 capsfilter：{e}");
                return;
            }
        };
        filter.set_property(
            "caps",
            gst::Caps::builder("video/x-raw").field("format", "BGRA").build(),
        );
        let sink_max = *self.pending_sink_buffers.lock().unwrap();
        let sink = match make_video_appsink(sink_max) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("gst: 建不出视频 appsink：{e}");
                return;
            }
        };
        // appsink 回调：新样 / 预卷样都交给 on_sample
        {
            let inner = self.clone();
            let run_c = run.clone();
            let inner2 = self.clone();
            let run_c2 = run.clone();
            let cb = gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    if let Ok(sample) = sink.pull_sample() {
                        inner.on_sample(&run_c, &sample);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .new_preroll(move |sink| {
                    if let Ok(sample) = sink.pull_preroll() {
                        inner2.on_sample(&run_c2, &sample);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build();
            sink.set_callbacks(cb);
        }

        let sink_el: gst::Element = sink.clone().upcast();
        if run.pipeline.add_many([&conv, &filter, &sink_el]).is_err() {
            eprintln!("gst: 视频元素加不进管线");
            return;
        }
        if gst::Element::link_many([&conv, &filter, &sink_el]).is_err() {
            eprintln!("gst: 视频链接不起来");
            return;
        }
        for e in [&conv, &filter, &sink_el] {
            e.sync_state_with_parent().ok();
        }
        let conv_sink = conv.static_pad("sink");
        *run.video_filter.lock().unwrap() = Some(filter);
        *run.video_sink.lock().unwrap() = Some(sink);
        *run.video_sink_pad.lock().unwrap() = conv_sink.clone();

        // 第一帧进 vapostproc 之前就按源尺寸定好输出大小：不然最先出来的几帧是源尺寸，
        // 硬解时源宽不对齐（竖屏 720、854 这类）就没有 PTS，连着 NO_PTS_LIMIT 帧会被当成
        // "这台机器丢时间戳"退回软解。接线时解码器的 caps 还没定，只能等第一帧。
        if let Some(cs) = conv_sink.clone() {
            let inner = self.clone();
            let run_c = run.clone();
            let done = Arc::new(AtomicBool::new(false));
            cs.add_probe(gst::PadProbeType::BUFFER, move |_pad, _info| {
                if done.swap(true, Ordering::AcqRel) {
                    return gst::PadProbeReturn::Ok;
                }
                if inner.read_source_size(&run_c).is_some() {
                    inner.apply_target_size(&run_c);
                }
                gst::PadProbeReturn::Ok
            });
        }

        if let Some(sink_pad) = conv_sink {
            if let Err(e) = pad.link(&sink_pad) {
                eprintln!("gst: 解码器输出接不到视频链：{e}");
                return;
            }
        }
        run.video_ready.store(true, Ordering::Release);
    }

    /// **量吞吐上限用的视频链**：解码器输出直接接
    /// `fakesink sync=false`，或先 `vapostproc ! capsfilter(BGRA,WxH)` 再 fakesink（L2）。
    ///
    /// 产品路径不走这里。这条链**不吃时钟**（`sync=false`），所以 CPI / fps 是解码器的上限，
    fn build_video_chain_raw(self: &Arc<Self>, run: &Arc<Run>, pad: &gst::Pad) {
        let pipeline = run.pipeline.clone();
        let sink = match gst::ElementFactory::make("fakesink").build() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("gst: 建不出 fakesink：{e}");
                return;
            }
        };
        sink.set_property("sync", false);
        sink.set_property("qos", false);
        sink.set_property("name", "perfsink");

        let mut chain: Vec<gst::Element> = Vec::new();
        if self.cfg.raw_sink_scaled {
            let (tw, th) = *run.surface_size.lock().unwrap();
            let conv_name = if run.use_hardware && self.rt.has_va_postproc() {
                "vapostproc"
            } else if runtime::find_factory("videoconvertscale").is_some() {
                "videoconvertscale"
            } else {
                "videoconvert"
            };
            let Ok(conv) = gst::ElementFactory::make(conv_name).build() else {
                eprintln!("gst: 建不出 {conv_name}");
                return;
            };
            let Ok(filter) = gst::ElementFactory::make("capsfilter").build() else {
                eprintln!("gst: 建不出 capsfilter");
                return;
            };
            let mut cb = gst::Caps::builder("video/x-raw").field("format", "BGRA");
            if tw > 0 && th > 0 {
                cb = cb.field("width", tw).field("height", th);
            }
            filter.set_property("caps", cb.build());
            chain.push(conv);
            chain.push(filter);
        }
        chain.push(sink.clone());

        if pipeline.add_many(&chain).is_err() {
            eprintln!("gst: L1/L2 链加不进管线");
            return;
        }
        for w in chain.windows(2) {
            if gst::Element::link_many([&w[0], &w[1]]).is_err() {
                eprintln!("gst: L1/L2 链连不起来");
                return;
            }
        }
        let Some(sink_pad) = sink.static_pad("sink") else { return };
        if pad.link(&sink_pad).is_err() {
            // L2 要接到 vapostproc 上，不是直接接 fakesink
            if let Some(first) = chain.first() {
                if let Some(first_sink) = first.static_pad("sink") {
                    let _ = pad.link(&first_sink);
                }
            }
        }
        let _ = sink.sync_state_with_parent();
        for e in &chain {
            let _ = e.sync_state_with_parent();
        }
        run.video_ready.store(true, Ordering::Release);
        self.video_branch_ready(run);
    }

    fn build_audio_chain(self: &Arc<Self>, run: &Arc<Run>, pad: &gst::Pad) {
        if run.audio_linked.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut chain: Vec<gst::Element> = Vec::new();
        let Ok(convert) = gst::ElementFactory::make("audioconvert").build() else {
            eprintln!("gst: 建不出 audioconvert");
            return;
        };
        chain.push(convert.clone());
        // scaletempo：变速时拉伸 / 压缩音频，音调不变（它只吃浮点，前后各一个 audioconvert）
        if runtime::find_factory("scaletempo").is_some() {
            if let Ok(tempo) = gst::ElementFactory::make("scaletempo").build() {
                chain.push(tempo);
            }
        }
        if let Ok(c2) = gst::ElementFactory::make("audioconvert").build() {
            chain.push(c2);
        }
        if let Ok(rs) = gst::ElementFactory::make("audioresample").build() {
            chain.push(rs);
        }
        let Ok(volume) = gst::ElementFactory::make("volume").build() else {
            eprintln!("gst: 建不出 volume");
            return;
        };
        volume.set_property("volume", self.volume.load(Ordering::Acquire) as f64 / 100.0);
        volume.set_property("mute", self.muted.load(Ordering::Acquire));
        chain.push(volume.clone());

        let sink: gst::Element = if self.cfg.debug_audio || self.audio_hook_active.load(Ordering::Acquire) {
            match self.debug_audio_sink(run) {
                Some(s) => s,
                None => return,
            }
        } else if self.cfg.silent || no_display() {
            match gst::ElementFactory::make("fakesink").build() {
                Ok(s) => {
                    s.set_property("sync", true);
                    s
                }
                Err(e) => {
                    eprintln!("gst: 建不出 fakesink：{e}");
                    return;
                }
            }
        } else {
            match audio_output(self.cfg.use_system_clock) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("gst: 建不出 autoaudiosink：{e}");
                    return;
                }
            }
        };
        chain.push(sink.clone());

        let refs: Vec<&gst::Element> = chain.iter().collect();
        if run.pipeline.add_many(refs.iter().copied()).is_err() {
            eprintln!("gst: 音频元素加不进管线");
            return;
        }
        if gst::Element::link_many(refs.iter().copied()).is_err() {
            eprintln!("gst: 音频链接不起来");
            return;
        }
        for e in &chain {
            e.sync_state_with_parent().ok();
        }
        *run.volume.lock().unwrap() = Some(volume);
        *run.audio_sink.lock().unwrap() = Some(sink);
        if let Some(sink_pad) = convert.static_pad("sink") {
            if let Err(e) = pad.link(&sink_pad) {
                eprintln!("gst: 解码器输出接不到音频链：{e}");
            }
        }
    }

    /// 测量用的音频 sink：appsink（`sync=true`），收到的缓冲连同到达时的管线运行时间交给 hook。
    fn debug_audio_sink(self: &Arc<Self>, run: &Arc<Run>) -> Option<gst::Element> {
        // builder 的 `build()` 不是 Result（属性名 / 类型不对是 panic）；这里只建不出来元素才失败，
        // 所以提前问一下工厂在不在。
        if runtime::find_factory("appsink").is_none() {
            eprintln!("gst: 没有 appsink 元素，测量用的音频 sink 建不出来");
            return None;
        }
        let sink = gst_app::AppSink::builder()
            .name("audiosink")
            .max_buffers(4)
            .sync(true)
            .caps(
                &gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("layout", "interleaved")
                    .build(),
            )
            .build();
        let inner = self.clone();
        let run_c = run.clone();
        let cb = gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                if let Ok(sample) = s.pull_sample() {
                    inner.on_audio_sample(&run_c, &sample);
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build();
        sink.set_callbacks(cb);
        Some(sink.upcast())
    }

    // ---------------- 帧 ----------------

    fn on_sample(self: &Arc<Self>, run: &Arc<Run>, sample: &gst::Sample) {
        if run.closed.load(Ordering::Acquire) || self.released.load(Ordering::Acquire) {
            return;
        }
        let Some(caps) = sample.caps() else { return };
        let Some(st) = caps.structure(0) else { return };
        let width: i32 = st.get("width").unwrap_or(0);
        let height: i32 = st.get("height").unwrap_or(0);
        if width <= 0 || height <= 0 {
            return;
        }
        let Some(buffer) = sample.buffer() else { return };
        let pts = buffer.pts().map(|t| t.nseconds() as i64).unwrap_or(-1);

        // 没有 PTS 的帧 appsink 不等时钟，画面会跑到声音前面。对齐宽度之后不该再出现；
        // 万一这台机器的 VA 输出还是丢时间戳，退回软解。
        if run.use_hardware && self.cfg.align_width && pts < 0 {
            let n = run.no_pts_frames.fetch_add(1, Ordering::AcqRel) + 1;
            if n == NO_PTS_LIMIT {
                self.exec(move |inner| {
                    if !inner.software_fallback.load(Ordering::Acquire) {
                        eprintln!("gst: 硬解输出的帧没有时间戳，画面会跟声音对不上，退回软解");
                        inner.software_fallback.store(true, Ordering::Release);
                        inner.stats.lock().unwrap().software_fallbacks += 1;
                        let now = inner.snapshot.lock().unwrap().position_ms;
                        inner.pending_start_ms.store(now, Ordering::Release);
                        inner.start_current();
                    }
                });
            }
        } else {
            run.no_pts_frames.store(0, Ordering::Release);
        }

        // 帧的 stride / offset 从 GstVideoMeta 读（GPU 下载出来的帧每行可能比 宽×4 宽）。
        let Some(frame) = read_video_frame(buffer, width, height) else { return };

        // 硬解源分辨率可能和画布不一样：共享内存按源尺寸给
        self.ensure_shm_for(width, height);

        let running = running_ns_pipeline(&run.pipeline);
        if let Some(hook) = run.frame_hook.lock().unwrap().clone() {
            // 3×3 个点的亮度取最小：整屏全白才算亮（测试视频的"闪"），画面里有亮色块不会误判
            let luma = compute_luma(&frame.bytes, frame.offset, width, height, frame.stride);
            hook(FrameStamp {
                pts_ns: pts,
                running_ns: running,
                luma,
                width,
                height,
            });
        }
        self.stats.lock().unwrap().frames_received += 1;

        self.track_pace(run, width, height, st);

        // 写进共享内存
        {
            let start = frame.offset.min(frame.bytes.len());
            let end = (frame.offset + frame.stride * height as usize).min(frame.bytes.len());
            run.shm
                .write_frame(&frame.bytes[start..end], width as u32, height as u32, frame.stride as u32, pts);
            self.frame_seq.store(run.shm.frame_seq(), Ordering::Release);
        }

        if !run.got_frame.swap(true, Ordering::AcqRel) {
            let run_c = run.clone();
            self.exec(move |inner| {
                if run_c.closed.load(Ordering::Acquire) {
                    return;
                }
                // 管线可能在视频支路还没接好时就预卷完了（先到的是音频）：第一帧到了再补一次
                // 解码器 / 尺寸的检查
                inner.detect_decoder(&run_c);
                inner.read_source_size(&run_c);
                inner.apply_target_size(&run_c);
            });
        }
        {
            let mut snap = self.snapshot.lock().unwrap();
            snap.has_frame = true;
            snap.frames = run.shm.frames_written();
        }
    }

    fn on_audio_sample(&self, run: &Arc<Run>, sample: &gst::Sample) {
        let Some(caps) = sample.caps() else { return };
        let Some(st) = caps.structure(0) else { return };
        let rate: i32 = st.get("rate").unwrap_or(0);
        let channels: i32 = st.get("channels").unwrap_or(0);
        if rate <= 0 || channels <= 0 {
            return;
        }
        let Some(buffer) = sample.buffer() else { return };
        let pts = buffer.pts().map(|t| t.nseconds() as i64).unwrap_or(-1);
        let Some(map) = buffer.map_readable().ok() else { return };
        let data = map.as_slice().to_vec();
        let running = running_ns_pipeline(&run.pipeline);
        if let Some(hook) = run.audio_hook.lock().unwrap().clone() {
            hook(AudioStamp {
                pts_ns: pts,
                running_ns: running,
                data,
                rate,
                channels,
            });
        }
    }

    /// 共享内存如果装不下这一帧（源比开播时估的大），重建一块。
    fn ensure_shm_for(&self, width: i32, height: i32) {
        let need = width as usize * 4 * height as usize;
        let mut guard = self.shm.lock().unwrap();
        let too_small = match guard.as_ref() {
            Some(s) => (s.slot_capacity() as usize) < need,
            None => true,
        };
        if !too_small {
            return;
        }
        if let Ok(s) = FrameShm::new(width as u32, height as u32) {
            *guard = Some(s);
        }
    }

    /// 出帧节奏跟踪：播放中每帧到达的间隔比标称间隔晚太多，说明这种编码在这个负载下跟不上
    /// （这台机器 4K60 的 AV1 就是这样，HEVC / AVC 不会）。每 [`PACE_WINDOW`] 帧结算一次，
    /// 迟到超过 [`PACE_MAX_LATE`] 帧就写进 [`DecodeLedger`]，下一次"自动"选编码会避开它。
    /// 缓冲 / 跳转期间不算（那是取流的事，不是解码的锅）。
    fn track_pace(&self, run: &Arc<Run>, w: i32, h: i32, st: &gst::StructureRef) {
        if run.frame_ns.load(Ordering::Acquire) <= 0 {
            let fps = st
                .get::<gst::Fraction>("framerate")
                .ok()
                .map(|f| f.numer() as f64 / f.denom().max(1) as f64);
            let Some(fps) = fps else { return };
            if fps <= 0.0 {
                return;
            }
            run.frame_ns.store((1_000_000_000.0 / fps) as i64, Ordering::Release);
        }
        let now = now_ns();
        let last = run.last_frame_ns.swap(now, Ordering::AcqRel);
        if last == 0
            || !run.want_playing.load(Ordering::Acquire)
            || self.snapshot.lock().unwrap().buffering
            || run.seeking.load(Ordering::Acquire)
        {
            return;
        }
        let frame_ns = run.frame_ns.load(Ordering::Acquire);
        let frames = run.pace_frames.fetch_add(1, Ordering::AcqRel) + 1;
        if now - last > frame_ns * 3 / 2 {
            run.pace_late.fetch_add(1, Ordering::AcqRel);
        }
        if frames >= PACE_WINDOW {
            if !run.pace_codec_resolved.swap(true, Ordering::AcqRel) {
                // 源没标编码（音画一体的老流）就从解码器元素名推：vah265dec / avdec_av1 …
                let codec = run.spec.codec.or_else(|| infer_codec_from_pipeline(&run.pipeline));
                *run.pace_codec.lock().unwrap() = codec;
            }
            let codec = *run.pace_codec.lock().unwrap();
            // 负载要按**源分辨率**算（解码器真正在解的那个尺寸），不是 vapostproc 缩放之后
            // 交出来的尺寸——选流那一头也是拿 VideoStream 的 width×height 算的，两边必须一致。
            let sw = {
                let v = run.source_w.load(Ordering::Acquire);
                if v > 0 { v } else { w as i64 }
            };
            let sh = {
                let v = run.source_h.load(Ordering::Acquire);
                if v > 0 { v } else { h as i64 }
            };
            let late = run.pace_late.load(Ordering::Acquire);
            if let Some(codec) = codec {
                let pixels_per_second = sw * sh * 1_000_000_000 / frame_ns.max(1);
                self.record_decode(codec, pixels_per_second, late <= PACE_MAX_LATE);
            }
            run.pace_frames.store(0, Ordering::Release);
            run.pace_late.store(0, Ordering::Release);
        }
    }

    /// 记一次解码实测（账本由宿主注入；没有就只记账不落盘）。
    fn record_decode(&self, codec: VideoCodec, pixels_per_second: i64, smooth: bool) {
        let hook = self.ledger.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(codec, pixels_per_second.max(0), smooth, now_ms());
        }
    }

    // ---------------- 总线 ----------------

    /// 从当前管线的总线上取消息并处理。**只在控制线程上调用**（见 `start_threads`）。
    fn drain_bus(self: &Arc<Self>) {
        let Some(run) = self.run.lock().unwrap().clone() else { return };
        let Some(bus) = run.pipeline.bus() else { return };
        // `timed_pop` 拿完为止（上限防死循环：管线被拆的时候消息可能一直在来）
        for _ in 0..256 {
            let Some(msg) = bus.timed_pop(gst::ClockTime::ZERO) else { break };
            if run.closed.load(Ordering::Acquire) {
                break;
            }
            self.on_bus_message(&run, &msg);
        }
    }

    /// 空闲时：没有管线就一直等新任务（零定时唤醒）；有管线就在总线上等消息（新任务靠 `wake_bus` 立刻打断）。
    fn wait_and_drain(self: &Arc<Self>) {
        let Some(run) = self.run.lock().unwrap().clone() else {
            let q = self.jobs.lock().unwrap();
            let guard = self
                .jobs_cv
                .wait_while(q, |q| q.is_empty() && !self.stopping.load(Ordering::Acquire))
                .unwrap();
            drop(guard);
            return;
        };
        // 先看一眼是不是刚放锁就有任务进来了（防 `run` 读取和 `wake_bus` 交错）
        if !self.jobs.lock().unwrap().is_empty() || self.stopping.load(Ordering::Acquire) {
            return;
        }
        if let Some(bus) = run.pipeline.bus() {
            // `wake_bus` 会往 bus 里 post 一条空 Application 消息，新任务能立刻唤醒 `timed_pop`；
            // 留 1s 超时作为极端竞态兜底（每秒 ≤1 次而非 100 次）。
            if let Some(msg) = bus.timed_pop(gst::ClockTime::from_seconds(1)) {
                if !run.closed.load(Ordering::Acquire) {
                    self.on_bus_message(&run, &msg);
                }
                self.drain_bus();
                self.kick_ticker();
            }
        } else {
            let q = self.jobs.lock().unwrap();
            let guard = self
                .jobs_cv
                .wait_while(q, |q| q.is_empty() && !self.stopping.load(Ordering::Acquire))
                .unwrap();
            drop(guard);
        }
    }

    fn on_bus_message(self: &Arc<Self>, run: &Arc<Run>, msg: &gst::Message) {
        use gst::MessageView;
        match msg.view() {
            MessageView::Error(e) => {
                let source = e.src().map(|s| s.name().to_string()).unwrap_or_default();
                let message = format!(
                    "{}（{}）",
                    e.error(),
                    e.debug().map(|d| d.to_string()).unwrap_or_default()
                );
                eprintln!("gst: 错误 {source}: {message}");
                let run_c = run.clone();
                self.exec(move |inner| inner.on_error(&run_c, &source, &message));
            }
            MessageView::Eos(_) => {
                let run_c = run.clone();
                self.exec(move |inner| inner.on_eos(&run_c));
            }
            MessageView::Buffering(e) => {
                let percent = e.percent();
                let name = e.src().map(|s| s.name().to_string()).unwrap_or_default();
                let run_c = run.clone();
                self.exec(move |inner| inner.on_buffering(&run_c, &name, percent));
            }
            MessageView::AsyncDone(_) => {
                // 只认管线自己发的 ASYNC_DONE（子元素的不管）
                let source_name = msg.src().map(|s| s.name().to_string()).unwrap_or_default();
                if source_name != run.pipeline.name().to_string() {
                    return;
                }
                let run_c = run.clone();
                self.exec(move |inner| inner.on_async_done(&run_c));
            }
            _ => {}
        }
    }

    fn on_async_done(&self, run: &Arc<Run>) {
        let Some(cur) = self.run.lock().unwrap().clone() else { return };
        if !Arc::ptr_eq(&cur, run) || run.closed.load(Ordering::Acquire) {
            return;
        }
        self.settle();
        if !run.prerolled.load(Ordering::Acquire) {
            if !self.video_branch_ready(run) {
                return;
            }
            run.prerolled.store(true, Ordering::Release);
            self.detect_decoder(run);
            self.read_source_size(run);
            self.apply_target_size(run);
            if let Some(dur) = self.query_duration_ms(&run.pipeline) {
                if dur > 0 {
                    self.snapshot.lock().unwrap().duration_ms = dur;
                }
            }
            let seek_to = run.seek_after_preroll.load(Ordering::Acquire);
            if seek_to >= 0 {
                run.seek_after_preroll.store(-1, Ordering::Release);
                self.do_seek(run, seek_to, false);
                return;
            }
        } else {
            run.seeking.store(false, Ordering::Release);
            run.seek_quiet.store(false, Ordering::Release);
        }
        self.apply_target(run);
    }

    /// 这个源有视频轨的时候，要等视频支路（appsink）接好、预卷到 PAUSED 才算"预卷好了"。
    ///
    /// 不等会踩到 basesink 的一个竞态：管线可能先只用音频支路预卷完就报 ASYNC_DONE，这时视频链
    /// 才接上，`sync_state_with_parent()` 会把视频 sink 直接切到 PLAYING（走异步预卷那条路），
    /// `have_preroll` 就永远停在真；之后每次 PLAYING→PAUSED 都被当成"已经预卷过"，视频 sink
    /// 不阻塞、接着按时钟把缓冲里那几百毫秒渲染出去（音频停了、界面位置也冻住了），于是暂停时
    /// 画面比 `position()` 超前约 0.6 秒，续播 / 按位置重开都会往回跳。
    ///
    /// 等视频支路接好，sink 会先预卷到 PAUSED（管线随后再发一次 ASYNC_DONE），这时才去播，
    /// `have_preroll` 正常清零。拿不到视频支路（源其实没有视频轨）就在 [`VIDEO_WAIT`]
    /// 之后照常开播，别卡住。
    fn video_branch_ready(&self, run: &Arc<Run>) -> bool {
        if run.video_ready.load(Ordering::Acquire) || run.spec.video_urls.is_empty() {
            return true;
        }
        let mut since = run.waiting_video_since.lock().unwrap();
        match *since {
            None => {
                *since = Some(Instant::now());
                if std::env::var("FEEDHUB_GST_DEBUG").is_ok() {
                    eprintln!("gst: 管线预卷完了但视频支路还没接上，等它接上再播");
                }
                false
            }
            Some(t) => {
                let timed_out = t.elapsed() > VIDEO_WAIT;
                if timed_out
                    && std::env::var("FEEDHUB_GST_DEBUG").is_ok()
                    && !run.video_wait_timeout_logged.swap(true, Ordering::AcqRel)
                {
                    eprintln!("gst: 视频支路一直没接上，按没有视频处理照常开播");
                }
                timed_out
            }
        }
    }

    fn on_buffering(&self, run: &Arc<Run>, source: &str, percent: i32) {
        let Some(cur) = self.run.lock().unwrap().clone() else { return };
        if !Arc::ptr_eq(&cur, run) || run.closed.load(Ordering::Acquire) {
            return;
        }
        let was = run.buffering();
        run.buffer_pct.lock().unwrap().insert(source.to_string(), percent);
        let now_buffering = run.buffering();
        if now_buffering && !was {
            *run.buffering_since.lock().unwrap() = Some(Instant::now());
        }
        if !now_buffering {
            *run.buffering_since.lock().unwrap() = None;
        }
        self.apply_target(run);
    }

    fn on_eos(self: &Arc<Self>, run: &Arc<Run>) {
        let Some(cur) = self.run.lock().unwrap().clone() else { return };
        if !Arc::ptr_eq(&cur, run) || run.closed.load(Ordering::Acquire) {
            return;
        }
        if self.looping.load(Ordering::Acquire)
            && run.prerolled.load(Ordering::Acquire)
            && run.want_playing.load(Ordering::Acquire)
        {
            // 循环（行内视频像 GIF 一样）：同一条管线 flush seek 回开头接着播，不重建管线、
            // 不重新连接；位置立刻归零，进度线跟着回去
            {
                let mut snap = self.snapshot.lock().unwrap();
                snap.position_ms = 0;
                snap.buffered_ms = 0;
            }
            // **循环用 quiet seek**：行内视频循环得像 GIF，中间不该闪一下大播放键 / 转圈。
            // 原来 Kotlin 引擎是 `seekTo(0)` 之后不动 `state.playing`（它只在 `play()`/`pause()`
            // 里写），所以"回到开头"对界面是无感的——这里 quiet seek 是同一个口径。
            self.do_seek(run, 0, true);
            return;
        }
        {
            let mut snap = self.snapshot.lock().unwrap();
            snap.playing = false;
            snap.ended = true;
            snap.buffering = false;
            let d = snap.duration_ms;
            snap.position_ms = d;
            snap.buffered_ms = d;
        }
        run.want_playing.store(false, Ordering::Release);
    }

    fn on_error(self: &Arc<Self>, run: &Arc<Run>, source_name: &str, message: &str) {
        let Some(cur) = self.run.lock().unwrap().clone() else { return };
        if !Arc::ptr_eq(&cur, run)
            || run.closed.load(Ordering::Acquire)
            || self.released.load(Ordering::Acquire)
        {
            return;
        }
        // 硬解 / GPU 处理的元素出错：同一个地址退回软解重来；别的错误换下一个地址
        if run.use_hardware
            && source_name.starts_with("va")
            && !self.software_fallback.load(Ordering::Acquire)
        {
            self.software_fallback.store(true, Ordering::Release);
            self.stats.lock().unwrap().software_fallbacks += 1;
            let now = self.snapshot.lock().unwrap().position_ms;
            self.pending_start_ms.store(now, Ordering::Release);
            self.start_current();
        } else {
            self.fail_over(run, message.to_string());
        }
    }

    /// 这个地址播不了：换下一个备用地址（从当前位置接着播），都不行才报给界面。
    fn fail_over(self: &Arc<Self>, run: &Arc<Run>, reason: String) {
        let Some(cur) = self.run.lock().unwrap().clone() else { return };
        if !Arc::ptr_eq(&cur, run) || self.released.load(Ordering::Acquire) {
            return;
        }
        let Some(spec) = self.current.lock().unwrap().clone() else { return };
        let attempt = self.attempt.load(Ordering::Acquire) as usize;
        if attempt + 1 < spec.video_urls.len() {
            self.attempt.store((attempt + 1) as u64, Ordering::Release);
            if run.prerolled.load(Ordering::Acquire) {
                let now = self.snapshot.lock().unwrap().position_ms;
                self.pending_start_ms.store(now, Ordering::Release);
            }
            self.software_fallback.store(false, Ordering::Release);
            self.stats.lock().unwrap().failovers += 1;
            self.start_current();
        } else {
            eprintln!("gst: 播放失败：{reason}");
            self.set_error("播放失败：连不上视频服务器，或这个视频暂时播不了".to_string());
            self.teardown(true);
        }
    }

    fn set_error(&self, text: String) {
        let mut snap = self.snapshot.lock().unwrap();
        snap.error = Some(text);
        snap.playing = false;
        snap.buffering = false;
    }

    // ---------------- 状态 ----------------

    /// 想播放 + 预卷好了 + 缓冲够了 + 没在跳转 → PLAYING，否则 PAUSED。
    fn apply_target(&self, run: &Arc<Run>) {
        let Some(cur) = self.run.lock().unwrap().clone() else { return };
        if !Arc::ptr_eq(&cur, run) || run.closed.load(Ordering::Acquire) {
            return;
        }
        // **demuxer 的缓冲百分比只管"界面转不转圈"，不拿它改管线状态**。
        //
        // 那个百分比是 `hlsdemux2` / `dashdemux2` 按已播时长平滑出来的水平，**只有管线在跑才爬得上去**：
        // 实测一段 6 秒的 HLS，分片在 0.24 秒里就全推进 decodebin3 了，百分比却停在 99（`unset env` 那轮）
        // 或 55（带 env 代理那轮），管线一停就永远等不到 100——拿它当 PAUSED→PLAYING 的闸门就是
        // "停了就再也起不来"。老 Kotlin 引擎"看起来没事"纯属竞态：它的第一条 BUFFERING 常常比
        // ASYNC_DONE 晚到，`bufferPct` 还是空的（`any{}` 对空集是 false）就先播起来了。
        //
        // 现在的口径：预卷好了就播；缓冲不够时界面照样转圈（`snap.buffering` 还是 demuxer 报的那一份），
        // 但管线**不来回切**状态——数据不够时 basesink 自己会顶住时钟、画面就停在那一帧，
        // 上游恢复之后自然续上。真的彻底没数据还有下面的 `STALL_TIMEOUT` 兜底（换备用地址 / 报错）。
        let play = run.want_playing.load(Ordering::Acquire)
            && run.prerolled.load(Ordering::Acquire)
            && !run.seeking.load(Ordering::Acquire);
        let want = if play { gst::State::Playing } else { gst::State::Paused };
        // **`set_state` 不能在持锁时调**：
        // `set_state` 会等 GStreamer 的流线程，而流线程在我们这边要投递总线消息；
        // 更直接的一条是它可能同步回到 `tick()` → `apply_target()`，而 std 的 Mutex
        // 不可重入——同一个线程第二次 `lock()` 就是自己等自己，进程永远卡在 futex 上。
        //
        // 所以：先在锁里算"要不要改"，放开锁，再调 `set_state`，最后回填。
        // 中间这点窗口没有害处：`target_state` 只是"我们上次要求的目标状态"，
        // 两次并发调用会算出同一个 `want`（输入都是原子量），后调的那次回填的是同一个值。
        let needs_change = {
            let t = run.target_state.lock().unwrap();
            *t != want
        };
        if needs_change {
            if std::env::var("FEEDHUB_GST_DEBUG").is_ok() {
                eprintln!(
                    "gst: apply_target → {want:?}（want_playing={} prerolled={} buffering={} seeking={}）",
                    run.want_playing.load(Ordering::Acquire),
                    run.prerolled.load(Ordering::Acquire),
                    run.buffering(),
                    run.seeking.load(Ordering::Acquire),
                );
            }
            let r = run.pipeline.set_state(want);
            if std::env::var("FEEDHUB_GST_DEBUG").is_ok() {
                eprintln!("gst: set_state({want:?}) → {r:?}");
            }
            *run.target_state.lock().unwrap() = want;
        }
        // 静默 seek（换倍速用不了即时变速时的那次回退）期间：进度**诚实地冻在当前帧**，
        // 弹幕也会跟着停。
        let quiet = run.seeking.load(Ordering::Acquire) && run.seek_quiet.load(Ordering::Acquire);
        {
            let mut snap = self.snapshot.lock().unwrap();
            snap.seeking = run.seeking.load(Ordering::Acquire);
            // **静默 seek 期间对界面报"不缓冲"**：这是"换倍速不转圈"的核心。`buffering` 的
            // 判据是 demuxer 报的百分比 < 100，而倍速变化时 dashdemux2 会短暂报一个低值
            // （它要重新排一下下游的时间轴，
            // 原来 Kotlin 引擎在这一条上写的是 `if (quiet) false else (...)`,这里照搬。
            snap.buffering = if quiet {
                false
            } else {
                !run.prerolled.load(Ordering::Acquire)
                    || run.buffering()
                    || run.seeking.load(Ordering::Acquire)
            };
            snap.playing = play;
        }
    }

    /// 跳转。[quiet] = true 时是**换倍速的回退路径**（即时变速用不了，只能带新速率 flush 一次）：
    /// 界面这一侧照旧报"在播、不缓冲"，不转圈、不冻进度、不停弹幕——不然"回退"本身就是用户
    /// 抱怨的那次重载。画面不会黑：flush 之后 appsink 还留着最后一帧，新的帧到了才换。
    fn do_seek(&self, run: &Arc<Run>, position_ms: i64, quiet: bool) {
        run.seeking.store(true, Ordering::Release);
        run.seek_quiet.store(quiet, Ordering::Release);
        if !quiet {
            let mut snap = self.snapshot.lock().unwrap();
            snap.buffering = true;
            snap.playing = false;
        }
        // quiet seek 期间位置由 `tick` 冻在最后一帧（`seeking` 为真就不更新），
        // 所以界面不会往回跳、也不会冻进度条——和"什么都没发生"一致。
        self.stats.lock().unwrap().flush_seeks += 1;
        let rate = *run.rate.lock().unwrap() as f64;
        let ok = run.pipeline.seek(
            rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            gst::ClockTime::from_mseconds(position_ms.max(0) as u64),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
        if let Err(e) = ok {
            eprintln!("gst: 跳转被拒绝（{position_ms} ms）：{e}");
            run.seeking.store(false, Ordering::Release);
            run.seek_quiet.store(false, Ordering::Release);
            self.apply_target(run);
        }
    }

    /// 只改速率、**不清管线**（GStreamer 1.18+ 的 `GST_SEEK_FLAG_INSTANT_RATE_CHANGE`）：
    /// 位置不动，上游不收到 FLUSH，dashdemux2 不会重新缓冲、音频 PTS 不断，画面停在最后一帧继续放。
    ///
    /// 为什么不用 [`EngineInner::do_seek`]：那次 flush 会让 dashdemux2 从头填
    /// 缓冲——实测换一次倍速 86～101 条 BUFFERING、4 条 reset-time、音频 PTS 有 4～44 处跳变
    /// （含一次 +3.98 s），引擎据此把 `buffering` 置真、管线按 PAUSED，用户看到的就是
    /// "转圈 + 进度冻住 + 弹幕停"。
    ///
    /// 口径：
    /// - `start` / `stop` 必须是 `NONE`（位置不动），
    /// - 不能同时带 `FLUSH`，
    /// - 方向不能反（0.25～4 全是正的，这一条天然满足）。
    ///
    /// 返回 `None` = 这条管线不支持即时变速（老版本 / 某个元素不吃），调用方回退到 flush seek。
    /// 老版本 GStreamer 连这个标志位都不认，会当未知标志忽略掉；所以版本先在
    /// [`Runtime::instant_rate_change`] 里挡一道。
    fn instant_rate_change(&self, run: &Arc<Run>, rate: f32) -> Option<()> {
        // `self` 这里是 `EngineInner`（不是 `Engine`），开关直接在自己身上
        if !self.allow_instant_rate.load(Ordering::Acquire) || !self.rt.instant_rate_change() {
            return None;
        }
        let ok = run.pipeline.seek(
            rate as f64,
            gst::SeekFlags::INSTANT_RATE_CHANGE,
            gst::SeekType::None,
            gst::ClockTime::ZERO,
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
        match ok {
            Ok(()) => Some(()),
            Err(e) => {
                eprintln!("gst: 即时变速调用失败：{e}");
                None
            }
        }
    }

    /// 周期任务：报告位置、估算缓冲、检查卡死。
    fn tick(self: &Arc<Self>) {
        let Some(run) = self.run.lock().unwrap().clone() else { return };
        if self.released.load(Ordering::Acquire) || run.closed.load(Ordering::Acquire) {
            return;
        }
        if run.prerolled.load(Ordering::Acquire) && !run.seeking.load(Ordering::Acquire) {
            let pos = self.query_position_ms(&run.pipeline);
            // 注意：`self.snapshot.lock().unwrap().duration_ms` 这种写法里那个临时
            // `MutexGuard` 的生存期会延长到整个 `if` 表达式的末尾（Rust 的 temporary lifetime
            // extension），于是 `query_duration_ms`（会等 GStreamer 的流锁）就**持着 snapshot 锁**
            // 在跑——另一个线程正好在 `apply_target` 里改 snapshot，两边一起卡住。
            // 必须先把值取出来、guard 丢掉，再去问管线。
            let known_duration = {
                let snap = self.snapshot.lock().unwrap();
                snap.duration_ms
            };
            let dur = if known_duration <= 0 {
                self.query_duration_ms(&run.pipeline).unwrap_or(-1)
            } else {
                -1
            };
            // dashdemux2 每条轨往前缓冲约 10~30 秒；缓冲够的时候按 30 秒估
            let ahead = if run.buffering() { 5_000 } else { 30_000 };
            // 锁只在这个作用域里拿：下面还会调 `on_async_done` / `fail_over`，
            // 它们自己也要拿 `snapshot` / `run`。
            {
                let mut snap = self.snapshot.lock().unwrap();
                if let Some(pos) = pos {
                    if pos >= 0 && !snap.ended {
                        snap.position_ms = pos;
                    }
                }
                if dur > 0 {
                    snap.duration_ms = dur;
                }
                let limit = if snap.duration_ms > 0 { snap.duration_ms } else { i64::MAX };
                // **不能用 `clamp`**：结束那一刻位置会略微超过时长，
                // `clamp` 的 `min > max` 会 panic，而 panic 会把 `snapshot` 那把锁毒掉、之后
                // 每次 `snapshot()` 都跟着 panic。
                // Kotlin 老引擎写的是 `coerceAtMost(limit).coerceAtLeast(position)`——这里照抄那个口径。
                snap.buffered_ms = (snap.position_ms + ahead).min(limit).max(snap.position_ms);
            }
        }
        // 预卷完了但在等视频支路：sink 已经到 PAUSED（在预卷里等 PLAYING）或等太久了，
        // 就按现在的情况接着走。正常路径是视频 sink 预卷完让管线再发一次 ASYNC_DONE，
        // 这里只是兜底。
        if !run.prerolled.load(Ordering::Acquire) {
            let waiting = *run.waiting_video_since.lock().unwrap();
            if let Some(t) = waiting {
                let sink_paused = run
                    .video_sink
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|s| s.current_state() == gst::State::Paused)
                    .unwrap_or(false);
                if sink_paused || t.elapsed() > VIDEO_WAIT {
                    self.on_async_done(&run);
                }
            }
        }
        let now = Instant::now();
        let open_timeout = !run.prerolled.load(Ordering::Acquire) && now - run.started_at > OPEN_TIMEOUT;
        let stalled = {
            let since = *run.buffering_since.lock().unwrap();
            since.map(|t| now - t > STALL_TIMEOUT).unwrap_or(false)
        };
        if open_timeout || stalled {
            let why = if open_timeout { "打开超时" } else { "缓冲超时" };
            // 先把锁放开再调 `fail_over`（它自己会拿 `run` / `current`）
            *run.buffering_since.lock().unwrap() = None;
            self.fail_over(&run, why.to_string());
        }
    }

    /// 找出实际在用的视频解码器：`vah264dec` 等 Hardware 类的是硬解，`avdec_*` 等是软解。
    fn detect_decoder(&self, run: &Arc<Run>) {
        for el in iterate_elements(&run.pipeline) {
            let Some(factory) = el.factory() else { continue };
            let klass = factory.metadata("klass").unwrap_or_default().to_string();
            if klass.contains("Decoder") && klass.contains("Video") {
                let name = factory.name().to_string();
                let hw = klass.contains("Hardware");
                let mut snap = self.snapshot.lock().unwrap();
                snap.decoder = Some(name);
                snap.hardware_decoding = Some(hw);
                return;
            }
        }
    }

    fn read_source_size(&self, run: &Arc<Run>) -> Option<(i32, i32)> {
        let caps = run
            .video_sink_pad
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|p| p.current_caps());
        if std::env::var("FEEDHUB_GST_DEBUG").is_ok() {
            eprintln!("gst: 视频输入 caps = {caps:?}");
        }
        let st = caps?.structure(0)?.to_owned();
        let w: i32 = st.get("width").ok()?;
        let h: i32 = st.get("height").ok()?;
        run.source_w.store(w as i64, Ordering::Release);
        run.source_h.store(h as i64, Ordering::Release);
        if w > 0 && h > 0 {
            let mut snap = self.snapshot.lock().unwrap();
            snap.video_width = w;
            snap.video_height = h;
        }
        Some((w, h))
    }

    /// 让 GPU（vapostproc）直接缩放到画布的像素大小：下载到内存、拷进 Skia 的只有屏幕上要
    /// 显示的那么多像素。
    ///
    /// 不放大（硬解时输出宽度取 [`ALIGN_PX`] 的整数倍，可能比画布 / 源宽多出不到半个
    /// [`ALIGN_PX`]），最宽 [`MAX_FRAME_WIDTH`]；变化不到 4% 不重新协商。
    ///
    /// 硬解时输出宽度必须是 64 的倍数（BGRA 每行 256 字节的整数倍）：不是的话 VA 表面每行带
    /// padding，vapostproc 要求下游认 GstVideoMeta，而 appsink 不认，它就退到"另拷一份进系统
    /// 内存池"的路径，这条路径出来的帧没有 PTS，appsink（sync=true）不等时钟，帧有多快放多快，
    /// 画面跑到声音前面约 0.6~0.7 秒（宽度 480 / 800 / 852 / 1030 都中招，640 / 960 / 1024 不会）。
    /// 输出比例保持源比例，画布按源比例拉伸显示，看不出差别。
    fn apply_target_size(&self, run: &Arc<Run>) {
        let Some(filter) = run.video_filter.lock().unwrap().clone() else { return };
        let sw = run.source_w.load(Ordering::Acquire) as i32;
        let sh = run.source_h.load(Ordering::Acquire) as i32;
        if sw <= 0 || sh <= 0 {
            return;
        }
        let (canvas_w, canvas_h) = *run.surface_size.lock().unwrap();
        let mut fit = (MAX_FRAME_WIDTH as f64 / sw as f64).min(1.0);
        if canvas_w > 0 && canvas_h > 0 {
            fit = fit
                .min(canvas_w as f64 / sw as f64)
                .min(canvas_h as f64 / sh as f64);
        }
        let mut w = ((sw as f64 * fit) as i32).max(16) & !1;
        let mut h = ((sh as f64 * fit) as i32).max(16) & !1;
        if run.use_hardware && self.cfg.align_width {
            w = ((((sw as f64 * fit) / ALIGN_PX as f64).round() as i32) * ALIGN_PX).max(ALIGN_PX);
            h = (((w as f64 * sh as f64 / sw as f64) as i32) & !1).max(16);
        }
        let tw = run.target_w.load(Ordering::Acquire) as i32;
        let th = run.target_h.load(Ordering::Acquire) as i32;
        if tw > 0
            && ((w - tw).abs() as f64) < tw as f64 * 0.04
            && ((h - th).abs() as f64) < th as f64 * 0.04
        {
            return;
        }
        run.target_w.store(w as i64, Ordering::Release);
        run.target_h.store(h as i64, Ordering::Release);
        filter.set_property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", "BGRA")
                .field("width", w)
                .field("height", h)
                .build(),
        );
    }

    /// 拆掉当前这条管线。
    ///
    /// [wait] 决定要不要**等到真的回到 NULL**：
    /// - `release()`用 `true`：那一刻必须把 fd、流线程、sink 全放干净，
    ///   否则开关 200 次就漏 200 条管线的资源。
    /// - `start_current()`（换集 / 换清晰度时重建管线）用 `false`：`set_state(Null)` 只是排队的
    ///   请求，新管线马上就会顶上来，**在这里干等会成为每次开播的固定延迟**（曾经写死等 2 秒，
    ///   结果经中转的播放 40 秒里有一大截耗在这）。资源由下一次 `release()` 统一回收。
    fn teardown(&self, wait: bool) {
        let Some(run) = self.run.lock().unwrap().take() else { return };
        run.closed.store(true, Ordering::Release);
        // **先断开信号处理器**：它们抓着 `Arc<Run>` / `Arc<EngineInner>`，而处理器挂在
        // `uridecodebin3` 上、`uridecodebin3` 在 `Run.pipeline` 里——不先断，`Run → pipeline →
        // element → closure → Arc<Run>` 这个环会让整条管线（连同它 bus 的 socketpair）永不释放。
        for (el, id) in run.signal_handlers.lock().unwrap().drain(..) {
            el.disconnect(id);
        }
        // 摘掉 appsink 的回调（它们同样抓着 `Arc<EngineInner>`）和 pad probe、元素引用
        if let Some(sink) = run.video_sink.lock().unwrap().take() {
            sink.set_callbacks(gst_app::AppSinkCallbacks::builder().build());
        }
        run.audio_sink.lock().unwrap().take();
        run.video_sink_pad.lock().unwrap().take();
        run.video_filter.lock().unwrap().take();
        run.volume.lock().unwrap().take();
        if let Some(bus) = run.bus.clone() {
            bus.unset_sync_handler();
        }
        let _ = run.pipeline.set_state(gst::State::Null);
        if !wait {
            return;
        }
        // 等到真的到 NULL：`souphttpsrc` 的 socket、`uridecodebin3` 的流线程都是那之后才关的
        let _ = run.pipeline.state(gst::ClockTime::from_seconds(5));
        for el in iterate_elements(&run.pipeline) {
            let _ = run.pipeline.remove(&el);
        }
        drop(run.bus.clone());
    }

    fn query_position_ms(&self, p: &gst::Pipeline) -> Option<i64> {
        p.query_position::<gst::ClockTime>().map(|t| t.mseconds() as i64)
    }

    fn query_duration_ms(&self, p: &gst::Pipeline) -> Option<i64> {
        p.query_duration::<gst::ClockTime>().map(|t| t.mseconds() as i64)
    }
}

static PIPELINE_COUNTER: AtomicU64 = AtomicU64::new(1);

fn make_video_appsink(max_buffers: u32) -> Result<gst_app::AppSink, String> {
    // builder 的 `build()` 不是 Result（属性名 / 类型不对是 panic），所以先确认元素在
    if runtime::find_factory("appsink").is_none() {
        return Err("没有 appsink 元素".to_string());
    }
    Ok(gst_app::AppSink::builder()
        .name("videosink")
        .max_buffers(max_buffers)
        .sync(true)
        .drop(false)
        .caps(&gst::Caps::builder("video/x-raw").field("format", "BGRA").build())
        .build())
}

// ---------------- 自由函数（管线里那些"口径"都在这里，好单测） ----------------

fn escape_structure_value(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 没有显示会话时用 fakesink 而不是 autoaudiosink。
fn no_display() -> bool {
    std::env::var("DISPLAY").is_err() && std::env::var("WAYLAND_DISPLAY").is_err()
}

/// 声卡输出（autoaudiosink）。
///
/// [`system_clock`] 为 true 时让里面真正干活的声卡 sink（pulsesink / pipewiresink / alsasink……）
/// **不给管线提供时钟**，管线就用系统时钟：
/// pulsesink 默认提供的 GstPulseSinkClock 按音频周期一截一截地走，视频帧按它放就时早时晚——
/// 实测真实小红书 4K60（声音走 USB 无线耳机）帧间隔最大 22～58 ms（标称 16.7），每次都有一帧
/// 迟到；换成系统时钟 17.2～17.8 ms、0 迟到。声卡 sink 不提供时钟后照常跟着管线时钟走
/// （skew 校正），实测音画差和以前一样、不随时间漂。
/// 挂在 autoaudiosink 自己的 `element-added` 上。
fn audio_output(system_clock: bool) -> Result<gst::Element, String> {
    let auto = gst::ElementFactory::make("autoaudiosink")
        .build()
        .map_err(|e| e.to_string())?;
    if system_clock {
        let bin = auto
            .clone()
            .downcast::<gst::Bin>()
            .map_err(|_| "autoaudiosink 不是 Bin".to_string())?;
        bin.connect("element-added", false, |values| {
            if let Some(el) = values[1].get::<gst::Element>().ok() {
                no_audio_clock(&el);
            }
            None
        });
    }
    Ok(auto)
}

/// **只测"这个 URL 拉得到吗"**（`dev`/测试用）：`souphttpsrc ! fakesink` 起到 EOS。
///
/// [proxy] 的语义和引擎里完全一样（`None` = 不设、`Some("")` = 显式直连、
/// `Some("http://…")` = 走这个代理），所以 `GstProxyEnvTest` 验的那张表可以直接用它跑，
/// 不必再引一份 Java 的 GStreamer 绑定。
pub fn probe_url(url: &str, proxy: Option<&str>, timeout_s: u32) -> bool {
    runtime::runtime();
    let Ok(src) = gst::ElementFactory::make("souphttpsrc").build() else { return false };
    let Ok(sink) = gst::ElementFactory::make("fakesink").build() else { return false };
    let pipeline = gst::Pipeline::new();
    if pipeline.add_many([&src, &sink]).is_err() || gst::Element::link_many([&src, &sink]).is_err() {
        return false;
    }
    src.set_property("location", url);
    src.set_property("timeout", timeout_s);
    if let Some(p) = proxy {
        if src.find_property("proxy").is_some() {
            src.set_property("proxy", p);
        }
    }
    if pipeline.set_state(gst::State::Playing).is_err() {
        let _ = pipeline.set_state(gst::State::Null);
        return false;
    }
    let Some(bus) = pipeline.bus() else {
        let _ = pipeline.set_state(gst::State::Null);
        return false;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s as u64 + 15);
    let mut ok = false;
    while std::time::Instant::now() < deadline {
        match bus.timed_pop(gst::ClockTime::from_mseconds(100)) {
            Some(msg) => match msg.view() {
                gst::MessageView::Eos(_) => {
                    ok = true;
                    break;
                }
                gst::MessageView::Error(_) => break,
                _ => {}
            },
            None => continue,
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    ok
}

/// `autoaudiosink` 内部实际选中的那个 sink 的工厂名（`"pulsesink"` / `"alsasink"` /
/// `"fakesink"` …）。没有声卡时 autoaudiosink 会退成 fakesink——那种情况测不出时钟行为。
pub fn audio_sink_name() -> Option<String> {
    runtime::runtime();
    let sink = gst::ElementFactory::make("autoaudiosink").build().ok()?;
    // 必须起到 READY 才会去选真 sink
    let _ = sink.set_state(gst::State::Ready);
    let bin = match sink.clone().downcast::<gst::Bin>() {
        Ok(b) => b,
        Err(_) => {
            let _ = sink.set_state(gst::State::Null);
            return None;
        }
    };
    let name = bin
        .iterate_elements()
        .into_iter()
        .flatten()
        .next()
        .and_then(|e| e.factory().map(|f| f.name().to_string()));
    let _ = sink.set_state(gst::State::Null);
    name
}

fn no_audio_clock(element: &gst::Element) {
    let Some(factory) = element.factory() else { return };
    let klass = factory.metadata("klass").unwrap_or_default().to_string();
    if !klass.contains("Sink") || !klass.contains("Audio") || element.is::<gst_app::AppSink>() {
        return;
    }
    if element.find_property("provide-clock").is_some() {
        element.set_property("provide-clock", false);
    }
}

/// 递归列出管线里所有元素。
fn iterate_elements(pipeline: &gst::Pipeline) -> Vec<gst::Element> {
    let mut out = Vec::new();
    collect_elements(pipeline.upcast_ref(), &mut out);
    out
}

fn collect_elements(bin: &gst::Bin, out: &mut Vec<gst::Element>) {
    for el in bin.iterate_elements() {
        let Ok(el) = el else { continue };
        if let Ok(b) = el.clone().dynamic_cast::<gst::Bin>() {
            collect_elements(&b, out);
        } else {
            out.push(el);
        }
    }
}

fn infer_codec_from_pipeline(pipeline: &gst::Pipeline) -> Option<VideoCodec> {
    for el in iterate_elements(pipeline) {
        let Some(f) = el.factory() else { continue };
        let name = f.name().to_string();
        if name.contains("av1") {
            return Some(VideoCodec::Av1);
        }
        if name.contains("h265") || name.contains("hevc") {
            return Some(VideoCodec::Hevc);
        }
        if name.contains("h264") || name.contains("avc") {
            return Some(VideoCodec::Avc);
        }
    }
    None
}

fn now_ns() -> i64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    now_ns() / 1_000_000
}

/// 管线运行时间：时钟现在的读数减去 base_time（sink 按这个对时间戳）。
fn running_ns_pipeline(p: &gst::Pipeline) -> i64 {
    let clock = p.clock();
    let base = p.base_time();
    match (clock, base) {
        (Some(c), Some(b)) => (c.time().nseconds() as i64) - (b.nseconds() as i64),
        _ => 0,
    }
}

struct VideoFrame {
    bytes: Vec<u8>,
    stride: usize,
    offset: usize,
}

/// 从 buffer 里读出像素 + stride / offset。
///
/// 行距和起始偏移从 `GstVideoMeta` 里读：GPU 下载出来的帧每行可能比 `宽×4` 宽（对齐），
/// 要从 meta 里读，不能假设紧密排列。
fn read_video_frame(buffer: &gst::BufferRef, width: i32, height: i32) -> Option<VideoFrame> {
    let meta = buffer.meta::<gst_video::VideoMeta>();
    let (stride, offset) = match &meta {
        Some(m) => {
            let s = m.stride()[0] as usize;
            let o = m.offset()[0] as usize;
            (if s >= width as usize * 4 { s } else { width as usize * 4 }, o)
        }
        None => (width as usize * 4, 0),
    };
    let need = offset + stride * height as usize;
    let map = buffer.map_readable().ok()?;
    let slice = map.as_slice();
    if slice.len() < need {
        return None;
    }
    Some(VideoFrame {
        bytes: slice[..need].to_vec(),
        stride,
        offset,
    })
}

/// 3×3 个点的亮度取最小（0~255）；整屏全白才算亮。
fn compute_luma(bytes: &[u8], offset: usize, width: i32, height: i32, stride: usize) -> i32 {
    let mut luma = 255i32;
    for gy in 1..=3 {
        for gx in 1..=3 {
            let c = offset + (height as usize * gy / 4) * stride + (width as usize * gx / 4) * 4;
            let l = if c + 2 < bytes.len() {
                (bytes[c + 2] as i32) * 3 / 10
                    + (bytes[c + 1] as i32) * 6 / 10
                    + (bytes[c] as i32) / 10
            } else {
                -1
            };
            luma = luma.min(l);
        }
    }
    luma
}

/// 一帧 BGRA 编成 PNG（验证 / 调试用；不开窗）。手工写 PNG，不引图片库。
pub fn encode_png(width: u32, height: u32, stride: usize, bgra: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    if width == 0 || height == 0 || bgra.len() < stride * height as usize {
        return None;
    }
    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0); // filter: None
        let row = &bgra[y * stride..y * stride + width as usize * 4];
        for px in row.chunks_exact(4) {
            raw.extend_from_slice(&[px[2], px[1], px[0], px[3]]); // BGRA → RGBA
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&raw).ok()?;
    let idat = z.finish().ok()?;

    fn crc32(data: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (i, e) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        let mut c = 0xFFFF_FFFFu32;
        for b in data {
            c = table[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        c ^ 0xFFFF_FFFF
    }

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut crc_src = Vec::with_capacity(4 + body.len());
        crc_src.extend_from_slice(kind);
        crc_src.extend_from_slice(body);
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&crc32(&crc_src).to_be_bytes());
    }

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    Some(out)
}

