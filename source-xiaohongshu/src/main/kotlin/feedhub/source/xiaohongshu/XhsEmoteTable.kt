package feedhub.source.xiaohongshu

import feedhub.core.arr
import feedhub.core.obj
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject

/**
 * 表情表：文字代码（`[笑哭R]`、`[doge]`）→ 图片地址（`picasso-static.xiaohongshu.com/fe-platform/<hash>.png`，静态资源、无签名）。
 * 来自 `GET /api/im/redmoji/detail`：`data.emoji.tabs[].collection[].emoji[]` = `{image_name, image}`
 * ；`/api/im/redmoji/version` 是版本号（当时是 5），
 * 我们不用它，表缓存在 `ctx.store` 里一周（和小黑盒一样）。
 */
internal class XhsEmoteTable(val map: Map<String, String>, val fetchedAtSec: Long) {
    fun isEmpty() = map.isEmpty()

    fun toStored(): String = json.encodeToString(Stored.serializer(), Stored(VERSION, fetchedAtSec, map))

    @Serializable
    private class Stored(val v: Int, val at: Long, val map: Map<String, String>)

    companion object {
        const val KEY = "xhs.emotes"
        const val TTL_SEC = 7 * 24 * 3600L
        private const val VERSION = 1
        private val json = Json { ignoreUnknownKeys = true }

        val EMPTY = XhsEmoteTable(emptyMap(), 0)

        fun fromStored(text: String): XhsEmoteTable? = runCatching {
            val s = json.decodeFromString(Stored.serializer(), text)
            if (s.v == VERSION) XhsEmoteTable(s.map, s.at) else null
        }.getOrNull()

        /** `redmoji/detail` 的响应。没有 `image` 字段的项跳过。 */
        fun fromDetail(response: JsonObject, nowSec: Long): XhsEmoteTable {
            val out = LinkedHashMap<String, String>()
            val tabs = response.obj("data")?.obj("emoji")?.arr("tabs").orEmpty()
            for (tab in tabs) for (collection in (tab as? JsonObject)?.arr("collection").orEmpty()) {
                for (e in (collection as? JsonObject)?.arr("emoji").orEmpty()) {
                    val o = e as? JsonObject ?: continue
                    val name = o.text("image_name") ?: continue
                    val image = o.text("image") ?: continue
                    out.putIfAbsent(name, image.toHttps())
                }
            }
            return XhsEmoteTable(out, nowSec)
        }
    }
}
