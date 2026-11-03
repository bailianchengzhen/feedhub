//! **翻译跑在哪个后端上**：NPU（Hexagon HTP）→ GPU（Adreno OpenCL）→ NPU+GPU 分层 → CPU，
//! 以及"起不来 / 输出不对就降一档并记住"的规则。
//!
//! ## 为什么需要这一层
//!
//! 桌面那条路只有一块 AMD 核显，规则是"按名字挑、挑不到就不启动"（[`crate::translate::server`]）。
//! 手机上不一样：同一个 `llama-server` 里编进了三个后端（CPU / GPUOpenCL / HTP），
//! 用 `--device` 选、用 `-ngl` 卸层，而且**能同时用两个设备**（`--device GPUOpenCL,HTP0 --split-mode layer`，
//! 层按下标切开、前一半在 GPU 上跑、后一半在 NPU 上跑）。哪一档能用取决于这台机器（HTP 版本、
//! Adreno 驱动、SELinux），试之前不知道，所以必须**试 + 记住**。
//!
//! ## 规则
//!
//! 候选顺序就是 [`Tier`] 的声明顺序：NPU → GPU → NPU+GPU → CPU。
//! 每次启动从"记住的那一档"开始试；起不来 / 一句话超时 / 输出是空串或乱码 → 降一档、
//! 写进 `ctx.store`（[`KEY_BACKEND`]）、当前接着试下一档，直到 CPU（CPU 是兜底，试不过就是真失败）。
//! 记录里带 App 版本（[`Memory::app`]）：**App 升级之后重新从 NPU 试**——新版本可能修好了 skel / 驱动的问题。
//!
//! ## 为什么是这几个启动参数
//!
//! | 档 | `--device` | `-ngl` | 其它 |
//! |---|---|---|---|
//! | NPU | `HTP0` | 99 | 单 session（1.8B 的权重 ~1 GB，远小于 3.5 GB 地址窗口，不用多 session） |
//! | GPU | `GPUOpenCL` | 99 | — |
//! | NPU+GPU | `GPUOpenCL,HTP0` | 99 | `--split-mode layer`（按层切；tensor 切要把每层的矩阵拆开，1.8B 上不划算） |
//! | CPU | 不给 `--device` | 0 | `-t` = 可用核数的一半（夹 2..6） |
//!
//! Android 上前三档统一 `-t 4`：Hexagon 后端**自己在 NPU 上开线程**（默认吃满 HVX 硬线程），
//! 宿主这 4 个线程只负责 graph 调度和 DMA，再给多也没用。
//!
//! 功耗档 [`POWER_MODE`]：`balanced`（**不是**后端默认的 `burst`）。`burst` 会关掉 DCVS、锁最高频，
//! 手机连续翻十几句就烫；`balanced` 留着 DCVS。这个变量必须在 backend registry 初始化前进环境，
//! 所以只能由拉起进程的人设，没有对应的 CLI 参数。
//!
//! ## 这一层不做的事
//!
//! 不自己探测"这台手机是哪个 HTP 版本"——`libggml-htp-v79.so` 是 llama.cpp 自己按
//! `htpdrv_get_arch()` 选的（v79 = 8 Elite）。这里只认 `--list-devices` 打出来的设备名。

use std::path::Path;

use crate::host::StoreHost;

/// `ctx.store` 里记"上次降到哪一档"的键。
pub const KEY_BACKEND: &str = "translate.backend.v1";

/// HTP 的功耗档（`GGML_HEXAGON_POWER_MODE`）。
///
/// `balanced`：DCVS 开着、不锁频。用户要的是"手机上能用"，不是跑分；`burst` 是后端默认，
/// 连续翻几句就降频，反而更不稳。
pub const POWER_MODE: &str = "balanced";

/// OpenCL kernel 编译缓存的目录名（放在 App 私有缓存目录下）。
///
/// Adreno 的 OpenCL 内核是**运行时编译**的，第一次要几秒；缓存下来以后只编译变化的那部分。
/// 不设的话 llama.cpp 用系统临时目录（安卓上 `/data/local/tmp` 之类，App 不一定写得进去）。
pub const OPENCL_CACHE_DIR: &str = "cl-cache";

/// 安卓上高通 OpenCL 实现的候选位置（`libOpenCL.so` 本体，由厂商提供）。
///
/// 为什么要有这一条：我们打进 APK 的是 **Khronos ICD loader**（`tools/translate/build-android.sh`），
/// 它自己不含驱动，只按 `/system/vendor/Khronos/OpenCL/vendors/*.icd` 找厂商实现——而**多数手机上没有
/// 这个目录**（厂商不会给 App 侧铺 ICD 清单）。所以 loader 那头要靠 `OCL_ICD_FILENAMES` 直接指到
/// 厂商的 `libOpenCL.so`（ICD loader 的 `khrIcdVendorAdd` 就是 `dlopen` 这个名字）。
///
/// 顺序：`/vendor` 优先（高通机型的实际位置），再 `/system/vendor`，再 `/odm`（少数机型）。
pub const OPENCL_VENDOR_CANDIDATES: &[&str] = &[
    "/vendor/lib64/libOpenCL.so",
    "/system/vendor/lib64/libOpenCL.so",
    "/odm/lib64/libOpenCL.so",
    "/vendor/lib64/libOpenCL_adreno.so",
];

/// 在候选里挑第一个**存在**的厂商 OpenCL 库，生成 `OCL_ICD_FILENAMES` 环境变量。
///
/// 一个都找不到就是 `None`：那这一档的 OpenCL 大概率起不来，`--list-devices` 里不会有
/// `GPUOpenCL`，[`usable_tiers`] 自然会跳过 GPU（不猜、不兜底到别的驱动）。
pub fn opencl_vendor_env<I, S>(candidates: I) -> Option<(String, String)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let found = candidates.into_iter().find(|p| Path::new(p.as_ref()).is_file())?;
    let found = found.as_ref();
    Some(("OCL_ICD_FILENAMES".to_string(), found.to_string()))
}

/// 一个后端档位。**声明顺序就是候选顺序**（NPU 最快最省电，CPU 是兜底）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Npu,
    Gpu,
    NpuGpu,
    Cpu,
}

impl Tier {
    /// 这个档位的名字（存进 `ctx.store` / 给界面看，**改成新的就认不出旧记录**）。
    pub fn name(self) -> &'static str {
        match self {
            Tier::Npu => "npu",
            Tier::Gpu => "gpu",
            Tier::NpuGpu => "npu+gpu",
            Tier::Cpu => "cpu",
        }
    }

    /// 设置页上那一行显示的字（和向量那边的写法一致：`NPU` / `GPU` / `NPU + GPU` / `CPU`）。
    pub fn label(self) -> &'static str {
        match self {
            Tier::Npu => "NPU（Hexagon HTP）",
            Tier::Gpu => "GPU（Adreno OpenCL）",
            Tier::NpuGpu => "NPU + GPU（分层）",
            Tier::Cpu => "CPU",
        }
    }

    /// 下一档（降级）；已经是 CPU 就是 `None`。
    pub fn next(self) -> Option<Tier> {
        match self {
            Tier::Npu => Some(Tier::Gpu),
            Tier::Gpu => Some(Tier::NpuGpu),
            Tier::NpuGpu => Some(Tier::Cpu),
            Tier::Cpu => None,
        }
    }

    /// 从名字反解（认不出来的当 NPU：宁可多试一档，也不要一上来就用 CPU）。
    pub fn from_name(name: &str) -> Tier {
        match name {
            "gpu" => Tier::Gpu,
            "npu+gpu" => Tier::NpuGpu,
            "cpu" => Tier::Cpu,
            _ => Tier::Npu,
        }
    }

    /// 从**设置里的覆盖值**反解（`tools/translate/phone_bench.sh` / 排错用）；`None` / 认不出来 = 自动。
    ///
    /// 认的口径（和 Kotlin `TranslationAndroidBackend` 的名字 + 存档里的拼写一致）：
    /// `"NPU"` / `"GPU"` / `"NPU_GPU"` / `"CPU_ONLY"`（大小写不敏感，也收 `"npu+gpu"` / `"cpu"`）。
    /// 它是产品默认值不是用户选择；
    /// 认了就等于把所有人钉死在 CPU 上（迁移口径写在 `settings.rs` 的 `normalize` 里）。
    pub fn from_override(value: Option<&str>) -> Option<Tier> {
        let v = value?.trim();
        if v.is_empty() {
            return None;
        }
        let lower = v.to_ascii_lowercase();
        match lower.as_str() {
            "npu" => Some(Tier::Npu),
            "gpu" => Some(Tier::Gpu),
            "npu_gpu" | "npu+gpu" | "npu-gpu" => Some(Tier::NpuGpu),
            "cpu_only" => Some(Tier::Cpu),
            _ => None,
        }
    }
}

/// `--list-devices` 里的一行（`HTP0: Hexagon (2048 MiB, 2048 MiB free)` /
/// `GPUOpenCL: Adreno (TM) 830 (…)` / `Vulkan0: AMD Radeon AMD 核显 (RADV PHOENIX) (…)`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendDevice {
    /// 冒号前面的名字，就是 `--device` 要的那个串（`HTP0` / `GPUOpenCL` / `Vulkan0`）。
    pub name: String,
    /// 冒号后面的描述（`Hexagon` / `Adreno (TM) 830` / `AMD Radeon AMD 核显 (RADV PHOENIX)`），
    /// 已经去掉尾巴上的 `(2048 MiB, …)`。
    pub description: String,
}

impl BackendDevice {
    pub fn is_npu(&self) -> bool {
        self.name.starts_with("HTP")
    }

    pub fn is_opencl(&self) -> bool {
        self.name == "GPUOpenCL"
    }
}

/// 这台机器该走哪条路。
///
/// **谁说了算**：操作系统是宿主的**事实**（宿主知道自己跑在 Android 还是 Linux 上），所以由宿主传进来
/// （[`super::engine::Config::platform`]）；**设备列表不参与**这个判断——x86_64 模拟器上
/// `--list-devices` 一个非 CPU 设备都没有，但它仍然是手机形态（该走"CPU 兜底"而不是"没有 AMD 核显 就不启动"）。
///
/// 反过来说，桌面上**永远**只走 AMD 核显 那条红线（不碰独显），就算哪天真出现了
/// HTP / OpenCL 也不改规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// 桌面：只有 Vulkan 设备，规则是"按名字挑 AMD 核显、挑不到不启动"（不碰独显）。
    DesktopVulkan,
    /// 手机：有 HTP / OpenCL 后端就按 [`Tier`] 试；一个都没有就 CPU 兜底。
    Mobile,
}

impl Platform {
    /// 宿主传进来的值：`mobile` = Android。
    pub fn from_host(mobile: bool) -> Platform {
        if mobile {
            Platform::Mobile
        } else {
            Platform::DesktopVulkan
        }
    }
}

/// 只看设备列表猜平台（**只给排障 / 界面预览用**，真正的判断走 [`Platform::from_host`]）。
pub fn platform_of(devices: &[BackendDevice]) -> Platform {
    if devices.iter().any(|d| d.is_npu() || d.is_opencl()) {
        Platform::Mobile
    } else {
        Platform::DesktopVulkan
    }
}

/// 解析 `--list-devices` 的**完整**输出（名字 + 描述）。和
/// [`crate::translate::server::parse_devices`]（只认 `VulkanN`）互补：
/// 那个是桌面红线要用的，这个是手机挑后端要用的。
pub fn parse_backend_devices(output: &str) -> Vec<BackendDevice> {
    let mut out = Vec::new();
    for line in output.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // 设备行是**缩进**的（`printf("  %s: %s (…)\n")`），后端日志在行首（`ggml-hex: …`），
        // 靠缩进就能把日志排掉——只认名字形状会让 `ggml-hex: Hexagon Arch version v79` 混进来。
        if !line.starts_with(' ') {
            continue;
        }
        let Some((name, rest)) = line.trim_start().split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name.contains(' ') {
            continue;
        }
        // `名字: 描述`：冒号后面必须是空格 + 非空描述（`(none)` 那行没有冒号，早被 split_once 挡掉；
        // `Vulkan1:` 这种空描述不算设备）
        if !rest.starts_with(' ') || rest.trim().is_empty() {
            continue;
        }
        let description = crate::translate::lang::trim(strip_memory_suffix(rest.trim()));
        out.push(BackendDevice {
            name: name.to_string(),
            description: description.to_string(),
        });
    }
    out
}

/// 去掉描述尾巴上的 `(\d+ MiB …)`（和 `server::strip_memory_suffix` 同一口径）。
fn strip_memory_suffix(name: &str) -> &str {
    if !name.ends_with(')') {
        return name;
    }
    for (i, b) in name.as_bytes().iter().enumerate() {
        if *b != b'(' {
            continue;
        }
        let rest = &name[i + 1..];
        let digits: usize = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            continue;
        }
        let after = rest[digits..].trim_start();
        let Some(after_mib) = after.strip_prefix("MiB") else {
            continue;
        };
        if after_mib.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let mut start = i;
        while start > 0 {
            let prev = name[..start].chars().next_back().unwrap();
            if prev.is_whitespace() {
                start -= prev.len_utf8();
            } else {
                break;
            }
        }
        return &name[..start];
    }
    name
}

/// 一次启动要传给 `llama-server` 的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub tier: Tier,
    /// 追加在基础参数后面的 argv（`--device X` / `-ngl N` / `--split-mode layer` / `-t N`）。
    pub args: Vec<String>,
    /// 额外环境变量（`GGML_HEXAGON_POWER_MODE` / `GGML_OPENCL_KERNEL_CACHE_DIR` / `LD_LIBRARY_PATH`）。
    pub env: Vec<(String, String)>,
}

/// 这台机器上某一档能不能用。
pub fn available(tier: Tier, devices: &[BackendDevice]) -> bool {
    let npu = devices.iter().any(|d| d.is_npu());
    let gpu = devices.iter().any(|d| d.is_opencl());
    match tier {
        Tier::Npu => npu,
        Tier::Gpu => gpu,
        Tier::NpuGpu => npu && gpu,
        // CPU 永远能跑（llama.cpp 自带 CPU 后端）
        Tier::Cpu => true,
    }
}

/// 从 [`Tier::Npu`] 开始，返回**能用**的档位（顺序不变）。CPU 一定在最后。
pub fn usable_tiers(devices: &[BackendDevice]) -> Vec<Tier> {
    [Tier::Npu, Tier::Gpu, Tier::NpuGpu, Tier::Cpu]
        .into_iter()
        .filter(|t| available(*t, devices))
        .collect()
}

/// 给某一档生成启动参数；这一档不可用时是 `None`（调用方接着降级）。
///
/// [`opencl_cache`]：OpenCL kernel 缓存目录（`null` 就不设，让 llama.cpp 用系统临时目录）。
pub fn launch_for(
    tier: Tier,
    devices: &[BackendDevice],
    opencl_cache: Option<&Path>,
    parallelism: u32,
) -> Option<Launch> {
    if !available(tier, devices) {
        return None;
    }
    let npu = devices.iter().find(|d| d.is_npu()).map(|d| d.name.clone());
    let mut args: Vec<String> = Vec::new();
    let mut env: Vec<(String, String)> = Vec::new();
    match tier {
        Tier::Npu => {
            let name = npu?;
            args.extend(["--device".into(), name, "-ngl".into(), "99".into()]);
            args.extend(["-t".into(), "4".into()]);
            env.push(("GGML_HEXAGON_POWER_MODE".into(), POWER_MODE.into()));
        }
        Tier::Gpu => {
            args.extend(["--device".into(), "GPUOpenCL".into(), "-ngl".into(), "99".into()]);
            args.extend(["-t".into(), "4".into()]);
        }
        Tier::NpuGpu => {
            let name = npu?;
            // 先 GPU 后 NPU：Hexagon 后端把 NPU 当 GPU 用，两个都是 "device"，顺序决定层怎么切。
            // GPU 在前是为了让第一层（token_embd）落在显存里，NPU 那边的地址窗口留给中后段。
            args.extend(["--device".into(), format!("GPUOpenCL,{name}")]);
            args.extend(["-ngl".into(), "99".into()]);
            args.extend(["--split-mode".into(), "layer".into()]);
            args.extend(["-t".into(), "4".into()]);
            env.push(("GGML_HEXAGON_POWER_MODE".into(), POWER_MODE.into()));
        }
        Tier::Cpu => {
            // 不给 --device = 不卸载任何层。线程数：可用核数的一半（夹 2..6），
            // 全部给满会和系统的调度抢核、手机上更烫更慢。
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            let threads = (cores / 2).clamp(2, 6);
            args.extend(["-ngl".into(), "0".into(), "-t".into(), threads.to_string()]);
        }
    }
    if matches!(tier, Tier::Gpu | Tier::NpuGpu) {
        if let Some(dir) = opencl_cache {
            env.push((
                "GGML_OPENCL_KERNEL_CACHE_DIR".into(),
                dir.to_string_lossy().to_string(),
            ));
        }
    }
    // `-np` 之外不需要别的：`-c` / `--host` / `--port` 由调用方统一给。
    let _ = parallelism;
    Some(Launch { tier, args, env })
}

/// `ctx.store` 里记的"上次降到哪一档 + 为什么"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    /// 下次从这个档位开始试。
    pub tier: Tier,
    /// 连续失败了几次（排障 / 界面文案用）。
    pub fails: u32,
    /// 记这笔时的 App 版本；版本变了就整条作废（升级后重新从 NPU 试）。
    pub app: String,
    /// 最近一次降级的原因（只截前 200 字，别把整段日志塞进 store）。
    pub reason: String,
}

impl Memory {
    /// 一个全新的记忆：从 NPU 开始。
    pub fn fresh() -> Memory {
        Memory {
            tier: Tier::Npu,
            fails: 0,
            app: String::new(),
            reason: String::new(),
        }
    }

    /// 读 store。**版本不一致 = 没读过**（App 升级后重新试 NPU）；坏 JSON 也按没读过算。
    pub fn load(store: &dyn StoreHost, app_version: &str) -> Memory {
        let Some(raw) = store.get(KEY_BACKEND) else {
            return Memory::fresh();
        };
        let Some(m) = parse_memory(&raw) else {
            return Memory::fresh();
        };
        if m.app != app_version {
            return Memory::fresh();
        }
        m
    }

    /// 写回 store（`tier` 是"下次从哪档开始"）。
    pub fn save(&self, store: &dyn StoreHost) {
        store.put(KEY_BACKEND, Some(&format_memory(self)));
    }

    /// 有 store 就写、没有就跳过（测试 / 只读场景）。
    pub fn save_if(&self, store: Option<&dyn StoreHost>) {
        if let Some(store) = store {
            self.save(store);
        }
    }

    /// 在这一档失败了：降到下一档并记一笔；已经是 CPU 就只记次数（没有下一档可降）。
    pub fn degrade(&self, failed: Tier, reason: &str, app_version: &str) -> Memory {
        let next = failed.next().unwrap_or(Tier::Cpu);
        Memory {
            tier: next,
            fails: self.fails.saturating_add(1),
            app: app_version.to_string(),
            reason: truncate_chars(reason, 200),
        }
    }
}

fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect()
}

/// 记忆的 JSON 形状（手写，不引 serde derive：字段少、格式要稳定）。
fn format_memory(m: &Memory) -> String {
    let mut out = String::from("{\"tier\":");
    crate::hosted::push_json_string(&mut out, m.tier.name());
    out.push_str(",\"fails\":");
    out.push_str(&m.fails.to_string());
    out.push_str(",\"app\":");
    crate::hosted::push_json_string(&mut out, &m.app);
    out.push_str(",\"reason\":");
    crate::hosted::push_json_string(&mut out, &m.reason);
    out.push('}');
    out
}

fn parse_memory(raw: &str) -> Option<Memory> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let tier = v.get("tier")?.as_str()?;
    Some(Memory {
        tier: Tier::from_name(tier),
        fails: v.get("fails").and_then(|f| f.as_u64()).unwrap_or(0) as u32,
        app: v.get("app").and_then(|a| a.as_str()).unwrap_or("").to_string(),
        reason: v.get("reason").and_then(|r| r.as_str()).unwrap_or("").to_string(),
    })
}

/// 输出看起来是不是坏了：空串、只有空白、或者替换字符 / 控制字符占了一大半。
///
/// 判"乱码"的口径保守：**只在明显坏掉时**降级（宁可多用一档正常的后端，也不要因为一句
/// 带 emoji 的译文就白白降级）。真的坏（HTP 内核写错内存）通常整句都是 `\u{FFFD}` 或空。
pub fn looks_broken(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    let total = t.chars().count();
    let bad = t
        .chars()
        .filter(|c| *c == '\u{FFFD}' || (*c).is_control() && *c != '\n' && *c != '\t')
        .count();
    total > 0 && bad * 3 >= total
}

