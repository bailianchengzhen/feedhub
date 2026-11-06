package feedhub.app.settings

import feedhub.app.BuildFeatures

/** 关于页用的信息。版本号要和 app/build.gradle.kts 里的 packageVersion 保持一致。 */
object AppInfo {
    const val NAME = "feedhub"
    const val VERSION = "0.1.1"
    const val DESCRIPTION = "十一个平台（B站 / 知乎 / 贴吧 / 小黑盒 / 酷安 / 小红书 / 抖音 / X / YouTube / Reddit / TikTok）的聚合信息流"

    class License(val name: String, val license: String)

    /** 用到的开源库（直接依赖）。 */
    val licenses: List<License> = buildList {
        add(License("Kotlin / kotlinx.coroutines / kotlinx.serialization", "Apache-2.0"))
        add(License("Compose Multiplatform（含 Material 3 和图标）", "Apache-2.0"))
        add(License("OkHttp / Okio", "Apache-2.0"))
        add(License("Coil（图片加载）", "Apache-2.0"))
        add(License("AndroidX SQLite（本地资料库驱动）", "Apache-2.0"))
        add(License("SQLite", "公有领域"))
        add(License("Wire（protobuf 运行时）", "Apache-2.0"))
        add(License("jsoup（HTML 解析）", "MIT"))
        add(License("jBCrypt", "ISC"))
        if (BuildFeatures.TRANSLATE) {
            add(License("Project Bergamot / Marian NMT（本地翻译引擎）", "MPL-2.0 / MIT"))
            add(License("Firefox Translations Models（英译中模型）", "CC-BY-SA-4.0 / CC0"))
        }
    }
}
