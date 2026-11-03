import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinJvmProjectExtension

plugins {
    alias(libs.plugins.kotlin.jvm) apply false
    alias(libs.plugins.kotlin.serialization) apply false
    alias(libs.plugins.kotlin.multiplatform) apply false
    alias(libs.plugins.compose.compiler) apply false
    alias(libs.plugins.compose) apply false
    // 安卓：:app 的安卓侧（Android-KMP 库）和 :androidApp 这个 APK 外壳。由各模块自己 apply。
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.android.kmp.library) apply false
    // Baseline Profile 生成器：com.android.test 模块；
    // 没用 androidx.baselineprofile 插件（1.4.1 / 1.5.0 在 AGP 9.4 上 apply 就崩，见那个模块的 build 文件）
    alias(libs.plugins.android.test) apply false
}

// 后端模块都是纯 JVM 库，之后要被 Android app 模块直接依赖，所以字节码目标定在 17。
subprojects {
    plugins.withId("org.jetbrains.kotlin.jvm") {
        extensions.configure<JavaPluginExtension> {
            sourceCompatibility = JavaVersion.VERSION_17
            targetCompatibility = JavaVersion.VERSION_17
        }
        extensions.configure<KotlinJvmProjectExtension> {
            compilerOptions.jvmTarget.set(JvmTarget.JVM_17)
        }
        val testTask = tasks.named<Test>("test") {
            useJUnitPlatform {
                // 在线冒烟测试会用真实 Cookie 打真实接口，默认不跑；要跑时用 ./gradlew liveTest
                excludeTags("live")
            }
            offlineGuard()
        }
        // 插件的演示环境（截图工具的 fake=，见 core 的 PlatformDemo）：src/demo/ 单独一个源码集，能看见 main 的 internal，
        // 打成 demoJar 给 :app 的截图 / 调试任务用，**不进** main 的产物（所以不进 App 发布包和 APK）
        if (file("src/demo").isDirectory) pluginDemoSourceSet()
        tasks.register<Test>("liveTest") {
            description = "带本机 Cookie 打真实接口的冒烟测试（会产生真实请求）"
            group = "verification"
            testClassesDirs = testTask.get().testClassesDirs
            classpath = testTask.get().classpath
            useJUnitPlatform { includeTags("live") }
            testLogging { events("passed", "skipped", "failed"); showStandardStreams = true }
            outputs.upToDateWhen { false }
        }
    }
}

/**
 * 离线测试的"出网闸"：JVM 默认的代理选择指向一个必然连不上的本机 SOCKS 端口（9 = discard，没人监听），
 * 回环地址照常直连（MockWebServer、本机中转、两个 sing-box 实例都在 127.0.0.1）。
 * 走默认 ProxySelector 的请求（OkHttp 默认、ProxyMode.System、HttpURLConnection）想连真实主机会立刻失败，
 * 显式 Proxy.NO_PROXY 的客户端拦不住，那类要靠代码审查。liveTest 不挂这道闸。
 */
fun Test.offlineGuard() {
    systemProperty("socksProxyHost", "127.0.0.1")
    systemProperty("socksProxyPort", "9")
    systemProperty("socksNonProxyHosts", "localhost|127.*|[::1]|0.0.0.0|[::0]")
    systemProperty("java.net.useSystemProxies", "false")
    // **Rust 联网层的那道闸**：上面那几行改的是 **JVM 的系统代理**，
    // OkHttp 走 `ProxySelector` 的请求因此连不出去；但 `feedhub-net` **不读系统代理**
    // （`wreq` 的 `auto_sys_proxy` 被显式 `no_proxy()` 关掉了——那会和 `feedhub-core/src/proxy.rs`
    // 的判定打架、也会破坏 fail-closed），所以那道闸对 Rust 路径**完全无效**。
    //
    // 踩到过：`CoolapkRequestEquivalenceTest` 的"播放"用例用 `base = CoolapkSource.API`
    // （真实 `api.coolapk.com`）+ 一个 OkHttp **Replay 拦截器**假造响应。切到 Rust 之后拦截器
    // 不再生效，Rust 真的连上了 `api2.coolapk.com`、服务器回了 `err_request_expired`（**真实响应**）。
    // 这一行让 `feedhub-net` 自己只放行回环（见 `client.rs` 的 `offline_blocked`）。
    environment("FEEDHUB_OFFLINE", "1")
}

/**
 * 平台插件的 `demo` 源码集（`src/demo/kotlin` + `src/demo/resources`）：依赖跟 main 一样，能用 main 的 internal。
 * 产物是 `demoJar`，通过 `demoElements` 这个配置给 `:app` 的截图 / 调试任务（`app/build.gradle.kts` 的 `pluginDemos`）。
 */
fun Project.pluginDemoSourceSet() {
    val sourceSets = extensions.getByType<SourceSetContainer>()
    val main = sourceSets.getByName("main")
    val demo = sourceSets.create("demo") {
        compileClasspath += main.output
        runtimeClasspath += main.output
    }
    configurations.getByName("demoImplementation").extendsFrom(configurations.getByName("implementation"))
    configurations.getByName("demoRuntimeOnly").extendsFrom(configurations.getByName("runtimeOnly"))
    extensions.configure<KotlinJvmProjectExtension> {
        target.compilations.getByName("demo").associateWith(target.compilations.getByName("main"))
    }
    val demoJar = tasks.register<Jar>("demoJar") {
        archiveClassifier.set("demo")
        from(demo.output)
    }
    configurations.create("demoElements") {
        isCanBeConsumed = true
        isCanBeResolved = false
        outgoing.artifact(demoJar)
    }
}
