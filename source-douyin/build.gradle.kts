plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(project(":core"))
    // Rust 解析 / 签名（没有开关，工厂直接建 Rust 版）。插件依赖桥接层；桥接层不依赖插件，边界不倒过来。
    implementation(project(":rust-bridge"))

    testImplementation(testFixtures(project(":core")))
    // 跑 Rust 解析的测试要 JNA（桌面 JNA 在 :app 里才随 vlcj 进来，这里单独带一份）
    testImplementation(libs.jna)
    testImplementation(kotlin("test"))
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.launcher)
}

// a_bogus 常量对拍工具（离线，不发任何网络请求；代码在 src/test 里，
tasks.register<JavaExec>("abogusProbe") {
    group = "verification"
    description = "a_bogus 常量对拍：selftest / oracle / capture <抓包 json>（离线）"
    dependsOn(tasks.named("testClasses"))
    classpath = sourceSets["test"].output + sourceSets["main"].output + configurations["testRuntimeClasspath"]
    mainClass.set("feedhub.source.douyin.dev.AbogusProbeKt")
    jvmArgs("-Dfile.encoding=UTF-8")
    workingDir = rootDir
    args((project.findProperty("args") as String?)?.split(" ")?.filter { it.isNotBlank() } ?: emptyList<String>())
}
