plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(project(":core"))
    // 推荐流 / 关注流的整套请求 + txid 的算法在 Rust（没有开关，工厂直接建 Rust 版）。
    // 插件依赖桥接层；桥接层不依赖插件，边界不倒过来。
    implementation(project(":rust-bridge"))

    testImplementation(testFixtures(project(":core")))
    testImplementation(kotlin("test"))
    // 跑 Rust 的测试要 JNA（桌面 JNA 在 :app 里才随 vlcj 进来，这里单独带一份）
    testImplementation(libs.jna)
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.launcher)
}

// txid 抓取链体检（在线，只发少数几个请求）：
//   ./gradlew :source-x:txidProbe -Pargs="live"
// 只取 x.com 首页 HTML + txid 需要的几个脚本，在本地算一个 txid，打印"找到了 / 卡在第几跳"，
// **不打印任何脚本内容、不发任何 GraphQL 请求**。走用户的 SOCKS5 代理。
tasks.register<JavaExec>("txidProbe") {
    group = "verification"
    description = "txid 抓取链在线体检（只发少数几个请求，不打印脚本内容）"
    dependsOn(tasks.named("testClasses"))
    classpath = sourceSets["test"].output + sourceSets["main"].output + configurations["testRuntimeClasspath"]
    mainClass.set("feedhub.source.x.dev.XtxProbeKt")
    jvmArgs("-Dfile.encoding=UTF-8")
    workingDir = rootDir
    args((project.findProperty("args") as String?)?.split(" ")?.filter { it.isNotBlank() } ?: emptyList<String>())
}

// X 登录态的在线只读验证：
//   ./gradlew :source-x:xliveProbe -Pargs="live [录制目录]"
// whoami → 推荐流 → 关注流 → 详情 + 评论；走用户的 SOCKS5 代理，只打印状态码和限速头，不打印 body / Cookie。
tasks.register<JavaExec>("xliveProbe") {
    group = "verification"
    description = "X 登录态在线体检（只读，走 SOCKS5 代理，有请求数上限）"
    dependsOn(tasks.named("testClasses"))
    classpath = sourceSets["test"].output + sourceSets["main"].output + configurations["testRuntimeClasspath"]
    mainClass.set("feedhub.source.x.dev.XLiveProbeKt")
    jvmArgs("-Dfile.encoding=UTF-8")
    workingDir = rootDir
    args((project.findProperty("args") as String?)?.split(" ")?.filter { it.isNotBlank() } ?: emptyList<String>())
}

