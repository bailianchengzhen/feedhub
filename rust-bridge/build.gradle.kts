import java.security.MessageDigest

plugins {
    alias(libs.plugins.kotlin.jvm)
    // `core/sync` 那几份 @Serializable 的报文模型（SyncPackage / PairingMessage / LibrarySnapshot）跟着搬进来了，
    // 它们本来在 :core（那边有这个插件）。
    alias(libs.plugins.kotlin.serialization)
}

/**
 * Rust 核心的 Kotlin 侧：UniFFI 生成的绑定 + 手写的适配层（[feedhub.rust.RustBili]）。
 *
 * **网络不在这里**：这个模块只含纯函数（解析 / 签名 / AV-BV），没有 OkHttp、没有 Cookie、没有代理。
 *
 * 构建顺序：
 * 1. `cargoBuildHost`：在 `rust/` 里 `cargo build --release`（桌面 .so + `uniffi-bindgen` 二进制）；
 * 2. `generateRustBindings`：用 `--library` 模式从编好的 .so 里读元数据，生成 Kotlin 绑定到 build 目录
 *    （**不提交生成物**：它必须和 .so 是同一份构建，提交了反而会漂移）；
 * 3. `stageHostLib`：把桌面的 `libfeedhub_ffi.so` 放进 jar 的 `feedhub-native/host/`（运行时解压到缓存目录再加载）。
 *
 * 没有 cargo 时**构建直接失败并说清楚怎么办**（不是静默跳过）：见 `checkRustToolchain`。
 * 选"报错"而不是"跳过开关"的理由：跳过之后 `./gradlew test` 会绿，但跑的是"没有 Rust"的另一套东西，
 * 而这个仓库的验收门恰恰是"Kotlin 和 Rust 两边逐字段相等"——静默跳过等于把门拆了。
 *
 * 桌面 GStreamer 播放引擎已拆至独立的 `:rust-gst`（`feedhub-gst` crate → `libfeedhub_gst.so`），
 * 因此这里编出的 `libfeedhub_ffi.so` 与安卓交叉编译出的 `libfeedhub_ffi.so` 拥有完全一致的 `uniffi_*` 符号集。
 */
dependencies {
    api(project(":core"))
    // JNA 由使用者提供：桌面已经有（vlcj 带 jna-jpms），安卓用 jna 的 aar（:androidApp）。
    // 这里只编译期需要（生成的绑定 import com.sun.jna.*）。
    compileOnly(libs.jna)
    testImplementation(libs.jna)
    testImplementation(kotlin("test"))
    // 对拍（`BrainParityTest` / `Sync*DualRunTest` / `ProxyCoreDualRunTest`）的参照实现：Kotlin 原版在
    // :core 的 testFixtures 里（`feedhub.brain.legacy` / `feedhub.core.sync.legacy` / `feedhub.core.proxy.legacy`）。
    testImplementation(testFixtures(project(":core")))
    // 取订阅的**请求等价**（MockWebServer）：`ProxyCoreSubscriptionTest` 用它当订阅源。`:core` 已经把
    // okhttp 作为 `api` 传上来了，这里只额外要 mockwebserver。
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.launcher)
}

// 内置代理核心端到端测试要真的起 sing-box（80 MB，不进仓库）。路径用 gradle 属性给、可以覆盖：
// `-Pfeedhub.singbox=/path/to/sing-box`（和 `:core` 那条一样的口径）。二进制不在时测试 assumeTrue 跳过。
val singBoxBinary: String = providers.gradleProperty("feedhub.singbox")
    .getOrElse(rootProject.file("deps/sing-box/linux-amd64/sing-box").path)
tasks.withType<Test>().configureEach {
    systemProperty("feedhub.singbox", singBoxBinary)
}

val rustDir = rootProject.file("rust")
val hostLib = rustDir.resolve("target/release/libfeedhub_ffi.so")
val bindgenBin = rustDir.resolve("target/release/uniffi-bindgen")
val generatedDir = layout.buildDirectory.dir("generated/uniffi")
val nativeResourceDir = layout.buildDirectory.dir("rust-native")

// 生成的 Kotlin 绑定进 main 源码集（build 目录里，不提交）
kotlin.sourceSets.getByName("main").kotlin.srcDir(generatedDir)

/** 没有 cargo 时给一句能照做的话，而不是 "Process 'command cargo' finished with non-zero exit value 127"。 */
val checkRustToolchain = tasks.register("checkRustToolchain") {
    group = "rust"
    description = "检查 cargo / rustc 在不在（不在就给出清楚的报错）"
    doLast {
        val out = try {
            val p = ProcessBuilder("cargo", "--version").redirectErrorStream(true).start()
            val text = p.inputStream.bufferedReader().use { it.readText() }.trim()
            val code = p.waitFor()
            if (code != 0) throw GradleException(
                "cargo 不可用（退出码 $code）：$text\n" +
                    "装 Rust 工具链：https://rustup.rs ，然后 cargo --version 能跑再构建。",
            )
            text
        } catch (e: java.io.IOException) {
            throw GradleException(
                "找不到 cargo（构建 Rust 核心需要它）：${e.message}\n" +
                    "装 Rust 工具链：https://rustup.rs （或把 CARGO 放进 PATH）。\n" +
                    "不想编 Rust 时没法跳过：:core 的 Kotlin 适配层引用了生成的绑定；" +
                    "这一期用\"构建直接报错\"代替\"静默跳过\"，见 rust-bridge/build.gradle.kts 的说明。",
            )
        }
        logger.lifecycle("Rust 工具链：$out")
    }
}

val cargoBuildHost = tasks.register<Exec>("cargoBuildHost") {
    group = "rust"
    description = "编桌面的 libfeedhub_ffi.so 和 uniffi-bindgen（release）"
    dependsOn(checkRustToolchain)
    workingDir = rustDir
    commandLine("cargo", "build", "--release", "--locked", "-j", "4", "-p", "feedhub-ffi", "-p", "uniffi-bindgen")
    environment("CARGO_TERM_COLOR", "never")
    inputs.files(
        fileTree(rustDir.resolve("crates")) {
            include("feedhub-core/**/*.rs", "feedhub-core/**/Cargo.toml")
            include("feedhub-net/**/*.rs", "feedhub-net/**/Cargo.toml")
            include("feedhub-ffi/**/*.rs", "feedhub-ffi/**/Cargo.toml")
            include("uniffi-bindgen/**/*.rs", "uniffi-bindgen/**/Cargo.toml")
        },
        rustDir.resolve("Cargo.toml"),
        rustDir.resolve("Cargo.lock"),
    )
    outputs.files(hostLib, bindgenBin)
    // cargo 自己会做增量，别让 Gradle 的 up-to-date 检查把"改了 .rs 但没改这几个文件"漏掉
    outputs.upToDateWhen { false }
}

val generateRustBindings = tasks.register<Exec>("generateRustBindings") {
    group = "rust"
    description = "用 uniffi-bindgen 从 .so 生成 Kotlin 绑定（--library 模式）"
    dependsOn(cargoBuildHost)
    // uniffi-bindgen 的 --library 模式会跑 `cargo metadata` 解析 crate 名字，必须在 rust/ 里跑
    workingDir = rustDir
    val out = generatedDir.get().asFile
    doFirst {
        out.deleteRecursively()
        out.mkdirs()
    }
    commandLine(
        bindgenBin.absolutePath,
        "generate",
        "--library", hostLib.absolutePath,
        "--language", "kotlin",
        "--out-dir", out.absolutePath,
        "--no-format",
    )
    inputs.files(hostLib, bindgenBin)
    outputs.dir(generatedDir)
}

val stageHostLib = tasks.register("stageHostLib") {
    group = "rust"
    description = "把桌面的 .so 与 sha256 戳记放进 jar 的资源（feedhub-native/host/），运行时免哈希快速校验"
    dependsOn(cargoBuildHost)
    inputs.file(hostLib)
    outputs.dir(nativeResourceDir)
    doLast {
        val dir = nativeResourceDir.get().asFile.resolve("feedhub-native/host")
        dir.mkdirs()
        val target = dir.resolve("libfeedhub_ffi.so")
        hostLib.copyTo(target, overwrite = true)
        val digest = MessageDigest.getInstance("SHA-256")
        target.inputStream().use { input ->
            val buf = ByteArray(1 shl 16)
            while (true) {
                val n = input.read(buf)
                if (n <= 0) break
                digest.update(buf, 0, n)
            }
        }
        val hex = digest.digest().joinToString("") { "%02x".format(it) }
        dir.resolve("libfeedhub_ffi.so.sha256").writeText("$hex ${target.length()}\n")
    }
}

sourceSets.getByName("main").resources.srcDir(nativeResourceDir)

tasks.named("compileKotlin") { dependsOn(generateRustBindings) }
tasks.named("processResources") { dependsOn(stageHostLib) }
// 测试也要能加载 .so（jar 资源里有就够；这里显式挂上，别依赖任务顺序的巧合）
tasks.named("test") { dependsOn(stageHostLib) }
