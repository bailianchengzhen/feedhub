package feedhub.core.sync

import feedhub.rust.RustSync
import uniffi.feedhub_ffi.ffiSyncFileHasRecipient
import uniffi.feedhub_ffi.ffiSyncFileMinIterations
import uniffi.feedhub_ffi.ffiSyncFileOpenWithDeviceKey
import uniffi.feedhub_ffi.ffiSyncFileOpenWithPassword
import uniffi.feedhub_ffi.ffiSyncFileReadHeader
import uniffi.feedhub_ffi.ffiSyncFileSealToRecipients
import uniffi.feedhub_ffi.ffiSyncFileSealWithPassword
import uniffi.feedhub_ffi.ffiSyncFileVersion
import uniffi.feedhub_ffi.ffiSyncGunzip
import uniffi.feedhub_ffi.ffiSyncGzip
import uniffi.feedhub_ffi.ffiSyncPasswordKey
import uniffi.feedhub_ffi.FfiSyncRecipient
import java.security.SecureRandom

/**
 * 导出文件的加密封装。**实现在 Rust**
 * （`feedhub-core/src/sync/file.rs`），这里是薄门面。
 *
 * 两种模式，同一套"正文 AES-256-GCM + AAD 覆盖整个文件头"：
 *
 * 1. **按设备加密**（默认）：随机生成 32 字节**文件密钥**，正文照旧 gzip + AES-256-GCM；
 *    文件头里给每个收件设备放一份"包好的文件密钥"（临时 X25519 → HKDF-SHA256 → AES-256-GCM，
 *    类似 age 的多收件人）。本机自己也算一个收件人，所以自己导出的文件自己也能导回。
 * 2. **密码**（备用 / 给没配对的设备）：PBKDF2-HMAC-SHA256（≥210,000 次）→ 32 字节密钥 → AES-256-GCM。
 *
 * 文件布局（大端）：
 * ```
 * 0   magic "FHSYNCF2"(8)
 * 8   version = 2 (1)
 * 9   mode: 1 = 密码, 2 = 收件设备 (1)
 * 10  device (8)         导出方设备 id
 * 18  generatedAt (8)
 * 26  seq (8)
 * 34  nonce (12)         正文 GCM 的 nonce
 * 46  —— 按 mode 分叉：
 *       mode 1: iterations(4) | salt(16)                      → 头共 66 字节
 *       mode 2: epk(32) | count(2) | count × [device(8) | wrapNonce(12) | wrappedFileKey(48)]
 *                                                             → 头共 80 + count×68 字节
 * 之后: AES-256-GCM(gzip(快照 JSON))，tag 16 字节跟在密文后
 * ```
 * **AAD = 上面整段文件头**（含收件人列表）——改版本 / 设备 / 序号 / 收件人 / epk 都认证失败。
 * 每个收件人的"包好的文件密钥"另外用 `AAD = 头部到 epk 为止的前缀` 认证。
 *
 * **旧文件（v1，"FHSYNCF1"）照旧能读**：v1 只有密码模式，[open] 仍然接受。
 *
 * **随机量从 Kotlin 给**：nonce / salt / 文件密钥 / 临时私钥都由这一层用 [SecureRandom]（或测试注入的
 * [java.util.Random] 替身）取好再传过去——`.so` 里不取随机数，对拍才能逐字节比。
 *
 * Kotlin 的原实现在 `core` 的 testFixtures（`feedhub.core.sync.legacy.LegacyLibrarySyncFile`），
 * 只当对拍参照（`SyncFileDualRunTest` / `SyncFileInteropTest`）。
 */
object LibrarySyncFile {
    const val EXTENSION = "feedhub-sync"

    /**
     * 最少迭代次数。打开时**低于这个值直接拒**，防止有人把文件头改成弱参数。
     *
     * 走 [RustSync.guard]：这是**属性取值**，可能比任何一次方法调用都早（设置页 / 测试直接读它），
     * 而 UniFFI 的函数一碰就会 `Native.load`——不先 `ensureLoaded` 拿到的是 `UnsatisfiedLinkError`，
     * 不是那条"说得清的错误"（桌面还得先解压 `.so` 再指路）。
     */
    val MIN_ITERATIONS: Int get() = RustSync.guard { ffiSyncFileMinIterations().toInt() }
    const val DEFAULT_ITERATIONS = 210_000
    const val MAX_ITERATIONS = 10_000_000

    /** 打开时最多接受多大的文件（密文）。正常全量快照 gzip 后几 MB。 */
    const val MAX_FILE_BYTES = 96L * 1024 * 1024

    /** 解压后最多接受多大（防 zip bomb）。 */
    const val MAX_PLAIN_BYTES = 64L * 1024 * 1024

    /** 一个文件最多发给多少台设备（自用：几台而已，上限只是防畸形头）。 */
    const val MAX_RECIPIENTS = 32

    /** 当前的文件格式版本（Rust 里是唯一的真相）。同样走 [RustSync.guard]（属性取值也可能是第一次碰 `.so`）。 */
    val VERSION: Int get() = RustSync.guard { ffiSyncFileVersion().toInt() }

    /** 旧版（只有密码）的文件格式版本，仍然能读。 */
    const val VERSION_PASSWORD_ONLY = 1

    enum class Mode { PASSWORD, RECIPIENTS }

    /** 这份文件是怎么解开的（界面显示"已用本机密钥解开"）。 */
    enum class Unlock { PASSWORD, DEVICE_KEY }

    /** 一个收件设备：设备 id + X25519 公钥。 */
    class Recipient(val device: String, publicKey: ByteArray) {
        val publicKey: ByteArray = publicKey.copyOf()

        init {
            // 校验在 Rust 的 `Recipient::new` 里；这里只做"早失败"（构造时立刻报，别等到导出）
            if (SyncDevice.decode(device) == null) throw IllegalArgumentException("收件设备 id 不是 8 字节的 base64url")
            if (this.publicKey.size != DeviceKey.PUBLIC_KEY_BYTES) throw IllegalArgumentException("收件设备公钥长度不对")
            if (this.publicKey.all { it == 0.toByte() }) throw IllegalArgumentException("收件设备公钥不合法")
        }
    }

    /** 文件头（不含密文）。打开文件后可以用它显示"来自哪台设备 / 什么时候生成 / 发给谁"，不用先解密。 */
    data class Header(
        val version: Int,
        val mode: Mode,
        /** 密码模式的 PBKDF2 迭代次数；按设备加密是 0。 */
        val iterations: Int,
        val device: String,
        val generatedAt: Long,
        val seq: Long,
        /** 收件设备的 id 列表（按设备加密才有；按设备加密时包括导出方自己）。 */
        val recipients: List<String>,
    )

    /** 解密成功的内容。 */
    data class Opened(
        val header: Header,
        /** 解压后的明文（快照 JSON 的字节）。 */
        val plaintext: ByteArray,
        val unlock: Unlock,
    )

    // ---------------------------------------------------------------- 写

    /**
     * 用密码加密（"改用密码" / 给没配对的设备）。返回可以落盘的完整文件字节。
     *
     * @param iterations 只在测试里改小；生产调用用默认值 [DEFAULT_ITERATIONS]
     * @param random 测试可注入，生产用 [SecureRandom]
     */
    fun seal(
        plaintext: ByteArray,
        password: CharArray,
        device: String,
        generatedAt: Long,
        seq: Long,
        iterations: Int = DEFAULT_ITERATIONS,
        random: SecureRandom = SecureRandom(),
    ): ByteArray {
        require(password.isNotEmpty()) { "同步密码不能为空" }
        require(iterations >= MIN_ITERATIONS) { "PBKDF2 迭代次数不能少于 $MIN_ITERATIONS" }
        require(iterations <= MAX_ITERATIONS) { "PBKDF2 迭代次数太多" }
        require(plaintext.size.toLong() <= MAX_PLAIN_BYTES) { "快照太大（${plaintext.size} 字节）" }
        val salt = ByteArray(16).also(random::nextBytes)
        val nonce = ByteArray(12).also(random::nextBytes)
        val plainPassword = password.concatToString()
        return try {
            RustSync.guard {
                ffiSyncFileSealWithPassword(
                    plaintext = plaintext,
                    password = plainPassword.toByteArray(Charsets.UTF_8),
                    device = device,
                    generatedAt = generatedAt,
                    seq = seq,
                    iterations = iterations.toUInt(),
                    salt = salt,
                    nonce = nonce,
                )
            }
        } finally {
            password.fill('\u0000')
        }
    }

    /**
     * 按设备加密：正文用随机文件密钥，文件头里给每个收件设备包一份文件密钥。
     *
     * @param recipients 收件设备（调用方应把**本机自己**也放进来）；按设备 id 去重，最多 [MAX_RECIPIENTS] 台。
     * @param random 测试可注入
     */
    fun sealToRecipients(
        plaintext: ByteArray,
        device: String,
        generatedAt: Long,
        seq: Long,
        recipients: List<Recipient>,
        random: SecureRandom = SecureRandom(),
    ): ByteArray {
        require(plaintext.size.toLong() <= MAX_PLAIN_BYTES) { "快照太大（${plaintext.size} 字节）" }
        val unique = recipients.distinctBy { it.device }
        if (unique.isEmpty()) throw LibrarySyncFormatException("没有收件设备（先配对，或者改用密码）")
        if (unique.size > MAX_RECIPIENTS) throw LibrarySyncFormatException("收件设备太多（最多 $MAX_RECIPIENTS 台）")
        val fileKey = ByteArray(32).also(random::nextBytes)
        val ephemeralSecret = ByteArray(DeviceKey.PRIVATE_KEY_BYTES).also(random::nextBytes)
        val nonce = ByteArray(12).also(random::nextBytes)
        val wrapNonces = unique.map { ByteArray(12).also(random::nextBytes) }
        return RustSync.guard {
            ffiSyncFileSealToRecipients(
                plaintext = plaintext,
                device = device,
                generatedAt = generatedAt,
                seq = seq,
                recipients = unique.map { FfiSyncRecipient(it.device, it.publicKey) },
                fileKey = fileKey,
                ephemeralSecret = ephemeralSecret,
                nonce = nonce,
                wrapNonces = wrapNonces,
            )
        }
    }

    // ---------------------------------------------------------------- 读

    /**
     * 用密码打开（v2 密码文件 / v1 旧文件）。
     * @throws LibrarySyncFormatException 格式 / 版本 / 密码 / 完整性不对（消息可以直接给用户看）。
     */
    fun open(bytes: ByteArray, password: CharArray): Opened {
        val plainPassword = password.concatToString()
        val opened = try {
            RustSync.guard {
                ffiSyncFileOpenWithPassword(bytes, plainPassword.toByteArray(Charsets.UTF_8))
            }
        } finally {
            password.fill('\u0000')
        }
        return Opened(opened.header.toModel(), opened.plaintext, unlockOf(opened.unlockedByDeviceKey))
    }

    /**
     * 用本机设备密钥打开按设备加密的文件。文件里没有本机的收件条目时抛错（不回落密码——按设备加密的文件没有密码）。
     * PBKDF2 密码文件调用它会得到"这个文件是密码加密的"。
     */
    fun openWithDeviceKey(bytes: ByteArray, key: DeviceKey): Opened {
        val opened = RustSync.guard {
            ffiSyncFileOpenWithDeviceKey(bytes, key.deviceId, key.copyPrivateKeyForRust())
        }
        return Opened(opened.header.toModel(), opened.plaintext, unlockOf(opened.unlockedByDeviceKey))
    }

    /**
     * 文件头（不解密）。**只用于显示**：是否可信要等 [open] / [openWithDeviceKey] 的 AAD 验过。
     */
    fun readHeader(bytes: ByteArray): Header = RustSync.guard {
        ffiSyncFileReadHeader(bytes).toModel()
    }

    /** 这个文件有没有本机的收件条目（按设备加密的文件才可能为 true）。读头部，不解密。 */
    fun hasRecipient(bytes: ByteArray, deviceId: String): Boolean = RustSync.guard {
        ffiSyncFileHasRecipient(bytes, deviceId)
    }

    // ---------------------------------------------------------------- 小件

    /** gzip。 */
    internal fun gzip(data: ByteArray): ByteArray = RustSync.guard { ffiSyncGzip(data) }

    internal fun gunzip(data: ByteArray): ByteArray = RustSync.guard { ffiSyncGunzip(data) }

    /** PBKDF2（口令按 UTF-8 编）——对拍用。 */
    internal fun derivePasswordKey(password: CharArray, salt: ByteArray, iterations: Int): ByteArray {
        val text = password.concatToString()
        val key = RustSync.guard { ffiSyncPasswordKey(text.toByteArray(Charsets.UTF_8), salt, iterations.toUInt()) }
        password.fill('\u0000')
        return key
    }

    private fun unlockOf(byDeviceKey: Boolean): Unlock = if (byDeviceKey) Unlock.DEVICE_KEY else Unlock.PASSWORD
}

/** `FfiSyncFileHeader` → [LibrarySyncFile.Header]。 */
internal fun uniffi.feedhub_ffi.FfiSyncFileHeader.toModel(): LibrarySyncFile.Header = LibrarySyncFile.Header(
    version = version.toInt(),
    mode = if (recipientsMode) LibrarySyncFile.Mode.RECIPIENTS else LibrarySyncFile.Mode.PASSWORD,
    iterations = iterations.toInt(),
    device = device,
    generatedAt = generatedAt,
    seq = seq,
    recipients = recipients,
)
