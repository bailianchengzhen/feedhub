package feedhub.app.image

/** 会动的图片格式。 */
internal enum class AnimatedFormat { GIF, WEBP }

/**
 * 按**文件内容**（开头几十个字节）判断是不是动图，不看 URL 后缀、也不全信 Content-Type：
 * 平台的图片地址经常没有后缀，或者后缀和实际格式不符。
 *
 * - GIF：`GIF87a` / `GIF89a` 开头。GIF 是不是真的有多帧要解码时才知道（单帧 GIF 当静态图处理）。
 * - 动态 WebP：`RIFF` + 长度 + `WEBP`，接着是扩展头 `VP8X`，它的标志字节里"动画"位（0x02）打开；或者紧跟着就是 `ANIM` 块。
 *   静态 WebP（`VP8 ` / `VP8L`，或者 `VP8X` 但没有动画位）不算。
 */
internal object AnimatedSniffer {
    /** 判断最多要看的字节数。 */
    const val HEADER_BYTES = 64

    fun sniff(header: ByteArray, length: Int = header.size): AnimatedFormat? {
        val n = minOf(length, header.size)
        if (n >= 6 && header.startsWith("GIF87a", n) || n >= 6 && header.startsWith("GIF89a", n)) return AnimatedFormat.GIF
        if (n >= 16 && header.startsWith("RIFF", n) && header.matches(8, "WEBP")) {
            // 第一个块：fourcc 在 12，长度在 16，VP8X 的标志字节在 20
            if (header.matches(12, "VP8X") && n >= 21 && (header[20].toInt() and 0x02) != 0) return AnimatedFormat.WEBP
            // 有的编码器不写 VP8X 标志位但有 ANIM 块（VP8X 头 18 字节：块头 8 + 内容 10，ANIM 块在 30）
            if (n >= 34 && header.matches(30, "ANIM")) return AnimatedFormat.WEBP
        }
        return null
    }

    private fun ByteArray.startsWith(s: String, n: Int): Boolean = s.length <= n && matches(0, s)

    private fun ByteArray.matches(offset: Int, s: String): Boolean {
        if (offset + s.length > size) return false
        for (i in s.indices) if (this[offset + i] != s[i].code.toByte()) return false
        return true
    }
}
