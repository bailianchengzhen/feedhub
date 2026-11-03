package feedhub.source.xiaohongshu

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * 小红书网页签名里**随版本变的值**，全放在 `resources/feedhub/source/xiaohongshu/sign_profile.json`：
 * 签名版本号、`webBuild`、UA、x3 里的环境常量和密钥流、b1 的指纹模板、x-rap-param 的指纹模板。
 * 小红书改版时只改这个文件，算法代码不用动；
 * 也可以把一份新的 JSON 整个放进 `ctx.store` 的 [KEY] 键，不用重新打包就能覆盖（见 [XhsSource]）。
 *
 * 当前内容取自 cv-cat/catbus（MIT）`static/xhs/xhs_pc/js/reference_profile.json` 和 `rap_fingerprint_template.json`
 * 密钥流取自 xhshow（MIT）的 `HEX_KEY`，两个项目的前 144 字节逐字一致。
 */
@Serializable
internal data class XhsProfile(
    val release: Release,
    /** x-s-common 的 x12 = `<dsllt>;<_dsl>` 里的 `_dsl`：服务端 `as.xiaohongshu.com/api/sec/v1/ds` 下发的时间锚点，不同节点给的值不同。 */
    val dsl: String,
    val mns: Mns,
    val b1: B1,
    val rap: Rap,
) {
    @Serializable
    data class Release(
        val signVersion: String,
        val appId: String,
        val platform: String,
        /** Cookie 里没有 `webBuild` 时才用它。 */
        val webBuild: String,
        val userAgent: String,
        val secChUa: String,
        val acceptLanguage: String,
    )

    @Serializable
    data class Mns(
        /** x3 里的 `envConst`（网页里近似 window 属性个数，稳态请求用这一档）。 */
        val envConst: Int,
        /** x3 环境位的后 14 字节。 */
        val envFpTailHex: String,
        /** 和 x3 明文逐字节异或的固定密钥流，至少 144 字节。 */
        val keystreamHex: String,
    ) {
        val envFpTail: ByteArray get() = XhsCodec.unhex(envFpTailHex)
        val keystream: ByteArray get() = XhsCodec.unhex(keystreamHex)
    }

    @Serializable
    data class B1(
        val sdkVersion: String,
        val frameCount: Int,
        val x37: String,
        val x38: String,
        val x39: String,
        val x43: String,
        val x45: String,
        val x46: String,
        val x48: String,
        val x49: String,
        val x50: String,
        val x51: String,
        val x52: String,
        val x82: String,
        /** `performance.timeOrigin` 相对页面加载时间（Cookie 里的 loadts）的偏移，毫秒。 */
        val timeOriginOffsetMs: Double,
        /** x84：行为遥测串，`__TIME_ORIGIN__` 换成 timeOrigin。 */
        val telemetryTemplate: String,
    )

    @Serializable
    data class Rap(
        /** 信封里的协议版本号（10301 = 0x283d）。 */
        val version: Int,
        /** 固定的行为 / 环境指纹，219 字节：开头是 `03ea` 会话密钥 TLV 和 `03eb` 请求哈希 TLV，后面是一串环境位。 */
        val templateHex: String,
    ) {
        val template: ByteArray get() = XhsCodec.unhex(templateHex)
    }

    companion object {
        /** 存在 `ctx.store` 里的覆盖版本（整份 JSON）。 */
        const val KEY = "xhs.profile"

        private val json = Json { ignoreUnknownKeys = true }

        fun parse(text: String): XhsProfile = json.decodeFromString(serializer(), text)

        /** 再序列化回 JSON（形状和 `sign_profile.json` 一样）：Rust 侧的签名函数只吃这个字符串，不读文件。 */
        fun encode(profile: XhsProfile): String = json.encodeToString(serializer(), profile)

        /** 随包的版本。 */
        fun bundled(): XhsProfile {
            val text = XhsProfile::class.java.getResourceAsStream("/feedhub/source/xiaohongshu/sign_profile.json")
                ?.use { it.readBytes().toString(Charsets.UTF_8) }
                ?: error("缺少 sign_profile.json")
            return parse(text)
        }
    }
}
