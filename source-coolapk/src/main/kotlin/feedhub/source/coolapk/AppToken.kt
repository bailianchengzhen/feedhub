package feedhub.source.coolapk

import org.mindrot.jbcrypt.BCrypt
import java.security.MessageDigest
import java.util.Base64

/**
 * 酷安 App 接口的 `X-App-Token`。纯算法，不联网。
 *
 * **v2**（信息流 / 详情 / 关注流这些老接口用，
 * 时间戳用整数秒。
 *
 * 步骤：
 * 1. `token = "token://com.coolapk.market/dcf01e569c1e3db93a3d0fcf191a622c?" + md5(秒) + "$" + md5(设备码) + "&com.coolapk.market"`
 * 2. bcrypt 的密码 = `md5(base64(token))`，salt = `("$2a$10$" + base64(秒) + "/" + md5(token))` 截 31 位再加 "u"
 *    （bcrypt 只用前 22 位 salt，后面的字符被忽略）
 * 3. 结果的 `$2a` 换成 `$2y`，整体 base64（去掉 "="），前面加 "v2"
 *
 * **v3**：算法和
 * coolapk-desktop `src-tauri/src/coolapk/auth.rs`（MIT）、YuleBest/coolapk-cli `internal/auth`（MIT）一致
 * （早先也对照过 ZaiLinn/c001apk 的 `getTokenV3`）；`auth_blob.b64` 原样取自 coolapk-cli（MIT）的
 * `internal/auth/auth_blob.b64`。固定时间向量在 [AppTokenTest] 里。
 *
 * 步骤（时间戳 `ts`、App 版本号 `code`）：
 * 1. `blob` 解码后取 `[((ts + code) % 100) * 4 + 0x80, +0x80)` 这一段，再按 base64 解码成 `segment`
 * 2. `plain = "com.coolapk.market&" + segment + "&" + md5(设备码) + "&" + ts + "&" + code`
 * 3. 密码 = `md5(base64(plain))`；salt 前 22 位来自 `base64(hex(ts) + "/" + md5(plain))`，末位按标准字母表回退 5 位
 * 4. bcrypt cost 4（`$2a$04$`），算完换 `$2y`，整体 base64（去掉 "="），前面加 "v3"
 *
 * base64 都是标准字母表、去掉 "="。
 * 16.2.0 / API 16 的 v3 在 api 的 `hotReplyList` 上 200（评论 `replyList` 另有验证码门，见 `CoolapkSource`）。
 */
internal object AppToken {
    private const val APP_LABEL = "token://com.coolapk.market/dcf01e569c1e3db93a3d0fcf191a622c"
    private const val APP_ID = "com.coolapk.market"

    /** 标准 base64 字母表（v3 的 salt 末位按这张表回退 5 位）。 */
    private const val STD_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

    /** bcrypt 自己的字母表；salt 里出现别的字符就让 v3 换下一秒（和参考实现一致）。 */
    private val BCRYPT_SALT = Regex("^[./A-Za-z0-9]{22}$")

    /** 酷安 v3 的密码表：1240 字符 base64，解码后 930 字节。取自 YuleBest/coolapk-cli（MIT）。 */
    private val v3Blob: ByteArray by lazy {
        val text = AppToken::class.java.getResourceAsStream("auth_blob.b64")
            ?.use { it.readBytes().toString(Charsets.US_ASCII) }
            ?: throw IllegalStateException("缺少酷安 v3 token 的 auth_blob.b64 资源")
        Base64.getDecoder().decode(text.trim())
    }

    fun v2(deviceCode: String, timestampSec: Long): String {
        val timeStamp = timestampSec.toString()
        val md5TimeStamp = md5(timeStamp)
        val md5DeviceCode = md5(deviceCode)
        val token = "$APP_LABEL?$md5TimeStamp\$$md5DeviceCode&$APP_ID"
        val md5Base64Token = md5(base64(token))
        // 文档里的 salt 前缀是 `$2y$10$`；jBCrypt 只认 `$2a$`，所以用 `$2a$` 算、结果再换回 `$2y`（两者算法相同）
        val bcryptSalt = "\$2a\$10\$${base64(timeStamp)}/${md5(token)}".substring(0, 31) + "u"
        val bcryptResult = "\$2y" + BCrypt.hashpw(md5Base64Token, bcryptSalt).substring(3)
        return "v2" + base64(bcryptResult)
    }

    /**
     * v3 token。`appCode` 必须和请求头里的 `X-App-Code` 一致（[CoolapkSource] 的评论身份用 2604201 = 16.2.0）。
     *
     * 个别时间点推出来的 22 位 salt 含 bcrypt 字母表之外的字符，参考实现都是**往后试最多 15 秒**；
     * 这里照做，所以同一个 `timestampSec` 的结果是确定的。
     */
    fun v3(deviceCode: String, timestampSec: Long, appCode: Long): String {
        val blob = v3Blob
        var last: IllegalArgumentException? = null
        for (offset in 0..15) {
            val t = timestampSec + offset
            try {
                val index = (((t + appCode) % 100) * 4 + 0x80).toInt()
                require(index + 0x80 <= blob.size) { "auth_blob 越界（index=$index，len=${blob.size}）" }
                val window = String(blob, index, 0x80, Charsets.US_ASCII)
                val segment = String(Base64.getDecoder().decode(window), Charsets.UTF_8)
                val plain = "$APP_ID&$segment&${md5(deviceCode)}&$t&$appCode"
                val password = md5(Base64.getEncoder().encodeToString(plain.toByteArray()))
                val source = base64("${t.toString(16)}/${md5(plain)}").substring(0, 22)
                val shifted = STD_ALPHABET[(STD_ALPHABET.indexOf(source[21]) - 5).mod(64)]
                val salt = source.substring(0, 21) + shifted
                // jBCrypt 对 salt 字符集是硬校验；不合格就换下一秒，和参考实现一样
                if (!BCRYPT_SALT.matches(salt)) continue
                val hash = "\$2y" + BCrypt.hashpw(password, "\$2a\$04\$$salt").substring(3)
                return "v3" + base64(hash)
            } catch (e: IllegalArgumentException) {
                last = e
            }
        }
        throw IllegalStateException("无法生成酷安 v3 token（16 秒内没有合法的 bcrypt salt）", last)
    }

    /**
     * v3 的密码表（解码后的 930 字节）给 Rust 版当参数用：表的所有权在插件（资源文件在 `src/main/resources`），
     * Rust 侧不复制这张表，所以每算一次 v3 token 都要过一遍 FFI（930 字节，可以忽略）。
     */
    internal fun v3BlobBytes(): ByteArray = v3Blob

    fun md5(s: String): String =
        MessageDigest.getInstance("MD5").digest(s.toByteArray()).joinToString("") { "%02x".format(it) }

    /** 标准 base64，去掉 "="。 */
    fun base64(s: String): String = Base64.getEncoder().withoutPadding().encodeToString(s.toByteArray())
}
