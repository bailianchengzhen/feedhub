//! **libva 的最小绑定**：只用来"问"某个 render node 支持哪些 profile / entrypoint
//! （等价 `vainfo --display drm --device`），**不建解码器、不解码**。
//!
//! 原来是 Kotlin 的 `VaApi.kt`（JNA）。搬过来的理由：它和 [`super::runtime`] 的
//! "设备侧确认能硬解"是同一件事的两半，拆在两个语言里就得把 `Device` 过边界；
//! 而且这一段是纯 C 调用，Rust 直接 `dlopen` 比 JNA 的反射式绑定干净。
//!
//! ## 为什么不用 `vainfo`
//!
//! 这台机器上没装（`vainfo` 属于 `vainfo` 包），而 libva 本体和驱动都在
//! （`libva.so.2` / `libva-drm.so.2` + `radeonsi_drv_video.so`），直接调它最省事、
//! 也不用 sudo 装东西。
//!
//! ## 为什么用 `dlopen` 而不是链接期绑定
//!
//! libva 是**可选**的：没装 VA-API 的机器上 App 还得能起来（退回软解）。链接期绑定会让
//! 整个 `.so` 加载失败。所以这里是 `libloading::Library::new("libva.so.2")`，拿不到就
//! 当作"这台机器没有 VA-API"。

use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};

use feedhub_core::play::VideoCodec;

/// VAEntrypointVLD：硬件解码入口点。
pub const ENTRYPOINT_VLD: i32 = 1;

/// 每种编码在 VA-API 里可能用的 profile id（任一有 VLD 就算这块卡能解这种编码）。
fn profiles_for(codec: VideoCodec) -> &'static [i32] {
    match codec {
        VideoCodec::Avc => &[7, 6, 13],  // H264High / H264Main / H264ConstrainedBaseline
        VideoCodec::Hevc => &[17, 18],   // HEVCMain / HEVCMain10
        VideoCodec::Av1 => &[32, 33],    // AV1Profile0 / AV1Profile1
        _ => &[],
    }
}

/// VA 的 profile / entrypoint 名字用不到（只做查表判定），所以不绑 `vaProfileStr`。

/// 一个 render node 上 VA-API 报出来的能力。
#[derive(Debug, Clone)]
pub struct Device {
    pub path: String,
    pub vendor: Option<String>,
    /// libva 报的驱动串（如 `"Mesa Gallium driver 25.0.7 for AMD Radeon AMD 核显"`）。
    pub driver: String,
    pub va_version: String,
    /// profile id → 它的 entrypoint 列表。
    pub profiles: BTreeMap<i32, Vec<i32>>,
}

impl Device {
    /// 这种编码在这块卡上有没有 VLD（真解码）入口点。
    pub fn supports_decode(&self, codec: VideoCodec) -> bool {
        profiles_for(codec)
            .iter()
            .any(|p| self.profiles.get(p).map(|eps| eps.contains(&ENTRYPOINT_VLD)).unwrap_or(false))
    }

    /// 人看得懂的 profile 清单。
    pub fn profile_text(&self) -> String {
        self.profiles
            .iter()
            .map(|(p, eps)| {
                let eps: Vec<String> = eps.iter().map(|e| e.to_string()).collect();
                format!("p{p}[{}]", eps.join("/"))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn describe(&self) -> String {
        format!(
            "{}（{}，{}，VA-API {}）",
            self.path,
            super::runtime::vendor_name(self.vendor.as_deref()),
            self.driver,
            self.va_version
        )
    }
}

// ---------------- dlopen 出来的 libva ----------------

/// `libva.so.2` 里我们要的那几个函数。
struct LibVa {
    _va: libloading::Library,
    _va_drm: libloading::Library,
    va_initialize: unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_int,
    va_terminate: unsafe extern "C" fn(*mut c_void) -> c_int,
    va_query_vendor_string: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    va_max_num_profiles: unsafe extern "C" fn(*mut c_void) -> c_int,
    va_query_config_profiles: unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_int,
    va_max_num_entrypoints: unsafe extern "C" fn(*mut c_void) -> c_int,
    va_query_config_entrypoints:
        unsafe extern "C" fn(*mut c_void, c_int, *mut c_int, *mut c_int) -> c_int,
    va_get_display_drm: unsafe extern "C" fn(c_int) -> *mut c_void,
}

// 函数指针是 Copy 的，Library 自己线程安全；这个结构只读，跨线程没问题。
unsafe impl Send for LibVa {}
unsafe impl Sync for LibVa {}

impl LibVa {
    fn load() -> Option<Self> {
        unsafe {
            let va = libloading::Library::new("libva.so.2").ok()?;
            let va_drm = libloading::Library::new("libva-drm.so.2").ok()?;
            Some(Self {
                va_initialize: *va.get(b"vaInitialize\0").ok()?,
                va_terminate: *va.get(b"vaTerminate\0").ok()?,
                va_query_vendor_string: *va.get(b"vaQueryVendorString\0").ok()?,
                va_max_num_profiles: *va.get(b"vaMaxNumProfiles\0").ok()?,
                va_query_config_profiles: *va.get(b"vaQueryConfigProfiles\0").ok()?,
                va_max_num_entrypoints: *va.get(b"vaMaxNumEntrypoints\0").ok()?,
                va_query_config_entrypoints: *va.get(b"vaQueryConfigEntrypoints\0").ok()?,
                va_get_display_drm: *va_drm.get(b"vaGetDisplayDRM\0").ok()?,
                _va: va,
                _va_drm: va_drm,
            })
        }
    }
}

/// 进程内只 `dlopen` 一次。
fn libva() -> Option<&'static LibVa> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Option<LibVa>> = OnceLock::new();
    CACHE.get_or_init(LibVa::load).as_ref()
}

/// 问 [path] 这块卡支不支持 VA-API。`vaInitialize` 失败返回 `None`
/// （比如驱动没装、节点不是自己的）。**只做查询，不做任何解码 / 计算。**
pub fn query(path: &str) -> Option<Device> {
    let va = libva()?;
    if !std::path::Path::new(path).exists() {
        return None;
    }
    let cpath = CString::new(path).ok()?;
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR) };
    if fd < 0 {
        return None;
    }
    // 从这一刻起无论哪条路退出都要 close(fd)
    let dpy = unsafe { (va.va_get_display_drm)(fd) };
    if dpy.is_null() {
        unsafe { libc::close(fd) };
        return None;
    }
    let mut major: c_int = 0;
    let mut minor: c_int = 0;
    if unsafe { (va.va_initialize)(dpy, &mut major, &mut minor) } != 0 {
        unsafe { libc::close(fd) };
        return None;
    }
    let result = unsafe { collect(va, dpy, path, major, minor) };
    unsafe {
        (va.va_terminate)(dpy);
        libc::close(fd);
    }
    result
}

unsafe fn collect(va: &LibVa, dpy: *mut c_void, path: &str, major: c_int, minor: c_int) -> Option<Device> {
    let mut profiles: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    let max_p = (va.va_max_num_profiles)(dpy);
    if max_p > 0 {
        let mut arr: Vec<c_int> = vec![0; max_p as usize];
        let mut n: c_int = 0;
        if (va.va_query_config_profiles)(dpy, arr.as_mut_ptr(), &mut n) == 0 {
            for i in 0..n.max(0) as usize {
                let p = arr[i];
                let max_e = (va.va_max_num_entrypoints)(dpy);
                if max_e <= 0 {
                    continue;
                }
                let mut eps: Vec<c_int> = vec![0; max_e as usize];
                let mut en: c_int = 0;
                if (va.va_query_config_entrypoints)(dpy, p, eps.as_mut_ptr(), &mut en) == 0 {
                    profiles.insert(p, eps[..en.max(0) as usize].to_vec());
                }
            }
        }
    }
    let driver = {
        let s = (va.va_query_vendor_string)(dpy);
        if s.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(s).to_string_lossy().into_owned()
        }
    };
    Some(Device {
        path: path.to_string(),
        vendor: super::runtime::vendor_of(path),
        driver,
        va_version: format!("{major}.{minor}"),
        profiles,
    })
}

/// 进程里有没有 libva（有没有装 VA-API 的驱动那一套）。测试和"这台机器能不能硬解"的判断用。
pub fn available() -> bool {
    libva().is_some()
}

/// 扫 `/dev/dri/renderD*`，问每一个能不能 `vaInitialize`（**只查询、不解码**）。
///
/// 和 [`query`] 的区别：这个**会问 NVIDIA 那块**，
/// 生产路径（`runtime::pick_internal_device`）不会。`VaProbe` 那种检查工具用它。
pub fn scan_all() -> Vec<Device> {
    let mut out = Vec::new();
    let dir = match std::fs::read_dir("/sys/class/drm") {
        Ok(d) => d,
        Err(_) => return out,
    };
    let mut names: Vec<String> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_render_node(n))
        .collect();
    names.sort_by_key(|n| node_index(n));
    for name in names {
        if let Some(dev) = query(&format!("/dev/dri/{name}")) {
            out.push(dev);
        }
    }
    out
}

/// `/sys/class/drm` 里的一项是不是 render node（`renderD128` 这种）。
pub fn is_render_node(name: &str) -> bool {
    name.strip_prefix("renderD")
        .map(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

fn node_index(name: &str) -> i32 {
    name.strip_prefix("renderD")
        .and_then(|r| r.parse::<i32>().ok())
        .unwrap_or(i32::MAX)
}

