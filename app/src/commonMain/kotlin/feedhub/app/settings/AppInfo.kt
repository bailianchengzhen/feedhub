package feedhub.app.settings

/** 关于页用的信息。版本号要和 app/build.gradle.kts 里的 packageVersion 保持一致。 */
object AppInfo {
    const val NAME = "feedhub"
    const val VERSION = "0.1.0"
    const val DESCRIPTION = "十一个平台（B站 / 知乎 / 贴吧 / 小黑盒 / 酷安 / 小红书 / 抖音 / X / YouTube / Reddit / TikTok）的聚合信息流"

    class License(val name: String, val license: String)

    /** 用到的开源库（直接依赖）。 */
    val licenses: List<License> = listOf(
        License("Kotlin / kotlinx.coroutines / kotlinx.serialization", "Apache-2.0"),
        License("Compose Multiplatform（含 Material 3 和图标）", "Apache-2.0"),
        License("OkHttp / Okio", "Apache-2.0"),
        License("Coil（图片加载）", "Apache-2.0"),
        License("AndroidX SQLite（本地资料库驱动）", "Apache-2.0"),
        License("SQLite", "公有领域"),
        License("Wire（protobuf 运行时）", "Apache-2.0"),
        License("jsoup（HTML 解析）", "MIT"),
        License("jBCrypt", "ISC"),
    )
}
