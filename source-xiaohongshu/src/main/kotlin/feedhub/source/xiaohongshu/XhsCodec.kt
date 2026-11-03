package feedhub.source.xiaohongshu

import java.security.MessageDigest
import java.util.zip.CRC32

/**
 * 小红书签名用到的小算法：自定义字母表的 Base64、MD5、xxHash32、MurmurHash3、RC4、CRC32 变体、自定义 4 路 ARX 哈希。
 * 全是纯函数，没有网络。
 *
 * 参考：
 * - xhshow（MIT）`utils/encoder.py` `utils/hash.py` `core/crc32_encrypt.py` `core/crypto.py:_custom_hash_v2`；
 * - cv-cat/Spider_XHS（同一作者已把这些脚本以 MIT 收进 cv-cat/catbus 的 `static/xhs/`）：`xhs_core/js/mns.js:dsf`、`xs_common.js:gens9`、`b1.js:rc4BinaryString`、
 *   `xhs_pc/params.py:murmur_hash3_32`。
 * 字母表、哈希初值这类常量是协议本身的事实，两个项目逐字一致。
 */
internal object XhsCodec {
    /** X-s 外壳、X-S-Common、b1 用的字母表。 */
    const val XS_ALPHABET = "ZmserbBoHQtNP+wOcza/LpngG8yJq42KWYj0DSfdikx3VT16IlUAFM97hECvuRX5"

    /** X-s 里层 x3（`mns0301_`）用的字母表。 */
    const val X3_ALPHABET = "MfgqrsbcyzPQRStuvC7mn501HIJBo2DEFTKdeNOwxWXYZap89+/A4UVLhijkl63G"

    fun base64(bytes: ByteArray, alphabet: String): String {
        val out = StringBuilder((bytes.size + 2) / 3 * 4)
        var i = 0
        while (i < bytes.size) {
            val b0 = bytes[i].toInt() and 0xFF
            val b1 = if (i + 1 < bytes.size) bytes[i + 1].toInt() and 0xFF else -1
            val b2 = if (i + 2 < bytes.size) bytes[i + 2].toInt() and 0xFF else -1
            out.append(alphabet[b0 shr 2])
            out.append(alphabet[((b0 and 3) shl 4) or (if (b1 < 0) 0 else b1 shr 4)])
            if (b1 < 0) {
                out.append("==")
            } else {
                out.append(alphabet[((b1 and 15) shl 2) or (if (b2 < 0) 0 else b2 shr 6)])
                if (b2 < 0) out.append('=') else out.append(alphabet[b2 and 63])
            }
            i += 3
        }
        return out.toString()
    }

    /** [base64] 的逆运算，测试和 [XhsTokenParts] 拆包用。字母表外的字符抛 [IllegalArgumentException]。 */
    fun base64Decode(text: String, alphabet: String): ByteArray {
        val body = text.trimEnd('=')
        val out = java.io.ByteArrayOutputStream(body.length * 3 / 4)
        var buffer = 0
        var bits = 0
        for (c in body) {
            val v = alphabet.indexOf(c)
            require(v >= 0) { "字母表里没有 '$c'" }
            buffer = (buffer shl 6) or v
            bits += 6
            if (bits >= 8) {
                bits -= 8
                out.write((buffer shr bits) and 0xFF)
            }
        }
        return out.toByteArray()
    }

    /** 和 JS 的 `JSON.stringify` 一样转义一个字符串（`/` 和非 ASCII 字符不转义）。签名里的 JSON 要和网页逐字节一致。 */
    fun jsonString(s: String): String = buildString(s.length + 2) {
        append('"')
        for (c in s) {
            when {
                c == '"' -> append("\\\"")
                c == '\\' -> append("\\\\")
                c == '\b' -> append("\\b")
                c == '\u000c' -> append("\\f")
                c == '\n' -> append("\\n")
                c == '\r' -> append("\\r")
                c == '\t' -> append("\\t")
                c < ' ' -> append("\\u%04x".format(c.code))
                else -> append(c)
            }
        }
        append('"')
    }

    fun md5(bytes: ByteArray): ByteArray = MessageDigest.getInstance("MD5").digest(bytes)

    fun md5(text: String): ByteArray = md5(text.toByteArray(Charsets.UTF_8))

    fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it.toInt() and 0xFF) }

    fun unhex(text: String): ByteArray {
        require(text.length % 2 == 0) { "十六进制长度要是偶数" }
        return ByteArray(text.length / 2) { text.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
    }

    /**
     * X-S-Common 里 `x9` 用的 CRC32 变体：标准 CRC32（多项式 0xEDB88320）再异或一次多项式，按有符号 32 位。
     * 网页写作 `(-1 ^ c ^ 0xEDB88320) | 0`，`c` 是还没取反的中间状态，所以等于 `crc32 ^ 0xEDB88320`。
     * 网页对字符串逐个 `charCodeAt`；b1 是纯 ASCII，按 ASCII 字节算。
     */
    fun crc32Variant(text: String): Int {
        val crc = CRC32()
        crc.update(text.toByteArray(Charsets.ISO_8859_1))
        return crc.value.toInt() xor 0xEDB88320.toInt()
    }

    /** xxHash32，seed 默认 0。 */
    fun xxh32(data: ByteArray, seed: Int = 0): Int {
        val p1 = 0x9E3779B1.toInt()
        val p2 = 0x85EBCA77.toInt()
        val p3 = 0xC2B2AE3D.toInt()
        val p4 = 0x27D4EB2F
        val p5 = 0x165667B1
        fun le32(o: Int) = (data[o].toInt() and 0xFF) or ((data[o + 1].toInt() and 0xFF) shl 8) or
            ((data[o + 2].toInt() and 0xFF) shl 16) or ((data[o + 3].toInt() and 0xFF) shl 24)

        val len = data.size
        var p = 0
        var h: Int
        if (len >= 16) {
            var v1 = seed + p1 + p2
            var v2 = seed + p2
            var v3 = seed
            var v4 = seed - p1
            while (p + 16 <= len) {
                v1 = Integer.rotateLeft(v1 + le32(p) * p2, 13) * p1
                v2 = Integer.rotateLeft(v2 + le32(p + 4) * p2, 13) * p1
                v3 = Integer.rotateLeft(v3 + le32(p + 8) * p2, 13) * p1
                v4 = Integer.rotateLeft(v4 + le32(p + 12) * p2, 13) * p1
                p += 16
            }
            h = Integer.rotateLeft(v1, 1) + Integer.rotateLeft(v2, 7) + Integer.rotateLeft(v3, 12) + Integer.rotateLeft(v4, 18)
        } else {
            h = seed + p5
        }
        h += len
        while (p + 4 <= len) {
            h = Integer.rotateLeft(h + le32(p) * p3, 17) * p4
            p += 4
        }
        while (p < len) {
            h = Integer.rotateLeft(h + (data[p].toInt() and 0xFF) * p5, 11) * p1
            p++
        }
        h = h xor (h ushr 15)
        h *= p2
        h = h xor (h ushr 13)
        h *= p3
        h = h xor (h ushr 16)
        return h
    }

    /** MurmurHash3 x86 32 位，输入按 UTF-8。`xy-direction` 用它。 */
    fun murmur3(key: String, seed: Int): Int {
        val data = key.toByteArray(Charsets.UTF_8)
        val c1 = 0xcc9e2d51.toInt()
        val c2 = 0x1b873593
        var h = seed
        val blocks = data.size / 4
        for (i in 0 until blocks) {
            var k = (data[4 * i].toInt() and 0xFF) or ((data[4 * i + 1].toInt() and 0xFF) shl 8) or
                ((data[4 * i + 2].toInt() and 0xFF) shl 16) or ((data[4 * i + 3].toInt() and 0xFF) shl 24)
            k *= c1
            k = Integer.rotateLeft(k, 15)
            k *= c2
            h = h xor k
            h = Integer.rotateLeft(h, 13)
            h = h * 5 + 0xe6546b64.toInt()
        }
        var k = 0
        val tail = blocks * 4
        val rem = data.size and 3
        if (rem >= 3) k = k xor ((data[tail + 2].toInt() and 0xFF) shl 16)
        if (rem >= 2) k = k xor ((data[tail + 1].toInt() and 0xFF) shl 8)
        if (rem >= 1) {
            k = k xor (data[tail].toInt() and 0xFF)
            k *= c1
            k = Integer.rotateLeft(k, 15)
            k *= c2
            h = h xor k
        }
        h = h xor data.size
        h = h xor (h ushr 16)
        h *= 0x85ebca6b.toInt()
        h = h xor (h ushr 13)
        h *= 0xc2b2ae35.toInt()
        h = h xor (h ushr 16)
        return h
    }

    /** RC4。b1 的加密用它（密钥 `xhswebmplfbt`）。 */
    fun rc4(data: ByteArray, key: ByteArray): ByteArray {
        val s = IntArray(256) { it }
        var j = 0
        for (i in 0 until 256) {
            j = (j + s[i] + (key[i % key.size].toInt() and 0xFF)) and 0xFF
            s[i] = s[j].also { s[j] = s[i] }
        }
        val out = ByteArray(data.size)
        var i = 0
        j = 0
        for (n in data.indices) {
            i = (i + 1) and 0xFF
            j = (j + s[i]) and 0xFF
            s[i] = s[j].also { s[j] = s[i] }
            out[n] = (data[n].toInt() xor s[(s[i] + s[j]) and 0xFF]).toByte()
        }
        return out
    }

    /**
     * x3 末尾 16 字节设备签名用的自定义哈希（网页里叫 `_dsf`）：4 路 ARX，每 8 字节一轮，输入长度要是 8 的倍数
     * （这里总是 8 字节时间戳 + 16 字节 MD5 = 24）。输出 16 字节，小端。
     */
    fun dsf(input: ByteArray): ByteArray {
        require(input.size % 8 == 0) { "dsf 输入要是 8 的倍数" }
        val len = input.size
        fun le32(o: Int) = (input[o].toInt() and 0xFF) or ((input[o + 1].toInt() and 0xFF) shl 8) or
            ((input[o + 2].toInt() and 0xFF) shl 16) or ((input[o + 3].toInt() and 0xFF) shl 24)

        var s0 = 0x6D2B79F5 xor len
        var s1 = 0x1B873593 xor (len shl 8)
        var s2 = 0x85EBCA6B.toInt() xor (len shl 16)
        var s3 = 0xC2B2AE35.toInt() xor (len shl 24)
        var o = 0
        while (o + 8 <= len) {
            val v0 = le32(o)
            val v1 = le32(o + 4)
            s0 = Integer.rotateLeft((s0 + v0) xor s2, 7)
            s1 = Integer.rotateLeft((s1 xor v0) + s3, 11)
            s2 = Integer.rotateLeft((s2 + v1) xor s0, 13)
            s3 = Integer.rotateLeft((s3 xor v1) + s1, 17)
            o += 8
        }
        val t0 = s0 xor len
        val t1 = s1 xor t0
        val t2 = s2 + t1
        val t3 = s3 xor t2
        val r0 = Integer.rotateLeft(t0, 9)
        val r1 = Integer.rotateLeft(t1, 13)
        val r2 = Integer.rotateLeft(t2, 17)
        val r3 = Integer.rotateLeft(t3, 19)
        val f0 = r0 + r2
        val f1 = r1 xor r3
        val f2 = r2 + f0
        val f3 = r3 xor f1
        val out = ByteArray(16)
        intArrayOf(f0, f1, f2, f3).forEachIndexed { i, w ->
            for (b in 0 until 4) out[i * 4 + b] = (w ushr (8 * b)).toByte()
        }
        return out
    }
}
