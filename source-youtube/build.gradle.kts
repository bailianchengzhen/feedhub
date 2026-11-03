plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

dependencies {
    api(project(":core"))
    // 推荐 / 订阅流的整套请求逻辑在 Rust，宿主回调也通过它（没有开关，工厂直接建 Rust 版）。
    // 插件依赖桥接层；桥接层不依赖插件，边界不倒过来。
    implementation(project(":rust-bridge"))

    // n/sig：跑 yt-dlp 的 EJS（Unlicense，随模块资源带）用的是 quickjs-kt（Apache-2.0）
    //
    // 这里写的是 KMP 的**根坐标**，不是某个变体：quickjs-kt 1.0.5 的元数据里有 androidJvm / jvm 两套变体
    // （`org.gradle.jvm.environment=android` → `quickjs-kt-android` 的 `quickjs-release.aar`；
    // `standard-jvm` → `quickjs-kt-jvm` 的 jar）。所以：
    // - 这个模块自己（kotlin("jvm")，standard-jvm 属性）编译 / 跑单测时解析成 `-jvm`（桌面 .so）；
    // - 被 `:androidApp` 的安卓运行时类路径拉过去时解析成 `quickjs-kt-android`（AAR 里是
    //   `classes.jar` + `jni/arm64-v8a`、`jni/x86_64` 的 `libquickjs.so`）。
    // **不要**在这里写死 `io.github.dokar3:quickjs-kt-android`：那会把桌面构建也换成安卓变体
    // （桌面会去 `System.loadLibrary` 安卓的 .so，加载失败）。`:androidApp:verifyQuickJsAndroid`
    // 守着"安卓那份只能是 AAR"；两套变体的公开 API 逐字节一致。
    implementation(libs.quickjs.kt)

    testImplementation(testFixtures(project(":core")))
    testImplementation(kotlin("test"))
    // 跑 Rust 那条路的测试要 JNA（桌面 JNA 在 :app 里才随 vlcj 进来，这里单独带一份）
    testImplementation(libs.jna)
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.launcher)
}

// 只读的在线排障 / 录夹具工具（默认什么都不做，必须显式给参数才发请求）：
//   ./gradlew :source-youtube:youtubeProbe -Pargs="player <videoId> out=<dir>"
//   ./gradlew :source-youtube:youtubeProbe -Pargs="browse default out=<dir>"
// 代码在 src/test 里（要用 internal 的解析实现），代理按 cli 的系统代理设置走。
tasks.register<JavaExec>("youtubeProbe") {
    group = "verification"
    description = "YouTube InnerTube 只读排障 / 录夹具（会发真实请求，参数见 source-youtube/README.md）"
    dependsOn(tasks.named("testClasses"))
    classpath = sourceSets["test"].output + sourceSets["main"].output + configurations["testRuntimeClasspath"]
    mainClass.set("feedhub.source.youtube.dev.YoutubeProbeKt")
    jvmArgs("-Dfile.encoding=UTF-8")
    workingDir = rootDir
    args((project.findProperty("args") as String?)?.split(" ")?.filter { it.isNotBlank() } ?: emptyList<String>())
}
