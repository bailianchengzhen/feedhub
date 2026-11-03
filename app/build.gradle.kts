import org.jetbrains.compose.desktop.application.dsl.TargetFormat
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import java.nio.file.Files
import java.nio.file.Paths
import java.security.MessageDigest
import java.util.zip.ZipFile

plugins {
    alias(libs.plugins.kotlin.multiplatform)
    alias(libs.plugins.compose.compiler)
    alias(libs.plugins.compose)
    alias(libs.plugins.kotlin.serialization)
    // 安卓侧：AGP 9 起 KMP 模块得用 com.android.kotlin.multiplatform.library（com.android.application 和
    // KMP 插件不再兼容）。这个模块只出 AAR，APK 的外壳是 :androidApp，见 settings.gradle.kts。
    alias(libs.plugins.android.kmp.library)
}

// ** 语言识别搬进 Rust
// （`rust/crates/feedhub-core/src/translate/lang.rs`，`lingua` crate 只编中 / 英两张模型），
// 所以这里原来那套"裁一个只有中英模型的 jar 再塞进 deb"（`linguaModels` / `linguaTrimmedJar`，
// 约 1.6 MB）整块删了；Kotlin 的 lingua 现在只在 `desktopTest`里用。

// 启用的插件里带演示环境（src/demo/，截图工具的 fake=，见 core 的 PlatformDemo）的那些：**只给**下面的截图 / 调试任务用，
// 不进 App 的发布包和 APK。清单和 :plugin-registry 是同一份（settings.gradle.kts 读的 plugins.toml），拆插件时跟着少。
@Suppress("UNCHECKED_CAST")
val pluginEntries = gradle.extra["feedhub.plugins"] as List<Map<String, String>>
val pluginDemos: Configuration by configurations.creating {
    isCanBeConsumed = false
    isCanBeResolved = true
    // 只要各插件的 demoJar；插件本身和它的依赖已经通过 :plugin-registry 在 desktop 的运行时类路径里了
    isTransitive = false
}
dependencies {
    for (module in pluginEntries.map { it.getValue("module") }) {
        if (rootProject.file("$module/src/demo").isDirectory) {
            add("pluginDemos", project(mapOf("path" to ":$module", "configuration" to "demoElements")))
        }
    }
}


/**
 * 本地向量走 NPU：`-Pfeedhub.embedNpu=true` 的构建才引 QNN。
 *
 * 不带属性时这两个 AAR 一个字节都不进 APK：NPU 那条路是实验开关，默认关，
 * 只有用户自己实测功耗时才用得上。带了属性才把 QNN EP 插件 + QNN 运行时加进 `:app` 的安卓侧
 * （APK 里只留 v79 的 skel / stub，见 `androidApp/build.gradle.kts` 的 `jniLibs.excludes`）。
 */
val embedNpu: Boolean = providers.gradleProperty("feedhub.embedNpu").orNull == "true"

// 界面：Compose Multiplatform。两个目标：桌面（jvm "desktop"，出 .deb）和安卓（kotlin 里的 `android {}`，
// 出 APK 用的 AAR），界面代码写在 commonMain；桌面专有的东西（窗口、文件路径、截图工具）在 desktopMain，
// 安卓的在 androidMain（MainActivity、安卓的组装、演示数据）。
kotlin {
    jvm("desktop") {
        compilerOptions.jvmTarget.set(JvmTarget.JVM_17)
    }
    // 安卓目标：compileSdk 37（本机装的是 platforms/android-37.0；Compose / Coil 的安卓 AAR 要求 ≥ 37），
    // minSdk 26。
    // androidResources 打开。
    // 这个块在 KMP + com.android.kotlin.multiplatform.library 下就叫 `android`（AGP 9.4 起，旧的 `androidLibrary` 已废弃）。
    android {
        namespace = "feedhub.app"
        compileSdk = libs.versions.android.compile.sdk.get().toInt()
        minSdk = libs.versions.android.min.sdk.get().toInt()
        compilerOptions.jvmTarget.set(JvmTarget.JVM_17)
        androidResources { enable = true }
    }

    sourceSets {
        // 桌面和安卓**共用**的 JVM 源码集：资料库的 Rust 门面
        // （`app/src/jvmShared/kotlin/feedhub/app/library/RustLibrary.kt`）要调 `:rust-bridge` 生成的
        // UniFFI 绑定，而绑定是 JVM 模块，commonMain 看不见（KMP 的 commonMain 不能依赖 JVM 模块）；
        // 两个 JVM 目标又只有这一份实现，所以挂一个中间层，不用在 desktopMain / androidMain 各抄一遍。
        //
        // **为什么不用默认层级模板的 `jvmMain`**：本项目的安卓目标来自
        // `com.android.kotlin.multiplatform.library` 的 `android {}` 块，`jvm("desktop")` + 它这组合下模板
        // **没有**建出 `jvmMain → desktopMain / androidMain` 的边。所以自己建这个中间源集；
        // 代价是默认模板整份不应用，KGP 会打一条 "Default Kotlin Hierarchy Template Not Applied Correctly"
        // 的告警——gradle.properties 里显式写了 `kotlin.mpp.applyDefaultHierarchyTemplate=false` 让它闭嘴
        // （这个项目里只有 :app 是 KMP，且模板本来也没给 :app 建边，关掉不改变任何源集的可见性）。
        val jvmShared = create("jvmShared") { dependsOn(commonMain.get()) }
        jvmShared.dependencies { implementation(project(":rust-bridge")) }
        getByName("desktopMain").dependsOn(jvmShared)
        getByName("androidMain").dependsOn(jvmShared)
        commonMain.dependencies {
            implementation(project(":core"))
            // 首页大脑（穿插 / 打分 / 过滤 / 搜索混排）的门面在桥接层：:core 在依赖顺序上在它下面，不能倒过来。
            implementation(project(":rust-bridge"))
            // 平台：只依赖生成的注册表（按 plugins.toml），不直接依赖任何 source-* 模块，
            implementation(project(":plugin-registry"))

            implementation(compose.runtime)
            implementation(compose.foundation)
            implementation(compose.ui)
            implementation(libs.compose.material3)
            implementation(libs.compose.material.icons)
            implementation(libs.coil.compose)
            implementation(libs.coil.network.okhttp)
            // 扫码登录：把平台给的二维码内容画出来（纯 Java，安卓也能用）
            implementation(libs.zxing.core)
        }
        getByName("desktopMain").dependencies {
            implementation(project(":rust-gst"))
            implementation(compose.desktop.currentOs)
            implementation(libs.coroutines.swing)
            implementation(libs.vlcj)
            // **GStreamer 的 Java 绑定（JNA）不再是运行时依赖**：播放引擎换成
            // Rust 的 gstreamer-rs 之后，生产路径一个 gst1-java-core 的类都不用。它降成
            // `desktopTestImplementation`（下面 desktopTest 那一块），只给**对拍参照**
            // （`video/legacy/` 里那份 Kotlin 引擎）用，不进 deb。
            // 内嵌 Chromium：只给网页登录 / 平台强制的人机验证（酷安评论的易盾）用。
            // App 进程不初始化 CEF；
            // 原生包第一次用时下载，不打进 deb。
            implementation(libs.jcefmaven)
            // 不再依赖 lingua 的 jar（那个 1.6 MB 的裁剪 jar 和它的 4 个运行时依赖一起删了）。
        }
        getByName("androidMain").dependencies {
            // MainActivity（setContent 入口）；Compose 本体由 org.jetbrains.compose 的库带进来
            implementation(libs.androidx.activity.compose)
            // 安卓播放器：Media3 ExoPlayer（DASH / HLS / OkHttp 数据源 / PlayerView），见 androidMain 的 Media3PlayerEngine
            implementation(libs.media3.exoplayer)
            implementation(libs.media3.exoplayer.dash)
            implementation(libs.media3.exoplayer.hls)
            implementation(libs.media3.datasource.okhttp)
            implementation(libs.media3.ui)
            // 登录 WebView：ProxyController（代理覆盖，fail-closed）+ getCookieInfo（退出登录精确清 Cookie）
            implementation(libs.androidx.webkit)
            // 本地向量：ONNX Runtime 的安卓 AAR。安卓侧用它跑推理；
            // **桌面**只借它 AAR 里的 `headers/` 编 JNI shim（见下面 stageDesktopEmbed），不链它的 .so 进 deb；
            // 模型文件本身在 :androidApp 的 assets 里（构建时从 -Pfeedhub.embedModelDir 拷进去）
            implementation(libs.onnxruntime.android)
            // 本地向量走 NPU：只有 `-Pfeedhub.embedNpu=true` 才带上 QNN EP 插件 + QNN 运行时。
            // 加进来就是白占几十 MB）。
            if (embedNpu) {
                implementation(libs.onnxruntime.android.qnn)
                implementation(libs.qnn.runtime)
            }
            // 「从电脑同步」的扫码：CameraX（预览 + 帧分析）和 zxing core（app 的 commonMain 已经带了解码用的类）
            implementation(libs.androidx.camera.camera2)
            implementation(libs.androidx.camera.lifecycle)
            implementation(libs.androidx.camera.view)
            implementation(libs.androidx.lifecycle.runtime.compose)
        }
        getByName("desktopTest").dependencies {
            implementation(kotlin("test"))
            // 对拍参照：Kotlin 版的 GStreamer 引擎（`video/legacy/`）**留在测试源集里**当对照实现。
            // 生产路径（`PlayerEngineFactory` → `RustGstEngine`）不用它，所以它不进 deb 的依赖。
            implementation(libs.gst1.java.core)
            // 测试用完整 jar（不打包）；运行时用 trimmed jar 的路径由 desktopMain 的 runtimeOnly 带进来。
            implementation(libs.lingua)
            implementation(libs.coroutines.test)
            // 代理的接线测试：拿 MockWebServer 当假代理 / 假图片 CDN（只在本机回环上，不发外部请求）
            implementation(libs.okhttp.mockwebserver)
            // `CliLibraryReadTest`：cli 的只读资料库（`library favorites/history`）和真的 SqliteLibrary 对拍
            implementation(project(":cli"))
            // 对拍参照：SqliteLibrary / BundledSqliteNative 移入测试源集作对照实现
            implementation(libs.androidx.sqlite.bundled)
        }
    }
}

// desktopMain 里有一个 Java 文件（`org.cef.browser.FeedhubOffscreenBrowser`：要继承 JCEF 的包内可见基类，Kotlin 做不到），
// 和 Kotlin 一样编译成 Java 17 的字节码
tasks.withType<JavaCompile>().matching { it.name.startsWith("compileDesktop") }.configureEach { options.release.set(17) }

// gst1-java-core 依赖 net.java.dev.jna:jna（版本范围，会解析到最新），而 vlcj-natives 已经带了同一份 JNA（jna-jpms 5.16.0）；
// 两个都留在类路径上类会重复，所以去掉前者，统一用 jna-jpms。
configurations.configureEach { exclude(group = "net.java.dev.jna", module = "jna") }

// 单元测试（desktopTest，JUnit 5）；让 ./gradlew test 也跑它们
tasks.withType<Test>().configureEach {
    useJUnitPlatform()
    // 离线测试连不出去（见根目录 build.gradle.kts 的 offlineGuard）
    systemProperty("socksProxyHost", "127.0.0.1")
    systemProperty("socksProxyPort", "9")
    systemProperty("socksNonProxyHosts", "localhost|127.*|[::1]|0.0.0.0|[::0]")
    systemProperty("java.net.useSystemProxies", "false")
}
// `./gradlew test` 会跑到 app 的这些单元测试（desktopTest，JUnit 5）。
//
// `tasks.register("test")` 却没了——`./gradlew test` 于是**一条 app 测试都不跑**，全量从
// 4400 多条掉到 2704 条、而且"0 失败"。谁改这一段都别把这行删了。
tasks.register("test") { dependsOn("desktopTest") }

// **GStreamer 的 GIO 代理解析器会把结果缓存一辈子**（进程级，没有公开的清除接口）。
// 有几个用例必须改进程环境变量（`http_proxy` / `no_proxy`，见 GstProxyEnvTest / VideoRelayPlaybackTest）
// 才能验代理语义，而它们跑完之后同一个 JVM 里的后继用例就会带着"回环也要走代理"的记忆——实测表现为
// 经中转的 HLS / DASH 只出 1 帧。
//
// 不再 `forkEvery = 1` 把所有 `Test`
// 都拖下水（那一条让全量慢约 2 分钟）。收窄的依据是"哪些类会 setenv / 设系统属性"：
// - `GstProxyEnvTest`：三个用例各 setenv `http_proxy`（还改 `no_proxy`）；
// - `VideoRelayPlaybackTest`：两条"env http_proxy 不该截走回环"的用例 setenv；
// - `RelayStallProbeTest`：设 `feedhub.relay.debug` 系统属性（进程级；默认不跑，写了开关文件才跑）。
// 这几类用 JUnit 的 `@Tag("gst-process-env")` 标出来，**单独一个 Test 任务**（`gstEnvTest`）跑、
// 那一支 `forkEvery = 1`；主 `desktopTest` 只把这几类**排除**掉。
//
// 为什么"排除 + 单独跑"而不是"同一个任务里 forkEvery 只对某几类生效"：Gradle 的 `forkEvery` 是
// 任务级的（数的是"每个 JVM 跑几个测试类"），没有按类的开关；JUnit 的 `@Isolated` 又只隔离并发。
// 所以让被污染的那几类独占 JVM 的唯一办法就是让它们跑在另一个任务里。
val gstProcessEnvTag = "gst-process-env"

tasks.withType<Test>().configureEach {
    // 主任务（`desktopTest` / `test` / 各模块的 test）：跳过会改进程环境的那几类，一个 JVM 跑一批
    if (name == "gstEnvTest") return@configureEach
    useJUnitPlatform { excludeTags(gstProcessEnvTag) }
}

// 被隔离的那几类：一个类一个 JVM（它们之间也会互相污染——改了 `http_proxy` 的那个进程里，
// 后面再跑另一个改 `no_proxy` 的类同样会带着"回环也走代理"的记忆）。
val gstEnvTest = tasks.register<Test>("gstEnvTest") {
    group = "verification"
    description = "会改进程环境（GIO 代理解析器缓存）的那几类：一个类一个 JVM"
    useJUnitPlatform { includeTags(gstProcessEnvTag) }
    forkEvery = 1
    // 这一支就是 `desktopTest` 那份 classpath / 测试类（KMP 的 JVM 测试源集在 Gradle 里不叫
    // `desktopTest`——那是 Kotlin 源集名，SourceSetContainer 里没有它），从主任务里照抄。
    val main = tasks.named<Test>("desktopTest").get()
    testClassesDirs = main.testClassesDirs
    classpath = main.classpath
    systemProperty("socksProxyHost", "127.0.0.1")
    systemProperty("socksProxyPort", "9")
    systemProperty("socksNonProxyHosts", "localhost|127.*|[::1]|0.0.0.0|[::0]")
    systemProperty("java.net.useSystemProxies", "false")
    systemProperty("feedhub.embed.ort.dir", desktopEmbedPath.get())
    dependsOn(stageDesktopEmbed)
    // 单独跑的那一支也要能被全量套件的入口带起来（见下面 test / check 的接线）
    outputs.upToDateWhen { false }
}
// `./gradlew test` 和 `./gradlew check` 都要把隔离的那一支跑掉。`test` 这个任务注册在 189 行附近
// （`tasks.register("test") { dependsOn("desktopTest") }`），这里用 matching 免得跟注册顺序绑死。
tasks.named("desktopTest") { finalizedBy(gstEnvTest) }
tasks.matching { it.name == "test" }.configureEach { dependsOn(gstEnvTest) }
tasks.named("check") { dependsOn(gstEnvTest) }

// 内置代理核心用的 sing-box 二进制：
// **不进仓库**（linux-amd64 那份 81 MB），构建时从 `-Pfeedhub.singbox.dir=<版本目录>`
// 取，校验 sha256 之后放进 deb 的 App resources 目录（jpackage 的 appResourcesRootDir，
// 运行时对应系统属性 compose.application.resources.dir，见 feedhub.proxy.ProxyCoreBridge.locate）。
// 安卓阶段用同一个目录下的 android-arm64/sing-box。
val singBoxDir: String = providers.gradleProperty("feedhub.singbox.dir")
    .getOrElse(rootProject.file("deps/sing-box").path)
val singBoxSha256: String = providers.gradleProperty("feedhub.singbox.sha256")
    .getOrElse("768d31fa7f68debc067bb4626539498a1e8751d36a7b99e1a54bee6a23815f93")
// App resources 目录（jpackage 的 appResourcesRootDir）：`common/` 里的东西会原样进 deb 的
// `lib/app/resources/`。现在有两份：`sing-box`（内置代理核心）和 `embed-ort`（桌面本地向量：运行库 + 模型）。
val appResources = layout.buildDirectory.dir("app-resources")
// jpackage 会在配置 / 执行时读这个目录，先建出来（里面只有目录，内容由下面两个 stage 任务放）
appResources.get().asFile.resolve("common").mkdirs()
/** 兼容老名字（stageSingBox 里用）。 */
val singBoxResources = appResources
val stageSingBox = tasks.register("stageSingBox") {
    group = "distribution"
    description = "把 sing-box 二进制复制进 App resources（打 deb 用），并校验 sha256"
    val src = file("$singBoxDir/linux-amd64/sing-box")
    val target = singBoxResources.map { it.file("common/sing-box") }
    inputs.file(src)
    inputs.property("sha256", singBoxSha256)
    outputs.file(target)
    doLast {
        check(src.isFile) {
            "找不到 sing-box 二进制：$src（用 -Pfeedhub.singbox.dir=<版本目录> 指定；默认是仓库的 deps/sing-box）"
        }
        val out = target.get().asFile
        out.parentFile.mkdirs()
        src.copyTo(out, overwrite = true)
        val digest = MessageDigest.getInstance("SHA-256")
        out.inputStream().use { input ->
            val buf = ByteArray(1 shl 16)
            while (true) {
                val n = input.read(buf)
                if (n <= 0) break
                digest.update(buf, 0, n)
            }
        }
        val actual = digest.digest().joinToString("") { "%02x".format(it) }
        check(actual == singBoxSha256.lowercase()) {
            "sing-box 的 sha256 不对：期望 $singBoxSha256，实际 $actual（换版本就同时改 -Pfeedhub.singbox.sha256）"
        }
        logger.lifecycle("已放入 sing-box（${out.length() / 1024 / 1024} MB，sha256 校验通过）：$out")
    }
}
// 打 deb 之前先把二进制放好
// prepareAppResources 把 appResourcesRootDir 拷进应用镜像，必须在 stageSingBox 之后（Gradle 9 会把这条隐式依赖判成错误）
tasks.matching { it.name == "packageDeb" || it.name.startsWith("prepareAppResources") }.configureEach { dependsOn(stageSingBox) }

// 桌面本地向量：ONNX Runtime CPU + q4 图
// **不进 git** 的东西有三类，全在这里拼起来：
// 1. `libonnxruntime.so.1.31.0`：ORT 1.31.0 的 linux-x64 运行库（**和安卓 AAR 同一个版本号**）。
//    从 `-Pfeedhub.ort.dir=<目录>`（默认 `deps/onnxruntime-linux-x64`）取，
//    sha256 钉住（换版本 = 同时改 `-Pfeedhub.ort.sha256`）。**为什么不用 ORT 的 JVM 绑定**：这台机器 / 这个仓库
//    的构建是离线的（`./gradlew test --offline`），Gradle 缓存里只有安卓的 AAR，`com.microsoft.onnxruntime:onnxruntime`
//    拿不到；任务红线又不许发在线请求。所以走 C API + 一层 JNI shim（`src/desktopMain/cpp/feedhub_onnx_embed.c`）。
// 2. `libfeedhub_onnx_embed.so`：上面那个 shim，**构建时用 gcc 现编**。头文件从 `onnxruntime-android:1.31.0`
//    AAR 里的 `headers/` 解出来（同一个 release，和运行库的 C API 版本对得上）。
// 3. 模型：和安卓 CPU 路径**同一张图**（`model_q4_acc4.onnx` + `model_q4.onnx_data`），外加共用的 `tokenizer.json`，
//    从 `-Pfeedhub.embedModelDir`（默认 `deps/embeddinggemma`，和 :androidApp 同一个属性）取。
//
// 缺任何一样都**照常构建**（只是这个 App 不带桌面向量：设置页写「未包含模型」，其它功能不受影响）：
// 这台机器是共享的，别人跑 `./gradlew test` 不该因为模型 / gcc 不在就挂。
val desktopEmbedModelDir: String = providers.gradleProperty("feedhub.embedModelDir")
    .getOrElse(rootProject.file("deps/embeddinggemma").path)
val desktopEmbedOrtDir: String = providers.gradleProperty("feedhub.ort.dir")
    .getOrElse(rootProject.file("deps/onnxruntime-linux-x64").path)
val desktopEmbedOrtSha256: String = providers.gradleProperty("feedhub.ort.sha256")
    .getOrElse("21901b1b02233484b80888c16f4f5473742f796bf5528d5d0c4073dbe9802001")
// - `model_q4.onnx_data` 是它的外部权重（174 MB，两个文件必须同目录）；
// - `tokenizer.json` 和安卓 / NPU 共用同一份词表。
val desktopEmbedModelSha256 = mapOf(
    "model_q4_acc4.onnx" to "1afd951bf447f631786cc1302921f94d4bc30f602ee6a61e8e517d0a3203c8a8",
    "model_q4.onnx_data" to "c3975f2d1ab7a1878ae31a7d7a9b7804a827aff3800b60dfceafce21cac3df49",
    "tokenizer.json" to "4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4",
)
// 头文件只借这一份 AAR（不参与编译，就是当 zip 用）
val ortHeaders: Configuration by configurations.creating {
    isCanBeConsumed = false
    isCanBeResolved = true
    isTransitive = false
}
dependencies { add("ortHeaders", libs.onnxruntime.android) }
// 桌面向量的产物目录：Gradle 任务（截图 / devMain / 测试）用 `-Dfeedhub.embed.ort.dir` 指到这儿
val desktopEmbedDir = layout.buildDirectory.dir("desktop-embed")

fun sha256Of(file: File): String {
    val digest = MessageDigest.getInstance("SHA-256")
    file.inputStream().use { input ->
        val buf = ByteArray(1 shl 16)
        while (true) {
            val n = input.read(buf)
            if (n <= 0) break
            digest.update(buf, 0, n)
        }
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
}

val stageDesktopEmbed = tasks.register("stageDesktopEmbed") {
    group = "distribution"
    description = "编 JNI shim + 把 ORT 运行库 / q4 模型拷进 App resources（缺东西就出一份空的，照常构建）"
    val out = desktopEmbedDir
    val resourcesOut = appResources.map { it.dir("common/embed-ort") }
    val cpp = file("src/desktopMain/cpp/feedhub_onnx_embed.c")
    val ortLib = file("$desktopEmbedOrtDir/libonnxruntime.so.1.31.0")
    val modelFiles = desktopEmbedModelSha256.keys.associateWith { name ->
        file(if (name == "tokenizer.json") "$desktopEmbedModelDir/$name" else "$desktopEmbedModelDir/onnx/$name")
    }
    inputs.file(cpp)
    inputs.file(ortLib)
    inputs.files(modelFiles.values)
    inputs.files(ortHeaders)
    inputs.property("ortDir", desktopEmbedOrtDir)
    inputs.property("modelDir", desktopEmbedModelDir)
    inputs.property("sha256", listOf(desktopEmbedOrtSha256) + desktopEmbedModelSha256.values)
    outputs.dir(out)
    outputs.dir(resourcesOut)
    doLast {
        val dir = out.get().asFile.apply { deleteRecursively(); mkdirs() }
        val staged = linkedMapOf<String, File>()
        // 1) 运行库（校验哈希；不对就不带——宁可不带，也别用一份来源不明的库算向量）
        if (ortLib.isFile) {
            val actual = sha256Of(ortLib)
            if (actual == desktopEmbedOrtSha256.lowercase()) {
                staged["libonnxruntime.so.1.31.0"] = ortLib
            } else {
                logger.warn(
                    "桌面本地向量：libonnxruntime 的 sha256 不对（期望 $desktopEmbedOrtSha256，实际 $actual），" +
                        "这次不带桌面向量（换运行库就同时改 -Pfeedhub.ort.sha256）",
                )
            }
        } else {
            logger.warn("桌面本地向量：找不到 $ortLib（用 -Pfeedhub.ort.dir=<目录> 指定），这次不带桌面向量")
        }
        // 2) 模型三件套（同样校验哈希）
        for ((name, file) in modelFiles) {
            val want = desktopEmbedModelSha256.getValue(name)
            if (!file.isFile) {
                logger.warn("桌面本地向量：找不到 $file，这次不带桌面向量")
                continue
            }
            val actual = sha256Of(file)
            if (actual != want) {
                logger.warn("桌面本地向量：$name 的 sha256 不对（期望 $want，实际 $actual），这次不带它")
                continue
            }
            staged[name] = file
        }
        // 3) JNI shim：gcc 现编（头文件从 ORT 的 AAR 里解）
        val headerAar = ortHeaders.files.firstOrNull { it.name.endsWith(".aar") }
        val javaHome = File(System.getProperty("java.home"))
        val gcc = listOf("gcc", "cc").firstOrNull { exe ->
            runCatching { ProcessBuilder(exe, "--version").redirectErrorStream(true).start().waitFor() == 0 }
                .getOrDefault(false)
        }
        if (gcc == null) {
            logger.warn("桌面本地向量：没找到 gcc，编不了 JNI shim，这次不带桌面向量")
        } else if (headerAar == null) {
            logger.warn("桌面本地向量：拿不到 onnxruntime-android 的 AAR（头文件来源），这次不带桌面向量")
        } else {
            val headerDir = temporaryDir.resolve("ort-headers")
            headerDir.deleteRecursively()
            headerDir.mkdirs()
            ZipFile(headerAar).use { zip ->
                zip.entries().asSequence()
                    .filter { it.name.startsWith("headers/") && it.name.endsWith(".h") && !it.isDirectory }
                    .forEach { entry ->
                        val target = File(headerDir, entry.name.removePrefix("headers/"))
                        target.outputStream().use { o -> zip.getInputStream(entry).use { it.copyTo(o) } }
                    }
            }
            // 编到临时目录：下面的拷贝循环会把它放进 build/desktop-embed 和 resources 两处
            // （直接编到 dir 里再拷到 dir 会"自己拷自己"）
            val shim = temporaryDir.resolve("libfeedhub_onnx_embed.so").apply { delete() }
            val cmd = listOf(
                gcc, "-shared", "-fPIC", "-O2", "-o", shim.absolutePath, cpp.absolutePath,
                "-I${javaHome.resolve("include")}", "-I${javaHome.resolve("include/linux")}",
                "-I${headerDir.absolutePath}",
                "-L${file(desktopEmbedOrtDir).absolutePath}", "-l:libonnxruntime.so.1.31.0",
                // 运行时和 shim 在同一个目录（deb 的 lib/app/resources/embed-ort/），按 $ORIGIN 找运行库
                "-Wl,-rpath,\$ORIGIN", "-lpthread",
            )
            val process = ProcessBuilder(cmd).redirectErrorStream(true).start()
            val output = process.inputStream.bufferedReader().readText()
            val code = process.waitFor()
            if (code == 0 && shim.isFile) {
                staged["libfeedhub_onnx_embed.so"] = shim
                logger.lifecycle("桌面本地向量：JNI shim 编好了（${shim.length() / 1024} KB）")
            } else {
                logger.warn("桌面本地向量：JNI shim 编译失败（退出码 $code），这次不带桌面向量：\n$output")
            }
        }
        // 拷到两处：build/desktop-embed（Gradle 任务 / 测试用）和 app resources（deb 用）
        val resDir = resourcesOut.get().asFile.apply { deleteRecursively(); mkdirs() }
        var total = 0L
        for ((name, from) in staged) {
            from.copyTo(File(dir, name), overwrite = true)
            File(dir, name).copyTo(File(resDir, name), overwrite = true)
            total += from.length()
        }
        // 运行库的 SONAME 是 `libonnxruntime.so.1`，shim 的 DT_NEEDED 就是这个（RPATH=$ORIGIN）。App 里是**先按全路径**
        // `System.load` 运行库、再 load shim，所以动态装载器按 soname 命中已装载的那份，**不需要**旁边再有
        // `libonnxruntime.so.1` 这个文件。只在 `build/desktop-embed`（开发 / 测试）里放一个软链：单独 `ldd` shim 时能解析。
        // **不能放进 app resources**：jpackage 拷 resources 时会把软链解成实体文件，deb 里就白多 29 MB 一份运行库。
        // 注意别写 `java.nio…`：脚本里 `java` 是 Gradle 的 JavaPluginExtension，会被它遮住（用文件顶部的 import）。
        if (File(dir, "libonnxruntime.so.1.31.0").isFile) {
            val link = File(dir, "libonnxruntime.so.1")
            link.delete()
            runCatching { Files.createSymbolicLink(link.toPath(), Paths.get("libonnxruntime.so.1.31.0")) }
                .onFailure { logger.warn("桌面本地向量：建 libonnxruntime.so.1 软链失败（${it.message}），靠 soname 命中已装载的那份") }
        }
        if (staged.isEmpty()) {
            logger.warn("桌面本地向量：这次什么都不带（设置页会写「未包含模型」）")
        } else {
            logger.lifecycle(
                "桌面本地向量：${staged.size} 个文件，共 ${total / 1024 / 1024} MB → $resDir" +
                    if (staged.size < 5) "（缺 ${5 - staged.size} 个，App 里本地向量不可用）" else "",
            )
        }
    }
}
// 打 deb 之前先编好 / 拷好（顺序同 sing-box：prepareAppResources 要读到它）
tasks.matching { it.name == "packageDeb" || it.name.startsWith("prepareAppResources") }
    .configureEach { dependsOn(stageDesktopEmbed) }

// 桌面本地向量的产物在 build/desktop-embed：测试 / 截图 / 调试入口用系统属性指过去
// （`DesktopEmbedRuntime.allInOneDir()` 第一个就查它）。找不到那 5 个文件也照跑：
// 依赖它的测试自己 skip，截图里就显示「未包含模型」。
val desktopEmbedPath = desktopEmbedDir.map { it.asFile.absolutePath }
tasks.withType<Test>().configureEach {
    dependsOn(stageDesktopEmbed)
    systemProperty("feedhub.embed.ort.dir", desktopEmbedPath.get())
}
tasks.matching { it.name == "screenshot" || it.name == "devMain" }.configureEach {
    dependsOn(stageDesktopEmbed)
    this as JavaExec
    systemProperty("feedhub.embed.ort.dir", desktopEmbedPath.get())
}

// 本地翻译的默认模型 / llama-server（桌面）：`-Pfeedhub.translate.model=` / `-Pfeedhub.translate.server=`，
// 默认找仓库 `deps/translate/` 下的那两份。文件在才写进启动参数（`DEFAULT_TRANSLATION_MODEL` 读它），
// 不在就不写：设置页里路径是空的，用户自己选。
val desktopTranslateModel: String = providers.gradleProperty("feedhub.translate.model")
    .getOrElse(rootProject.file("deps/translate/HY-MT1.5-1.8B-Q4_K_M.gguf").path)
val desktopTranslateServer: String = providers.gradleProperty("feedhub.translate.server")
    .getOrElse(rootProject.file("deps/translate/llama.cpp/llama-server").path)

compose.desktop {
    application {
        mainClass = "feedhub.app.MainKt"
        jvmArgs += listOfNotNull(
            desktopTranslateModel.takeIf { File(it).isFile }?.let { "-Dfeedhub.translate.model=$it" },
            desktopTranslateServer.takeIf { File(it).isFile }?.let { "-Dfeedhub.translate.server=$it" },
        )
        // JCEF 的 OSR（离屏渲染）在 JDK 16+ 要打开这几个包，见 jcefmaven README。
        // 打进 deb 的启动器也会带这些 --add-exports。
        jvmArgs += listOf(
            "-Dfile.encoding=UTF-8",
            "--add-exports", "java.base/java.lang=ALL-UNNAMED",
            "--add-exports", "java.desktop/sun.awt=ALL-UNNAMED",
            "--add-exports", "java.desktop/sun.java2d=ALL-UNNAMED",
        )
        nativeDistributions {
            targetFormats(TargetFormat.Deb)
            packageName = "feedhub"
            packageVersion = "0.1.0"
            description = "多平台聚合信息流"
            vendor = "feedhub"
            // jpackage 默认只打 java.base 等少数模块；OkHttp / Coil 要用到这些
            // jdk.httpserver：GStreamer 引擎在本机回环上提供 MPD 清单（MpdServer）
            modules("java.net.http", "java.sql", "jdk.unsupported", "java.naming", "jdk.httpserver")
            // App 自带的资源目录：里面的 common/ 会进 deb 的 lib/app/resources（sing-box 二进制就放这儿，见 stageSingBox）
            appResourcesRootDir.set(appResources)
            linux {
                shortcut = true
                menuGroup = "Network"
            }
        }
    }
}

// jlink 出来的运行时留下 bin/java（插件默认带 --strip-native-commands）：jpackage 的 Linux 启动器每次启动都跑两遍
// `dpkg -S <启动器路径>` 找自己属于哪个包（这台机器一次 ≈170 ms），所以 App（bin/feedhub-launch）和验证码子进程
// （CaptchaHelperLauncher）都直接用 bin/java 起 JVM，不经过它。
// 这个开关是插件的 internal 属性（Kotlin 编出来的 getter 叫 getStripNativeCommands$compose），DSL 里没有，只能反射设。
tasks.withType<org.jetbrains.compose.desktop.application.tasks.AbstractJLinkTask>().configureEach {
    @Suppress("UNCHECKED_CAST")
    val strip = javaClass.getMethod("getStripNativeCommands\$compose").invoke(this) as Property<Boolean>
    strip.set(false)
}

// jpackage 生成的 Depends 只有它自己扫到的系统库，不知道播放器运行时要加载 GStreamer / libvlc（JNA 动态加载），
// Compose 的 DSL 也没有加依赖的选项，所以在打好的 .deb 上补：解开、改 DEBIAN/control、按 root 属主重新打包。
// Depends：GStreamer（首选播放引擎：libgstreamer 本体 + base / good / bad / libav 插件——bad 里有 va 硬解和 dashdemux2，good 里有 souphttpsrc / qtdemux / scaletempo）；
// JCEF / CEF（内嵌 Chromium，只给验证码和以后的网页登录）运行时要加载 GTK3 / NSS / X11 / GBM；
// 原生 CEF 包不在 deb 里（第一次运行下载到 ~/.cache/feedhub/jcef），这里只声明系统库。
// Recommends：VLC（GStreamer 起不来时的退路）和 VA-API 驱动（没有就只能软解）。
val debExtraDepends = listOf(
    "libgstreamer1.0-0", "gstreamer1.0-plugins-base", "gstreamer1.0-plugins-good", "gstreamer1.0-plugins-bad", "gstreamer1.0-libav",
    "libgtk-3-0t64", "libnss3", "libnspr4", "libatk1.0-0t64", "libatk-bridge2.0-0t64",
    "libcups2t64", "libgbm1", "libdrm2", "libxkbcommon0", "libxcomposite1", "libxdamage1",
    "libxrandr2", "libx11-xcb1", "libxshmfence1", "libpango-1.0-0", "libcairo2",
)
val debRecommends = listOf("libvlc5", "vlc-plugin-base", "mesa-va-drivers | intel-media-va-driver")
tasks.matching { it.name == "packageDeb" }.configureEach {
    inputs.file("packaging/feedhub-launch.sh")
    inputs.dir(rootDir.resolve("skill/feedhub")).withPropertyName("skill")
    doLast {
        val deb = layout.buildDirectory.dir("compose/binaries/main/deb").get().asFile
            .listFiles { f -> f.name.endsWith(".deb") }!!.single()
        val root = temporaryDir.resolve("deb-root").apply { deleteRecursively() }
        fun run(vararg cmd: String) {
            val p = ProcessBuilder(*cmd).inheritIO().start()
            check(p.waitFor() == 0) { "命令失败：${cmd.joinToString(" ")}" }
        }
        run("dpkg-deb", "-R", deb.path, root.path)
        val control = root.resolve("DEBIAN/control")
        val lines = mutableListOf<String>()
        var hasRecommends = false
        for (line in control.readLines()) {
            when {
                line.startsWith("Depends:") -> lines += line.trimEnd() + ", " + debExtraDepends.joinToString(", ")
                line.startsWith("Recommends:") -> { hasRecommends = true; lines += line.trimEnd() + ", " + debRecommends.joinToString(", ") }
                else -> lines += line
            }
        }
        if (!hasRecommends) {
            // control 里没有 Recommends 行：加在 Depends 后面（Depends 多半是单行；jpackage 没写 Depends 就加在末尾）
            val at = lines.indexOfFirst { it.startsWith("Depends:") }
            lines.add(if (at >= 0) at + 1 else lines.size, "Recommends: " + debRecommends.joinToString(", "))
        }
        control.writeText(lines.joinToString("\n", postfix = "\n"))
        // 启动性能：
        // 0) 运行时的 bin 只留 java（jlink 不去 native 命令时还会带 keytool / jwebserver 之类，用不上）。
        // 1) jlink 出来的运行时没有 JDK 自带的 CDS 基础归档（lib/server/classes.jsa），连 JDK 自己的类都要现解析；
        //    用这份运行时自己的 bin/java -Xshare:dump 生成一份（和 JDK 自带的一样：不带类路径，任何类路径都能用）。
        //    归档和 libjvm / lib/modules 绑定，不绑安装路径。动态 AppCDS（启动器脚本里的 AutoCreateSharedArchive）也要有它才能用。
        // 2) 装上启动器脚本 bin/feedhub-launch（直接用 bin/java 起 App + MALLOC_ARENA_MAX + 用户缓存里的 AppCDS 归档），菜单项改指向它。
        val opt = root.resolve("opt/feedhub")
        val java = opt.resolve("lib/runtime/bin/java")
        check(java.canExecute()) { "运行时里没有 bin/java（jlink 的 --strip-native-commands 没关掉？）" }
        opt.resolve("lib/runtime/bin").listFiles()!!.filter { it.name != "java" }.forEach { it.delete() }
        val dump = ProcessBuilder(java.path, "-Xshare:dump").redirectErrorStream(true)
        val dp = dump.start()
        val dumpLog = dp.inputStream.bufferedReader().readText()
        check(dp.waitFor() == 0 && opt.resolve("lib/runtime/lib/server/classes.jsa").isFile) { "生成 CDS 基础归档失败：\n$dumpLog" }
        val launch = opt.resolve("bin/feedhub-launch")
        file("packaging/feedhub-launch.sh").copyTo(launch, overwrite = true)
        launch.setExecutable(true, false)
        // 给 agent 的说明书（skill）：仓库的 skill/feedhub/ 整个装到 /opt/feedhub/skill/feedhub/
        // （进阶设置 →「对外接口（agent）」那一页显示这个路径）。**deb 里没有 feedhub-cli**：
        // 对外接口是 App 自己的一部分，不需要任何独立程序。
        val skillSrc = rootDir.resolve("skill/feedhub")
        check(skillSrc.resolve("SKILL.md").isFile) { "找不到说明书：$skillSrc/SKILL.md" }
        val skillDst = opt.resolve("skill/feedhub").apply { mkdirs() }
        skillSrc.copyRecursively(skillDst, overwrite = true)
        check(skillDst.resolve("SKILL.md").isFile) { "说明书没装进 deb" }
        // 也不能反过来把 cli 的 jar 带进去
        check(opt.resolve("lib/app").listFiles()!!.none { it.name.startsWith("cli") }) {
            "deb 的 lib/app 里还有 cli 的 jar（:app 的 desktopMain 不该再 runtimeOnly(project(\":cli\"))）"
        }
        val desktop = opt.resolve("lib/feedhub-feedhub.desktop")
        desktop.writeText(desktop.readText().replace(Regex("(?m)^Exec=/opt/feedhub/bin/feedhub$"), "Exec=/opt/feedhub/bin/feedhub-launch"))
        check(desktop.readText().contains("Exec=/opt/feedhub/bin/feedhub-launch")) { "菜单项的 Exec 没改成启动器脚本" }
        run("dpkg-deb", "--root-owner-group", "-b", root.path, deb.path)
        root.deleteRecursively()
    }
}

// 截图工具：不开窗口，把界面画到 PNG（agent 自己验证界面用），见 desktopMain 的 feedhub.app.dev.Screenshot。
// 用法：./gradlew :app:screenshot -Pargs="home build/shot.png 1280 800 8"
tasks.register<JavaExec>("screenshot") {
    group = "application"
    description = "把某个界面画成 PNG：参数 <界面> <输出文件> [宽] [高] [等待秒数]"
    val desktopMain = kotlin.jvm("desktop").compilations.getByName("main")
    dependsOn(desktopMain.compileTaskProvider)
    classpath = files(desktopMain.output.allOutputs, desktopMain.runtimeDependencyFiles, pluginDemos)
    mainClass.set("feedhub.app.dev.ScreenshotKt")
    jvmArgs("-Djava.awt.headless=true", "-Dfile.encoding=UTF-8")
    workingDir = rootDir
    args((project.findProperty("args") as String?)?.split(" ")?.filter { it.isNotBlank() } ?: emptyList<String>())
}

// 播放器验证（不依赖 Compose）：选流、用 VLC 播几秒、把一帧存成 PNG，见 desktopMain 的 feedhub.app.dev.PlayerProbe。
tasks.register<JavaExec>("playerProbe") {
    group = "application"
    description = "用 VLC 播一个 B站视频几秒，打印状态和 CPU，存一帧 PNG"
    val desktopMain = kotlin.jvm("desktop").compilations.getByName("main")
    dependsOn(desktopMain.compileTaskProvider)
    classpath = files(desktopMain.output.allOutputs, desktopMain.runtimeDependencyFiles, pluginDemos)
    mainClass.set("feedhub.app.dev.PlayerProbeKt")
    jvmArgs("-Djava.awt.headless=true", "-Dfile.encoding=UTF-8")
    workingDir = rootDir
    args((project.findProperty("args") as String?)?.split(" ")?.filter { it.isNotBlank() } ?: emptyList<String>())
}

// 跑 desktopMain 里的某个调试入口（feedhub.app.dev 下的检查工具）：./gradlew :app:devMain -Pmain=<类名>
tasks.register<JavaExec>("devMain") {
    group = "application"
    description = "跑 desktopMain 里的调试入口：-Pmain=<主类> [-Pargs=\"...\"]"
    val desktopMain = kotlin.jvm("desktop").compilations.getByName("main")
    dependsOn(desktopMain.compileTaskProvider)
    classpath = files(desktopMain.output.allOutputs, desktopMain.runtimeDependencyFiles, pluginDemos)
    mainClass.set(providers.gradleProperty("main"))
    jvmArgs("-Djava.awt.headless=true", "-Dfile.encoding=UTF-8")
    workingDir = rootDir
    args((project.findProperty("args") as String?)?.split(" ")?.filter { it.isNotBlank() } ?: emptyList<String>())
}

// 性能摸底（feedhub.app.dev.PerfProbe）要自己起 java（带 AWT 的 JDK、自定 JVM 参数 / 探针 / 环境变量），
// 这里只把调试入口的类路径写到 build/dev-classpath.txt：`java -cp @app/build/dev-classpath.txt …`
tasks.register("devClasspath") {
    group = "application"
    description = "把调试入口（desktopMain + 演示环境）的运行时类路径写到 build/dev-classpath.txt"
    val desktopMain = kotlin.jvm("desktop").compilations.getByName("main")
    dependsOn(desktopMain.compileTaskProvider)
    val cp = files(desktopMain.output.allOutputs, desktopMain.runtimeDependencyFiles, pluginDemos)
    val out = layout.buildDirectory.file("dev-classpath.txt")
    inputs.files(cp)
    outputs.file(out)
    doLast { out.get().asFile.writeText(cp.files.joinToString(File.pathSeparator) { "\"${it.absolutePath}\"" }) }
}

// JCEF 验证：用内嵌 Chromium 打开本地验证码页，确认这台 Linux 上 OSR 能显示易盾滑块。
// **会开窗口**（和 :app:run 不一样，这个任务就是给人看的）；第一次运行会下载 CEF 原生包到
// ~/.cache/feedhub/jcef（约 100MB），之后复用。
tasks.register<JavaExec>("jcefProbe") {
    group = "application"
    description = "用 JCEF 打开本地验证码页（会开窗口；首次下载 CEF 内核）"
    val desktopMain = kotlin.jvm("desktop").compilations.getByName("main")
    dependsOn(desktopMain.compileTaskProvider)
    classpath = files(desktopMain.output.allOutputs, desktopMain.runtimeDependencyFiles, pluginDemos)
    // 这台机器装的是 headless JDK，没有 libjawt.so；用 deb/app image 里的 jlink runtime 跑：
    // -Pfeedhub.jcef.java=<...>/lib/runtime/bin/java。不传就用当前 Gradle 的 java。
    providers.gradleProperty("feedhub.jcef.java").orNull?.let { setExecutable(it) }
    mainClass.set("feedhub.app.dev.JcefProbeKt")
    // 这台机器是 Wayland 会话，但 Java/CEF 走 XWayland；CEF 检测到 WAYLAND_DISPLAY 会去连 Wayland，
    // 原生浏览器进程直接 ContentMainRun exit 28。probe 里先强制 X11。
    environment.remove("WAYLAND_DISPLAY")
    environment["GDK_BACKEND"] = "x11"
    providers.gradleProperty("feedhub.jcef.osr").orNull?.let { environment["JCEF_OSR"] = it }
    providers.gradleProperty("feedhub.jcef.single").orNull?.let { environment["JCEF_SINGLE"] = it }
    providers.gradleProperty("feedhub.jcef.initonly").orNull?.let { environment["JCEF_INIT_ONLY"] = it }
    providers.gradleProperty("feedhub.jcef.dir").orNull?.let { environment["JCEF_DIR"] = it }
    jvmArgs(
        "-Dfile.encoding=UTF-8",
        "--add-exports", "java.base/java.lang=ALL-UNNAMED",
        "--add-exports", "java.desktop/sun.awt=ALL-UNNAMED",
        "--add-exports", "java.desktop/sun.java2d=ALL-UNNAMED",
    )
    workingDir = rootDir
    args((project.findProperty("args") as String?)?.split(" ")?.filter { it.isNotBlank() } ?: emptyList<String>())
}
