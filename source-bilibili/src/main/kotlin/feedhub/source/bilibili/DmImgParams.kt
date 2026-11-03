package feedhub.source.bilibili

import java.util.Base64
import kotlin.random.Random

/**
 * 网页端空间类接口要带的 `dm_img_*` 四个参数（不带会回 -352）。
 *
 * 网页上 `dm_img_str` / `dm_cover_img_str` 是浏览器 WebGL 信息的 Base64（去掉末尾两个字符），
 * `dm_img_list`、`dm_img_inter` 是鼠标轨迹和窗口信息。这里不冒充某个具体显卡：
 * 两个 `str` 用随机长度的可打印 ASCII（`&` 到 `~`）按同样的格式编码，每次请求重新生成——固定值本身就是可匹配的特征；
 * 另外两个给"没有交互"的空值。
 */
internal object DmImgParams {
    private const val FIRST_PRINTABLE = 0x26
    private const val PRINTABLE_COUNT = 0x7E - FIRST_PRINTABLE + 1

    fun next(): Map<String, String> = mapOf(
        "dm_img_list" to "[]",
        "dm_img_str" to webglLike(minLen = 16, maxLen = 64),
        "dm_cover_img_str" to webglLike(minLen = 32, maxLen = 128),
        "dm_img_inter" to """{"ds":[],"wh":[0,0,0],"of":[0,0,0]}""",
    )

    /** 长度在 [minLen, maxLen] 里随机的一串可打印字符，Base64 后去掉末尾两个字符（网页 JS 也是这么截的）。 */
    private fun webglLike(minLen: Int, maxLen: Int): String {
        val len = Random.nextInt(minLen, maxLen + 1)
        val raw = ByteArray(len)
        for (i in 0 until len) raw[i] = (FIRST_PRINTABLE + Random.nextInt(PRINTABLE_COUNT)).toByte()
        val encoded = Base64.getEncoder().encodeToString(raw)
        return encoded.substring(0, encoded.length - 2)
    }
}
