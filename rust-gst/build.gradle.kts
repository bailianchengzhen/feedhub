import java.security.MessageDigest

plugins {
    alias(libs.plugins.kotlin.jvm)
}

/**
 * 桌面播放引擎（GStreamer 1.26 + VA-API 硬解）的独立 UniFFI 绑定模块。
 *
 * 只有 `:app` 的 `desktopMain` 依赖本模块，安卓 APK 和 dex 完全不碰 `libfeedhub_gst.so` 与 `RustGstPlayer`。
 */
dependencies {
    api(project(":core"))
    implementation(project(":rust-bridge"))
    compileOnly(libs.jna)
}

val rustDir = rootProject.file("rust")
val gstLib = rustDir.resolve("target/release/libfeedhub_gst.so")
val bindgenBin = rustDir.resolve("target/release/uniffi-bindgen")
val generatedDir = layout.buildDirectory.dir("generated/uniffi")
val nativeResourceDir = layout.buildDirectory.dir("rust-native")

kotlin.sourceSets.getByName("main").kotlin.srcDir(generatedDir)

val gstPrefix: String = providers.gradleProperty("feedhub.gstPrefix")
    .getOrElse(rootProject.file("deps/gst-prefix").path)

fun pkgConfigDirsOf(prefix: File): List<File> =
    listOf(prefix.resolve("usr/lib/x86_64-linux-gnu/pkgconfig"), prefix.resolve("usr/lib/pkgconfig"))
        .filter { it.resolve("gstreamer-1.0.pc").isFile }

fun systemHasGStreamer(): Boolean = runCatching {
    val p = ProcessBuilder("pkg-config", "--exists", "gstreamer-1.0")
        .redirectErrorStream(true)
        .start()
    p.waitFor() == 0
}.getOrElse { false }

fun resolveGstPkgConfig(): Pair<File, List<File>>? {
    if (systemHasGStreamer()) return null
    val prefix = File(gstPrefix)
    val dirs = pkgConfigDirsOf(prefix)
    if (dirs.isEmpty()) {
        throw GradleException(
            "找不到 GStreamer 的开发头文件（pkg-config 里没有 gstreamer-1.0）。桌面播放引擎（gstreamer-rs）" +
                "要编它，二选一：\n" +
                "  1. 装开发包：sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev " +
                "libgstreamer-plugins-bad1.0-dev；\n" +
                "  2. 或者把离线前缀同步到 $gstPrefix，或用 -Pfeedhub.gstPrefix=<你的前缀> 指过去。",
        )
    }
    return prefix to dirs
}

val cargoBuildGst = tasks.register<Exec>("cargoBuildGst") {
    group = "rust"
    description = "编桌面的 libfeedhub_gst.so（release）"
    // 复用 :rust-bridge:cargoBuildHost 编好的 uniffi-bindgen，并串行化同一 workspace 的 cargo 构建
    dependsOn(":rust-bridge:cargoBuildHost")
    workingDir = rustDir
    commandLine("cargo", "build", "--release", "--locked", "-j", "4", "-p", "feedhub-gst")
    environment("CARGO_TERM_COLOR", "never")
    doFirst {
        val found = resolveGstPkgConfig() ?: return@doFirst
        val (prefix, dirs) = found
        environment(
            "PKG_CONFIG_PATH",
            (dirs + listOfNotNull(System.getenv("PKG_CONFIG_PATH"))).joinToString(File.pathSeparator),
        )
        environment("PKG_CONFIG_SYSROOT_DIR", System.getenv("PKG_CONFIG_SYSROOT_DIR") ?: prefix.absolutePath)
        logger.lifecycle("[rust-gst] pkg-config 里没有 gstreamer-1.0，改用离线前缀 $prefix")
    }
    inputs.files(
        fileTree(rustDir.resolve("crates")) {
            include("feedhub-core/**/*.rs", "feedhub-core/**/Cargo.toml")
            include("feedhub-gst/**/*.rs", "feedhub-gst/**/Cargo.toml")
        },
        rustDir.resolve("Cargo.toml"),
        rustDir.resolve("Cargo.lock"),
    )
    outputs.file(gstLib)
    outputs.upToDateWhen { false }
}

val generateGstBindings = tasks.register<Exec>("generateGstBindings") {
    group = "rust"
    description = "用 uniffi-bindgen 从 libfeedhub_gst.so 生成 Kotlin 绑定（uniffi.feedhub_gst）"
    dependsOn(cargoBuildGst, ":rust-bridge:cargoBuildHost")
    workingDir = rustDir
    val out = generatedDir.get().asFile
    doFirst {
        out.deleteRecursively()
        out.mkdirs()
    }
    commandLine(
        bindgenBin.absolutePath,
        "generate",
        "--library", gstLib.absolutePath,
        "--language", "kotlin",
        "--out-dir", out.absolutePath,
        "--no-format",
    )
    inputs.files(gstLib, bindgenBin)
    outputs.dir(generatedDir)
}

val stageGstLib = tasks.register("stageGstLib") {
    group = "rust"
    description = "把桌面的 libfeedhub_gst.so 与 sha256 戳记放进 jar 的资源（feedhub-native/host/）"
    dependsOn(cargoBuildGst)
    inputs.file(gstLib)
    outputs.dir(nativeResourceDir)
    doLast {
        val dir = nativeResourceDir.get().asFile.resolve("feedhub-native/host")
        dir.mkdirs()
        val target = dir.resolve("libfeedhub_gst.so")
        gstLib.copyTo(target, overwrite = true)
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
        dir.resolve("libfeedhub_gst.so.sha256").writeText("$hex ${target.length()}\n")
    }
}

sourceSets.getByName("main").resources.srcDir(nativeResourceDir)

tasks.named("compileKotlin") { dependsOn(generateGstBindings) }
tasks.named("processResources") { dependsOn(stageGstLib) }
