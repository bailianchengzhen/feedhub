package feedhub.source.youtube

import feedhub.core.int
import feedhub.core.long
import feedhub.core.text
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

/**
 * 可空链上的取字段（`a.field("x")?.field("y")?.str("z")`）。接收者是 [JsonElement]`?`，所以
 * `JsonObject` / `JsonArray` 元素（`runs[0]` 之类）都能直接接着链。
 *
 * 为什么不直接用 core 的 `JsonObject.obj(key)` 那一组：它们只接受**非空**接收者，而 InnerTube 的响应
 * 一层层都可能缺；同名做成可空重载又会和 JVM 签名撞（Kotlin 的可空只是元数据）。所以这一组用不同的名字，
 * 只在 `source-youtube` 里用。
 */
internal fun JsonElement?.field(key: String): JsonObject? = (this as? JsonObject)?.get(key) as? JsonObject

internal fun JsonElement?.array(key: String): JsonArray? = (this as? JsonObject)?.get(key) as? JsonArray

internal fun JsonElement?.str(key: String): String? = (this as? JsonObject)?.text(key)

internal fun JsonElement?.intOf(key: String): Int? = (this as? JsonObject)?.int(key)

internal fun JsonElement?.longOf(key: String): Long? = (this as? JsonObject)?.long(key)
