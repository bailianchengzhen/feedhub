plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(project(":core"))
    // Rust 平台层（推荐流的整套请求逻辑 + 签名 + 解析；没有开关，工厂直接建 Rust 版）。
    // 插件依赖桥接层；桥接层不依赖插件，边界不倒过来。
    implementation(project(":rust-bridge"))

    testImplementation(testFixtures(project(":core")))
    testImplementation(kotlin("test"))
    // 跑 Rust 解析 / 签名的测试要 JNA（桌面 JNA 在 :app 里才随 vlcj 进来，这里单独带一份）
    testImplementation(libs.jna)
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.launcher)
}

// 在线探针（只在用户给了代理、明确要验的时候手工跑；不进 ./gradlew test）：
//   ./gradlew :source-tiktok:tiktokProbe -Pargs="probe"
//   ./gradlew :source-tiktok:tiktokProbe -Pargs="record <目录>"
// 请求预算写死在探针里（≤ 15 个接口请求），走配置的代理，遇风控立刻停。
tasks.register<JavaExec>("tiktokProbe") {
    group = "verification"
    description = "TikTok 在线探针：首页 hydration / 推荐流 / 详情 / 评论 / 楼中楼（走 SOCKS5，有请求预算）"
    dependsOn(tasks.named("testClasses"))
    classpath = sourceSets["test"].output + sourceSets["main"].output + configurations["testRuntimeClasspath"]
    mainClass.set("feedhub.source.tiktok.dev.TiktokProbeKt")
    jvmArgs("-Dfile.encoding=UTF-8")
    workingDir = rootDir
    args((project.findProperty("args") as String?)?.split(" ")?.filter { it.isNotBlank() } ?: emptyList<String>())
}
