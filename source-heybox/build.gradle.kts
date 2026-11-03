plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(project(":core"))
    // 推荐流的解析 / hkey 签名走 Rust 核心。插件依赖桥接层；桥接层不依赖插件，边界不倒过来。
    implementation(project(":rust-bridge"))

    testImplementation(testFixtures(project(":core")))
    testImplementation(kotlin("test"))
    // 跑 Rust 解析的测试要 JNA（桌面 JNA 在 :app 里才随 vlcj 进来，这里单独带一份）
    testImplementation(libs.jna)
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.launcher)
}
