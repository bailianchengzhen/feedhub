plugins {
    // 对拍（Kotlin 解析 vs Rust 解析）的公用工具放在 testFixtures 里，给各平台插件的测试复用：
    // `testImplementation(testFixtures(project(":core")))`。见 core/src/testFixtures 的 FeedItemDiff。
    `java-test-fixtures`
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(libs.coroutines.core)
    api(libs.serialization.json)
    api(libs.okhttp)
    // HTML 正文 → RichText（HtmlRichText）。纯 Java，安卓和桌面都能用。
    api(libs.jsoup)
    // 跨设备同步的设备密钥：X25519 + HKDF-SHA256（lightweight API，MIT）。
    // 这份 bcprov 只给 `feedhub.core.sync.legacy.LegacyDeviceKeys`用。
    testFixturesApi(libs.bouncycastle.bcprov)

    testFixturesApi(libs.serialization.json)

    testImplementation(kotlin("test"))
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    // 所以那三处走"注册 + 反射"（照 `CookieJsonBridge` 的先例）。这行让 `:core` 自己的单测也装上那份注册，
    // 于是它们测的就是**生产里跑的那一份**，而不是另一个 Kotlin 版。**不是循环依赖**：`:rust-bridge` 只要
    // `:core` 的 main / testFixtures，`:core:test` 要 `:rust-bridge` 的 main，两个配置不同。
    testImplementation(project(":rust-bridge"))
    // 上面那条意味着 `:core:test` 会加载 .so（JNA）。
    testImplementation(libs.jna)
    testRuntimeOnly(libs.junit.launcher)
}

// 内置代理核心用到的 sing-box 二进制（80 MB，**不进仓库**）。延迟测试和端到端测试要真的起它，
// 路径用 gradle 属性给、可以覆盖：`-Pfeedhub.singbox=/path/to/sing-box`（`rust-bridge` 的端到端测试也用同一个属性）。
// 二进制不在时这些测试会 assumeTrue 跳过（离线测试的主体仍然是绿的）。
val singBoxBinary: String = providers.gradleProperty("feedhub.singbox")
    .getOrElse(rootProject.file("deps/sing-box/linux-amd64/sing-box").path)
tasks.withType<Test>().configureEach {
    systemProperty("feedhub.singbox", singBoxBinary)
}
