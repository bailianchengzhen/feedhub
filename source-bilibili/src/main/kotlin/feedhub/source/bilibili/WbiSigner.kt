package feedhub.source.bilibili

import java.security.MessageDigest

/**
 * B站网页接口的 WBI 签名（补上 `wts`、`w_rid` 两个参数）。
 *
 * 协议是公开的：
 * 1. `/x/web-interface/nav` 的 `wbi_img` 给两张图的地址，文件名（去扩展名）就是 img_key、sub_key，每天换一次；
 * 2. 两者首尾相接成 64 个字符，按固定的 32 项下标表挑字符，得到 mixin key；
 * 3. 参数补上 `wts`（秒），按键名排序；值里删掉 `!'()*`，键和值都按 `encodeURIComponent` 编码，拼成 `k=v&…`；
 * 4. `w_rid` = 小写十六进制的 md5(拼好的 query + mixin key)。
 *
 * 签错时服务端只回 -403，本地看不出来，所以靠单测里的定值兜底。
 */
object WbiSigner {
    /** mixin key 的下标表：第 n 个字符取 `img_key + sub_key` 的第 `PICK[n]` 位。 */
    private val PICK = intArrayOf(
        46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35,
        27, 43, 5, 49, 33, 9, 42, 19, 29, 28, 14, 39, 12, 38, 41, 13,
    )

    private const val HEX_UPPER = "0123456789ABCDEF"

    fun mixinKey(imgKey: String, subKey: String): String {
        val joined = imgKey + subKey
        val out = CharArray(PICK.size) { n -> joined[PICK[n]] }
        return String(out)
    }

    /**
     * 返回补上 `wts`、`w_rid` 的参数表：原有的键保持原位置，新键接在后面（已有同名键时只换值）。
     * 纯函数，key 和 `wts` 由调用方给，便于定值回归。
     */
    fun sign(params: Map<String, String>, mixinKey: String, wts: Long): Map<String, String> {
        val out = LinkedHashMap(params)
        out["wts"] = wts.toString()
        val query = StringBuilder()
        for (key in out.keys.sorted()) {
            if (query.isNotEmpty()) query.append('&')
            appendComponent(query, key)
            query.append('=')
            appendComponent(query, stripReserved(out.getValue(key)))
        }
        query.append(mixinKey)
        out["w_rid"] = md5Hex(query.toString())
        return out
    }

    /** `encodeURIComponent` 不转义 `!'()*`，网页 JS 签名前直接把它们从值里删掉。 */
    private fun stripReserved(value: String): String =
        value.filter { it != '!' && it != '\'' && it != '(' && it != ')' && it != '*' }

    /**
     * 按 UTF-8 逐字节编码：RFC 3986 的 unreserved（字母、数字、`-_.~`）原样，其余写成 `%XX`（大写）。
     * 去掉那五个字符后就和 JS `encodeURIComponent` 一致。不能用 URLEncoder：它把空格编成 `+`。
     */
    private fun appendComponent(sb: StringBuilder, s: String) {
        for (byte in s.encodeToByteArray()) {
            val b = byte.toInt() and 0xFF
            val c = b.toChar()
            val unreserved = c in 'a'..'z' || c in 'A'..'Z' || c in '0'..'9' || c == '-' || c == '_' || c == '.' || c == '~'
            if (unreserved) {
                sb.append(c)
            } else {
                sb.append('%').append(HEX_UPPER[b ushr 4]).append(HEX_UPPER[b and 0xF])
            }
        }
    }

    private fun md5Hex(s: String): String {
        val digest = MessageDigest.getInstance("MD5").digest(s.encodeToByteArray())
        val sb = StringBuilder(digest.size * 2)
        for (byte in digest) {
            val b = byte.toInt() and 0xFF
            sb.append(Character.forDigit(b ushr 4, 16)).append(Character.forDigit(b and 0xF, 16))
        }
        return sb.toString()
    }

    /** `https://i0.hdslb.com/bfs/wbi/7cd0...077c.png` → `7cd0...077c` */
    fun keyFromUrl(url: String): String {
        val name = url.substring(url.lastIndexOf('/') + 1)
        val dot = name.indexOf('.')
        return if (dot < 0) name else name.substring(0, dot)
    }
}
