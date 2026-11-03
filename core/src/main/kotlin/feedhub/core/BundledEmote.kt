package feedhub.core

/**
 * 随插件带的表情图的地址（不是网络图片）：`feedhub-emoji:<表情包>/<文件名>`，例如 `feedhub-emoji:coolapk/e012.png`。
 *
 * 有的平台的表情官方没有公开图片地址（酷安），图片随**插件**一起带：放在插件模块的 `src/main/resources/feedhub-emoji/<表情包>/`
 * （[RESOURCE_DIR]，打进插件的 jar），插件把文字代码对到"哪个包的哪个文件"，放进 [Span.Emote.url]；
 * 前端（app）给图片加载器注册一个认这个 scheme 的取图器，按类路径资源读。
 * 插件拆掉了，图跟着没了；app 不知道有哪些表情包。
 */
object BundledEmote {
    const val SCHEME = "feedhub-emoji"

    /** 类路径上的资源目录：图片在 `feedhub-emoji/<表情包>/<文件名>`。 */
    const val RESOURCE_DIR = "feedhub-emoji"

    /** [pack] 和 [file] 都要是简单的 ASCII 名字（字母、数字、`_`、`-`、`.`），[file] 不能是 `..`，免得拼出资源路径时跳出目录。 */
    fun url(pack: String, file: String): String {
        require(NAME.matches(pack) && NAME.matches(file) && file != "..") { "表情包 / 文件名不合法：$pack/$file" }
        return "$SCHEME:$pack/$file"
    }

    /** 反过来拆：不是这个 scheme、或者不是 `<包>/<文件名>` 的形状就返回 null。 */
    fun parse(url: String): Ref? {
        if (!url.startsWith("$SCHEME:")) return null
        val rest = url.removePrefix("$SCHEME:").removePrefix("//")
        val slash = rest.indexOf('/')
        if (slash <= 0) return null
        val pack = rest.substring(0, slash)
        val file = rest.substring(slash + 1)
        if (!NAME.matches(pack) || !NAME.matches(file) || file == "..") return null
        return Ref(pack, file)
    }

    data class Ref(val pack: String, val file: String)

    private val NAME = Regex("[A-Za-z0-9_.-]+")
}
