package feedhub.source.tieba

import java.security.MessageDigest

/**
 * 贴吧 App 签名：参数按 key 排序（同 key 再按 value），拼成无分隔的 `k=v` 串，末尾加盐 `tiebaclient!!!`，
 * 取 md5（小写十六进制），作为 `sign` 参数。值用原文（不做 URL 编码）。
 *
 * 用在两种地方：
 * - 表单接口（`application/x-www-form-urlencoded`，`c.tieba.baidu.com` / `tiebac.baidu.com` 的 `/c/...`）；
 * - protobuf 接口的 multipart 里那些普通表单字段（不含 `data` 文件段），见 [TiebaSource] 的关注流。
 *
 * 移植自 aiotieba `src/aiotieba/helper/crypto/sign.py` 的 `compute_sign`（Unlicense）；
 * TiebaLite `api/retrofit/interceptors/SortAndSignInterceptor.kt`（GPL-3.0）是同一个算法。
 * 纯函数，无网络。
 */
object TiebaSign {
    const val APP_SALT = "tiebaclient!!!"

    /**
     * 网页接口（全吧搜索 `/mo/q/search/thread`）用的 PC salt，aiotieba `helper/crypto/const.py` 的 `PC_SALT`。
     * 算法和 App 侧一样，只是盐不同（[compute] 的 `salt` 参数）。
     */
    const val PC_SALT = "36770b1f34c9bbf2e7d1a99d2b82fa9e"

    fun compute(params: List<Pair<String, String>>, salt: String = APP_SALT): String {
        val md5 = MessageDigest.getInstance("MD5")
        params.sortedWith(compareBy<Pair<String, String>> { it.first }.thenBy { it.second })
            .forEach { (k, v) -> md5.update("$k=$v".toByteArray(Charsets.UTF_8)) }
        md5.update(salt.toByteArray(Charsets.UTF_8))
        return md5.digest().joinToString("") { "%02x".format(it.toInt() and 0xff) }
    }

    /** 去掉已有的 `sign`，按 key 排好序，末尾追加 `sign`。 */
    fun signed(params: List<Pair<String, String>>, salt: String = APP_SALT): List<Pair<String, String>> {
        val rest = params.filter { it.first != "sign" }.sortedWith(compareBy({ it.first }, { it.second }))
        return rest + ("sign" to compute(rest, salt))
    }
}
