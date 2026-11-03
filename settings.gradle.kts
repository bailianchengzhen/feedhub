pluginManagement {
    repositories {
        gradlePluginPortal()
        mavenCentral()
        google()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        mavenCentral()
        google()
    }
}

rootProject.name = "feedhub"

include(":core")
// Rust 核心的 Kotlin 侧（UniFFI 生成的绑定 + 适配层）。纯 JVM，不依赖任何平台插件；
// 平台插件（source-bilibili）反过来依赖它。见 rust-bridge/build.gradle.kts。
include(":rust-bridge")
// 桌面播放引擎（GStreamer 1.26 + VA-API 硬解）的独立 UniFFI 绑定模块；仅桌面依赖，不进安卓 APK / dex。
include(":rust-gst")

// 平台插件
// 启用哪些 source-* 模块、按什么顺序（= 界面顺序），只看仓库根的 plugins.toml；每个模块根有一份 plugin.toml
// （id + 描述常量的完整名字）。这里读两者、做基本检查，include 启用的模块，并把清单交给 :plugin-registry
// 生成唯一的注册表（app 和 cli 共用）。不在清单里的 source-* 模块不参与构建——拆插件 = 从清单里删一行。
// TOML 只用到最简单的写法（`key = "字符串"`、`key = ["字符串", …]`、`#` 注释），所以这里手写了一个小解析，
// 不引第三方库；cli 的 `./feedhub plugin` 命令用同样的规则读写（feedhub.cli.PluginManifest）。

/** 从 TOML 文本里取一个键的值：字符串或字符串数组（只认这两种）。没有这个键返回 null。 */
fun tomlStrings(text: String, key: String, file: File): List<String>? {
    val noComments = text.lines().joinToString("\n") { line ->
        // 去掉不在引号里的 # 注释
        var inStr = false
        val cut = line.indexOfFirst { c -> if (c == '"') inStr = !inStr; c == '#' && !inStr }
        if (cut >= 0) line.substring(0, cut) else line
    }
    val m = Regex("""(?m)^\s*${Regex.escape(key)}\s*=\s*(\[[^\]]*]|"[^"\n]*")""").find(noComments) ?: return null
    val value = m.groupValues[1]
    val strings = Regex(""""([^"\n]*)"""").findAll(value).map { it.groupValues[1] }.toList()
    if (value.startsWith("[")) {
        val rest = value.removePrefix("[").removeSuffix("]").replace(Regex(""""[^"\n]*""""), "").replace(",", "").trim()
        if (rest.isNotEmpty()) throw GradleException("$file：$key 的数组里只能有字符串（多了 `$rest`）")
    }
    return strings
}

val pluginManifest = file("plugins.toml")
if (!pluginManifest.isFile) throw GradleException("找不到 ${pluginManifest}：平台插件清单（见 docs/平台插件指南.md）")
val pluginModules: List<String> = tomlStrings(pluginManifest.readText(), "plugins", pluginManifest)
    ?: throw GradleException("$pluginManifest 里没有 plugins = [ … ]")
val pluginEntries: List<Map<String, String>> = pluginModules.map { module ->
    val dir = file(module)
    val meta = dir.resolve("plugin.toml")
    if (!Regex("[a-z][a-z0-9-]*").matches(module)) throw GradleException("plugins.toml：模块名不合法：$module")
    if (!dir.resolve("build.gradle.kts").isFile) throw GradleException("plugins.toml 里的 $module 不是一个模块（没有 $module/build.gradle.kts）")
    if (!meta.isFile) throw GradleException("$module 没有 plugin.toml（平台插件的构建期描述，见 docs/平台插件指南.md）")
    val text = meta.readText()
    val id = tomlStrings(text, "id", meta)?.singleOrNull() ?: throw GradleException("$meta 缺 id = \"…\"")
    val descriptor = tomlStrings(text, "descriptor", meta)?.singleOrNull() ?: throw GradleException("$meta 缺 descriptor = \"…\"")
    if (!Regex("[a-z][a-z0-9_-]{0,31}").matches(id)) throw GradleException("$meta：id 不合法：$id（小写字母开头，只能有小写字母、数字、-、_）")
    if (!Regex("[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)*\\.[A-Za-z_][A-Za-z0-9_]*").matches(descriptor)) {
        throw GradleException("$meta：descriptor 要写成完整名字，例如 feedhub.source.bilibili.BilibiliPlugin（现在是 $descriptor）")
    }
    mapOf("module" to module, "id" to id, "descriptor" to descriptor)
}
pluginEntries.groupBy { it.getValue("id") }.filterValues { it.size > 1 }.keys.firstOrNull()?.let { dup ->
    throw GradleException("plugins.toml：两个插件用了同一个 id：$dup")
}
pluginModules.groupBy { it }.filterValues { it.size > 1 }.keys.firstOrNull()?.let { dup ->
    throw GradleException("plugins.toml：$dup 写了两遍")
}
for (e in pluginEntries) include(":${e.getValue("module")}")
// 贴吧的 protobuf 定义只给 :source-tieba 的对拍用（Kotlin 参照实现），目录不在就不建
if ("source-tieba" in pluginModules && file("source-tieba/legacy-proto/build.gradle.kts").isFile) {
    include(":source-tieba-legacy-proto")
    project(":source-tieba-legacy-proto").projectDir = file("source-tieba/legacy-proto")
}
// 交给 :plugin-registry（生成注册表）和 app（截图工具的演示环境）用
gradle.extra["feedhub.plugins"] = pluginEntries
include(":plugin-registry")

include(":cli")
include(":app")
// 安卓 APK 的外壳（manifest / applicationId / 主题资源）。界面和逻辑都在 app 的 commonMain；
// app 的安卓侧是 Android-KMP 库（AGP 9 起 KMP 模块不能直接配 com.android.application）。
include(":androidApp")
// Baseline Profile 的生成器：com.android.test 的 macrobenchmark 模块，
// **不参与** APK 的组装，只在真机 / 模拟器上跑 `:baselineprofile:generateBaselineProfile` 时用。
// 当前没跑，APK 里先用 `androidApp/src/main/baselineProfiles/`
// 那份手写的；生成器跑完会把结果写回同一个目录，
include(":baselineprofile")
