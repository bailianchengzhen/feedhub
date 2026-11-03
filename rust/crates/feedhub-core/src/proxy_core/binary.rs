//! sing-box 可执行文件：**定位 + 运行时 sha256 校验 + 版本号**（`core/proxy/SingBoxBinary.kt` 的 Rust 版）。
//!
//! 二进制不进仓库（80 MB 太大）：桌面构建时用 gradle 属性 `-Pfeedhub.singbox.dir=…` 指定，
//! 运行时按 [`locate`] 的顺序找；安卓只认 `nativeLibraryDir/libsingbox.so`（[`is_android_layout`]）。
//!
//! `locate` 就是一条"按什么顺序找"
//! 的策略，所以搬了。**宿主仍然传一个显式的候选路径进来**（打包后的 resources 目录 / `nativeLibraryDir`
//! 那种只有宿主知道，`compose.application.resources.dir` 这种系统属性也是宿主的）——Rust 只做
//! "按顺序挑第一个存在的普通文件"这件事，以及 sha256 / 版本号。
//!
//! sha256 的期望值也搬进来了：`arm64 → 安卓那份`、`x86_64 → linux 那份`，和 Kotlin 的
//! `expectedSha256` 一致；宿主可以用显式参数覆盖（`FEEDHUB_SINGBOX_SHA256` 由宿主读环境变量后传进来，
//! 因为**读环境变量是宿主的事**——安卓上 `System.getenv` 和桌面上不是一回事）。

use std::path::{Path, PathBuf};

/// sing-box 的版本（官方 release 拷来的那一份）。
pub const VERSION: &str = "1.14.3";

/// linux-amd64 那份的 sha256。
pub const SHA256_LINUX_AMD64: &str = "768d31fa7f68debc067bb4626539498a1e8751d36a7b99e1a54bee6a23815f93";

/// 安卓 arm64 那份的 sha256。
pub const SHA256_ANDROID_ARM64: &str = "64324301d1049474f6010b5a015d3143271a8df83bae83ae8814167ac8f74f5a";

/// 显式指定二进制的环境变量名（**读它的是宿主**，这里只留名字给错误文案用）。
pub const ENV_PATH: &str = "FEEDHUB_SINGBOX";

/// 换一份自己编译的二进制时用这个环境变量给出它的 sha256（同样是宿主读）。
pub const ENV_SHA256: &str = "FEEDHUB_SINGBOX_SHA256";

/// 找二进制。
///
/// `candidates` 是宿主按自己的优先级排好的候选（显式参数、环境变量、App resources、用户数据目录、
/// 开发路径……），**按顺序取第一个存在的普通文件**。
/// 目录当作"里面的 `sing-box`"（Kotlin 的 `asBinary` 就是这么做的）。
///
/// `extra` 里放宿主不知道该放哪的候选（比如安卓的 `nativeLibraryDir/libsingbox.so`），
/// 它们排在 `candidates` **前面**：安卓那边"只认 nativeLibraryDir"是硬要求。
pub fn locate(candidates: &[PathBuf], extra: &[PathBuf]) -> Option<PathBuf> {
    for candidate in extra.iter().chain(candidates.iter()) {
        let binary = as_binary(candidate);
        if binary.is_file() {
            return Some(binary);
        }
    }
    None
}

/// 环境变量给的是目录时补上文件名 `sing-box`；是文件就原样。
pub fn as_binary(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("sing-box")
    } else {
        path.to_path_buf()
    }
}

/// 这份二进制**应该**是哪个哈希：`arch` 用宿主报上来的（`std::env::consts::ARCH` 或
/// `System.getProperty("os.arch")` 都可以），认不出来就是 `None` = 不校验。
///
/// 显式 `override_sha256` 优先（宿主从 `FEEDHUB_SINGBOX_SHA256` 读出来传进去）。
pub fn expected_sha256(arch: &str, override_sha256: Option<&str>) -> Option<String> {
    if let Some(sha) = override_sha256.filter(|s| !crate::hosted::is_kotlin_blank(s)) {
        return Some(sha.to_lowercase());
    }
    match arch.to_lowercase().as_str() {
        "aarch64" | "arm64" => Some(SHA256_ANDROID_ARM64.to_string()),
        "amd64" | "x86_64" => Some(SHA256_LINUX_AMD64.to_string()),
        _ => None,
    }
}

/// 文件的 sha256（小写十六进制）。读不了就是 `None`（错误里不带文件内容）。
pub fn sha256(path: &Path) -> Option<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return None,
        }
    }
    Some(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// 校验哈希。返回 `None` = 通过，否则是"拒绝运行"的原因（原因里不含机密）。
///
/// 文案和 Kotlin 版逐字一样（界面上直接显示）。
pub fn verify(path: &Path, expected: Option<&str>) -> Option<String> {
    let expected = expected?;
    if !path.is_file() {
        return Some(format!("找不到 sing-box 可执行文件：{}", path.display()));
    }
    let Some(actual) = sha256(path) else {
        return Some("读不了 sing-box（IOException）".to_string());
    };
    if actual.eq_ignore_ascii_case(expected) {
        return None;
    }
    Some(format!(
        "sing-box 的 sha256 不对（期望 {}…，实际 {}…）：拒绝运行。换过自己编译的二进制时，用环境变量 {} 给出它的哈希。",
        &expected[..expected.len().min(12)],
        &actual[..actual.len().min(12)],
        ENV_SHA256
    ))
}

/// 读版本号（`sing-box version` 的第一行）。读不出来返回 `None`。
///
/// **进程的启动 / 等待 / 超时都在这里**。超时会 kill 掉。
pub fn version(path: &Path, timeout_ms: u64) -> Option<String> {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    let mut child = std::process::Command::new(path)
        .arg("version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
    });
    let timeout = std::time::Duration::from_millis(timeout_ms.max(1));
    let line = match rx.recv_timeout(timeout) {
        Ok(line) => line,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    let _ = child.wait();
    let line = line.trim();
    let version = line.strip_prefix("sing-box version ").unwrap_or(line).trim();
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

