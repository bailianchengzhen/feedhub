package feedhub.core.sync

import feedhub.rust.RustSync
import uniffi.feedhub_ffi.ffiSyncBase64urlDecode
import uniffi.feedhub_ffi.ffiSyncBase64urlEncode
import uniffi.feedhub_ffi.ffiSyncGcmOpen
import uniffi.feedhub_ffi.ffiSyncGcmSeal
import uniffi.feedhub_ffi.ffiSyncProtocol
import uniffi.feedhub_ffi.ffiSyncProtocolAad
import java.security.SecureRandom

/**
 * 「登录互传」的线上协议常量。
 *
 * 只有两件事要过网：**一次** `GET /v2/sync/<会话 id>`（发出去）或 `POST`（收进来）的密文，
 * 别的什么都不传。明文（平台 → Cookie）用二维码里的 256 位一次性密钥做 **AES-256-GCM**
 * 认证加密；附加认证数据（AAD）把密文绑在会话 id + 协议版本 + 方向上，换会话 / 换版本 /
 * 翻方向都解不开。
 *
 * 数值来自 Rust（`feedhub-core/src/sync/offer.rs` 的 `protocol`）：**协议常量只能有一份**，
 * 否则改了这边忘了那边，两端就悄悄不兼容了。
 */
object SyncProtocol {
    private val values by lazy { RustSync.guard { ffiSyncProtocol() } }

    /** 二维码 / URL 的协议版本。改协议就 +1（旧客户端会明着拒绝，不会猜着解）。 */
    val VERSION: Int get() = values.version

    /** 老版本（没有方向字段）的协议版本：新客户端扫到 v1 的码照能扫。 */
    val VERSION_V1: Int get() = values.versionV1

    /** 二维码内容的开头，用来和别的二维码区分（扫到别的码直接忽略）。 */
    val QR_PREFIX: String get() = values.qrPrefix

    /** 服务端只认这一个路径前缀，后面接会话 id。 */
    val PATH_PREFIX: String get() = values.pathPrefix

    /** v1 的路径前缀（兼容老码）。 */
    val PATH_PREFIX_V1: String get() = values.pathPrefixV1

    /** 二维码有效期：两分钟（和服务端的看门狗是同一个数）。 */
    val TTL_MS: Long get() = values.ttlMs

    val KEY_BYTES: Int get() = values.keyBytes.toInt()
    val SESSION_BYTES: Int get() = values.sessionBytes.toInt()
    const val NONCE_BYTES = 12
    const val TAG_BITS = 128

    /** 响应体（密文）上限：正常的 Cookie 罐远小于它，超了不读。 */
    val MAX_SEALED_BYTES: Int get() = values.maxSealedBytes.toInt()

    /** 明文上限定得比密文小（GCM 只多一个 nonce + tag）。 */
    val MAX_PLAIN_BYTES: Int get() = values.maxPlainBytes.toInt()

    /** 一次同步最多带多少个 Cookie（防止对端塞爆内存）。 */
    val MAX_COOKIES: Int get() = values.maxCookies.toInt()

    /**
     * GCM 的附加认证数据：协议版本 + 会话 id + 方向（v2）。
     * 二维码里只放会话 id / 方向，不放这个，两端自己拼。
     */
    fun aad(sessionId: String, direction: SyncDirection): ByteArray =
        RustSync.guard { ffiSyncProtocolAad(sessionId, direction.toFfi()) }
}

/** 同步过程中可以直接给用户看的失败（消息里**不含**密钥 / Cookie 值）。 */
class SyncException(message: String, cause: Throwable? = null) : Exception(message, cause)

/**
 * 同步用的密码学：随机密钥 / 会话 id、AES-GCM 封装（nonce 前缀在密文前）、Base64URL 编解码。
 * **算法实现在 Rust**（`feedhub-core/src/sync/crypto.rs`），和 Kotlin 原实现逐字节相同
 * （`SyncCryptoDualRunTest` 用固定密钥 / nonce 的定值向量对拍，`SyncFileInteropTest` 验互通）。
 *
 * **不做任何降级**：解不开就是解不开。
 */
object SyncCrypto {
    private val random = SecureRandom()

    fun newKey(): ByteArray = ByteArray(SyncProtocol.KEY_BYTES).also(random::nextBytes)

    /** 128 位会话 id，base64url（22 个字符）。服务端路径里带的那个。 */
    fun newSessionId(): String = base64Url(ByteArray(SyncProtocol.SESSION_BYTES).also(random::nextBytes))

    /**
     * 封装：`nonce(12) || ciphertext || tag(16)`。
     * @throws IllegalArgumentException 密钥长度不对。
     */
    fun seal(key: ByteArray, aad: ByteArray, plaintext: ByteArray): ByteArray {
        require(key.size == SyncProtocol.KEY_BYTES) { "同步密钥必须是 ${SyncProtocol.KEY_BYTES} 字节" }
        val nonce = ByteArray(SyncProtocol.NONCE_BYTES).also(random::nextBytes)
        return nonce + RustSync.guard { ffiSyncGcmSeal(key, nonce, plaintext, aad) }
    }

    /**
     * 解开；**密钥不对、内容被改、AAD 不匹配、长度不对**一律返回 null（不抛细节，免得调用方误当成"再试一次"）。
     */
    fun open(key: ByteArray, aad: ByteArray, sealed: ByteArray): ByteArray? {
        if (key.size != SyncProtocol.KEY_BYTES) return null
        if (sealed.size < SyncProtocol.NONCE_BYTES + SyncProtocol.TAG_BITS / 8) return null
        val nonce = sealed.copyOfRange(0, SyncProtocol.NONCE_BYTES)
        val body = sealed.copyOfRange(SyncProtocol.NONCE_BYTES, sealed.size)
        return RustSync.guard { ffiSyncGcmOpen(key, nonce, body, aad) }
    }

    fun base64Url(bytes: ByteArray): String = RustSync.guard { ffiSyncBase64urlEncode(bytes) }

    fun base64UrlDecode(text: String): ByteArray? = RustSync.guard { ffiSyncBase64urlDecode(text) }

    /**
     * **只给对拍**：用指定的 nonce 封装（返回 `ciphertext || tag`，**不带** nonce）。
     * 生产路径的 [seal] 每次取新 nonce，定值向量要比对必须能固定它。
     */
    internal fun sealWithNonce(key: ByteArray, nonce: ByteArray, aad: ByteArray, plaintext: ByteArray): ByteArray {
        require(key.size == SyncProtocol.KEY_BYTES) { "同步密钥必须是 ${SyncProtocol.KEY_BYTES} 字节" }
        return RustSync.guard { ffiSyncGcmSeal(key, nonce, plaintext, aad) }
    }

    /** **只给对拍**：和 [seal] 一样（nonce 前缀在密文前），但 nonce 由调用方给。 */
    internal fun sealForTest(key: ByteArray, nonce: ByteArray, aad: ByteArray, plaintext: ByteArray): ByteArray =
        nonce + RustSync.guard { ffiSyncGcmSeal(key, nonce, plaintext, aad) }
}
