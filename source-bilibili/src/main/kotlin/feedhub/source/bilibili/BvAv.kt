package feedhub.source.bilibili

/**
 * BV 号 ⇄ AV 号（aid）互转。评论、弹幕接口要的是 aid，而我们的 id 是 BV 号，所以不查接口、直接算。
 *
 * 换算规则是公开协议：
 * - aid → BV：`(2^51 | aid) xor 23442827791579`，按 58 进制写成 9 位（高位在前，字母表固定），前面接 `BV1`，
 *   再交换第 3 / 9 位和第 4 / 7 位（下标从整个 12 位字符串算起）；
 * - BV → aid：倒过来做，最后用 `2^51 - 1` 去掉最高位再 xor 回去。
 *
 * 期望值见 `BvAvTest`：用的是详情接口里真实返回的 bvid / aid 对。aid 到 2^51 之后这套规则就不够用了，B站公告里说到那时再换。
 */
internal object BvAv {
    private const val XOR = 23442827791579L
    private const val TOP_BIT = 1L shl 51
    private const val LOW_BITS = TOP_BIT - 1
    private const val DIGITS = "FcwAPNKTMug3GV5Lj7EJnHpWsx4tb8haYeviqBz6rkCy12mUSDQX9RdoZf"
    private const val PREFIX = "BV1"
    private const val LENGTH = 12

    /** 看起来像 BV 号：`BV1` + 9 个 base58 字符（只检查格式，不保证存在）。 */
    fun isBvid(s: String): Boolean =
        s.length == LENGTH && s.startsWith(PREFIX) && (PREFIX.length until LENGTH).all { DIGITS.indexOf(s[it]) >= 0 }

    fun toBvid(aid: Long): String? {
        if (aid !in 1 until TOP_BIT) return null
        val out = CharArray(LENGTH)
        for (i in PREFIX.indices) out[i] = PREFIX[i]
        var n = (TOP_BIT or aid) xor XOR
        for (pos in LENGTH - 1 downTo PREFIX.length) {
            out[pos] = DIGITS[(n % DIGITS.length).toInt()]
            n /= DIGITS.length
        }
        shuffle(out)
        return String(out)
    }

    /** 不是合法 BV 号返回 null。 */
    fun toAid(bvid: String): Long? {
        if (!isBvid(bvid)) return null
        val chars = bvid.toCharArray()
        shuffle(chars)
        var n = 0L
        for (pos in PREFIX.length until LENGTH) {
            n = n * DIGITS.length + DIGITS.indexOf(chars[pos])
        }
        return (n and LOW_BITS) xor XOR
    }

    /** 交换第 3 / 9 位和第 4 / 7 位；做两次等于没做，所以两个方向共用。 */
    private fun shuffle(a: CharArray) {
        a[3] = a[9].also { a[9] = a[3] }
        a[4] = a[7].also { a[7] = a[4] }
    }
}
