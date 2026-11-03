//! **从本机 Chrome 导入 Cookie 的整条路**。
//!
//! 读库和问钥匙环还在宿主
//! （`ChromeCookieImporter`：复制库文件、`androidx.sqlite` 读六列、`ProcessBuilder` 跑
//! `kwallet-query`）。理由也写在那儿：**"读文件"不是留宿主的理由**
//! （通用说明里 Rust 用标准库读写文件是允许的）。所以现在：
//!
//! 1. `profile/Cookies` 复制到私有临时目录（Chrome 运行时锁着这个库）——`std::fs`，目录 700，
//!    走的时候整目录删掉；
//! 2. 用 **`rusqlite`**（这个仓库已经为了资料库编进 `.so` 的那个）只读打开副本，读
//!    `SELECT value FROM meta WHERE key='version'` 和 `cookies` 表的六列；
//! 3. 需要 `v11` 的密码时跑 `kwallet-query` / `secret-tool`（`std::process`，超时 120 秒、
//!    读 stdout、丢掉 stderr）——和 Kotlin 那两行命令、那个超时、那个"拿不到就报 NoKey"一样；
//!    库里**没有** `v11` 行时一个命令都不跑（Kotlin 那边也是 `lazy`，"用不到不弹框"）；
//! 4. 解密 / 收哪些 / 过期 / 分组还是 [`super::import_rows`]；
//! 5. 临时目录删掉。
//!
//! **密钥绝不外泄**：密码只在这个函数里从 `Child` 的 stdout 读出来、直接喂给 `derive_key`，
//! 不打印、不落盘、不进错误文案；错误里只有路径 / 命令名 / 行数。
//!
//! 测试不碰真的 Chrome 库、不碰钥匙环（见 `tests.rs` 的 `import_reads_a_fake_sqlite_db`）：
//! 夹具是用 rusqlite 现造的假 `Cookies` 库（`peanuts` 那份 `v10` 密文），密码命令换成
//! `printf` 之类不碰钥匙环的子进程。

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use super::{import_rows, CookieImportSpec, ChromeCookieRow, ImportOutcome};

/// 问钥匙环的超时（KWallet 第一次会弹授权框，给用户留时间）：和 Kotlin 那行
/// `p.waitFor(120, TimeUnit.SECONDS)` 一样。
pub const PASSWORD_TIMEOUT: Duration = Duration::from_secs(120);

/// `v10` 的密码：没有钥匙环时 Chrome 在 Linux 上一直用这个固定值。
pub const PASSWORD_V10: &str = "peanuts";

/// 导入失败的原因。**文案里不带任何 Cookie 的值 / key / 密码**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChromeImportError {
    /// 找不到 Chrome 的 Cookie 库（profile 目录里没有 `Cookies`）。
    MissingDb { path: String },
    /// 打不开 / 读不了那个 SQLite 库（被别的进程锁着、不是 SQLite、表结构不认识）。
    Sqlite { detail: String },
    /// 取不到 Chrome Safe Storage 的密码（KWallet / 钥匙环都没有）。
    NoKey,
    /// 这个平台不支持从浏览器导入（插件描述里没有 `chromeImport`）。
    Unsupported { platform: String },
}

impl std::fmt::Display for ChromeImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDb { path } => write!(f, "没找到 Chrome 的 Cookie 文件（{path}）"),
            Self::Sqlite { detail } => write!(f, "读不了 Chrome 的 Cookie 库：{detail}"),
            Self::NoKey => write!(
                f,
                "KWallet 和钥匙环里都没取到 Chrome 的密钥（Chrome Safe Storage）"
            ),
            Self::Unsupported { platform } => write!(f, "{platform}不支持从浏览器导入 Cookie"),
        }
    }
}

/// 一个平台要导入什么 + 它的 key（错误 / 结果按 key 回去）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformImportSpec {
    /// 平台 key（`bili` / `zhihu` …），只用来把结果对回去。
    pub platform: String,
    pub spec: CookieImportSpec,
}

/// 一个平台的导入结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformImportOutcome {
    pub platform: String,
    /// `None` = 这个平台一条都没收到（调用方**不清**它的域）。
    pub outcome: Option<ImportOutcome>,
}

/// 整条导入（复制库 → 读 → 必要时要密码 → 解 → 分组 → 删副本）。
///
/// `password_commands` 是"按顺序试"的命令表（默认见 [`default_password_commands`]）；测试里换成
/// 不碰钥匙环的命令。库里没有 `v11` 行时**一条命令都不跑**。
#[derive(Debug, Clone)]
pub struct ChromeImporter {
    profile: PathBuf,
    password_commands: Vec<Vec<String>>,
    password_timeout: Duration,
}

impl ChromeImporter {
    /// 用默认的钥匙环命令（`kwallet-query` → `secret-tool` v2 → `secret-tool` v1）。
    pub fn new(profile: impl Into<PathBuf>) -> Self {
        Self {
            profile: profile.into(),
            password_commands: default_password_commands(),
            password_timeout: PASSWORD_TIMEOUT,
        }
    }

    /// 测试 / 排障用：换一组密码命令（`[]` = 不要密码，也就是遇到 `v11` 行就报 NoKey）。
    pub fn with_password_commands(mut self, commands: Vec<Vec<String>>) -> Self {
        self.password_commands = commands;
        self
    }

    /// 换超时（测试里给 5 秒，别等两分钟）。
    pub fn with_password_timeout(mut self, timeout: Duration) -> Self {
        self.password_timeout = timeout;
        self
    }

    pub fn profile(&self) -> &Path {
        &self.profile
    }

    /// 库文件：`<profile>/Cookies`。
    pub fn db_path(&self) -> PathBuf {
        self.profile.join("Cookies")
    }

    /// 跑一次导入（一个进程里只复制一次库、只读一次表，平台再多也是这一趟）。
    ///
    /// 顺序和 Kotlin 那版一样：**先把整张表读出来**（所有平台共用），再逐个平台解。
    pub fn import(
        &self,
        platforms: &[PlatformImportSpec],
        now_ms: i64,
    ) -> Result<Vec<PlatformImportOutcome>, ChromeImportError> {
        let db = self.db_path();
        if !db.is_file() {
            return Err(ChromeImportError::MissingDb {
                path: db.to_string_lossy().to_string(),
            });
        }
        let temp = TempDir::create()?;
        let copy = temp.path().join("Cookies");
        std::fs::copy(&db, &copy).map_err(|e| ChromeImportError::Sqlite {
            detail: format!("复制 Cookie 库失败：{e}"),
        })?;
        let table = read_rows(&copy)?;
        // 整个导入里只取一次密码（Kotlin 也是：`val passwordV11 by lazy { chromePassword() }`），
        // 而且要真有 v11 行才取——库里没有 v11 时**一条命令都不跑**（不弹授权框）。
        let needs_v11 = table
            .rows
            .iter()
            .any(|r| r.encrypted_value.starts_with(b"v11"));
        let password_v11 = if needs_v11 {
            self.fetch_password().ok_or(ChromeImportError::NoKey)?
        } else {
            PASSWORD_V10.to_string()
        };
        let mut out = Vec::with_capacity(platforms.len());
        for item in platforms {
            let outcome = import_rows(
                table.db_version,
                PASSWORD_V10,
                &password_v11,
                &table.rows,
                &item.spec,
                now_ms,
            );
            out.push(PlatformImportOutcome {
                platform: item.platform.clone(),
                outcome,
            });
        }
        Ok(out)
    }

    /// 按顺序试每条命令，第一条成功（退出码 0、stdout 非空）的就是密码。
    fn fetch_password(&self) -> Option<String> {
        for cmd in &self.password_commands {
            if let Some(secret) = run_password_command(cmd, self.password_timeout) {
                return Some(secret);
            }
        }
        None
    }
}

/// 默认的三条命令（和 Kotlin `ChromeCookieImporter.chromePassword` 逐字一样）。
///
/// 先 KWallet（KDE），再 GNOME 钥匙环的两个 schema。**命令不存在 / 失败 / 输出为空**都返回 None，
/// 接着试下一条。
pub fn default_password_commands() -> Vec<Vec<String>> {
    vec![
        vec![
            "kwallet-query".into(),
            "-f".into(),
            "Chrome Keys".into(),
            "-r".into(),
            "Chrome Safe Storage".into(),
            "kdewallet".into(),
        ],
        vec![
            "secret-tool".into(),
            "lookup".into(),
            "xdg:schema".into(),
            "chrome_libsecret_os_crypt_password_v2".into(),
            "application".into(),
            "chrome".into(),
        ],
        vec![
            "secret-tool".into(),
            "lookup".into(),
            "xdg:schema".into(),
            "chrome_libsecret_os_crypt_password_v1".into(),
            "application".into(),
            "chrome".into(),
        ],
    ]
}

/// 跑一条命令拿密码：读 stdout、丢掉 stderr、**超时就杀掉**。
///
/// 派生的子进程在一条自己的线程里读 stdout（不读就会把子进程堵在写管道上），主线程按
/// 50ms 的粒度轮询 `try_wait` 到超时。返回的是 `trim_end_matches('\n')` 之后非空的那一段
/// （Kotlin 是 `readText().trimEnd('\n')`）。
pub fn run_password_command(argv: &[String], timeout: Duration) -> Option<String> {
    use std::io::Read;
    let (program, args) = argv.split_first()?;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let bytes = reader.join().ok()?;
                if !status.success() {
                    return None;
                }
                let text = String::from_utf8_lossy(&bytes).trim_end_matches('\n').to_string();
                return if text.is_empty() { None } else { Some(text) };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // 超时：杀掉并收尸（`Child::kill` 是 SIGKILL，和 Kotlin 超时后 `destroy()` 的意图一样）。
                    //
                    // **不等读 stdout 的那条线程**：命令要是自己又起了子进程（`sh -c "…"`），那个孙进程
                    // 继承着管道的写端，我们要是在这里 `join` 就会一直等到它退出（比超时还久）。
                    // 直接把它甩开：管道写端一关，它自己就结束了。
                    let _ = child.kill();
                    let _ = child.wait();
                    drop(reader);
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(reader);
                return None;
            }
        }
    }
}

/// 从库里读出来的东西（每个导入进程只读一次，所有平台共用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeTable {
    /// `SELECT value FROM meta WHERE key = 'version'`；读不出来就是 0（Kotlin `toIntOrNull() ?: 0`）。
    pub db_version: i64,
    pub rows: Vec<ChromeCookieRow>,
}

/// 只读打开一份 Chrome 的 `Cookies` 库，读版本 + 整张 `cookies` 表。
///
/// **只读**（`SQLITE_OPEN_READ_ONLY`）：这一份是复制出来的副本，我们一个字节也不该写。
/// 列的顺序 / 类型和 Kotlin `readRows` 那六列一样。
pub fn read_rows(db: &Path) -> Result<ChromeTable, ChromeImportError> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| {
        ChromeImportError::Sqlite {
            detail: e.to_string(),
        }
    })?;
    let db_version: i64 = conn
        .query_row("SELECT value FROM meta WHERE key = 'version'", [], |row| {
            // 老版本 Chrome 的 meta.value 是 TEXT；有的构建是 INTEGER。两种都认。
            // 读不懂（行不在 / 类型怪）就是 0，和 Kotlin `st.getText(0).toIntOrNull() ?: 0` 一样。
            Ok(match row.get_ref(0) {
                Ok(rusqlite::types::ValueRef::Integer(v)) => v,
                Ok(rusqlite::types::ValueRef::Text(t)) => std::str::from_utf8(t)
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .unwrap_or(0),
                _ => 0,
            })
        })
        .unwrap_or(0);
    let mut stmt = conn
        .prepare(
            "SELECT host_key, name, path, expires_utc, is_secure, is_httponly, encrypted_value FROM cookies",
        )
        .map_err(|e| ChromeImportError::Sqlite {
            detail: e.to_string(),
        })?;
    let mut rows = stmt.query([]).map_err(|e| ChromeImportError::Sqlite {
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    loop {
        let row = match rows.next() {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                return Err(ChromeImportError::Sqlite {
                    detail: e.to_string(),
                })
            }
        };
        // 和 Kotlin 一样：文本列按文本取、整型列按整型取、`encrypted_value` 是一段 BLOB。
        // 有一列类型怪（NULL）就按"读不出来"处理，别为一个浏览器内部字段把整次导入干掉。
        let host_key: String = row.get(0).unwrap_or_default();
        let name: String = row.get(1).unwrap_or_default();
        let path: String = row.get(2).unwrap_or_default();
        let expires_utc: i64 = row.get(3).unwrap_or(0);
        let is_secure: i64 = row.get(4).unwrap_or(0);
        let is_httponly: i64 = row.get(5).unwrap_or(0);
        let encrypted_value: Vec<u8> = row.get(6).unwrap_or_default();
        out.push(ChromeCookieRow {
            host_key,
            name,
            path,
            expires_utc,
            is_secure: is_secure != 0,
            is_httponly: is_httponly != 0,
            encrypted_value,
        });
    }
    Ok(ChromeTable {
        db_version,
        rows: out,
    })
}

// ===========================================================================
// 私有临时目录（700，Drop 时整个删掉）
// ===========================================================================

/// 一个只有本用户能进的临时目录（权限 700），`Drop` 时删掉整棵子树。
///
/// 名字用 `pid + 纳秒 + 计数` 拼，`create_dir` 撞了就换一个（不引 `tempfile`：这里就二十行，
/// 而且这个 crate 的依赖树是每一台远端机器都要能 `--offline` 编出来的）。
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn create() -> Result<Self, ChromeImportError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir();
        for _ in 0..64 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let name = format!(
                "feedhub-ck-{}-{}-{}",
                std::process::id(),
                nanos,
                COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let path = base.join(name);
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(ChromeImportError::Sqlite {
                        detail: format!("建临时目录失败：{e}"),
                    })
                }
            }
        }
        Err(ChromeImportError::Sqlite {
            detail: "建临时目录失败（名字一直撞）".to_string(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
