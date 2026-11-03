package feedhub.core

import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import java.util.concurrent.ConcurrentHashMap

/**
 * 平台标识：就是一个字符串 id（`bili`、`yt`……），按 id 比较。用在 cli 参数、存储键和 [FeedItem.key] 里，**定了就别改**。
 *
 * core 不知道有哪些平台。插件用 [define] 定义自己的标识（同时登记显示名）；别的地方只按 id 引用
 * （`Platform("bili")`），不依赖那个插件的代码——插件拆掉了，引用它的数据照样读得出来，只是没有这个平台可用。
 *
 * **持久化兼容**（见 [PlatformSerializer]）：读的时候 id 和老版本写下的 enum 名（`"BILIBILI"`、`"YOUTUBE"`……，
 * 见 [LEGACY_NAMES]）都认；写的时候**老的 11 个平台照旧写 enum 名**、新平台写 id——这样新版本写下的存档 / 收藏，
 * 回退到旧版本也照样读得回来（旧版本只认 enum 名）。
 */
@JvmInline
@Serializable(with = PlatformSerializer::class)
value class Platform(val key: String) {
    /** 显示名。插件 [define] 时登记；没登记过（插件没装）就用 id 本身。 */
    val label: String get() = labels[key] ?: key

    override fun toString(): String = key

    companion object {
        private val labels = ConcurrentHashMap<String, String>()

        /** 合法的 id：小写字母开头，小写字母 / 数字 / `-` / `_`，最长 32。 */
        val ID: Regex = Regex("[a-z][a-z0-9_-]{0,31}")

        /**
         * 插件定义自己的平台标识，同时登记显示名（[label]）。每个插件一次，放在插件模块的顶层常量里。
         * 同一个 id 登记两个不同的名字说明两个插件撞了 id，直接报错。
         */
        fun define(key: String, label: String): Platform {
            require(ID.matches(key)) { "平台 id 不合法：$key（小写字母开头，只能有小写字母、数字、-、_，最长 32）" }
            require(label.isNotBlank()) { "平台 $key 的显示名是空的" }
            val old = labels.putIfAbsent(key, label)
            require(old == null || old == label) { "平台 id $key 已经登记成「$old」，又要登记成「$label」：两个插件撞了 id？" }
            return Platform(key)
        }

        /**
         * 老版本（`Platform` 还是 enum 时）存下来的 enum 名 → id。**只增不改**：这是历史数据的读法，
         * 新平台用不到它（它们从一开始就按 id 存）。
         */
        val LEGACY_NAMES: Map<String, String> = mapOf(
            "BILIBILI" to "bili",
            "ZHIHU" to "zhihu",
            "TIEBA" to "tieba",
            "HEYBOX" to "heybox",
            "COOLAPK" to "coolapk",
            "XHS" to "xhs",
            "DOUYIN" to "douyin",
            "X" to "x",
            "YOUTUBE" to "yt",
            "REDDIT" to "reddit",
            "TIKTOK" to "tiktok",
        )

        /** 插件化之前就有的 11 个平台（老存档里"见过"的平台，见 `PlatformCatalog.freshSince`）。 */
        val LEGACY: Set<Platform> get() = LEGACY_NAMES.values.mapTo(LinkedHashSet()) { Platform(it) }

        private val LEGACY_BY_KEY: Map<String, String> = LEGACY_NAMES.entries.associate { (name, key) -> key to name }

        /** 存储里读出来的一个平台：老的 enum 名换成 id，别的原样当 id（不认识的 id 也照样读出来，由上层决定怎么处理）。 */
        fun fromStored(text: String): Platform = Platform(LEGACY_NAMES[text] ?: text)

        /** 写进存储的写法：老的 11 个平台写 enum 名（旧版本读得回），新平台写 id。 */
        fun toStored(platform: Platform): String = LEGACY_BY_KEY[platform.key] ?: platform.key
    }
}

/**
 * [Platform] 的序列化（也能当 Map 的键）：读 id 或老的 enum 名；写 [Platform.toStored]（老平台写 enum 名、新平台写 id）。
 *
 * 为什么老平台不改写成 id：`state.json` 的设置、资料库 `item_json` 里的条目都是这个格式，改成 id 之后旧版本
 * （回退安装、还没升级的另一台机器）一读就是"不认识的枚举值"，整份设置 / 整条收藏读不出来。写 enum 名两头都认。
 * SQLite 的 `platform` 列一直存的是 id（`platform.key`），不受影响。
 */
object PlatformSerializer : KSerializer<Platform> {
    override val descriptor: SerialDescriptor = PrimitiveSerialDescriptor("feedhub.core.Platform", PrimitiveKind.STRING)
    override fun serialize(encoder: Encoder, value: Platform) = encoder.encodeString(Platform.toStored(value))
    override fun deserialize(decoder: Decoder): Platform = Platform.fromStored(decoder.decodeString())
}
