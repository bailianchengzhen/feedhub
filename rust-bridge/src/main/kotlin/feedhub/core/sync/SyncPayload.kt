package feedhub.core.sync

import feedhub.core.FeedCookieJar
import feedhub.core.Platform
import okhttp3.Cookie
import uniffi.feedhub_ffi.FfiCookieEntry
import uniffi.feedhub_ffi.FfiLoginBuildResult
import uniffi.feedhub_ffi.FfiLoginCount
import uniffi.feedhub_ffi.FfiLoginPackage
import uniffi.feedhub_ffi.FfiLoginPlatform
import uniffi.feedhub_ffi.FfiLoginSkip
import uniffi.feedhub_ffi.ffiSyncLoginApply
import uniffi.feedhub_ffi.ffiSyncLoginBuild
import uniffi.feedhub_ffi.ffiSyncLoginPackageDecode
import uniffi.feedhub_ffi.ffiSyncLoginPackageEncode

/**
 * 同步的明文（**只在内存里、只在 AES-GCM 里面**出现）：平台 id → Cookie 列表。
 *
 * 为什么带 Cookie 的完整属性：另一台设备要和这一台一模一样（域、路径、hostOnly、secure、
 * httpOnly、过期时间），只传 name=value 会把 host-only/域 Cookie 的区别丢掉，登录态可能在子域上失效。
 *
 * 平台 id 用 [Platform.key] 的字符串。发送方和接收方各按**自己**装的插件算"这个平台的地盘"，
 * **不信任报文里给的域**（见 [SyncScope]）。
 *
 * 编解码 / 校验 / 先清后写全在 Rust（`feedhub-core/src/sync/login.rs`）；这里只是过边界的模型。
 */
data class SyncPackage(
    val v: Int = SyncProtocol.VERSION,
    val sentAtMs: Long = 0L,
    val platforms: List<SyncPlatform> = emptyList(),
)

data class SyncPlatform(
    val id: String,
    val cookies: List<SyncCookie> = emptyList(),
)

data class SyncCookie(
    val name: String,
    val value: String,
    val domain: String,
    val path: String = "/",
    val expiresAt: Long,
    val secure: Boolean = false,
    val httpOnly: Boolean = false,
    val hostOnly: Boolean = false,
) {
    fun toCookie(): Cookie = Cookie.Builder()
        .name(name).value(value)
        // OkHttp 的 Cookie 里域一律不带前导点（Cookie.parse 会剥掉）；对端就算写了个点也按域 Cookie 处理。
        .apply { if (hostOnly) hostOnlyDomain(domain.trimStart('.')) else domain(domain.trimStart('.')) }
        .path(path).expiresAt(expiresAt)
        .apply { if (secure) secure() }
        .apply { if (httpOnly) httpOnly() }
        .build()

    companion object {
        fun of(c: Cookie) = SyncCookie(
            name = c.name,
            value = c.value,
            domain = c.domain,
            path = c.path,
            expiresAt = c.expiresAt,
            secure = c.secure,
            httpOnly = c.httpOnly,
            hostOnly = c.hostOnly,
        )
    }
}

internal fun SyncCookie.toFfi(): FfiCookieEntry = FfiCookieEntry(
    name = name,
    value = value,
    domain = domain,
    path = path,
    expiresAtMs = expiresAt,
    secure = secure,
    httpOnly = httpOnly,
    hostOnly = hostOnly,
)

internal fun FfiCookieEntry.toModel(): SyncCookie = SyncCookie(
    name = name,
    value = value,
    domain = domain,
    path = path,
    expiresAt = expiresAtMs,
    secure = secure,
    httpOnly = httpOnly,
    hostOnly = hostOnly,
)

internal fun SyncPackage.toFfi(): FfiLoginPackage = FfiLoginPackage(
    v = v,
    sentAtMs = sentAtMs,
    platforms = platforms.map { p -> FfiLoginPlatform(p.id, p.cookies.map { it.toFfi() }) },
)

internal fun FfiLoginPackage.toModel(): SyncPackage = SyncPackage(
    v = v,
    sentAtMs = sentAtMs,
    platforms = platforms.map { p -> SyncPlatform(p.id, p.cookies.map { it.toModel() }) },
)

/**
 * 明文报文的编解码 + 形状校验。校验在**解密之后**做（密文是二进制的，先按上限读，
 * 解不开就直接拒；解得开也不能信它的内容）。
 *
 * 实现在 Rust：编码和 kotlinx.serialization（`encodeDefaults = true`）**逐字节一致**，
 * 这样同密钥同 nonce 的密文能和 Kotlin 原版对拍；解码 `ignoreUnknownKeys = true`，
 * **v1 的包新版本照读**（老客户端发来的）。
 */
object SyncCodec {
    fun encode(pkg: SyncPackage): ByteArray =
        syncGuard { ffiSyncLoginPackageEncode(pkg.toFfi()) }

    fun decode(bytes: ByteArray): SyncPackage =
        syncGuard { ffiSyncLoginPackageDecode(bytes) }.toModel()
}

/**
 * 发送方：从本机 Cookie 罐里挑出选中平台的 Cookie，做成 [SyncPackage]。
 *
 * 只挑、不改罐子；`expiresAt <= now` 的过期 Cookie 不带走。
 * 一个平台**一条都没有就整个跳过**：绝不发一个"空平台"过去，否则接收端按协议会先清它的域。
 */
object SyncPackageBuilder {
    fun build(
        jar: FeedCookieJar,
        platforms: Collection<Platform>,
        scopes: Map<Platform, SyncScope>,
        now: Long = System.currentTimeMillis(),
    ): SyncBuildResult {
        val result: FfiLoginBuildResult = syncGuard {
            ffiSyncLoginBuild(jar.rustJarOrThrow(), platforms.map { it.key }, scopes.toFfiScopes(), now)
        }
        return SyncBuildResult(
            pkg = result.`package`.toModel(),
            counts = result.counts.toCounts(),
            skipped = result.skipped.map { Platform(it) },
        )
    }
}

/**
 * 接收方：把这些 Cookie 按平台**只写进它自己的域**——先 `clearDomain` 再写。
 *
 * 三条硬规矩（在 Rust 里执行）：
 * 1. 平台必须是本端装了的（[scopes] 里有），否则整段忽略；
 * 2. Cookie 的域必须在本端这个平台的 Scope 里（对端塞别的域一概不要）；
 * 3. 过滤完一条不剩就**不清也不写**（不能让一个坏包清掉本机现有的登录态）。
 */
object SyncApplier {
    fun apply(
        jar: FeedCookieJar,
        pkg: SyncPackage,
        scopes: Map<Platform, SyncScope>,
        now: Long = System.currentTimeMillis(),
    ): SyncApplyResult {
        val result = syncGuard {
            ffiSyncLoginApply(jar.rustJarOrThrow(), pkg.toFfi(), scopes.toFfiScopes(), now)
        }
        return SyncApplyResult(
            applied = result.applied.toCounts(),
            skipped = result.skipped.associate { it.platformId to it.reason },
        )
    }
}

/** 出码方界面默认勾哪些：本机 Cookie 罐里有该平台域下未过期 Cookie 的平台（按插件顺序）。 */
object SyncDefaults {
    fun platforms(
        jar: FeedCookieJar,
        scopes: Map<Platform, SyncScope>,
        now: Long = System.currentTimeMillis(),
    ): List<Platform> {
        val names: List<String> = syncGuard {
            uniffi.feedhub_ffi.ffiSyncLoginDefaultPlatforms(jar.rustJarOrThrow(), scopes.toFfiScopes(), now)
        }
        return names.map(::Platform)
    }
}

/** 给 `FfiLoginCount` 列表排序 / 过滤用（只读平台 id + 个数）。 */
internal fun List<FfiLoginCount>.toPlatformCounts(): List<SyncPlatformCount> =
    map { SyncPlatformCount(Platform(it.platformId), it.count.toInt()) }

/** 把 scopes 里不认识的平台 id 标出来（界面显示"这台设备没有这个平台，会跳过"）。 */
internal fun Map<String, String>.knownSkipped(known: Collection<Platform>): Map<String, String> =
    filterKeys { id -> known.none { it.key == id } }
