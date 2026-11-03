//! **`cookies.json` 的落盘 + 跨进程文件锁**。
//!
//! 但"文本放哪儿"还在宿主（`CookieFile`：桌面是 `NioCookieFile`，
//! 安卓是 `FileCookieFile`，锁是 `FileLocks` 的 `FileChannel.lock()`）。
//! 所以这一份把
//! 落盘的三个动作和那把锁全接过来了，Kotlin 只剩"告诉 Rust 文件在哪"。
//!
//! ## 和 Kotlin 的口径要一个字节都不差
//!
//! - **原子替换**：先写同目录下的临时文件（唯一名字，两个进程同时写不会互相写坏）、`sync_all()`、
//!   再 `rename` 覆盖（Linux 上 `rename(2)` 是原子的）。临时文件建出来就是 600。
//! - **600 权限**：`set_permissions(0o600)`。安卓没有 POSIX 权限那一层（应用沙箱管），失败不算错。
//! - **坏文件留底**：`<名字>.bad-<毫秒>`（和 Kotlin `quarantine` 的命名一样），留不了底就是错
//!   （宁可起不来也不覆盖用户的登录态）。
//! - **跨进程锁**：`<文件>.lock` 上的**整文件排他锁**。这一条最要紧——**必须用 `fcntl` 记录锁，
//!   不能用 `flock`**：Kotlin 的 `FileChannel.lock()` 在 Linux 上就是 `fcntl(F_SETLK)`，
//!   而 `flock` 和 `fcntl` 在 Linux 上是**两套互不可见的锁**（同一个文件，一边 flock 一边 fcntl
//!   会同时"拿到"）。老版本 Kotlin 进程（cli）和新版 Rust 实现要能同时开着、互相不丢改动，
//!   所以这边也走 `fcntl(F_SETLKW)`（阻塞排队，不是抢不到就报错）。
//!
//! ## 为什么还要一把进程内的锁
//!
//! `fcntl` 的锁是**进程级**的：同一个进程里两个线程对同一个文件各拿一把，谁也不挡谁（第二次
//! 只是把同一把锁"再拿一次"）；而且这个进程**关掉任何一个指向该文件的 fd** 就会把锁全放掉。
//! 所以进程内先过一把按路径分的 `Mutex`（等于 Kotlin `FileLocks` 里那层 `ReentrantLock`），
//! 再拿 `fcntl` 锁——两层都拿到才算拿到。这样"两个 OkHttp 线程同时写回"和"两个进程同时写回"
//! 都是排队的。
//!
//! ## 时钟
//!
//! 留底文件名里的毫秒数由**调用方注入**（[`FileStorage::new`] 的第二个参数 = 打开罐子那一刻的
//! `now_ms`）：这一层不读系统时钟，和其余模块一样（`quarantine` 只在打开时发生）。

use std::path::{Path, PathBuf};

use super::CookieStorage;

/// 一个真实的 `cookies.json`（桌面 `~/.config/feedhub/cookies.json`、安卓 `filesDir/cookies.json`）。
///
/// 三个动作的行为见模块头。`CookieStorage` 的 `locked` 用 `<文件>.lock` 上的 `fcntl` 排他锁。
#[derive(Debug, Clone)]
pub struct FileStorage {
    path: PathBuf,
    /// 打开罐子那一刻的毫秒数：留底文件的名字用它（`cookies.json.bad-<毫秒>`）。
    now_ms: i64,
}

impl FileStorage {
    /// `path` 是 `cookies.json` 本身；`now_ms` 只用来给留底文件起名。
    pub fn new(path: impl Into<PathBuf>, now_ms: i64) -> Self {
        Self {
            path: path.into(),
            now_ms,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl CookieStorage for FileStorage {
    fn read(&self) -> Option<String> {
        match std::fs::read(&self.path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => Some(text),
                // 不是 UTF-8：当"读不懂"（后续走留底），和 Kotlin `readText()` 的行为一致
                Err(_) => Some(String::new()),
            },
            Err(_) => None,
        }
    }

    fn write(&self, text: &str) -> Result<(), String> {
        write_private(&self.path, text.as_bytes())
    }

    fn quarantine(&self) -> Option<String> {
        let backup = self.path.with_file_name(format!(
            "{}.bad-{}",
            self.path.file_name()?.to_string_lossy(),
            self.now_ms
        ));
        match std::fs::rename(&self.path, &backup) {
            Ok(()) => Some(backup.to_string_lossy().to_string()),
            Err(_) => None,
        }
    }

    fn locked(&self, block: &mut dyn FnMut()) {
        let lock_path = lock_file_for(&self.path);
        // 进程内那一层（fcntl 的锁是进程级的，挡不住同进程的第二个线程）
        let entry = registry_entry(&lock_path);
        let guard = entry.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(not(target_os = "android"))]
        let _os = OsFileLock::acquire(&lock_path);
        block();
        #[cfg(not(target_os = "android"))]
        drop(_os);
        drop(guard);
    }

    fn file_stamp(&self) -> Option<(u64, u64, i64, i64, u64)> {
        let meta = std::fs::metadata(&self.path).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some((
                meta.dev(),
                meta.ino(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.len(),
            ))
        }
        #[cfg(not(unix))]
        {
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| (d.as_secs() as i64, d.subsec_nanos() as i64))
                .unwrap_or((0, 0));
            Some((0, 0, mtime_ns.0, mtime_ns.1, meta.len()))
        }
    }
}

/// 被保护文件 [path] 的锁文件：`cookies.json` → `cookies.json.lock`（和 Kotlin `FileLocks.lockFileFor` 一样）。
pub fn lock_file_for(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    path.with_file_name(format!("{name}.lock"))
}

/// 进程内那层锁的注册表（按锁文件的绝对路径分）。数量很少（每人几个文件），不清理。
fn registry_entry(lock_path: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let map = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let key = std::path::absolute(lock_path).unwrap_or_else(|_| lock_path.to_path_buf());
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
}

// ===========================================================================
// 原子写（600 + 临时文件 + fsync + rename）
// ===========================================================================

/// 整份覆盖写：**先写临时文件、`sync_all`、再原子换名**。
///
/// 直接往目标文件上写的话，进程在写到一半时被杀（国产 ROM 杀后台、崩溃、断电）磁盘上就留下半截 JSON，
/// 下次启动只能改名留底后当空罐——所有平台的登录态一起没。换名是原子的：读到的要么是旧的一整份，
/// 要么是新的一整份。
///
/// 临时文件用 `<名字>.tmp-<pid>-<计数>`（唯一，两个进程同时写不会互相写坏），建出来就是 600；
/// 写完 `sync_all()` 再换名（免得换名成功而内容是空的）；失败时把临时文件删掉。
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "cookies.json".to_string());
    let tmp = path.with_file_name(format!(
        "{name}.tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<(), String> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp).map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;
        // 600：建出来就设了，这里再兜一次（有些文件系统忽略 create 的 mode）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        // fsync：换名成功但内容还在页缓存里时断电会丢（安卓上 `out.fd.sync()` 就是这一条）
        let _ = file.sync_all();
        drop(file);
        match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                // 少数文件系统不允许换名覆盖已存在的文件：退回"删掉再换"
                let _ = std::fs::remove_file(path);
                std::fs::rename(&tmp, path).map_err(|_| e.to_string())
            }
        }
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

// ===========================================================================
// fcntl 记录锁（和 Kotlin 的 FileChannel.lock() 同一套）
// ===========================================================================

/// 一把已经拿到的 `fcntl` 整文件排他锁（`Drop` 时放掉并关 fd）。
///
/// 只在本进程内用；拿锁是**阻塞**的（`F_SETLKW`），和 Kotlin `FileChannel.lock()` 一样排队。
#[cfg_attr(target_os = "android", allow(dead_code))]
struct OsFileLock {
    #[cfg(unix)]
    file: std::fs::File,
}

#[cfg_attr(target_os = "android", allow(dead_code))]
impl OsFileLock {
    /// 拿锁。拿不到（目录不可写、fd 用光）就**返回 None 继续跑**——和 Kotlin `FileLocks.acquire` 抛错
    /// 不同，但这里"锁文件建不出来"意味着连 `cookies.json` 都写不进去，后面的写回会报自己的错，
    /// 不必在锁这一步就把 App 拦住（安卓上尤其：私有目录里建不出 `.lock` 是不可想象的）。
    fn acquire(lock_path: &Path) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if let Some(parent) = lock_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(lock_path)
                .ok()?;
            // 整文件排他锁（l_start = 0、l_len = 0 = 到文件尾）
            let mut fl: libc::flock = unsafe { std::mem::zeroed() };
            fl.l_type = libc::F_WRLCK as libc::c_short;
            fl.l_whence = libc::SEEK_SET as libc::c_short;
            fl.l_start = 0;
            fl.l_len = 0;
            loop {
                let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLKW, &fl) };
                if rc == 0 {
                    return Some(Self { file });
                }
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue; // 被信号打断：接着排队
                }
                // 其余错误（ENOLCK / EACCES…）：不拿锁继续，免得整个 App 起不来（见上面的说明）
                return None;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = lock_path;
            None
        }
    }
}

#[cfg(unix)]
impl Drop for OsFileLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = libc::F_UNLCK as libc::c_short;
        fl.l_whence = libc::SEEK_SET as libc::c_short;
        fl.l_start = 0;
        fl.l_len = 0;
        // 放锁（关 fd 也会放，但显式放一次更清楚；失败无所谓，drop 紧接着就 close）
        unsafe {
            libc::fcntl(self.file.as_raw_fd(), libc::F_SETLK, &fl);
        }
    }
}

