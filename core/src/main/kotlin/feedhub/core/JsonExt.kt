package feedhub.core

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.longOrNull

/** 解析平台响应用的 Json：容忍多余字段、宽松格式。 */
val FeedJson: Json = Json {
    ignoreUnknownKeys = true
    isLenient = true
    explicitNulls = false
    coerceInputValues = true
}

/** cli 输出用的 Json。 */
val PrettyJson: Json = Json {
    prettyPrint = true
    explicitNulls = false
    encodeDefaults = false
}

// 下面这组小函数用来在不写 DTO 的情况下安全地取字段：类型不对或不存在都返回 null。
// 平台响应里数字经常是字符串（反过来也有），所以 long() / string() 两种都兼容。

fun JsonElement?.obj(): JsonObject? = this as? JsonObject
fun JsonElement?.arr(): JsonArray? = this as? JsonArray

fun JsonObject.obj(key: String): JsonObject? = this[key] as? JsonObject
fun JsonObject.arr(key: String): JsonArray? = this[key] as? JsonArray

fun JsonObject.string(key: String): String? {
    val p = this[key] as? JsonPrimitive ?: return null
    if (p is JsonNull) return null
    return p.contentOrNull
}

/** 非空白字符串，否则 null。 */
fun JsonObject.text(key: String): String? = string(key)?.takeIf { it.isNotBlank() }

fun JsonObject.long(key: String): Long? {
    val p = this[key] as? JsonPrimitive ?: return null
    if (p is JsonNull) return null
    return p.longOrNull ?: p.doubleOrNull?.toLong() ?: p.contentOrNull?.trim()?.toLongOrNull()
}

fun JsonObject.int(key: String): Int? = long(key)?.toInt()

fun JsonObject.double(key: String): Double? {
    val p = this[key] as? JsonPrimitive ?: return null
    if (p is JsonNull) return null
    return p.doubleOrNull ?: p.contentOrNull?.trim()?.toDoubleOrNull()
}

fun JsonObject.bool(key: String): Boolean? {
    val p = this[key] as? JsonPrimitive ?: return null
    if (p is JsonNull) return null
    return p.booleanOrNull ?: p.longOrNull?.let { it != 0L }
}

/**
 * 递归问"这棵 JSON 里有没有哪个字符串满足谓词"。用来替代 `json.toString()` 后再扫一遍：
 * 后者会把整份响应重新序列化（评论页可能几十到几百 KB），这里只走已经解析好的树，不产生大字符串。
 * 只看字符串值，不看对象键（表情代码 / 文本都出现在值里）。
 */
fun JsonElement?.anyString(predicate: (String) -> Boolean): Boolean = when (this) {
    null -> false
    is JsonObject -> values.any { it.anyString(predicate) }
    is JsonArray -> any { it.anyString(predicate) }
    is JsonPrimitive -> !(this is JsonNull) && isString && predicate(content)
}

/** 按路径取对象，例如 `json.path("data", "wbi_img")`。 */
fun JsonObject.path(vararg keys: String): JsonObject? {
    var cur: JsonObject? = this
    for (k in keys) cur = cur?.obj(k) ?: return null
    return cur
}

fun parseJsonObject(platform: Platform, body: String): JsonObject = try {
    FeedJson.parseToJsonElement(body) as? JsonObject
        ?: throw ParseException(platform, "响应不是 JSON 对象：${body.take(200)}")
} catch (e: SourceException) {
    throw e
} catch (e: Exception) {
    throw ParseException(platform, "响应不是合法 JSON：${body.take(200)}", e)
}
