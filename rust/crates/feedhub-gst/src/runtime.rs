//! **GStreamer 的进程级初始化和"这台机器能干什么"**：库能不能加载、必需的元素在不在、
//! VA-API 用的是哪块显卡、能硬解哪些编码。**只用核显、不唤醒独显**。
//!
//! 原来在 Kotlin 的 `GstRuntime.kt`。搬过来的理由：这里面全是"决策"（挑设备、改 rank、
//! 判定硬解能力、决定哪些编码算硬解），留在 Kotlin
//! 就得把 `hardwareCodecs` / `rejectedCodecs` / 设备路径一整套过边界给引擎用。
//!
//! ## 不唤醒独显的四道闸
//!
//! - 挑 VA 设备时按 sysfs 里的 PCI 厂商把 NVIDIA 的 render node **直接排除**，
//!   连它的驱动都不加载（不 `vaInitialize`）；
//! - nvcodec（NVIDIA 的 nvh264dec / nvav1dec 等）的 rank 全降到 NONE，decodebin 不会选它们；
//! - 万一 va 插件还是解析到了 NVIDIA 的设备（比如老版本 GStreamer 不认我们的选择），
//!   那它注册的 va 解码器也全降 rank；
//! - 建管线时 [`super::engine`] 还会把每个 `va*` 元素的 `device-path` 显式指到选定的那块核显。
//!
//! ## "能不能硬解"不是看元素建不建得出来
//!
//! 还要用 libva 问设备侧这种编码有没有 VLD（真解码）入口点，见 [`super::vaapi`]。
//! 只有设备侧确认了才算进 [`Runtime::hardware_codecs`]——不然界面上的"硬解"标签就是假的。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use gst::prelude::*;
use gstreamer as gst;

use feedhub_core::play::VideoCodec;

use super::vaapi;

/// NVIDIA 的 PCI 厂商 id。
pub const NVIDIA: &str = "0x10de";
/// AMD 的 PCI 厂商 id。
pub const AMD: &str = "0x1002";
/// Intel 的 PCI 厂商 id。
pub const INTEL: &str = "0x8086";

/// 建管线必需的元素（缺一个就说明这台机器没装齐，报给界面）。
const REQUIRED: &[&str] = &[
    "uridecodebin3",
    "decodebin3",
    "souphttpsrc",
    "dashdemux2",
    "qtdemux",
    "appsink",
    "capsfilter",
    "audioconvert",
    "audioresample",
    "volume",
    "autoaudiosink",
    "fakesink",
];

/// 硬解用的 VA-API 解码器元素。
fn va_decoder_name(codec: VideoCodec) -> Option<&'static str> {
    match codec {
        VideoCodec::Avc => Some("vah264dec"),
        VideoCodec::Hevc => Some("vah265dec"),
        VideoCodec::Av1 => Some("vaav1dec"),
        _ => None,
    }
}

/// 所有要探测的硬解编码。
pub const VA_CODECS: &[VideoCodec] = &[VideoCodec::Avc, VideoCodec::Hevc, VideoCodec::Av1];

/// 有其中一个就说明装了解 H.264 的东西（没有的话连软解都播不了）。
const H264_DECODERS: &[&str] = &["avdec_h264", "openh264dec", "vah264dec"];

/// 初始化结果 + 本机能力。**一个进程一份**（[`runtime`] 拿它）。
pub struct Runtime {
    /// `None` = 能用；`Some(原因)` = 不能用。
    pub problem: Option<String>,
    /// 选定的 VA-API 设备路径（如 `/dev/dri/renderD128`，核显）；没有可用的就是 `None`。
    pub va_device_path: Option<String>,
    /// 选定设备的 PCI 厂商（`0x1002` = AMD，`0x8086` = Intel）；不知道就 `None`。
    pub va_vendor: Option<String>,
    /// 选定设备上 libva 报的驱动串。
    pub va_driver: Option<String>,
    /// GStreamer 的 va 元素**实际**解析到的设备（只读属性报出来的）；和 `va_device_path`
    /// 不一致就是钉失败了。
    pub va_resolved_device: Option<String>,
    /// 本机**确认**能硬解的编码：元素在 + 设备侧有这种编码的 VLD 入口点。
    pub hardware_codecs: Vec<VideoCodec>,
    /// 探测过、但设备侧没确认的编码 → 原因（用 Vec 而不是 BTreeMap：`VideoCodec` 没实现 `Ord`，
    /// 而这里最多三条、顺序就是 [`VA_CODECS`]，没必要为了排序给它加 derive）。
    pub rejected_codecs: Vec<(VideoCodec, String)>,
    /// 每个 VA 解码器元素的原始 rank（[`Runtime::set_hardware`] 靠它开 / 关硬解）。
    va_ranks: BTreeMap<String, gst::Rank>,
    /// GStreamer 版本串（`Gst.getVersionString()` 的替身），拿不到就 `None`。
    pub version: Option<String>,
}

impl Runtime {
    /// 这台机器能不能用 `GST_SEEK_FLAG_INSTANT_RATE_CHANGE` 换倍速（GStreamer **1.18+**）。
    ///
    /// 为什么要在版本号上挡一道：那个标志位是 1.18 才加的，老版本收到不认识的标志位
    /// **不报错**，当成没写——于是 `gst_element_seek` 会做一次普通 seek（位置不动、
    /// 速率也不改），我们却以为变速成功了。元素级的拒绝（返回 false）另外由调用方回退。
    pub fn instant_rate_change(&self) -> bool {
        if self.problem.is_some() {
            return false;
        }
        match gst_version_tuple() {
            Some((major, minor, _)) => major > 1 || (major == 1 && minor >= 18),
            None => false,
        }
    }

    /// 有没有 vapostproc（GPU 上转 BGRA 和缩放）。
    pub fn has_va_postproc(&self) -> bool {
        self.problem.is_none()
            && self.va_device_path.is_some()
            && find_factory("vapostproc").is_some()
    }

    /// 开 / 关硬解：改 va 解码器的 rank（进程级，下一个建的管线生效）。
    pub fn set_hardware(&self, enabled: bool) {
        for (name, rank) in &self.va_ranks {
            if let Some(f) = find_factory(name) {
                f.set_rank(if enabled { *rank } else { gst::Rank::NONE });
            }
        }
    }
}

/// `ElementFactory::find` 找不到是 `Err`（不是 `None` 也不抛异常）——所以一律走这个。
pub fn find_factory(name: &str) -> Option<gst::ElementFactory> {
    gst::ElementFactory::find(name)
}

/// GStreamer 的版本（major, minor, micro）。
pub fn gst_version_tuple() -> Option<(u32, u32, u32)> {
    let (mut major, mut minor, mut micro, mut nano) = (0u32, 0u32, 0u32, 0u32);
    // gst::version() 是编译期的；运行时版本要问库本身。
    unsafe {
        gst::ffi::gst_version(&mut major, &mut minor, &mut micro, &mut nano);
    }
    let _ = nano;
    if major == 0 {
        None
    } else {
        Some((major, minor, micro))
    }
}

/// GStreamer 版本串（`"1.26.2"`）。
pub fn gst_version_string() -> Option<String> {
    gst_version_tuple().map(|(a, b, c)| format!("{a}.{b}.{c}"))
}

/// 中转出来的清单 / 整段 mp4 都在本机回环上：把回环放进 `no_proxy`，GIO / libsoup 才不会
/// 把它们交给 env 代理（libproxy 不忽略回环；见 `PlayerProxy.ensureLoopbackNotProxied`
/// 和 `GstProxyEnvTest`）。
///
/// 只影响 GIO / libsoup 这一侧（JVM 的系统代理看的是 `http.nonProxyHosts`，本来就排除回环）。
/// 重复调用没有副作用。
pub fn ensure_loopback_not_proxied() {
    for name in ["no_proxy", "NO_PROXY"] {
        let current = std::env::var(name).unwrap_or_default();
        let Some(merged) = merge_no_proxy(&current) else { continue };
        // Rust 2024 起 std::env::set_var 是 unsafe（多线程下改环境不安全）。GStreamer 初始化
        // 时是单线程路径（[`runtime`] 的 OnceLock 里），所以这里包 unsafe 是可以的——
        // 但**必须**在起任何 GStreamer 线程之前调，[`probe`] 就是这么排的。
        unsafe { std::env::set_var(name, merged) };
    }
}

/// 把回环写进一份 `no_proxy` 的**纯函数**（好单测，不碰进程环境）。
///
/// 返回 `None` = 不用改（已经有 `*`、或者三个回环写法都在）；否则返回合并后的整串。
/// 已有的值原样保留（逗号分隔），缺的补在后面。
pub fn merge_no_proxy(current: &str) -> Option<String> {
    const WANT: &[&str] = &["127.0.0.1", "localhost", "::1"];
    let have: Vec<String> = current
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if have.iter().any(|h| h == "*") {
        return None;
    }
    let missing: Vec<&str> = WANT
        .iter()
        .copied()
        .filter(|w| !have.iter().any(|h| h == w))
        .collect();
    if missing.is_empty() {
        return None;
    }
    Some(if current.is_empty() {
        missing.join(",")
    } else {
        format!("{},{}", current, missing.join(","))
    })
}

/// `/dev/dri/renderD128` → 它所在显卡的 PCI 厂商，如 `"0x1002"`。
pub fn vendor_of(device_path: &str) -> Option<String> {
    let name = Path::new(device_path).file_name()?.to_string_lossy().into_owned();
    std::fs::read_to_string(format!("/sys/class/drm/{name}/device/vendor"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `/dev/dri/renderD128` → 这块卡的 PCI `vendor:device`（去掉 0x、小写，如 `1002:1234`）。
/// 解码环境指纹用（换显卡 / eGPU 之后旧账本作废）；读不到就 `None`。
pub fn pci_id_of(device_path: &str) -> Option<String> {
    let name = Path::new(device_path).file_name()?.to_string_lossy().into_owned();
    let dir = format!("/sys/class/drm/{name}/device");
    let read = |f: &str| -> Option<String> {
        std::fs::read_to_string(format!("{dir}/{f}")).ok().map(|s| {
            s.trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X")
                .to_lowercase()
        })
    };
    let vendor = read("vendor").filter(|s| !s.is_empty())?;
    let device = read("device").filter(|s| !s.is_empty())?;
    Some(format!("{vendor}:{device}"))
}

/// PCI 厂商 id → 人看得懂的名字（检查工具的输出用）。
pub fn vendor_name(vendor: Option<&str>) -> String {
    match vendor {
        Some(AMD) => "AMD".into(),
        Some(INTEL) => "Intel".into(),
        Some(NVIDIA) => "NVIDIA".into(),
        None => "未知".into(),
        Some(other) => other.into(),
    }
}

/// 进程级的那一份 [`Runtime`]：第一次用时初始化 + 探测（会阻塞几百毫秒），之后都是缓存值。
///
/// **不要在启动路径上调**（`decode_environment_fingerprint` 会触发它，那里也只是在需要时才问）。
pub fn runtime() -> &'static Runtime {
    static CACHE: OnceLock<Runtime> = OnceLock::new();
    CACHE.get_or_init(probe)
}

fn probe() -> Runtime {
    // 1. 先把回环写进 no_proxy（要在 GStreamer 起来之前，见 [`ensure_loopback_not_proxied`] 的说明）
    ensure_loopback_not_proxied();
    // 2. 自己在 sysfs 里挑一块核显（不碰 NVIDIA）
    let preferred = pick_internal_device();

    // 3. 初始化 GStreamer。`--gst-disable-segtrap`：注册表要重扫时 GStreamer 会在本进程里加载
    //    插件，期间装自己的 SIGSEGV 处理，和别的运行时撞上会被当成"插件崩了"、整个进程 exit(255)
    //    Kotlin 那版是 `Gst.init(Version.BASELINE, "feedhub", "--gst-disable-segtrap")`。
    if let Err(e) = gst::init() {
        return broken(format!("GStreamer 初始化失败：{e}"));
    }
    //    segtrap 在上面的 init 里已经关了（`gst::init()` 用默认参数）。要保证关掉，
    //    再显式设一次 `GST_DEBUG` 同级的那份开关——见下面 `disable_segtrap()`。
    disable_segtrap();

    // 4. 必需元素在不在
    let missing: Vec<&str> = REQUIRED.iter().copied().filter(|n| find_factory(n).is_none()).collect();
    if !missing.is_empty() {
        return broken(format!("GStreamer 缺少元素：{}", missing.join(", ")));
    }
    if !H264_DECODERS.iter().any(|n| find_factory(n).is_some()) {
        return broken("GStreamer 缺少 H.264 解码器（gstreamer1.0-libav）".to_string());
    }

    // 5. 先把 NVIDIA 的解码器 / 编码器降到 NONE：不降的话它们和 va 同 rank，
    //    decodebin 可能先选中，独显就被唤醒了
    {
        let registry = gst::Registry::get();
        for f in registry.features_by_plugin("nvcodec") {
            f.set_rank(gst::Rank::NONE);
        }
    }

    // 6. 设备侧确认能硬解哪些
    let mut rt = Runtime {
        problem: None,
        va_device_path: None,
        va_vendor: None,
        va_driver: None,
        va_resolved_device: None,
        hardware_codecs: Vec::new(),
        rejected_codecs: Vec::new(),
        va_ranks: BTreeMap::new(),
        version: gst_version_string(),
    };
    probe_va(&mut rt, preferred);

    // 7. `FEEDHUB_HWDEC=0` 强制软解（对比 CPU 占用用）
    if std::env::var("FEEDHUB_HWDEC").as_deref() == Ok("0") {
        rt.set_hardware(false);
    }
    rt
}

fn broken(why: String) -> Runtime {
    Runtime {
        problem: Some(why),
        va_device_path: None,
        va_vendor: None,
        va_driver: None,
        va_resolved_device: None,
        hardware_codecs: Vec::new(),
        rejected_codecs: Vec::new(),
        va_ranks: BTreeMap::new(),
        version: None,
    }
}

/// 关掉 GStreamer 的 segtrap trailer。
///
/// `gst::init()` 不接受额外命令行参数（`gst::init_check` 那一族都不行），所以走了和
/// `GST_REGISTRY_UPDATE=no` 一样的另一条路：`GST_DEBUG` 之外的 `gst_registry_fork_set_enabled`
/// 管的是 fork 子进程、不是 segtrap。segtrap 的开关是 `gst_segtrap_set_enabled(FALSE)`——
/// 这个函数**在** `libgstreamer-1.0` 里（`gstutils.h`），直接调。
fn disable_segtrap() {
    unsafe {
        gst::ffi::gst_segtrap_set_enabled(glib::ffi::GFALSE);
    }
}

/// 自己在 sysfs 里挑一块**核显**：扫 `/dev/dri/renderD*`，按 PCI 厂商把 NVIDIA 的直接跳过
/// （连它的 VA 驱动都不加载），剩下的按节点号顺序问 libva 谁能初始化，第一个成功的就先用它。
/// `None` = 这台机器上没有可用的核显 VA。
///
/// 注意这只是"我们希望用哪块"。GStreamer 1.26 里 va 元素的 `device-path` 是**只读**的、
/// 也没有 `GST_VA_DRM_DEVICE` 之类的环境变量可以钉——实际用哪块由插件的 udev 枚举顺序决定。
/// 所以 [`probe_va`] 还会把元素**真正解析到**的设备读回来核对，万一落到独显上就把 va 解码器
/// 全降 rank（宁可用软解也不在独显上解码）。
fn pick_internal_device() -> Option<vaapi::Device> {
    let dir = std::fs::read_dir("/sys/class/drm").ok()?;
    let mut nodes: Vec<String> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| vaapi::is_render_node(n))
        .collect();
    nodes.sort_by_key(|n| n.strip_prefix("renderD").and_then(|r| r.parse::<i32>().ok()).unwrap_or(i32::MAX));
    for node in nodes {
        let path = format!("/dev/dri/{node}");
        if vendor_of(&path).as_deref() == Some(NVIDIA) {
            continue; // 独显：一次都不碰
        }
        if let Some(device) = vaapi::query(&path) {
            return Some(device);
        }
    }
    None
}

/// 设备侧确认每种编码能不能真硬解，并核对 va 插件**实际**用的是哪块卡。
///
/// - 先读一次元素的 `device-path`（只读属性，报的就是真正解析到的设备）：是 NVIDIA 就说明
///   va 插件选了独显，这时把 `vah264dec` / `vah265dec` / `vaav1dec` 的 rank 全降到 NONE，
///   宁可用软解也不在独显上解码；是别的核显就**以它为准**（它才是真在用的那块），再问 libva；
/// - 然后逐个编码查设备侧有没有 VLD（真解码）入口点。
fn probe_va(rt: &mut Runtime, preferred: Option<vaapi::Device>) {
    let reject_all = |rt: &mut Runtime, why: String| {
        for codec in VA_CODECS {
            if let Some(name) = va_decoder_name(*codec) {
                if let Some(f) = find_factory(name) {
                    f.set_rank(gst::Rank::NONE);
                }
            }
            rt.rejected_codecs.push((*codec, why.clone()));
        }
        rt.hardware_codecs.clear();
    };

    // 元素实际解析到的设备
    rt.va_resolved_device = find_factory("vah264dec").and_then(|f| {
        f.create().build().ok().and_then(|el| {
            el.property::<Option<String>>("device-path")
        })
    });

    if let Some(resolved) = rt.va_resolved_device.clone() {
        if vendor_of(&resolved).as_deref() == Some(NVIDIA) {
            eprintln!("gst: va 插件把解码器落在了独显 {resolved} 上，宁可用软解也不用它");
            reject_all(rt, format!("va 插件把解码器落在了独显 {resolved} 上"));
            return;
        }
    }

    // 实际解析到的设备优先（它才是真在用的）；拿不到就退回自己挑的那块
    let device = rt
        .va_resolved_device
        .clone()
        .and_then(|p| vaapi::query(&p))
        .or(preferred);
    let Some(device) = device else {
        reject_all(rt, "没有可用的 VA-API 核显设备".to_string());
        return;
    };

    rt.va_device_path = Some(device.path.clone());
    rt.va_vendor = device.vendor.clone();
    rt.va_driver = Some(device.driver.clone());

    let mut ok = Vec::new();
    let mut rejected: Vec<(VideoCodec, String)> = Vec::new();
    for codec in VA_CODECS {
        let Some(name) = va_decoder_name(*codec) else { continue };
        let Some(factory) = find_factory(name) else {
            rejected.push((*codec, format!("GStreamer 没有 {name} 元素")));
            continue;
        };
        if !device.supports_decode(*codec) {
            rejected.push((
                *codec,
                format!(
                    "{} 上没有 {:?} 的 VLD 解码入口点（{}）",
                    device.path, *codec, device.driver
                ),
            ));
            factory.set_rank(gst::Rank::NONE);
            continue;
        }
        rt.va_ranks.insert(name.to_string(), factory.rank());
        ok.push(*codec);
    }
    rt.rejected_codecs = rejected;
    rt.hardware_codecs = ok;
}

/// 这台机器**解码环境**的指纹：选定 VA 设备的 PCI `vendor:device` + libva 报的驱动串
/// （Mesa 版本）+ GStreamer 版本。`DecodeLedger` 拿它当落盘记录的失效判据：任何一项变了
/// （换显卡 / 插拔 eGPU / 升级 Mesa / 升级 GStreamer），旧记录整份作废，重新学。
///
/// 拿不到就返回 `None`（没装 GStreamer、VA-API 起不来、sysfs 读不到）：那时候**不读也不写**
/// 账本——宁可不落盘，也不拿一份不知道对谁有效的记录去选流。
///
/// 读它会触发一次 [`runtime`] 的惰性探测（挑设备 + 问 libva），之后都是缓存值。
pub fn decode_environment_fingerprint() -> Option<String> {
    let rt = runtime();
    if rt.problem.is_some() {
        return None;
    }
    let device = rt.va_device_path.as_deref()?;
    let pci = pci_id_of(device)?;
    let driver = rt.va_driver.as_deref()?.trim();
    if driver.is_empty() {
        return None;
    }
    let gst_version = rt.version.as_deref()?.trim();
    if gst_version.is_empty() {
        return None;
    }
    Some(format!("{pci}|{driver}|GStreamer {gst_version}"))
}

