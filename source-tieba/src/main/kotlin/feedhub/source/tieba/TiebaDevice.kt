package feedhub.source.tieba

import feedhub.core.KeyValueStore
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.UUID
import java.util.zip.CRC32

/**
 * 假装成的那台手机。随机生成一次后存进 `ctx.store`（`tieba.android_id` / `tieba.uuid` / `tieba.client_id`），
 * 以后一直用同一组，和真 App 装一次就不变一样。派生出来的 `cuid_galaxy2`、`c3_aid` 算法见 [TiebaDeviceIds]。
 */
internal class TiebaDevice(
    /** 16 位小写十六进制，等价于 Android 的 Settings.Secure.ANDROID_ID。 */
    val androidId: String,
    val uuid: String,
    /** `wappc_{毫秒时间戳}_{三位随机数}`，TiebaLite `RetrofitTiebaApi.randomClientId` 的格式。 */
    val clientId: String,
) {
    val cuidGalaxy2: String = TiebaDeviceIds.cuidGalaxy2(androidId)
    val c3Aid: String = TiebaDeviceIds.c3Aid(androidId, uuid)

    /** "安装时间"：取 client_id 里的时间戳，免得再多存一个键。 */
    val installTimeMs: Long = clientId.split('_').getOrNull(1)?.toLongOrNull() ?: 0L

    companion object {
        const val BRAND = "Xiaomi"
        const val MODEL = "23127PN0CC"
        const val OS_VERSION = "34"
        const val IMEI = "000000000000000"
        const val SCREEN_W = 1080
        const val SCREEN_H = 2400
        const val SCREEN_DIP = 3.0

        // internal 而不是 private：`TiebaRequestEquivalenceTest` 要塞固定设备标识进 store，
        // 好让 Kotlin 版 / Rust 版两次运行的请求体逐字节可比（不改这两个键的含义）。
        internal const val KEY_ANDROID_ID = "tieba.android_id"
        internal const val KEY_UUID = "tieba.uuid"
        internal const val KEY_CLIENT_ID = "tieba.client_id"

        @Synchronized
        fun load(store: KeyValueStore): TiebaDevice {
            val random = SecureRandom()
            val androidId = store.get(KEY_ANDROID_ID) ?: ByteArray(8).also(random::nextBytes)
                .joinToString("") { "%02x".format(it.toInt() and 0xff) }
                .also { store.put(KEY_ANDROID_ID, it) }
            val uuid = store.get(KEY_UUID) ?: UUID.randomUUID().toString().also { store.put(KEY_UUID, it) }
            val clientId = store.get(KEY_CLIENT_ID)
                ?: "wappc_${System.currentTimeMillis()}_${100 + random.nextInt(900)}".also { store.put(KEY_CLIENT_ID, it) }
            return TiebaDevice(androidId, uuid, clientId)
        }

        /**
         * 保证三个键都在 `store` 里（缺的补一次随机值，和 [load] 完全一样）。
         *
         * Rust 平台对象只**读**这三个键（`feedhub-core/src/tieba/device.rs`：不取随机数、不造身份），
         * 所以第一次用它之前先在这里补一次；之后一直用同一组，和真 App 装一次就不变一样。
         */
        @Synchronized
        fun ensure(store: KeyValueStore) {
            load(store)
        }
    }
}

/**
 * 贴吧 App 的设备标识算法（百度 "helios" 哈希）。
 *
 * 移植自 aiotieba `src/aiotieba/helper/crypto/csrc/tbcrypto/cuid.c`（Unlicense），
 * 和 TiebaLite `utils/helios/Hasher.java`、`utils/CuidUtils.kt`、`utils/UIDUtil.kt`（GPL-3.0）的结果一致。
 * 纯函数。
 */
internal object TiebaDeviceIds {
    /** `MD5("com.baidu" + androidId)` 的大写十六进制 + `|V` + helios(前面那串) 的 base32。 */
    fun cuidGalaxy2(androidId: String): String {
        val md5Hex = MessageDigest.getInstance("MD5").digest("com.baidu$androidId".toByteArray())
            .joinToString("") { "%02X".format(it.toInt() and 0xff) }
        return "$md5Hex|V" + base32(helios(md5Hex.toByteArray()))
    }

    /** `A00-` + base32(SHA1("com.helios" + androidId + uuid)) + `-` + helios(前面那串) 的 base32。 */
    fun c3Aid(androidId: String, uuid: String): String {
        val sha1 = MessageDigest.getInstance("SHA-1").digest("com.helios$androidId$uuid".toByteArray())
        val head = "A00-" + base32(sha1) + "-"
        return head + base32(helios(head.toByteArray()))
    }

    /** 5 字节的 helios 哈希：CRC32 和 xxHash32 交替混进一个 40 位的状态。 */
    fun helios(src: ByteArray): ByteArray {
        var sec = (1L shl 40) - 1
        val buf = ByteArray(STEP * 4)
        buf.fill(0xff.toByte(), 0, STEP)

        val crc = CRC32()
        crc.update(src)
        crc.update(buf, 0, STEP)
        sec = mix(sec, crc.value, 8, xor = false)
        write(buf, STEP, sec)

        sec = mix(sec, xxh32(src + buf.copyOfRange(0, STEP * 2)).toLong() and 0xffffffffL, 0, xor = true)
        write(buf, STEP * 2, sec)

        sec = mix(sec, xxh32(src + buf.copyOfRange(0, STEP * 3)).toLong() and 0xffffffffL, 1, xor = true)
        write(buf, STEP * 3, sec)

        crc.update(buf, STEP, STEP * 3)
        sec = mix(sec, crc.value, 7, xor = true)

        return ByteArray(STEP).also { write(it, 0, sec) }
    }

    private const val STEP = 5

    /** 把 [sec] 的第 start..start+31 位换成（它和 [hash] 做 xor / and 的结果）。 */
    private fun mix(sec: Long, hash: Long, start: Int, xor: Boolean): Long {
        val mask = (1L shl (start + 32)) - 1
        val cur = (mask and sec) ushr start
        val v = if (xor) cur xor hash else cur and hash
        var out = sec
        for (i in 0 until 32) {
            val bit = 1L shl (start + i)
            out = if (v and (1L shl i) != 0L) out or bit else out and bit.inv()
        }
        return out
    }

    private fun write(dst: ByteArray, off: Int, sec: Long) {
        for (i in 0 until STEP) dst[off + i] = (sec ushr (8 * i)).toByte()
    }

    /** RFC 4648 base32，不补 `=`。 */
    fun base32(bytes: ByteArray): String = buildString {
        var buffer = 0
        var bits = 0
        for (b in bytes) {
            buffer = (buffer shl 8) or (b.toInt() and 0xff)
            bits += 8
            while (bits >= 5) {
                append(ALPHABET[(buffer ushr (bits - 5)) and 31])
                bits -= 5
            }
        }
        if (bits > 0) append(ALPHABET[(buffer shl (5 - bits)) and 31])
    }

    private const val ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"

    /** xxHash32（seed 0），按 xxHash 规范实现。 */
    fun xxh32(data: ByteArray, seed: Int = 0): Int {
        val len = data.size
        var i = 0
        var h: Int
        if (len >= 16) {
            var v1 = seed + P1 + P2
            var v2 = seed + P2
            var v3 = seed
            var v4 = seed - P1
            while (i <= len - 16) {
                v1 = round(v1, intLE(data, i)); i += 4
                v2 = round(v2, intLE(data, i)); i += 4
                v3 = round(v3, intLE(data, i)); i += 4
                v4 = round(v4, intLE(data, i)); i += 4
            }
            h = v1.rotateLeft(1) + v2.rotateLeft(7) + v3.rotateLeft(12) + v4.rotateLeft(18)
        } else {
            h = seed + P5
        }
        h += len
        while (i + 4 <= len) {
            h = (h + intLE(data, i) * P3).rotateLeft(17) * P4
            i += 4
        }
        while (i < len) {
            h = (h + (data[i].toInt() and 0xff) * P5).rotateLeft(11) * P1
            i++
        }
        h = (h xor (h ushr 15)) * P2
        h = (h xor (h ushr 13)) * P3
        return h xor (h ushr 16)
    }

    private fun round(acc: Int, input: Int): Int = (acc + input * P2).rotateLeft(13) * P1

    private fun intLE(b: ByteArray, i: Int): Int =
        (b[i].toInt() and 0xff) or ((b[i + 1].toInt() and 0xff) shl 8) or
            ((b[i + 2].toInt() and 0xff) shl 16) or ((b[i + 3].toInt() and 0xff) shl 24)

    private const val P1 = -0x61c8864f // 0x9E3779B1
    private const val P2 = -0x7a143589 // 0x85EBCA77
    private const val P3 = -0x3d4d51c3 // 0xC2B2AE3D
    private const val P4 = 0x27D4EB2F
    private const val P5 = 0x165667B1
}
