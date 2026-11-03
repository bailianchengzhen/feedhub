plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(project(":core"))
    // 推荐流 / 关注流的整套请求逻辑在 Rust，宿主回调也通过它（没有开关，工厂直接建 Rust 版）。
    // 插件依赖桥接层；桥接层不依赖插件，边界不倒过来。
    implementation(project(":rust-bridge"))

    testImplementation(testFixtures(project(":core")))
    testImplementation(kotlin("test"))
    // 跑 Rust 那条路的测试要 JNA（桌面 JNA 在 :app 里才随 vlcj 进来，这里单独带一份）
    testImplementation(libs.jna)
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.launcher)
}

// 在线探针（只在用户给了代理、明确要验的时候手工跑；不进 ./gradlew test）：
//   ./gradlew :source-reddit:redditProbe -Pargs="probe"
//   ./gradlew :source-reddit:redditProbe -Pargs="record <目录>"
// 请求预算写死在探针里（≤ 12 个接口请求），走配置的代理，遇风控立刻停。
tasks.register<JavaExec>("redditProbe") {
    group = "verification"
    description = "Reddit 在线探针：whoami / 首页 / 关注 / 详情评论 / more（走 SOCKS5，有请求预算）"
    dependsOn(tasks.named("testClasses"))
    classpath = sourceSets["test"].output + sourceSets["main"].output + configurations["testRuntimeClasspath"]
    mainClass.set("feedhub.source.reddit.dev.RedditProbeKt")
    jvmArgs("-Dfile.encoding=UTF-8")
    workingDir = rootDir
    args((project.findProperty("args") as String?)?.split(" ")?.filter { it.isNotBlank() } ?: emptyList<String>())
}
