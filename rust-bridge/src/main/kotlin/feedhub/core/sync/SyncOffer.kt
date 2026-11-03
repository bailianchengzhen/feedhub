package feedhub.core.sync

import uniffi.feedhub_ffi.FfiSyncDirection
import uniffi.feedhub_ffi.FfiSyncOffer
import uniffi.feedhub_ffi.ffiSyncOfferAad
import uniffi.feedhub_ffi.ffiSyncOfferContent
import uniffi.feedhub_ffi.ffiSyncOfferLooksLike
import uniffi.feedhub_ffi.ffiSyncOfferParse
import uniffi.feedhub_ffi.ffiSyncOfferPath
import uniffi.feedhub_ffi.ffiSyncOfferUrl

/**
 * 二维码里装的东西：出码方的局域网地址、端口、一次性会话 id、256 位一次性密钥、协议版本和方向。
 * **不含任何 Cookie**；二维码本身就是"两分钟内能用一次的入场券"。
 *
 * 线上形状（单行，v2 多一个方向字段）：
 * ```
 * feedhub-sync:2?h=192.168.0.5&p=43127&s=<22 字符 base64url>&k=<43 字符 base64url>&d=send|receive
 * ```
 * 字段都是必填、不认识的多余字段直接拒（协议宁严勿松）；host 只认 IPv4 字面量，
 * 免得二维码把客户端引去做 DNS 或连到 `localhost` 以外的东西（10.0.2.2 这类模拟器地址也认）。
 *
 * **v1 的码（没有 `d=`）新版本照能扫**，解析出来按"发出去"处理（老客户端只会发出去）。
 *
 * 解析 / 格式化在 Rust（`feedhub-core/src/sync/offer.rs`）。故意不重写 [toString] 出密钥：
 * 日志 / 异常里只会看到地址和会话 id。
 */
class SyncOffer(
    val host: String,
    val port: Int,
    val sessionId: String,
    private val keyBytes: ByteArray,
    val version: Int = SyncProtocol.VERSION,
    val direction: SyncDirection = SyncDirection.SEND,
) {
    init {
        // 构造器的四处 `require`（版本 / IPv4 字面量 / 端口 / 密钥长度 / 会话 id 长度）在 Rust。
        // 把那条错误翻回 IllegalArgumentException——构造器的契约一直是它。
        try {
            syncGuard { ffiSyncOfferPath(raw()) }
        } catch (e: SyncException) {
            throw IllegalArgumentException(e.message)
        }
    }

    /** 一次性密钥的副本（调用方可以随便改）。 */
    val key: ByteArray get() = keyBytes.copyOf()

    internal fun keyInternal(): ByteArray = keyBytes

    internal fun toFfi(): FfiSyncOffer = raw()

    /** 二维码内容。 */
    val content: String get() = syncGuard { ffiSyncOfferContent(toFfiRaw()) }

    /** 服务端只认的那一个路径（v1 / v2 的路径前缀由 Rust 按版本选）。 */
    val path: String get() = syncGuard { ffiSyncOfferPath(toFfiRaw()) }

    /** 扫码方去连的地址（明文 HTTP 只传密文）。 */
    val url: String get() = syncGuard { ffiSyncOfferUrl(toFfiRaw()) }

    /** GCM 的附加认证数据（会话 id + 方向绑进密文；v1 只有会话 id）。 */
    fun aad(): ByteArray = syncGuard { ffiSyncOfferAad(toFfiRaw()) }

    /** 日志 / 调试用：不含密钥。 */
    override fun toString(): String =
        "SyncOffer($host:$port, session=$sessionId, v=$version, dir=$direction)"

    override fun equals(other: Any?): Boolean =
        other is SyncOffer && other.host == host && other.port == port &&
            other.sessionId == sessionId && other.keyBytes.contentEquals(keyBytes) &&
            other.version == version && other.direction == direction

    override fun hashCode(): Int =
        listOf(host, port, sessionId, version, direction).hashCode() * 31 + keyBytes.contentHashCode()

    private fun toFfiRaw(): FfiSyncOffer = raw()

    private fun raw(): FfiSyncOffer = FfiSyncOffer(
        host = host,
        port = port.toUInt(),
        sessionId = sessionId,
        key = keyBytes,
        version = version,
        direction = direction.toFfi(),
    )

    companion object {
        /** 生成一个新会话（新密钥 + 新会话 id）。生产路径的出码方用 Rust 的 `start_new` 生成。 */
        fun create(host: String, port: Int, direction: SyncDirection = SyncDirection.SEND): SyncOffer =
            SyncOffer(host, port, SyncCrypto.newSessionId(), SyncCrypto.newKey(), direction = direction)

        /** 这个字符串是不是我们这种二维码（别的二维码一律不理）。 */
        fun looksLikeSync(content: String): Boolean = syncGuard { ffiSyncOfferLooksLike(content) }

        /**
         * 解析扫到 / 粘进来的二维码内容；**任何一处不对都返回 null**（不猜、不自动修正）。
         * 调用方给用户看的是"二维码无效，请重新生成"，不把密钥写进错误里。
         */
        fun parse(content: String): SyncOffer? = syncGuard {
            ffiSyncOfferParse(content)?.let { fromFfi(it) }
        }

        internal fun fromFfi(offer: FfiSyncOffer): SyncOffer = SyncOffer(
            host = offer.host,
            port = offer.port.toInt(),
            sessionId = offer.sessionId,
            keyBytes = offer.key,
            version = offer.version,
            direction = SyncDirection.fromFfi(offer.direction),
        )
    }
}

/** 给 UI / 调试：方向的中文名直接从枚举来。 */
internal fun FfiSyncDirection.toDirection(): SyncDirection = SyncDirection.fromFfi(this)
