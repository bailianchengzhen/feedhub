package feedhub.source.douyin

import feedhub.core.arr
import feedhub.core.obj
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * 抖音的表情表：文字代码（`[赞]`）→ 图片地址。做法照小红书 / 小黑盒（`XhsSource.emotesFor`）：
 * 正文 / 评论里出现 `[xxx]` 记号时去取一次，缓存在 `ctx.store` 一周，取不到就降级成文字。
 *
 * 接口是 `GET /aweme/v1/web/emoji/list`（来源：离线资料里 `mafqla/douyin-api` 的 `api/common_api.py`
 * 有这一条路由；**响应结构离线资料里没有**，所以 [fromList] 用"按常见键名找代码、递归找图片"的宽松取法，
 * 结构不对就返回空表 —— 不编）。
 */
internal class DouyinEmoteTable(val map: Map<String, String>, val fetchedAtSec: Long) {
    fun isEmpty() = map.isEmpty()

    fun toStored(): String = json.encodeToString(Stored.serializer(), Stored(VERSION, fetchedAtSec, map))

    @Serializable
    private class Stored(val v: Int, val at: Long, val map: Map<String, String>)

    companion object {
        const val KEY = "douyin.emotes"
        const val TTL_SEC = 7 * 24 * 3600L
        private const val VERSION = 1
        private val json = Json { ignoreUnknownKeys = true }

        val EMPTY = DouyinEmoteTable(emptyMap(), 0)

        /** 评论 / 正文里像表情代码的记号；`[xxx]` 里没有括号和引号，长度 1~16。 */
        val EMOTE_TOKEN = Regex("""\[[^\[\]"'<>]{1,16}]""")

        fun fromStored(text: String): DouyinEmoteTable? = runCatching {
            val s = json.decodeFromString(Stored.serializer(), text)
            if (s.v == VERSION) DouyinEmoteTable(s.map, s.at) else null
        }.getOrNull()

        /** `emoji/list` 的响应（或任何一层嵌套）。认不出就返回空表。 */
        fun fromList(response: JsonObject, nowSec: Long): DouyinEmoteTable {
            val out = LinkedHashMap<String, String>()
            for (list in candidateLists(response)) {
                for (e in list) {
                    val o = e as? JsonObject ?: continue
                    val code = codeOf(o) ?: continue
                    val url = imageOf(o) ?: continue
                    out.putIfAbsent(code, url.toHttps())
                }
                if (out.isNotEmpty()) break
            }
            return DouyinEmoteTable(out, nowSec)
        }

        /** 常见的位置：顶层 `emoji_list` / `data.emoji_list` / `data.list` / `stickers`；都不行就递归找第一个对象数组。 */
        private fun candidateLists(json: JsonObject): List<JsonArray> {
            val named = listOf(
                json.arr("emoji_list"),
                json.obj("data")?.arr("emoji_list"),
                json.obj("data")?.arr("list"),
                json.arr("stickers"),
                json.obj("data")?.arr("stickers"),
                json.arr("list"),
            )
            val found = named.filterNotNull()
            if (found.isNotEmpty()) return found
            return deepArrays(json).take(3)
        }

        private fun deepArrays(o: JsonObject): List<JsonArray> {
            val out = mutableListOf<JsonArray>()
            for (v in o.values) when (v) {
                is JsonArray -> if (v.any { it is JsonObject }) out += v
                is JsonObject -> out += deepArrays(v)
                else -> {}
            }
            return out
        }

        private val CODE_KEYS = listOf(
            "display_name", "emoji_name", "name", "text", "code", "display_name_zh", "desc", "label",
        )

        private fun codeOf(o: JsonObject): String? {
            for (k in CODE_KEYS) {
                val v = o.text(k) ?: continue
                if (v.length >= 3 && v.startsWith("[") && v.endsWith("]")) return v
            }
            return null
        }

        private val URL_KEYS = listOf("emoji_url", "url_list", "icon", "image", "static_url", "url", "src", "uri")

        /** 先按常见键名找（`url_list` 取第一项），都找不到就递归找第一个 http 开头的字符串。 */
        private fun imageOf(o: JsonObject): String? {
            for (k in URL_KEYS) {
                val v = o[k] ?: continue
                when (v) {
                    is JsonPrimitive -> v.content.takeIf { it.startsWith("http") }?.let { return it }
                    is JsonArray -> v.firstNotNullOfOrNull { (it as? JsonPrimitive)?.content?.takeIf { u -> u.startsWith("http") } }
                        ?.let { return it }
                    is JsonObject -> stringIn(v)?.let { return it }
                    else -> {}
                }
            }
            return stringIn(o)
        }

        private fun stringIn(o: JsonObject): String? {
            for (v in o.values) when (v) {
                is JsonPrimitive -> v.content.takeIf { it.startsWith("http") }?.let { return it }
                is JsonArray -> v.firstNotNullOfOrNull { (it as? JsonPrimitive)?.content?.takeIf { u -> u.startsWith("http") } }
                    ?.let { return it }
                is JsonObject -> stringIn(v)?.let { return it }
                else -> {}
            }
            return null
        }
    }
}
