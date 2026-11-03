package feedhub.source.douyin

/**
 * SM3 杂凑算法（GB/T 32905-2016）的纯 Kotlin 实现，字节数组进、32 字节摘要出。
 *
 * 为什么自己写：a_bogus 里 SM3 用了三次（query+salt 两次、body+salt 两次、UA 的 Base64 结果一次），
 * 而 Android 上系统不对外暴露 SM3，引入 BouncyCastle 又要多打一份包，所以直接实现（国标算法很短）。
 *
 * 正确性有独立来源：`Sm3Test` 用的是 GB/T 32905-2016 附录 A 的两条标准测试向量。
 *
 * 移植来源：`ylcangel__douyin_sign/a_bogus/sm3.js`（Apache-2.0，Copyright AngelToms）里的
 * `_SM3` 是同一算法的 JS 实现，本文件是它的 Kotlin 重写；行为以国标为准（两者一致）。
 */
internal object Sm3 {
    private const val BLOCK_SIZE = 64

    private val IV = intArrayOf(
        0x7380166f, 0x4914b2b9, 0x172442d7, 0xda8a0600.toInt(),
        0xa96f30bc.toInt(), 0x163138aa, 0xe38dee4d.toInt(), 0xb0fb0e4e.toInt(),
    )

    /** 摘要（32 字节）。 */
    fun digest(message: ByteArray): ByteArray {
        val padded = pad(message)
        val v = IV.copyOf()
        val w = IntArray(68)
        val w1 = IntArray(64)
        var offset = 0
        while (offset < padded.size) {
            compress(v, padded, offset, w, w1)
            offset += BLOCK_SIZE
        }
        val out = ByteArray(32)
        for (i in 0 until 8) {
            out[i * 4] = (v[i] ushr 24).toByte()
            out[i * 4 + 1] = (v[i] ushr 16).toByte()
            out[i * 4 + 2] = (v[i] ushr 8).toByte()
            out[i * 4 + 3] = v[i].toByte()
        }
        return out
    }

    /** 两次 SM3（a_bogus 对 query / body 用的就是两次）。 */
    fun digestTwice(message: ByteArray): ByteArray = digest(digest(message))

    /** 十六进制小写，测试和调试用。 */
    fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

    private fun pad(message: ByteArray): ByteArray {
        val bitLen = message.size.toLong() * 8
        var padLen = BLOCK_SIZE - (message.size % BLOCK_SIZE)
        if (padLen < 9) padLen += BLOCK_SIZE
        val out = ByteArray(message.size + padLen)
        message.copyInto(out)
        out[message.size] = 0x80.toByte()
        for (i in 0 until 8) {
            out[out.size - 8 + i] = (bitLen ushr (56 - 8 * i)).toByte()
        }
        return out
    }

    private fun compress(v: IntArray, block: ByteArray, offset: Int, w: IntArray, w1: IntArray) {
        for (i in 0 until 16) {
            val p = offset + i * 4
            w[i] = ((block[p].toInt() and 0xff) shl 24) or
                ((block[p + 1].toInt() and 0xff) shl 16) or
                ((block[p + 2].toInt() and 0xff) shl 8) or
                (block[p + 3].toInt() and 0xff)
        }
        for (j in 16 until 68) {
            var x = w[j - 16] xor w[j - 9] xor rotl(w[j - 3], 15)
            x = x xor rotl(x, 15) xor rotl(x, 23)
            w[j] = x xor rotl(w[j - 13], 7) xor w[j - 6]
        }
        for (j in 0 until 64) w1[j] = w[j] xor w[j + 4]

        var a = v[0]
        var b = v[1]
        var c = v[2]
        var d = v[3]
        var e = v[4]
        var f = v[5]
        var g = v[6]
        var h = v[7]

        for (j in 0 until 64) {
            val tj = if (j < 16) 0x79cc4519 else 0x7a879d8a
            val ss1 = rotl(rotl(a, 12) + e + rotl(tj, j), 7)
            val ss2 = ss1 xor rotl(a, 12)
            val ff = if (j < 16) a xor b xor c else (a and b) or (a and c) or (b and c)
            val gg = if (j < 16) e xor f xor g else (e and f) or (e.inv() and g)
            val tt1 = ff + d + ss2 + w1[j]
            val tt2 = gg + h + ss1 + w[j]
            d = c
            c = rotl(b, 9)
            b = a
            a = tt1
            h = g
            g = rotl(f, 19)
            f = e
            e = tt2 xor rotl(tt2, 9) xor rotl(tt2, 17)
        }

        v[0] = v[0] xor a
        v[1] = v[1] xor b
        v[2] = v[2] xor c
        v[3] = v[3] xor d
        v[4] = v[4] xor e
        v[5] = v[5] xor f
        v[6] = v[6] xor g
        v[7] = v[7] xor h
    }

    private fun rotl(x: Int, n: Int): Int {
        val k = n % 32
        return if (k == 0) x else (x shl k) or (x ushr (32 - k))
    }
}
