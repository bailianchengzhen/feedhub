plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(project(":core"))
    // X-App-Token 里的 bcrypt，c001apk 用的也是它（Rust 版自己实现了 bcrypt，这里留着给 Kotlin 参照实现）
    implementation(libs.jbcrypt)
    // Rust 解析 / X-App-Token（没有开关，工厂直接建 Rust 版）。插件依赖桥接层；桥接层不依赖插件，边界不倒过来。
    implementation(project(":rust-bridge"))

    testImplementation(testFixtures(project(":core")))
    testImplementation(kotlin("test"))
    // 跑 Rust 解析的测试要 JNA（桌面 JNA 在 :app 里才随 vlcj 进来，这里单独带一份）
    testImplementation(libs.jna)
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.launcher)
}
