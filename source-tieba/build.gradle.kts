plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(project(":core"))
    // Rust 解析 / App 签名。插件依赖桥接层；桥接层不依赖插件，边界不倒过来。
    implementation(project(":rust-bridge"))
    // 扫码登录：百度只给二维码图片，要把图片解成二维码内容（见 QrImage）
    implementation(libs.zxing.core)

    testImplementation(testFixtures(project(":core")))
    // 对拍参照：Kotlin 原来那份贴吧实现在 src/test 的 legacy/ 里，它要的 protobuf 定义单独一个模块（见 settings）
    findProject(":source-tieba-legacy-proto")?.let { testImplementation(it) }
    testImplementation(kotlin("test"))
    // 跑 Rust 解析的测试要 JNA（桌面 JNA 在 :app 里才随 vlcj 进来，这里单独带一份）
    testImplementation(libs.jna)
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.launcher)
}
