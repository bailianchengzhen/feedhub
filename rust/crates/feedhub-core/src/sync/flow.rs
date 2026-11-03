//! **手动导出 / 导入的编排**。
//!
//! 快照编解码 / 合并引擎 / 文件容器 / 加密 / 二维码早就在 Rust（[`crate::sync`]）；这一层搬的是
//! **"导出 / 导入各走哪条路、什么时候报什么错"**：
//!
//! | 这里 | Kotlin |
//! |---|---|
//! | [`plan_export_to_paired`] | `LibrarySync.exportToPairedDevices` 的前半（挑收件人 / 判失败原因） |
//! | [`plan_export_password`] | `LibrarySync.export` 的前半（密码长度 / 快照） |
//! | [`plan_import`] | `LibrarySync.inspect` + `preview` 的判定（文件头 → 要不要问密码 / 给谁的文件 / 头尾对不对） |
//! | [`default_file_name`] | `LibrarySync.defaultFileName`（`feedhub-yyyyMMdd-HHmm.feedhub-sync`） |
//! | [`retention_cutoff`] | `LibrarySync.retentionCutoffMs` |
//!
//! **留在宿主（Kotlin）的**：
//! - **文件选择器 / 分享面板**（`SyncFiles` 的 `save` / `open`）——操作系统的活；
//! - **设备密钥的读写**（`DeviceKeyStore`：600 权限 + 原子替换）——读写文件属于宿主，
//!   Rust 只在算加密的那一步用它交进来的字节；
//! - **资料库的读写**（`syncSnapshot` / `previewImport` / `applyImport` / `syncPeers`）——
//!   这些是 SQLite 的活，Rust 的 `FfiLibrary` 已经有；`LibrarySync` 这一层只接住它给的结果；
//! - **协程 / 弹框**。
//!
//! ## 为什么是"计划"而不是"直接做"
//!
//! 导出要在**用户选完文件位置之后**才知道文件名，导入要在**用户输完密码之后**才能解开。所以
//! Rust 给的是**计划**（该给谁、用哪个密钥、错在哪一步），宿主按计划做 I/O，再把结果说给用户。
//! 这样"哪个错该说哪句话"只有一份，而 `android.app.Activity` / `javax.swing.JFileChooser`
//! 那些东西一个字节都不进 `.so`。

use crate::sync::file::{self, Header, Mode, Opened, Recipient, Unlock};
use crate::sync::snapshot::{LibSyncError, LibrarySnapshot};

/// 同步密码的最短长度。
pub const MIN_PASSWORD_LENGTH: usize = 12;

/// 同步文件的扩展名（Kotlin `LibrarySyncFile.EXTENSION` = `"feedhub-sync"`）。
///
/// 文件名长这样：`feedhub-20261028-0507.feedhub-sync`（和 Kotlin `defaultFileName` 逐字一样）。
pub const EXTENSION: &str = "feedhub-sync";

// ===========================================================================
// 导出：挑收件人
// ===========================================================================

/// 一台"已经配对过"的设备（Kotlin `SyncPeer`，只带编排用得到的三项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub device_id: String,
    /// 公钥（base64url，无填充）。
    pub public_key: String,
}

/// 导出计划：收件人 + 跳过了几台 + 要用的快照参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedExportPlan {
    /// 收件人（**本机在最前面**，和 Kotlin 一样）。
    pub recipients: Vec<Recipient>,
    /// 公钥读不出来的设备数（正常是 0，界面会提一句）。
    pub skipped_peers: usize,
}

/// 一次"发给全部已配对设备"的导出计划（Kotlin `exportToPairedDevices` 的前半）。
///
/// 失败原因（Kotlin 那几句原话都在这儿，界面直接用）：
/// - 这个版本没接文件选择 → 由宿主在调这里之前判（`files == null`）；
/// - 这台设备上的资料库不支持同步 → 同上（`library.supportsSync`）；
/// - 没有设备密钥 / 没有设备标识 / 还没有配对的设备 / 生成设备密钥失败 / 公钥都读不出来。
///
/// [`self_id`] 是本机的设备 id（8 字节 base64url）；[`self_public_key`] 是本机设备密钥的公钥
/// （已经 base64url 编码好，由宿主从 `DeviceKey` 取）。
pub fn plan_export_to_paired(
    self_id: &str,
    self_public_key: &str,
    peers: &[Peer],
) -> Result<PairedExportPlan, String> {
    if peers.is_empty() {
        return Err("还没有配对的设备：先点「配对新设备」，或者改用密码导出".to_string());
    }
    let mut skipped = 0usize;
    let mut recipients: Vec<Recipient> = Vec::with_capacity(peers.len() + 1);
    // 本机自己也在收件人里（这样导出之后本机也能导回来，和 Kotlin 一样）
    match Recipient::new(self_id, &base64_url_decode_strict(self_public_key).unwrap_or_default()) {
        Ok(self_recipient) => recipients.push(self_recipient),
        // 本机自己的公钥读不出来：Kotlin 那边是 `store.loadOrCreate(selfId)` 的 `getOrNull() ?: 失败`
        Err(_) => return Err("生成 / 读取本机设备密钥失败".to_string()),
    }
    for peer in peers {
        match base64_url_decode_strict(&peer.public_key) {
            Some(bytes) if bytes.len() == 32 => {
                match Recipient::new(&peer.device_id, &bytes) {
                    Ok(r) => recipients.push(r),
                    Err(_) => skipped += 1,
                }
            }
            // 长度不是 32 / 解不出来：跳过这台（Kotlin `pub == null || pub.size != 32 → skippedPeers++`）
            _ => skipped += 1,
        }
    }
    if recipients.len() <= 1 {
        return Err("已配对设备的公钥都读不出来：移除后重新配对一次".to_string());
    }
    Ok(PairedExportPlan {
        recipients,
        skipped_peers: skipped,
    })
}

/// 密码导出的校验（Kotlin `export(password)` 的前半）：密码够不够长。
pub fn check_password(password_len: usize) -> Result<(), String> {
    if password_len < MIN_PASSWORD_LENGTH {
        return Err(format!("同步密码至少 {MIN_PASSWORD_LENGTH} 位"));
    }
    Ok(())
}

/// 这一份快照按"发给全部已配对设备"加密之后的结果（宿主接着调文件选择器写盘）。
///
/// 快照 → 明文 → 逐个收件人包文件密钥，全在 [`crate::sync::file`] 里（这一层只负责把参数摆对：
/// 设备 id / 生成时间 / 序号都来自**快照自己**，和 Kotlin 一样）。
///
/// 随机源由宿主注入（生产是 [`file::RandomSource::system`]，测试是固定序列），
/// 因为"加密用的随机数"和别处一样属于"由宿主给"那一类。
pub fn seal_to_recipients(
    snapshot: &LibrarySnapshot,
    recipients: &[Recipient],
    random: &mut file::RandomSource<'_>,
) -> Result<Vec<u8>, String> {
    let plain = crate::sync::snapshot::encode(snapshot);
    file::seal_to_recipients(
        &plain,
        &snapshot.device,
        snapshot.generated_at,
        snapshot.seq,
        recipients,
        random,
    )
    .map_err(|e| e.to_string())
}

/// 快照按密码加密之后的结果。迭代次数用生产的默认值（[`file::DEFAULT_ITERATIONS`]）。
pub fn seal_with_password(
    snapshot: &LibrarySnapshot,
    password: &str,
    random: &mut file::RandomSource<'_>,
) -> Result<Vec<u8>, String> {
    check_password(password.chars().count())?;
    let plain = crate::sync::snapshot::encode(snapshot);
    file::seal(
        &plain,
        password,
        &snapshot.device,
        snapshot.generated_at,
        snapshot.seq,
        file::DEFAULT_ITERATIONS,
        random,
    )
    .map_err(|e| e.to_string())
}

// ===========================================================================
// 导入：文件头 → 能不能解 / 要不要密码
// ===========================================================================

/// 只看文件头的结论（Kotlin `ImportInspection`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    pub mode: Mode,
    /// 收件设备 id（只有按设备加密才有）。
    pub recipients: Vec<String>,
    /// 文件里有本机的收件条目 → 可以用本机密钥直接解，不问密码。
    pub for_this_device: bool,
    /// 文件头都读不出来时的错误（`None` = 读出来了）。
    pub error: Option<String>,
}

/// 只看文件头（Kotlin `LibrarySync.inspect`）。
pub fn inspect(bytes: &[u8], self_id: Option<&str>) -> Inspection {
    match file::read_header(bytes) {
        Ok(header) => {
            let mine = header.mode == Mode::Recipients
                && self_id.map(|id| header.recipients.iter().any(|r| r == id)).unwrap_or(false);
            Inspection {
                mode: header.mode,
                recipients: header.recipients.clone(),
                for_this_device: mine,
                error: None,
            }
        }
        Err(e) => Inspection {
            // 头都读不出来时 mode / recipients 给个空的兜底（Kotlin 那边也是 `header = null`）
            mode: Mode::Password,
            recipients: Vec::new(),
            for_this_device: false,
            error: Some(e.to_string()),
        },
    }
}

/// 解开文件的结果，或"还缺什么"。
#[derive(Debug, Clone, PartialEq)]
pub enum UnlockOutcome {
    /// 解开了：明文快照 + 怎么解开的。
    Opened {
        snapshot: LibrarySnapshot,
        unlock: Unlock,
    },
    /// 这是密码文件：界面弹密码框，输完再 `preview(password)`。
    NeedsPassword,
    /// 打不开（文件坏了 / 不是发给这台设备的 / 密钥不在 / 头尾对不上）。
    Failed(String),
}

/// 解开一份同步文件（Kotlin `LibrarySync.preview` 的判定那半边）。
///
/// 三条口径照 Kotlin：
/// 1. **按设备加密**的文件**不看 `password`**：用本机设备密钥解；收件人里没有本机 → 明说；
///    设备密钥不在（重装过 / 被删了）→ 明说"和导出方重新配对一次"；
/// 2. **密码文件**：`password == None` 就是 [`UnlockOutcome::NeedsPassword`]；
/// 3. 解开之后**再核对一次**明文里的 `device` / `seq` / `generatedAt` 和文件头（文件头在 AAD 里，
///    这一步防"拿别的文件头套密文"）。
///
/// [`device_private_key`] 是本机设备私钥的 32 字节（宿主从 `DeviceKeyStore` 读，Rust 不读文件）。
pub fn preview(
    bytes: &[u8],
    self_id: Option<&str>,
    device_private_key: Option<&[u8]>,
    password: Option<&str>,
) -> UnlockOutcome {
    let header: Header = match file::read_header(bytes) {
        Ok(h) => h,
        Err(e) => return UnlockOutcome::Failed(e.to_string()),
    };
    let opened: Opened = if header.mode == Mode::Recipients {
        let Some(self_id) = self_id else {
            return UnlockOutcome::Failed(
                "这个同步文件不是发给这台设备的（文件里 {} 台收件设备，没有本机）。\
                 让导出方把你加进已配对设备，或者改用密码导出"
                    .to_string(),
            );
        };
        if !header.recipients.iter().any(|r| r == self_id) {
            return UnlockOutcome::Failed(format!(
                "这个同步文件不是发给这台设备的（文件里 {} 台收件设备，没有本机）。\
                 让导出方把你加进已配对设备，或者改用密码导出",
                header.recipients.len()
            ));
        }
        let Some(key) = device_private_key else {
            return UnlockOutcome::Failed(
                "这台设备上没有配对时用的设备密钥（重装过 / 密钥文件被删了）：\
                 和导出方重新配对一次，或者让对方改用密码导出"
                    .to_string(),
            );
        };
        match file::open_with_device_key(bytes, self_id, key) {
            Ok(o) => o,
            Err(e) => return UnlockOutcome::Failed(sync_error_message(&e)),
        }
    } else {
        match password {
            None => return UnlockOutcome::NeedsPassword,
            Some(pw) => match file::open(bytes, pw) {
                Ok(o) => o,
                Err(e) => return UnlockOutcome::Failed(sync_error_message(&e)),
            },
        }
    };
    let snapshot = match crate::sync::snapshot::decode(&opened.plaintext) {
        Ok(s) => s,
        Err(e) => return UnlockOutcome::Failed(e.to_string()),
    };
    // 文件头在 AAD 里，解密后再核对一次明文里的三样东西
    if snapshot.device != opened.header.device
        || snapshot.seq != opened.header.seq
        || snapshot.generated_at != opened.header.generated_at
    {
        return UnlockOutcome::Failed("文件的头部和内容对不上，这个文件不完整".to_string());
    }
    UnlockOutcome::Opened {
        snapshot,
        unlock: opened.unlock,
    }
}

/// 同步文件的错误 → 给用户看的一句话。
///
/// 和 Kotlin `LibrarySyncFormatException` 的消息一致（"文件被改过 / 密码不对"那种）；**不外泄任何
/// 密钥 / 明文片段**（`LibSyncError::Format` 里带的都是这种话）。
fn sync_error_message(e: &LibSyncError) -> String {
    e.to_string()
}

// ===========================================================================
// 文件名 / 保留期
// ===========================================================================

/// 导出的默认文件名：`feedhub-yyyyMMdd-HHmm.feedhub-sync`（Kotlin `defaultFileName`）。
///
/// **按 UTC 格式化**？不是——Kotlin 用的是 `SimpleDateFormat` 的**默认时区**。这一层不动时区：
/// 宿主把"本地时间的年月日时分"算好传进来，Rust 只拼字符串（时区是操作系统的知识）。
pub fn default_file_name(local_year: i32, month: u32, day: u32, hour: u32, minute: u32) -> String {
    format!(
        "feedhub-{:04}{:02}{:02}-{:02}{:02}.{EXTENSION}",
        local_year, month, day, hour, minute
    )
}

/// 本机"只保留最近 N 天"的截止时间；没开就是 `None`（保留期是本机策略，不传播）。
pub fn retention_cutoff(history_limit_on: bool, history_days: i64, now_ms: i64) -> Option<i64> {
    if !history_limit_on {
        return None;
    }
    Some(now_ms - history_days * 86_400_000)
}

/// 把一段 base64url（无填充，也认 `+` `/`）解成字节；解不出来/长度不对就是 `None`。
///
/// 和 Kotlin `SyncCrypto.base64UrlDecode` 一样（那边也是失败返回 null）。用的是
/// [`crate::sync::b64`] 那一份（URL 解码器还认 `+` `/` 这条 JDK 怪癖已经在那儿实现了）。
fn base64_url_decode_strict(text: &str) -> Option<Vec<u8>> {
    crate::sync::b64::decode(text)
}

