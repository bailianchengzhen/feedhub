// 唯一的平台注册表：按仓库根的 plugins.toml **生成** feedhub.plugins.InstalledPlugins（app 和 cli 共用）。
// 清单由 settings.gradle.kts 读好、检查过，放在 gradle.extra["feedhub.plugins"]（模块 / id / 描述常量的完整名字）。
// 拆一个插件 = 从 plugins.toml 删一行：这里的依赖和生成的代码跟着少一个，别的模块一行都不用改。
plugins {
    alias(libs.plugins.kotlin.jvm)
}

@Suppress("UNCHECKED_CAST")
val pluginEntries = gradle.extra["feedhub.plugins"] as List<Map<String, String>>

dependencies {
    api(project(":core"))
    // 启用的插件（它们自己的依赖——quickjs、wire、jbcrypt……——在各自的 build.gradle.kts 里，跟着插件一起来）
    for (e in pluginEntries) implementation(project(":${e.getValue("module")}"))

    testImplementation(kotlin("test"))
    // 插件契约自检（`PluginContract.check`）会造一个 `SourceContext.inMemory()`，
    // 和别的插件模块一样，测试里显式带上 JNA。
    testImplementation(libs.jna)
    // 校验逻辑在 Rust）
    testImplementation(project(":rust-bridge"))
    testRuntimeOnly(libs.junit.launcher)
}

val generatePluginRegistry = tasks.register("generatePluginRegistry") {
    group = "build"
    description = "按 plugins.toml 生成 feedhub.plugins.InstalledPlugins"
    val out = layout.buildDirectory.dir("generated/plugin-registry/kotlin")
    val entries = pluginEntries.map { listOf(it.getValue("module"), it.getValue("id"), it.getValue("descriptor")) }
    inputs.property("entries", entries)
    outputs.dir(out)
    doLast {
        val dir = out.get().asFile.resolve("feedhub/plugins")
        dir.deleteRecursively()
        dir.mkdirs()
        val ids = entries.joinToString(", ") { "\"${it[1]}\"" }
        val modules = entries.joinToString(", ") { "\"${it[0]}\"" }
        val plugins = entries.joinToString("") { "            ${it[2]}, // ${it[0]}\n" }
        dir.resolve("InstalledPlugins.kt").writeText(
            """
            |// 由 plugins.toml 生成（:plugin-registry:generatePluginRegistry），别手改。
            |package feedhub.plugins
            |
            |import feedhub.core.plugin.PlatformCatalog
            |
            |/** 这次构建启用的平台插件。顺序 = plugins.toml 的顺序 = 界面上的顺序。app 和 cli 共用这一份。 */
            |object InstalledPlugins {
            |    /** plugins.toml 里的模块名（和 [catalog] 一一对应）。 */
            |    val modules: List<String> = listOf($modules)
            |
            |    /** 各模块 plugin.toml 里写的 id。 */
            |    val ids: List<String> = listOf($ids)
            |
            |    val catalog: PlatformCatalog = PlatformCatalog(
            |        listOf(
            |$plugins        ),
            |    ).also { c ->
            |        // plugin.toml 写的 id 和描述里 Platform.define 的 id 对不上，说明有一边写错了：启动时就报，别等到存储里出怪数据
            |        check(c.platforms.map { it.key } == ids) { "plugin.toml 的 id ${'$'}ids 和插件描述的 id ${'$'}{c.platforms.map { it.key }} 对不上" }
            |    }
            |}
            |""".trimMargin(),
        )
    }
}
kotlin.sourceSets.named("main") { kotlin.srcDir(generatePluginRegistry) }

// 边界检查（PluginBoundaryTest）要扫整个仓库的源码
tasks.named<Test>("test") {
    systemProperty("feedhub.root", rootDir.absolutePath)
    systemProperty("feedhub.plugins.modules", pluginEntries.joinToString(",") { it.getValue("module") })
    inputs.files(fileTree(rootDir) { include("*/src/**/*.kt", "*/build.gradle.kts", "plugins.toml", "*/plugin.toml"); exclude("**/build/**") })
        .withPropertyName("repoSources").withPathSensitivity(PathSensitivity.RELATIVE)
}
