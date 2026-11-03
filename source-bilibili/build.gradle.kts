plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
    // 弹幕分段接口返回 protobuf：用 Square Wire 从 src/main/proto 生成 Kotlin 消息类。
    // 选 Wire 的理由和 source-tieba 一样：纯 JVM 的代码生成（不用下载平台相关的 protoc 可执行文件），运行时只有 wire-runtime + okio
    // （okio 本来就随 OkHttp 引入），生成的是不可变 Kotlin 类，Android 上也能直接用。这里只有一份很小的 dm.proto。
    alias(libs.plugins.wire)
}

wire {
    kotlin {}
}

dependencies {
    api(project(":core"))
    // Rust 解析 / WBI 签名（实验开关，默认关）。插件依赖桥接层；桥接层不依赖插件，边界不倒过来。
    implementation(project(":rust-bridge"))
    implementation(libs.wire.runtime)

    testImplementation(testFixtures(project(":core")))
    testImplementation(kotlin("test"))
    // 跑 Rust 解析的测试要 JNA（桌面 JNA 在 :app 里才随 vlcj 进来，这里单独带一份）
    testImplementation(libs.jna)
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.launcher)
}
