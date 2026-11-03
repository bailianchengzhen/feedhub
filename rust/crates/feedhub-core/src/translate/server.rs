//! `llama-server` 的启动策略 + 进程管理（`app/.../translate/VulkanDevices.kt` + `LlamaServerPolicy.kt`
//! + `LlamaTranslationEngine.kt` 里除 HTTP 之外的部分）。
//!
//! ## 规则：**只用 AMD 的 GPU（RADV 驱动），不碰独显**
//!
//! 两层保险：
//! 1. 启动 server 前用 [`vk_driver_env`] 只让 Vulkan loader 加载 RADV（`VK_DRIVER_FILES` 和旧名
//!    `VK_ICD_FILENAMES` 都设成 radeon_icd.json），NVIDIA 驱动**根本不会出现**在设备列表里；
//! 2. 同样的环境变量跑一次 `--list-devices`，[`choose_device`] **按名字**（含 `RADV` 或 `Radeon`）挑出
//!    `-dev VulkanN`；挑不到就报错、**不启动**，绝不退回别的 GPU。
//!
//! 以前靠 `GGML_VK_VISIBLE_DEVICES=0` + 枚举顺序"碰巧"落在 AMD 核显 上；驱动一变顺序就可能反过来。
//! 所以这里**同时**把 `GGML_VK_VISIBLE_DEVICES` 从环境里删掉。
//!
//! ## 安卓：设备由 [`SpawnSpec`] 给，不走 Vulkan 名字匹配
//!
//! 手机上没有 RADV，那套"锁核显"无从谈起。安卓上同一个 `llama-server` 里编进了
//! CPU / GPUOpenCL / HTP 三个后端，**选哪个、怎么卸层**由 [`crate::translate::device`] 算出来
//! （NPU → GPU → NPU+GPU → CPU 的候选顺序 + 降级记忆），这一层只负责把它拼成 argv / 环境变量：
//!
//! | 谁 | [`SpawnSpec::extra_args`] | [`SpawnSpec::env`] |
//! |---|---|---|
//! | 桌面 | `-ngl 99 -dev VulkanN` | [`vk_driver_env`]（只加载 RADV 的三个变量） |
//! | 手机 NPU | `--device HTP0 -ngl 99 -t 4` | [`mobile_env`] + `GGML_HEXAGON_POWER_MODE` |
//! | 手机 GPU | `--device GPUOpenCL -ngl 99 -t 4` | [`mobile_env`] + OpenCL 内核缓存 / `OCL_ICD_FILENAMES` |
//! | 手机混合 | `--device GPUOpenCL,HTP0 -ngl 99 --split-mode layer -t 4` | 同上 |
//! | 手机 CPU | `-ngl 0 -t N` | [`mobile_env`] |
//!
//! 安卓 CPU 那条**不用 `-dev CPU`**：`llama-server` 的 `-dev` 只接受 `--list-devices` 里
//! **非 CPU** 的设备（`common/arg.cpp` 的 `parse_device_list` 见到 CPU 设备直接 `invalid device: CPU`），
//! 所以"只用 CPU"的正确写法是 `-ngl 0`。
//!
//! ## 进程
//!
//! 启一个子进程（`std::process`）：`-m 模型 -c 每槽上下文×槽数 -np 槽数 --host 127.0.0.1 --port 端口`
//! + [`SpawnSpec::extra_args`]，工作目录 = server 所在目录，stdout / stderr 追加进
//! `<缓存目录>/llama-server.log`（超过 8 MB 先删掉重来）。
//!
//! 停：**先 SIGTERM**（Java 的 `Process.destroy()` 就是它；用 `libc::kill`，std 的 `Child::kill` 是 SIGKILL，
//! 语义不一样），等 3 秒（App 退出时 300 ms），还没退再 SIGKILL 并 `wait()` 回收（不然留僵尸）。

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// RADV（AMD 开源 Vulkan 驱动）的 ICD 清单；只加载它，NVIDIA 的 icd 不出现在列表里。
pub const RADV_ICD_FILE: &str = "/usr/share/vulkan/icd.d/radeon_icd.json";

/// 一个 Vulkan 设备（`llama-server --list-devices` 输出里的一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub index: i32,
    /// 名字（末尾的 `(8160 MiB, 8016 MiB free)` 去掉，但 `(RADV PHOENIX)` 这种驱动名保留）。
    pub name: String,
}

impl Device {
    /// `llama-server -dev` 参数里的设备名。
    pub fn argument(&self) -> String {
        format!("Vulkan{}", self.index)
    }
}

/// Java 正则里 `\s` 的口径（`[ \t\n\x0B\f\r]`，**不是** Unicode 空白）。
///
/// Kotlin 那三个 `Regex` 用的是 `java.util.regex` 的默认（ASCII）语义，`parseLlamaDevices` 的对拍
/// 要连这个也一致，所以不能直接用 Rust 的 `is_whitespace`（它会认 NBSP / U+2007 之类）。
fn is_regex_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// 解析 `llama-server --list-devices` 的输出（Kotlin `parseLlamaDevices`）。
///
/// 只认 `VulkanN: 名字` 这种行（别的后端如 `CUDA0:` 不算），名字末尾的 `(8160 MiB, 8016 MiB free)`
/// 内存信息去掉，但 `(RADV PHOENIX)` 这种驱动名保留。
pub fn parse_devices(output: &str) -> Vec<Device> {
    let mut out = Vec::new();
    for line in output.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let rest = line.trim_start_matches(is_regex_space);
        let Some(rest) = rest.strip_prefix("Vulkan") else {
            continue;
        };
        let digits: usize = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            continue;
        }
        let Ok(index) = rest[..digits].parse::<i32>() else {
            continue;
        };
        let after_digits = &rest[digits..];
        // `\s*:\s*(.+?)\s*$`
        let after_ws = after_digits.trim_start_matches(is_regex_space);
        let Some(after_colon) = after_ws.strip_prefix(':') else {
            continue;
        };
        // `(.+?)` 至少要一个字符：`Vulkan1:` 后面什么都没有就不算设备，`Vulkan1:   `（空格）**算**，
        // 名字 trim 之后是空串。
        if after_colon.is_empty() {
            continue;
        }
        let name = after_colon
            .trim_start_matches(is_regex_space)
            .trim_end_matches(is_regex_space);
        let name = strip_memory_suffix(name);
        // 最后那一步是 Kotlin 的 `String.trim()`（含 NBSP 类），不是正则的 `\s`
        out.push(Device {
            index,
            name: crate::translate::lang::trim(name).to_string(),
        });
    }
    out
}

/// 去掉尾巴上的 `(\d+ MiB …)`（Kotlin `Regex("""\s*\(\d+\s*MiB\b.*\)$""")`）。
fn strip_memory_suffix(name: &str) -> &str {
    if !name.ends_with(')') {
        return name;
    }
    let bytes = name.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'(' {
            continue;
        }
        let rest = &name[i + 1..];
        let digits: usize = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            continue;
        }
        let after = rest[digits..].trim_start_matches(is_regex_space);
        let Some(after_mib) = after.strip_prefix("MiB") else {
            continue;
        };
        // `\b`：MiB 后面不能紧跟单词字符
        if after_mib.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        // 匹配从 `\s*` 的起点开始算（Kotlin 是 `name.substring(0, it.range.first)`）
        let mut start = i;
        while start > 0 {
            let prev = name[..start].chars().next_back().unwrap();
            if is_regex_space(prev) {
                start -= prev.len_utf8();
            } else {
                break;
            }
        }
        return &name[..start];
    }
    name
}

/// 按名字挑 AMD 的 GPU：名字里含 `RADV`（驱动名）或 `Radeon` 的 Vulkan 设备。挑不到返回 `None`
/// （调用方必须报错、不启动）。
///
/// 不按序号挑，也不因为"只有一块设备"就随便用——那正是要修的问题。
pub fn choose_device(devices: &[Device]) -> Option<&Device> {
    devices.iter().find(|d| {
        let lower = d.name.to_lowercase();
        lower.contains("radv") || lower.contains("radeon")
    })
}

/// 启动 `llama-server` 时的 Vulkan 环境变量（不含 `GGML_VK_VISIBLE_DEVICES`：按名字挑设备，不按序号）。
///
/// `LD_LIBRARY_PATH` 设成 server 所在目录（llama.cpp 的动态库在那儿）。
pub fn vk_driver_env(server: &Path, icd_file: &Path) -> Vec<(String, String)> {
    let dir = server.parent().unwrap_or_else(|| Path::new("")).to_string_lossy().to_string();
    let icd = icd_file.to_string_lossy().to_string();
    vec![
        // 新名 / 旧名都设：loader 版本不同认识的变量不同
        ("VK_DRIVER_FILES".to_string(), icd.clone()),
        ("VK_ICD_FILENAMES".to_string(), icd),
        ("LD_LIBRARY_PATH".to_string(), dir),
    ]
}

/// 手机上启动 `llama-server` 的环境变量。
///
/// 两个都要：
/// - `LD_LIBRARY_PATH`：`libggml-*.so` / `libllama*.so` 和 server 放在同一个目录；
/// - `ADSP_LIBRARY_PATH`：Hexagon 后端用 `remote_session_control` 把 `libggml-htp-v79.so`
///   交给 FastRPC 的 DSP 侧加载器，它**按这个变量**找 skel。
///
/// **不设** `GGML_HEXAGON_DEVICES`：设备由 `--device` 指定（[`crate::translate::device`] 算出来的），
/// 两条路同时设会打架。**不设** Vulkan 的 `VK_*`：安卓这份构建没有 Vulkan 后端。
pub fn mobile_env(server: &Path) -> Vec<(String, String)> {
    let dir = server.parent().unwrap_or_else(|| Path::new("")).to_string_lossy().to_string();
    vec![
        ("LD_LIBRARY_PATH".to_string(), dir.clone()),
        ("ADSP_LIBRARY_PATH".to_string(), dir),
    ]
}

/// 跑一次 `llama-server --list-devices`，返回**原始输出**（stdout + stderr 拼起来）。
///
/// 空 `extra_env` = 用当前进程的环境。进程起不来返回 `None`。
pub fn list_devices_raw(server: &Path, extra_env: &[(String, String)]) -> Option<String> {
    if !server.is_file() {
        return None;
    }
    let mut command = Command::new(server);
    command.arg("--list-devices");
    if let Some(dir) = server.parent() {
        command.current_dir(dir);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.env_remove("GGML_VK_VISIBLE_DEVICES");
    for (name, value) in extra_env {
        command.env(name, value);
    }
    let mut child = spawn_retrying_busy(&mut command).ok()?;
    // stderr 单独抽干（Java 是 redirectErrorStream(true)，两边都算进输出）。
    // 用一个短命的线程读 stderr：单线程"先读完 stdout 再读 stderr"在 stderr 写满 64 KB 时会互等。
    let mut stderr = child.stderr.take();
    let drain = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });
    let mut output = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut output);
    }
    let err = drain.join().unwrap_or_default();
    output.push_str(&err);
    let _ = child.wait();
    Some(output)
}

/// `Command::spawn`，撞上 `ETXTBSY`（"Text file busy"）时短暂重试。
///
/// 刚写完的可执行文件（测试里现写的假 `llama-server` 脚本）在多线程进程里可能还被**别的线程刚 fork、
/// 还没 exec 的子进程**继承着写句柄，这时 exec 它会得到 `ETXTBSY`。窗口只有 fork→exec 那一下，
/// 等几毫秒再试就过。实测不重试时 `translate::` 并行跑 15 遍挂 5 遍。
fn spawn_retrying_busy(command: &mut Command) -> std::io::Result<std::process::Child> {
    let mut tries = 0;
    loop {
        match command.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

/// 用 [`vk_driver_env`] 跑一次 `--list-devices`，返回设备列表。
///
/// 进程起不来 / server 或 ICD 文件不在时返回 `None`（调用方不启动 server）。
/// 让 NVIDIA 驱动根本不出现，所以正常情况这里只有一块 RADV 设备。
pub fn list_devices(server: &Path, icd_file: &Path) -> Option<Vec<Device>> {
    if !server.is_file() || !icd_file.is_file() {
        return None;
    }
    let output = list_devices_raw(server, &vk_driver_env(server, icd_file))?;
    Some(parse_devices(&output))
}

/// 找一个空闲端口（`ServerSocket(0)` 的替身：绑一下 127.0.0.1:0 再放开）。
///
/// 和 Kotlin 一样有"放开到 server 绑上去"之间的窗口；本地端口撞车概率极低，
/// 真撞了表现为 server 起不来（有明确的错误信息），不会静默跑到别的端口。
pub fn free_port() -> Option<u16> {
    TcpListener::bind(("127.0.0.1", 0)).ok().and_then(|l| l.local_addr().ok()).map(|a| a.port())
}

/// `llama-server` 什么时候该停 / 重启（Kotlin `LlamaServerPolicy`）。
///
/// 两条规则：空闲 5 分钟、累计 150 次请求。
/// **重启只在没有请求在跑时发生**——那一半由 [`crate::translate::engine::Engine`] 的 in-flight 计数保证。
pub struct Policy {
    max_requests: u32,
    idle_ms: i64,
    requests: u32,
    last_used: i64,
}

impl Policy {
    pub fn new(max_requests: u32, idle_ms: i64) -> Policy {
        Policy {
            max_requests,
            idle_ms,
            requests: 0,
            last_used: 0,
        }
    }

    pub fn request_count(&self) -> u32 {
        self.requests
    }

    /// server 刚起来 / 刚重启：计数和时间戳归零。
    pub fn mark_started(&mut self, now_ms: i64) {
        self.requests = 0;
        self.last_used = now_ms;
    }

    /// 成功完成一次请求。
    pub fn note_request(&mut self, now_ms: i64) {
        self.requests += 1;
        self.last_used = now_ms;
    }

    /// 累计请求数到上限：该重启了。
    pub fn should_restart(&self) -> bool {
        self.requests >= self.max_requests
    }

    /// 空闲超过 `idle_ms`（`idle_ms <= 0` 表示随进程常驻、不做空闲回收；还没发过请求时也不算）。
    pub fn idle_too_long(&self, now_ms: i64) -> bool {
        self.idle_ms > 0 && self.requests > 0 && now_ms - self.last_used > self.idle_ms
    }
}

/// 一次启动的全部参数（桌面的 AMD 核显 和手机的 NPU/GPU 都走这一份）。
///
/// 桌面上 [`extra_args`] 是 `["-dev", "Vulkan0"]`、[`env`] 是 [`vk_driver_env`]；
/// 手机上 [`extra_args`] 是 [`crate::translate::device::Launch::args`]、
/// [`env`] 是 [`mobile_env`] + [`crate::translate::device::Launch::env`]。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnSpec {
    /// 追加在基础参数后面的 argv（`-dev Vulkan0` / `--device HTP0` / `-ngl 99` / …）。
    pub extra_args: Vec<String>,
    /// 额外环境变量（`LD_LIBRARY_PATH` / `ADSP_LIBRARY_PATH` / `GGML_HEXAGON_POWER_MODE` / …）。
    pub env: Vec<(String, String)>,
    /// 要 `env_remove` 掉的变量名（桌面要删 `GGML_VK_VISIBLE_DEVICES`，按名字挑设备不按序号）。
    pub env_remove: Vec<String>,
    /// 记在 [`Process::device`] 里的名字（排障 / 设置页显示）。
    pub device: String,
}

impl SpawnSpec {
    /// 桌面的那条：`-dev VulkanN -ngl 99` + 只加载 RADV 的三个环境变量。
    pub fn desktop(device: &Device, server: &Path, icd_file: &Path) -> SpawnSpec {
        SpawnSpec {
            extra_args: vec![
                "-ngl".to_string(),
                "99".to_string(),
                "-dev".to_string(),
                device.argument(),
            ],
            env: vk_driver_env(server, icd_file),
            env_remove: vec!["GGML_VK_VISIBLE_DEVICES".to_string()],
            device: device.name.clone(),
        }
    }
}

/// 一个跑着的 `llama-server`（没有进程时是 `None`）。
pub struct Process {
    child: Child,
    /// 起它的时候用的模型 / server 路径（设置变了要重启）。
    pub model: String,
    pub server: String,
    /// 实际用的那一档 / 设备名（桌面 = `VulkanN` 那个设备的名字；安卓 = `npu` / `gpu` / `npu+gpu` / `cpu`）。
    /// 只用于错误文案 / 日志，不参与任何判断。
    pub device: String,
}

impl Process {
    /// 起一个 `llama-server`。
    ///
    /// 设备 / 卸层 / 环境全都由 [`SpawnSpec`] 给（桌面 [`SpawnSpec::desktop`]，
    /// 安卓 [`crate::translate::device::launch_for`]）：这一层不再校验 ICD、也不认识"后端"这个概念。
    pub fn spawn(
        server: &Path,
        model: &Path,
        spec: &SpawnSpec,
        port: u16,
        context_per_slot: u32,
        slots: u32,
        threads: Option<u32>,
        log: &Path,
    ) -> Result<Process, String> {
        if !server.is_file() {
            return Err(format!("找不到 llama-server：{}", server.display()));
        }
        if !model.is_file() {
            return Err(format!("找不到翻译模型：{}", model.display()));
        }
        // 日志超过 8 MB 就删掉重来（不然一直长）
        if let Ok(meta) = std::fs::metadata(log) {
            if meta.len() > 8 * 1024 * 1024 {
                let _ = std::fs::remove_file(log);
            }
        }
        if let Some(parent) = log.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .map_err(|e| format!("打不开翻译服务的日志 {}：{e}", log.display()))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("打不开翻译服务的日志 {}：{e}", log.display()))?;

        let mut command = Command::new(server);
        command
            .arg("-m")
            .arg(model)
            // 每个槽都要够放一条正文（4096 是 -np 1 时的原值），所以总上下文按槽数放大
            .arg("-c")
            .arg((context_per_slot * slots).to_string())
            .arg("-np")
            .arg(slots.to_string())
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            // 卸层数 / 设备名由 SpawnSpec 给：桌面上是 `-dev VulkanN`，手机上还有 `-ngl` / `--device`
            .args(&spec.extra_args);
        // 桌面的 `-t` 来自 tuning（`None` = 不传）；手机上它已经在 `extra_args` 里了（`launch_for` 按档位算的），
        // 这里不能重复给
        if let Some(t) = threads {
            if !spec.extra_args.iter().any(|a| a == "-t") {
                command.arg("-t").arg(t.to_string());
            }
        }
        if let Some(dir) = server.parent() {
            command.current_dir(dir);
        }
        command.stdout(Stdio::from(out)).stderr(Stdio::from(err));
        for name in &spec.env_remove {
            command.env_remove(name);
        }
        for (name, value) in &spec.env {
            command.env(name, value);
        }
        let child = spawn_retrying_busy(&mut command)
            .map_err(|e| format!("llama-server 起不来（{}）：{e}", server.display()))?;
        Ok(Process {
            child,
            model: model.to_string_lossy().to_string(),
            server: server.to_string_lossy().to_string(),
            device: spec.device.clone(),
        })
    }

    /// 还活着吗（`Process.isAlive`）。
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// 提前退出时的退出码（还活着时是 `None`）。
    pub fn exit_code(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => status.code(),
            _ => None,
        }
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// 停：先 SIGTERM，等一会儿，还不退就 SIGKILL（`force` 时只等 300 ms）。
    ///
    /// 返回 `true` = 正常（SIGTERM 之后自己退了），`false` = 强杀的。
    pub fn stop(&mut self, force: bool) -> bool {
        let wait_ms = if force { 300 } else { 3_000 };
        #[cfg(unix)]
        unsafe {
            // Java 的 Process.destroy() 在 Linux 上就是 SIGTERM
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_millis(wait_ms as u64);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) => {}
                Err(_) => break,
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        false
    }

    /// 只回收，不杀（已经自己退了的情况）。
    pub fn reap(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => status.code(),
            _ => None,
        }
    }
}

/// 兜底：`Process` 被丢掉时**一定**把子进程收掉。
///
/// 正常路径（[`Process::stop`] / [`crate::translate::engine::Engine::close`]）都会先 SIGTERM 再 wait；
/// 这个 Drop 是为了"正常路径没走到"的情况——测试里断言失败 panic、或者别的 early return。
/// 没有它就会留一个挂着的 `llama-server`。SIGKILL 免得 Drop 里阻塞。
impl Drop for Process {
    fn drop(&mut self) {
        // try_wait 先看一眼：已经退了的就别再发信号（pid 可能已经被复用）
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// 日志文件路径（`<缓存目录>/llama-server.log`）。
pub fn log_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("llama-server.log")
}

