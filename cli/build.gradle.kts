plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
    application
}

dependencies {
    implementation(project(":core"))
    // 首页大脑 / 搜索混排 / 内容过滤的 Rust 门面（和 App 共用一份规则，见 rust-bridge/.../brain/）
    implementation(project(":rust-bridge"))
    // 平台：只依赖生成的注册表（按 plugins.toml），不直接依赖任何 source-* 模块，
    implementation(project(":plugin-registry"))
    implementation(libs.clikt)
    // login 命令画终端 / PNG 二维码
    implementation(libs.zxing.core)
    // `library favorites/history`：只读打开 App 的同一个 library.db（androidx.sqlite 的 JVM 版，和 App 用的是同一个驱动）
    implementation(libs.androidx.sqlite.bundled)

    testImplementation(kotlin("test"))
    testImplementation(libs.coroutines.test)
    // cli 的代理离线测试要假服务端（假 HTTP 代理 / 目标服务器）
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.launcher)
}

application {
    applicationName = "feedhub"
    mainClass.set("feedhub.cli.MainKt")
    applicationDefaultJvmArgs = listOf("-Dstdout.encoding=UTF-8", "-Dstderr.encoding=UTF-8", "-XX:TieredStopAtLevel=1")
}

// PluginManifestTest 要用仓库里的插件模板（tools/plugin-template）
tasks.named<Test>("test") {
    systemProperty("feedhub.root", rootDir.absolutePath)
    inputs.dir(rootDir.resolve("tools/plugin-template")).withPropertyName("pluginTemplate").withPathSensitivity(PathSensitivity.RELATIVE)
}
