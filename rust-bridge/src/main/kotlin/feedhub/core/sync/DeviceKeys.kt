package feedhub.core.sync

import feedhub.rust.RustSync
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import uniffi.feedhub_ffi.FfiSyncDeviceKey
import uniffi.feedhub_ffi.ffiSyncHkdfSha256
import uniffi.feedhub_ffi.ffiSyncHlcCompareVersion
import uniffi.feedhub_ffi.ffiSyncHlcCounter
import uniffi.feedhub_ffi.ffiSyncHlcMaxVersion
import uniffi.feedhub_ffi.ffiSyncHlcOf
import uniffi.feedhub_ffi.ffiSyncHlcPhysical
import uniffi.feedhub_ffi.ffiSyncPairingCode
import uniffi.feedhub_ffi.ffiSyncX25519Agree
import uniffi.feedhub_ffi.ffiSyncX25519PublicKey
import java.nio.file.AtomicMoveNotSupportedException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.PosixFilePermission
import java.security.SecureRandom

/**
 * 跨设备同步的**设备密钥**。
 *
 * 每台设备第一次需要时生成一对 **X25519** 长期密钥：
 * - 私钥**只在本机**（桌面：`~/.config/feedhub/sync-device-key.json`，600；安卓：App 私有目录里的同名文件）；
 * - 公钥跟设备 id（`sync_meta.device_id`，8 字节）一起在配对时交换；
 * - 设备密钥不导出、不同步、不打印（[toString] 只有设备 id）。
 *
 * **曲线运算在 Rust**（`feedhub-core/src/sync/crypto.rs`，x25519-dalek）：私钥活在 Rust 侧的
 * [FfiSyncDeviceKey] 里，**Kotlin 只在落盘的那一刻拿一次字节**（[copyPrivateKeyForRust] 的反向）。
 * 为什么这么绕：这一层原来用 BouncyCastle，而 Rust 那边同步要用 X25519，两边必须是同一套算法 / 编码
 * （老 App 和新 App 要能互相同步）；私钥只在 Rust 对象里意味着"算的时候不会在 JVM 堆上多留一份"。
 *
 */
class DeviceKey internal constructor(
    val deviceId: String,
    /** Rust 侧的密钥对象（私钥在里面）。 */
    internal val native: FfiSyncDeviceKey,
) {
    /** 公钥的副本（调用方可以随便改）。 */
    val publicKey: ByteArray get() = native.publicKey()

    /** 公钥的 base64url（放进配对报文 / 文件头）。 */
    val publicKeyBase64: String get() = native.publicKeyBase64()

    /**
     * 和对方的公钥做一次 X25519：返回 32 字节共享秘密。
     * @throws SyncException 对方公钥长度不对、或者是一个低阶点（共享秘密全 0）。
     */
    fun agree(peerPublicKey: ByteArray): ByteArray {
        if (peerPublicKey.size != PUBLIC_KEY_BYTES) throw SyncException("对方的设备公钥长度不对")
        return RustSync.guard { native.agree(peerPublicKey) }
            ?: throw SyncException("和对方的设备公钥算不出共享密钥（公钥不合法）")
    }

    /** 只给测试 / 掉落判断用：私钥是否和公钥对得上。 */
    internal fun matchesPublicKey(): Boolean = RustSync.guard { native.matchesPublicKey() }

    /**
     * 私钥字节的**一份拷贝**，只给 `DeviceKeyStore` 落盘用。调用方必须马上填零。
     *
     * 名字沿用 Kotlin 原实现（`copyPrivateKey`）：原来私钥是 JVM 里的一个字段，现在活在 Rust 侧，
     * 这里只是把字节取一份出来落盘。
     */
    /**
     * 私钥的副本（调用方可以随便改）。**只在"交给 Rust 去解密"那一步用**（`LibrarySync.preview`）：
     * 私钥不进日志、不落盘、不跨进程。
     */
    fun copyPrivateKey(): ByteArray = RustSync.guard { native.privateKeyBytes() }

    internal fun copyPrivateKeyForRust(): ByteArray = copyPrivateKey()

    /** 日志 / 异常里只有设备 id，**永远不含密钥**。 */
    override fun toString(): String = "DeviceKey($deviceId)"

    override fun equals(other: Any?): Boolean = other is DeviceKey && other.deviceId == deviceId &&
        other.publicKey.contentEquals(publicKey)

    override fun hashCode(): Int = deviceId.hashCode() * 31 + publicKey.contentHashCode()

    companion object {
        const val PUBLIC_KEY_BYTES = 32
        const val PRIVATE_KEY_BYTES = 32

        /** 生成一把新的设备密钥（测试可以注入 [random]）。 */
        fun generate(deviceId: String, random: SecureRandom = SecureRandom()): DeviceKey {
            val privateKey = ByteArray(PRIVATE_KEY_BYTES).also(random::nextBytes)
            return DeviceKey(deviceId, RustSync.guard { FfiSyncDeviceKey.generate(deviceId, privateKey) })
        }

        /** 从文件里的私钥恢复（公钥现算，不信文件里写的那份）。 */
        internal fun of(deviceId: String, privateKey: ByteArray): DeviceKey? {
            if (privateKey.size != PRIVATE_KEY_BYTES) return null
            val native = runCatching {
                RustSync.guard { FfiSyncDeviceKey.fromPrivateKey(deviceId, privateKey) }
            }.getOrNull() ?: return null
            return DeviceKey(deviceId, native)
        }

        /** 只用来给文件里的每个收件人包一份文件密钥的临时密钥对（用完就丢，不落盘、不算"设备"）。 */
        internal fun ephemeral(random: SecureRandom = SecureRandom()): DeviceKey = generate("ephemeral", random)

        /** 公钥长度检查（和 Rust 的 `X25519_BYTES` 对齐）。 */
        internal fun publicKeyOf(privateKey: ByteArray): ByteArray? = RustSync.guard {
            ffiSyncX25519PublicKey(privateKey)
        }
    }
}

/**
 * 设备密钥的落盘：一个 600 权限的小 JSON 文件（`sync-device-key.json`）。
 *
 * - 桌面传 `~/.config/feedhub`，安卓传 `context.filesDir`；测试一律用临时目录（绝不动用户真实文件）。
 * - **只碰自己这个文件名**：不读、不改同目录里已有的别的文件（`state.json` / `cookies.json` / `library.db`……）。
 * - 写文件走"同目录临时文件 → 设置 600 → 原子替换"，中途崩了不会留下半截私钥。
 * - 设备 id 变了（重装 / 换了库）就重新生成一把。
 *
 * **落盘留在 Kotlin**：这是平台的事（POSIX 权限 / 原子替换 / 安卓私有目录），Rust 那边没有文件系统代码；
 * 私钥字节从 [DeviceKey.copyPrivateKeyForRust] 拿一次、写完立刻填零。
 */
class DeviceKeyStore(
    private val dir: Path,
    private val clock: () -> Long = System::currentTimeMillis,
    private val random: SecureRandom = SecureRandom(),
) {
    val file: Path get() = dir.resolve(FILE_NAME)

    /** 读本机的设备密钥；没有 / 坏了 / 设备 id 对不上都返回 null（不抛，界面按"还没配对"处理）。 */
    fun load(deviceId: String): DeviceKey? {
        val stored = runCatching { readStored() }.getOrNull() ?: return null
        if (stored.device != deviceId) return null
        val priv = SyncCrypto.base64UrlDecode(stored.privateKey) ?: return null
        val key = DeviceKey.of(deviceId, priv) ?: return null
        // 文件里的公钥必须和私钥算出来的一致；对不上说明文件被改过，当作没有
        if (!key.publicKey.keyEquals(stored.publicKey)) return null
        return key
    }

    /** 读，没有就生成一把并落盘。 */
    fun loadOrCreate(deviceId: String): DeviceKey {
        load(deviceId)?.let { return it }
        val key = DeviceKey.generate(deviceId, random)
        save(key)
        return key
    }

    /** 写（覆盖）。目录不存在就建；文件权限 600。 */
    fun save(key: DeviceKey) {
        Files.createDirectories(dir)
        val privateKey = key.copyPrivateKey()
        val stored = try {
            StoredDeviceKey(
                v = FORMAT_VERSION,
                device = key.deviceId,
                privateKey = SyncCrypto.base64Url(privateKey),
                publicKey = key.publicKeyBase64,
                createdAt = clock(),
            )
        } finally {
            privateKey.fill(0)
        }
        val bytes = JSON.encodeToString(StoredDeviceKey.serializer(), stored).toByteArray(Charsets.UTF_8)
        val tmp = dir.resolve("$FILE_NAME.tmp-${SyncCrypto.base64Url(ByteArray(6).also(random::nextBytes))}")
        try {
            Files.write(tmp, bytes)
            restrictToOwner(tmp)
            try {
                Files.move(tmp, file, StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
            } catch (e: AtomicMoveNotSupportedException) {
                Files.move(tmp, file, StandardCopyOption.REPLACE_EXISTING)
            }
            restrictToOwner(file)
        } finally {
            runCatching { Files.deleteIfExists(tmp) }
        }
    }

    /** 删掉本机的设备密钥（"这台设备不再同步"时用；配对的设备列表不在这里）。 */
    fun delete(): Boolean = runCatching { Files.deleteIfExists(file) }.getOrDefault(false)

    private fun readStored(): StoredDeviceKey? {
        if (!Files.isRegularFile(file)) return null
        if (Files.size(file) > MAX_FILE_BYTES) return null
        val text = String(Files.readAllBytes(file), Charsets.UTF_8)
        return JSON.decodeFromString(StoredDeviceKey.serializer(), text)
    }

    /** 权限压到 rw-------（POSIX 不行就退回 File API；安卓 / Windows 上都不抛）。 */
    private fun restrictToOwner(path: Path) {
        val posix = runCatching {
            Files.setPosixFilePermissions(
                path,
                setOf(PosixFilePermission.OWNER_READ, PosixFilePermission.OWNER_WRITE),
            )
        }
        if (posix.isSuccess) return
        runCatching {
            val f = path.toFile()
            f.setReadable(false, false)
            f.setWritable(false, false)
            f.setReadable(true, true)
            f.setWritable(true, true)
        }
    }

    @Serializable
    private data class StoredDeviceKey(
        val v: Int,
        val device: String,
        val privateKey: String,
        val publicKey: String,
        val createdAt: Long,
    )

    companion object {
        const val FILE_NAME = "sync-device-key.json"
        private const val FORMAT_VERSION = 1
        private const val MAX_FILE_BYTES = 4 * 1024L
        private val JSON = Json { ignoreUnknownKeys = true }
    }
}

/** 两段公钥（base64url）比较，不因为编码/填充差异误判。 */
private fun ByteArray.keyEquals(base64: String): Boolean {
    val other = SyncCrypto.base64UrlDecode(base64) ?: return false
    return other.contentEquals(this)
}

/**
 * 配对时给用户看的**6 位短校验码**：两边公钥 + 两边设备 id 的 SHA-256 取前 4 字节，
 * 对 1,000,000 取余、补足 6 位。**实现在 Rust**（`sync/crypto.rs` 的 `pairing_code`）。
 *
 * 两台设备算出来必须一样（和设备顺序无关），所以按设备 id 排序后再拼。
 * 用户目测两边一致才点「确认」——防扫错码 / 局域网中间人。
 */
object PairingCode {
    fun of(aDevice: String, aPublicKey: ByteArray, bDevice: String, bPublicKey: ByteArray): String {
        require(aDevice != bDevice) { "不能和自己配对" }
        require(aPublicKey.size == DeviceKey.PUBLIC_KEY_BYTES) { "设备公钥长度不对" }
        require(bPublicKey.size == DeviceKey.PUBLIC_KEY_BYTES) { "设备公钥长度不对" }
        return RustSync.guard { ffiSyncPairingCode(aDevice, aPublicKey, bDevice, bPublicKey) }
            ?: throw IllegalArgumentException("算不出配对码")
    }
}

/** HKDF-SHA256（RFC 5869）。**实现在 Rust**。age 那种"每个收件人一份包好的文件密钥"用它派生包密钥。 */
object Hkdf {
    fun sha256(ikm: ByteArray, salt: ByteArray, info: ByteArray, length: Int): ByteArray {
        require(length in 1..(255 * 32)) { "HKDF 输出长度不合法" }
        return RustSync.guard { ffiSyncHkdfSha256(ikm, salt, info, length.toUInt()) }
    }
}

/**
 * HLC 的编解码 / 比较——**数值运算在 Rust**（`sync/crypto.rs` 的 `hlc`）。
 *
 * 为什么要过边界：平局按设备 id 定胜负是合并正确性的一部分，两份实现漂一点点，
 * 两台设备的合并结果就会不一样。这里只是转发。
 */
internal object SyncHlcNative {
    fun of(physicalMs: Long, counter: Int): Long = RustSync.guard { ffiSyncHlcOf(physicalMs, counter) }
    fun physical(hlc: Long): Long = RustSync.guard { ffiSyncHlcPhysical(hlc) }
    fun counter(hlc: Long): Int = RustSync.guard { ffiSyncHlcCounter(hlc) }
    fun compareVersion(hlcA: Long, devA: String, hlcB: Long, devB: String): Int =
        RustSync.guard { ffiSyncHlcCompareVersion(hlcA, devA, hlcB, devB) }
    fun maxVersion(hlcA: Long, devA: String, hlcB: Long, devB: String): Long =
        RustSync.guard { ffiSyncHlcMaxVersion(hlcA, devA, hlcB, devB) }
}
