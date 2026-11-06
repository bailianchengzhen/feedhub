import java.security.MessageDigest

/**
 * 可选 AI 能力编译开关（默认全部 `false` = **纯净基础版**：无 NPU / ONNX 向量模型、无 GGUF / llama-server）：
 * - `-Pfeedhub.embed=true`：打包 CPU 向量模型（`assets/embed/`）；
 * - `-Pfeedhub.embedNpu=true`：打包 HTP NPU 向量模型 + 封面视觉图（自动隐含 `embedEnabled = true`）；
 * - `-Pfeedhub.translate=true`：打包 GGUF 翻译模型（`assets/translate/`）与 `libllamaserver.so`。
 */
val embedNpu: Boolean = providers.gradleProperty("feedhub.embedNpu").orNull == "true"
val embedEnabled: Boolean = embedNpu || providers.gradleProperty("feedhub.embed").orNull == "true"
val translateEnabled: Boolean = providers.gradleProperty("feedhub.translate").orNull == "true"

// 安卓 APK 的外壳：只有 manifest / applicationId / 资源。界面、状态、平台实现全在 :app（它的安卓侧是
// com.android.kotlin.multiplatform.library 出的 AAR）。
//
// 为什么单独一个模块：AGP 9 起 com.android.application 和 Kotlin Multiplatform 插件不能同时用，
// 共享代码模块必须换成 com.android.kotlin.multiplatform.library，APK 的入口单独放一个纯安卓模块。
// 见 https://www.jetbrains.com/help/kotlin-multiplatform-dev/multiplatform-project-agp-9-migration.html
// 界面主体（MainActivity）在 :app 的 androidMain 里，这个模块平时只有 manifest / 资源；唯一的 Kotlin 源码是
// `src/debug` 里的 DebugStartActivity（只在 debug 构建里存在的启动参数入口，见它的 KDoc）。Kotlin 支持是 AGP 9 内置的。
plugins {
    alias(libs.plugins.android.application)
    // DebugStartActivity 里也写了 Compose（`setContent { App(...) }`），这个模块必须和 :app 一样过一遍
    // Compose 编译器插件：不然 @Composable lambda 不会被编译成 Compose 认的形态（参数是 Function0 而不是
    // Function2），编得过、装到机器上一调 setContent 就 NoSuchMethodError。
    alias(libs.plugins.compose.compiler)
}

android {
    namespace = "feedhub.app.android"
    compileSdk = libs.versions.android.compile.sdk.get().toInt()
    buildToolsVersion = libs.versions.android.build.tools.get()

    defaultConfig {
        applicationId = "feedhub.app"
        // 本地向量走 NPU：QNN EP 插件的 AAR 自己声明 `minSdkVersion 27`（manifest 合并会检查），
        // 所以只有带了 `-Pfeedhub.embedNpu=true` 的包把 minSdk 提到 27；不带的包仍然是 26，
        minSdk = if (embedNpu) 27 else libs.versions.android.min.sdk.get().toInt()
        targetSdk = libs.versions.android.target.sdk.get().toInt()
        versionCode = 2
        versionName = "0.1.1"

        // ABI：真机（arm64-v8a），模拟器是 x86_64，
        // 别的 ABI（armeabi-v7a / x86）的 .so 打进去也没人用，只是白占体积。
        // libsingbox.so 只有 android-arm64 那一份（见下面 stageSingBoxAndroid），x86_64 的 APK 里没有它，
        // 那一档就是模拟器 / benchmark 用的。
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }

        // 演示数据开关的默认值：只有 `benchmark` 构建类型会把它改成 "true"。
        // MainActivity 的 isDemoRequested 读 manifest 里的这个 meta-data（release 里永远是 false，
        // release 仍然只认 debuggable，见 feedhub.app.isDemoRequested）。
        manifestPlaceholders["feedhubDemoBuild"] = "false"

        // instrumented 测试：`am instrument` / `connectedAndroidTest`
        // 用的就是它。默认值是 androidx.test.runner 里的那个类名，显式写出来免得 AGP 版本换了默认值。
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    // 不混淆，规则见 `proguard-rules.pro` 的 `-dontobfuscate`），
    // 以及一个只在模拟器 / macrobenchmark 上用的 `benchmark` 构建类型。
    buildTypes {
        // `debug` 平时不变；加 `-Pfeedhub.rustProbe=true` 时把 applicationId 换成 `feedhub.app.rustprobe`，
        // 这样在模拟器上装探针**不会覆盖**别人正在用的 feedhub.app。
        getByName("debug") {
            if (providers.gradleProperty("feedhub.rustProbe").orNull == "true") {
                applicationIdSuffix = ".rustprobe"
                versionNameSuffix = "-rustprobe"
            }
        }
        getByName("release") {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
        // `benchmark`：给模拟器上的 Baseline Profile 生成器和 macrobenchmark 用。
        // - 从 release 拷全部配置（同一套 R8 规则 / 资源压缩 / ABI），所以测出来的就是 release 的代码形状；
        // - 用 debug 签名（自用 App 不发布，release 的正式签名不在仓库里）；
        // - `isDebuggable = false`：可调试会关掉 ART 的 AOT 优化、让数字没法比；
        // - manifest 里的 `<profileable android:shell="true"/>` 本来所有变体都有，这里再提一句；
        // - 只有这个构建类型的 manifest placeholder `feedhubDemoBuild = true`，所以 `--ez demo true`
        //   只在这里（和 debug）有效，release 依旧不认。
        create("benchmark") {
            initWith(getByName("release"))
            isDebuggable = false
            signingConfig = signingConfigs.getByName("debug")
            manifestPlaceholders["feedhubDemoBuild"] = "true"
            // 测试模块 / 依赖里没有 benchmark 变体时回退到 release（和 AndroidX 的 macrobenchmark 模板一致）
            matchingFallbacks += listOf("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    // 本地向量：外部权重 `.onnx_data` **不压缩**。ORT 虽然是从 noBackupFilesDir 里
    // 拷出来的那份文件加载（不是从 APK 里 mmap），但不压缩能让第一次解包更快、也避免大文件在 APK 里被压坏
    androidResources {
        if (embedEnabled) noCompress += "onnx_data"
        // ONNX external data）。两个都**不压缩**：第一次把权重拷到 App 私有目录时不用现场解压
        // 271 MB，也少一次 CPU 峰值。
        if (embedNpu) noCompress += listOf("onnx", "weights")
        // 翻译模型（Project Bergamot 英译中离线模型，共约 48 MB）：`.bin` 与 `.spm` 不压缩，
        // 首次使用时从 assets 纯 IO 拷入 noBackupFilesDir（避免在手机上额外做 ZIP 流式解压）。
        if (translateEnabled) noCompress += listOf("bin", "spm")
    }

    // 别把桌面包里那些只给 JVM 用的 META-INF 带进 APK
    packaging {
        resources {
            excludes.add("META-INF/AL2.0")
            excludes.add("META-INF/LGPL2.1")
            excludes.add("META-INF/INDEX.LIST")
            excludes.add("META-INF/DEPENDENCIES")
        }
        // 内置代理核心的 sing-box 二进制要**真的解出来**放进 nativeLibraryDir
        // （Android 只允许执行 nativeLibraryDir 下的文件；extractNativeLibs=false 时 .so 会以未压缩方式
        // 留在 APK 里，nativeLibraryDir 里根本没有这个文件，也就没有可执行的路径）。
        jniLibs {
            useLegacyPackaging = true
            // 本地向量走 NPU：`qnn-runtime` 的 AAR 里带 V68 / V69 / V73 / V75 / V79 / V81 六个 arch 的
            // skel + stub、GPU / DSP 后端和 83 MB 的 HTP prepare 库。用户的机器（8 Elite）是 **HTP v79**，
            // 只需要 v79 那一对 skel / stub + HTP / System 本体（prepare 留着：第一次在前台编图要用它）。
            // 其余全部不进 APK（省 ~80 MB 未压缩体积，也少几条没人用的 .so）。
            excludes += listOf(
                "**/libQnnGpu.so",
                "**/libQnnDsp.so",
                "**/libQnnDspV66Skel.so",
                "**/libQnnDspV66Stub.so",
                "**/libQnnHtpV68Skel.so",
                "**/libQnnHtpV68Stub.so",
                "**/libQnnHtpV69Skel.so",
                "**/libQnnHtpV69Stub.so",
                "**/libQnnHtpV73Skel.so",
                "**/libQnnHtpV73Stub.so",
                "**/libQnnHtpV75Skel.so",
                "**/libQnnHtpV75Stub.so",
                "**/libQnnHtpV81Skel.so",
                "**/libQnnHtpV81Stub.so",
            )
        }
        // 桌面用的 Rust .so 是 :rust-bridge 的 jar 资源（feedhub-native/host/…），APK 里用不到
        // （安卓用 jniLibs 里那份），别让它白占 ~950 KB。
        resources { excludes.add("feedhub-native/**") }
    }
}

// 内置代理核心（sing-box）在安卓上的二进制
// **不进仓库**（android-arm64 那份 83 MB）：构建时从 `-Pfeedhub.singbox.dir=<版本目录>`
// 取 `android-arm64/sing-box`，校验 sha256 之后按 ABI 目录的形状放进 `jniLibs/arm64-v8a/libsingbox.so`
// （Android 只认 `lib*.so` 这个名字 + ABI 目录，AGP 才会把它当本地库打进 APK）。
// 运行时由 `feedhub.proxy.ProxyCoreBridge.locate` 定位 `nativeLibraryDir/libsingbox.so` 并再校验一次 sha256。
val singBoxDir: String = providers.gradleProperty("feedhub.singbox.dir")
    .getOrElse(rootProject.file("deps/sing-box").path)
val singBoxSha256Android: String = providers.gradleProperty("feedhub.singbox.sha256.android")
    .getOrElse("64324301d1049474f6010b5a015d3143271a8df83bae83ae8814167ac8f74f5a")
// 注意：**不要**写进 `src/main/jniLibs`（源码树），生成物一律放 build 目录
val singBoxJniLibs = layout.buildDirectory.dir("singbox-jniLibs")
val stageSingBoxAndroid = tasks.register("stageSingBoxAndroid") {
    group = "build"
    description = "把 android-arm64 的 sing-box 复制成 jniLibs/arm64-v8a/libsingbox.so（校验 sha256）"
    val src = file("$singBoxDir/android-arm64/sing-box")
    val target = singBoxJniLibs.map { it.file("arm64-v8a/libsingbox.so") }
    inputs.file(src)
    inputs.property("sha256", singBoxSha256Android)
    outputs.file(target)
    doLast {
        check(src.isFile) {
            "找不到安卓用的 sing-box 二进制：$src（用 -Pfeedhub.singbox.dir=<版本目录> 指定；默认是仓库的 deps/sing-box）"
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
        check(actual == singBoxSha256Android.lowercase()) {
            "sing-box（android-arm64）的 sha256 不对：期望 $singBoxSha256Android，实际 $actual" +
                "（换版本就同时改 -Pfeedhub.singbox.sha256.android）"
        }
        logger.lifecycle("已放入 sing-box（android-arm64，${out.length() / 1024 / 1024} MB，sha256 校验通过）：$out")
    }
}
// 打包前必须先放好（否则 APK 里没有这个文件，运行时会说"找不到 sing-box"）
tasks.matching { it.name.contains("JniLibFolders") || it.name == "preBuild" }.configureEach {
    dependsOn(stageSingBoxAndroid)
}

// 让 AGP 从 build 目录里那份 jniLibs 取（默认的 src/main/jniLibs 保持空着，源码树里不放 83 MB 的生成物）
android.sourceSets["main"].jniLibs.srcDir(singBoxJniLibs.get().asFile)

// 本地向量模型
// 三个大文件（`model_q4_acc4.onnx` 0.5 MB + `model_q4.onnx_data` 174 MB + `tokenizer.json` 32 MB）**不进 git**：
// 构建时从 `-Pfeedhub.embedModelDir=<模型目录>`拷到 build 目录，再作为 assets 打进 APK。
// 目录 / 文件不存在时**照常出包**（只是不带模型；App 里显示「未包含模型」，整条通路由 `OnnxTextEmbedder.available` 关掉），
// `./gradlew test` 和 debug 构建都不依赖模型在不在。
val embedModelDir: String = providers.gradleProperty("feedhub.embedModelDir")
    .getOrElse(rootProject.file("deps/embeddinggemma").path)
// 源文件（相对模型目录）：onnx-community 把图放在 onnx/ 下，词表和 README 在根上
val embedFiles = listOf(
    "embed/model_q4_acc4.onnx" to "onnx/model_q4_acc4.onnx",
    "embed/model_q4.onnx_data" to "onnx/model_q4.onnx_data",
    "embed/tokenizer.json" to "tokenizer.json",
)
val embedAssets = layout.buildDirectory.dir("embed-assets")
if (embedEnabled && !embedNpu) {
    val stageEmbedModel = tasks.register("stageEmbedModel") {
        group = "build"
        description = "把 EmbeddingGemma 2 的图 / 权重 / 词表拷成 assets（没有模型目录就出一份空的，照常出包）"
        val src = file(embedModelDir)
        inputs.property("modelDir", src.absolutePath)
        inputs.property("embedNpu", embedNpu)
        inputs.files(embedFiles.map { (_, from) -> File(src, from) }.filter { it.isFile })
        outputs.dir(embedAssets)
        doLast {
            val root = embedAssets.get().asFile
            val out = root.resolve("embed")
            out.deleteRecursively()
            val missing = embedFiles.map { (_, from) -> File(src, from) }.filterNot { it.isFile }
            if (missing.isNotEmpty()) {
                logger.lifecycle(
                    "本地向量：跳过（${src.absolutePath} 里缺 ${missing.joinToString { it.name }}）。" +
                        "这次 APK 不带模型，App 里显示「未包含模型」，其它功能不受影响。",
                )
                out.mkdirs()
                return@doLast
            }
            out.mkdirs()
            var total = 0L
            for ((to, from) in embedFiles) {
                val target = File(root, to)
                File(src, from).copyTo(target, overwrite = true)
                total += target.length()
            }
            logger.lifecycle("本地向量：已放进 assets ${embedFiles.size} 个文件，共 ${total / 1024 / 1024} MB（来自 ${src.absolutePath}）")
        }
    }
    // 让 AGP 从 build 目录里那份 assets 取（只有开启 -Pfeedhub.embed=true 且非 NPU 包时挂入，基础版完全不打进 APK）
    android.sourceSets["main"].assets.srcDir(embedAssets.get().asFile)
    tasks.matching { it.name.startsWith("merge") && it.name.endsWith("Assets") }.configureEach {
        dependsOn(stageEmbedModel)
    }
    tasks.matching { it.name == "preBuild" }.configureEach { dependsOn(stageEmbedModel) }
}

// 本地向量走 NPU：HTP 版模型
// 只有 `-Pfeedhub.embedNpu=true` 才拷：`npu-research` 里数值验过的 **w8a16 + int8 词表**、固定形状 `[8,L]`、
// 只留文字路径的图。源文件**不进 git**。
//
// 所以拆成
// `model_w8a16_b8.weights`（271 MB，ONNX external data）+ 每张图 1.5 MB 的图结构（三张引用同一个文件、
// 同一组 offset，见 npu-research 的 `make_shared_weights.py`）。
// sha256 钉住：免得哪天拷到 w8a8 / w4a16 那几个精度崩掉的对照版本，还以为在测这一张。
//
// **序列长度是烘进图里的**（HTP 不支持动态形状），所以一个长度一张图。
//
// 以前纯文字带
// L32 / L128 两张（吃 `input_ids`），封面另带一张"文本+图"（吃 `inputs_embeds`），三张的权重逐字节相同、
// 只差输入口和量化校准。现在把词表查表挪到宿主（词表矩阵在共用权重里的位置写在 `embed-table.json`），
// 用"纯文字 30 批 + 封面 30 批"的混合校准重新量化，一张图两种输入都能吃：
//   - 对 fp32 的 256 维余弦：纯文字 0.99758（原 L128 专用图 0.99757）、封面 0.99742（原封面图 0.99742）；
//   - 和原来三张图的输出直接比：余弦中位 0.9999、最小 0.9987——所以 `item_embedding.model` 不变，不重算；
//   - 共用权重的 293 个张量逐字节相同（sha256 不变），4 个 RoPE 常量内联进图；
//   - `qnn_capability.py --offload-io 0`：1 个分区（App 里这张图用 `offload_graph_io_quantization=0`）。
// 短文本也走 128：真机上 L32 一批 257 ms、L128 1241 ms，多花的时间换掉一整份 context binary（几百 MB）
// 和一个常驻会话。生成脚本：`tools/embed/merge_text_cover.py`（源图在 npu-research，见脚本头）。
// 上一版的三张图留在 `out/embed-rope/graphs` / `out/embed-cover/graphs`（**别覆盖、别删**：对照用）。
// ctx 缓存靠图指纹失效（`NpuAssets.ctxFileName`），换图不用手动清缓存。
val npuGraphDir: String = providers.gradleProperty("feedhub.npuGraphDir")
    .getOrElse(rootProject.file("deps/npu/embed-merged").path)

/** 共用权重（文本图和词表矩阵都在它里面）。**当前没变**（合图不碰权重）。 */
val npuWeightsFile = "model_w8a16_b8.weights"
val npuWeightsSha256 = "2c9746d68f025faa7490d93af6ac916444c69f08e4eed501e1ca4322196b204b"

/** 文本图（`model_w8a16_b8_l128_emb.onnx`，名字要和 `NpuAssets.fileFor(128)` 一致）+ 词表矩阵位置。 */
val npuModelSha256 = mapOf(
    "model_w8a16_b8_l128_emb.onnx" to "3afcfb26cf488e11535e25127f4fc5bc38e4c2c563dc33c899c46deed409239b",
    "embed-table.json" to "4e59737c7fc0008ca8bf511f8f991165cc280759d8bc9edf5f3418571be5ea39",
)
val npuAssets = layout.buildDirectory.dir("embed-npu-assets")
if (embedNpu) {
    val stageNpuModel = tasks.register("stageNpuModel") {
        group = "build"
        description = "把 HTP 版（w8a16，[8,128]，吃 inputs_embeds）的 EmbeddingGemma 2 图、词表矩阵位置和共用权重拷成 assets/embed-npu/（校验 sha256）"
        inputs.property("sha256", npuModelSha256.toSortedMap().toString())
        inputs.property("weights", npuWeightsSha256)
        inputs.property("graphDir", npuGraphDir)
        // 词表也进这个包的 assets/embed-npu/
        inputs.file(File(embedModelDir, "tokenizer.json"))
        outputs.dir(npuAssets)
        doLast {
            val outDir = npuAssets.get().asFile.resolve("embed-npu")
            fun sha256(file: File): String {
                val digest = MessageDigest.getInstance("SHA-256")
                file.inputStream().use { input ->
                    val buf = ByteArray(1 shl 20)
                    while (true) {
                        val n = input.read(buf)
                        if (n <= 0) break
                        digest.update(buf, 0, n)
                    }
                }
                return digest.digest().joinToString("") { "%02x".format(it) }
            }
            fun stage(from: File, name: String, expected: String) {
                check(from.isFile) {
                    "找不到 $name：$from（用 -Pfeedhub.npuGraphDir=<目录> 指定；默认是" +
                        "npu-research 的 out/embed-merged/graphs，由 tools/embed/merge_text_cover.py 生成，" +
                        "共用权重和 embed-rope/graphs 里那份是同一个文件）。" +
                        "不想带 NPU 就别加 -Pfeedhub.embedNpu=true。"
                }
                val actual = sha256(from)
                check(actual == expected) { "$name 的 sha256 不对：期望 $expected，实际 $actual（$from）" }
                val target = File(outDir, name)
                from.copyTo(target, overwrite = true)
                logger.lifecycle(
                    "本地向量 NPU：已放进 assets/embed-npu/$name（${target.length() / 1024 / 1024} MB，sha256 校验通过）",
                )
            }

            outDir.deleteRecursively()
            outDir.mkdirs()
            // 共用权重先放（图的 external data 指向它，词表矩阵也在它里面；少一个文件就建不起会话）
            stage(File(npuGraphDir, npuWeightsFile), npuWeightsFile, npuWeightsSha256)
            for ((name, expected) in npuModelSha256.toSortedMap()) stage(File(npuGraphDir, name), name, expected)
            // 词表（32 MB，
            // `assets/embed/` 是空的，所以词表跟 NPU 的图放一起（同一份 tokenizer.json，两个量化版本逐字节相同）。
            val tokenizer = File(embedModelDir, "tokenizer.json")
            check(tokenizer.isFile) {
                "找不到词表：$tokenizer（用 -Pfeedhub.embedModelDir=<模型目录> 指定）。" +
                    "NPU 这条路要靠它分词，没有词表就一条都算不出来。"
            }
            tokenizer.copyTo(File(outDir, "tokenizer.json"), overwrite = true)
            logger.lifecycle(
                "本地向量 NPU：词表 tokenizer.json（${tokenizer.length() / 1024 / 1024} MB，和 CPU 那份同一个文件）",
            )
            logger.lifecycle(
                "本地向量 NPU：文本图（128 token）+ 共用权重 $npuWeightsFile（" +
                    "${outDir.listFiles()?.sumOf { it.length() }?.div(1024 * 1024)} MB）",
            )
        }
    }
    // 封面向量：视觉图
    // 视觉图由 `tools/embed-vision/make_cover_graphs.py` 生成（IO Q/DQ 留在 QNN 里，
    // `qnn_capability.py --offload-io 0` 报 1 个分区），自带权重（162.7 MB），不引用共用的那份。
    // 现在纯文字那张图本身就吃 `inputs_embeds`，词表矩阵位置也跟着它放在 `assets/embed-npu/`。
    val coverGraphDir: String = providers.gradleProperty("feedhub.coverGraphDir")
        .getOrElse(rootProject.file("deps/npu/embed-cover").path)
    val coverModelSha256 = mapOf(
        "cover_vis_b1_8x8_qnn.onnx" to "37d4d15f5dead2363633c34142853a2f380c045ba3dda5a22abf6f5c3a6b6ac8",
    )
    val coverAssets = layout.buildDirectory.dir("embed-cover-assets")
    val stageCoverModel = tasks.register("stageCoverModel") {
        group = "build"
        description = "把封面向量的视觉图拷成 assets/embed-cover/（校验 sha256；文本那一步借 assets/embed-npu/ 的图）"
        inputs.property("sha256", coverModelSha256.toSortedMap().toString())
        inputs.property("graphDir", coverGraphDir)
        outputs.dir(coverAssets)
        doLast {
            val outDir = coverAssets.get().asFile.resolve("embed-cover")
            fun sha256(file: File): String {
                val digest = MessageDigest.getInstance("SHA-256")
                file.inputStream().use { input ->
                    val buf = ByteArray(1 shl 20)
                    while (true) {
                        val n = input.read(buf)
                        if (n <= 0) break
                        digest.update(buf, 0, n)
                    }
                }
                return digest.digest().joinToString("") { "%02x".format(it) }
            }
            val missing = coverModelSha256.keys.filterNot { File(coverGraphDir, it).isFile }
            if (missing.isNotEmpty()) {
                logger.lifecycle(
                    "封面向量：跳过（$coverGraphDir 里缺 ${missing.joinToString()}）。" +
                        "这次 APK 不带封面向量的视觉图，App 里那一条显示「没带图」，纯文字向量照旧。",
                )
                outDir.mkdirs()
                return@doLast
            }
            outDir.deleteRecursively()
            outDir.mkdirs()
            for ((name, expected) in coverModelSha256.toSortedMap()) {
                val from = File(coverGraphDir, name)
                val actual = sha256(from)
                check(actual == expected) {
                    "$name 的 sha256 不对：期望 $expected，实际 $actual（$from）。" +
                        "改过图就重新跑 tools/embed-vision/make_cover_graphs.py，再把新值抄回 coverModelSha256。"
                }
                from.copyTo(File(outDir, name), overwrite = true)
                logger.lifecycle(
                    "封面向量：已放进 assets/embed-cover/$name（${from.length() / 1024 / 1024} MB，sha256 校验通过）",
                )
            }
            logger.lifecycle(
                "封面向量：视觉图（${outDir.listFiles()?.sumOf { it.length() }?.div(1024 * 1024)} MB）；" +
                    "文本那一步借 assets/embed-npu/ 里的文本图和共用权重 $npuWeightsFile",
            )
        }
    }
    android.sourceSets["main"].assets.srcDir(coverAssets.get().asFile)
    tasks.matching { it.name.startsWith("merge") && it.name.endsWith("Assets") }.configureEach {
        dependsOn(stageCoverModel)
    }
    tasks.matching { it.name == "preBuild" }.configureEach { dependsOn(stageCoverModel) }
    // assets 里那几个目录是独立的 srcDir（CPU 那份 246 MB、NPU 这份 273 MB、封面视觉图 163 MB），不互相覆盖
    android.sourceSets["main"].assets.srcDir(npuAssets.get().asFile)
    tasks.matching { it.name.startsWith("merge") && it.name.endsWith("Assets") }.configureEach {
        dependsOn(stageNpuModel)
    }
    tasks.matching { it.name == "preBuild" }.configureEach { dependsOn(stageNpuModel) }
}

// instrumented 测试要的两份夹具：**直接指向** source-youtube 的测试资源，
// 不复制一份（2.9 MB 的播放器脚本 + 一堆响应夹具，复制了会两边漂移）。
// 测试 APK 里它们躺在 assets/fixtures/... 下（见 YoutubeEjsAndroidTest 的读取方式）。
android.sourceSets["androidTest"].assets.srcDir(rootProject.file("source-youtube/src/test/resources"))

// Rust 核心（libfeedhub_ffi.so）：交叉编译 arm64-v8a + x86_64，放进 APK 的 jniLibs
// 不做 `cargo-ndk`：链接器直接用 NDK 里的 clang，用环境变量告诉 cargo。
// ABI 只留 arm64-v8a（真机）+ x86_64（模拟器），和上面的 abiFilters 一致。
// 16 KB 页对齐在 rust/.cargo/config.toml 里给两个安卓目标加了 `-Wl,-z,max-page-size=16384`。
val rustDir = rootProject.file("rust")
val rustJniLibs = layout.buildDirectory.dir("rust-jniLibs")

/** NDK 位置：`-Pfeedhub.ndk.dir=` → `ANDROID_NDK_HOME` → `$ANDROID_HOME/ndk/29.0.13113456` → 本机解压的那份。 */
fun resolveNdk(): File {
    val home = System.getProperty("user.home")
    val sdk = System.getenv("ANDROID_HOME") ?: System.getenv("ANDROID_SDK_ROOT") ?: "$home/Android/Sdk"
    val candidates = listOfNotNull(
        (providers.gradleProperty("feedhub.ndk.dir").orNull)?.let { file(it) },
        System.getenv("ANDROID_NDK_HOME")?.let { file(it) },
        file("$sdk/ndk/29.0.13113456"),
        // SDK 里没有这一版 NDK 时，放（或软链）到仓库的 `deps/android-ndk`
        // 或者用 -Pfeedhub.ndk.dir= 指过去。
        rootProject.file("deps/android-ndk"),
    )
    return candidates.firstOrNull { File(it, "toolchains/llvm/prebuilt/linux-x86_64/bin").isDirectory }
        ?: throw GradleException(
            "找不到 NDK（交叉编译 Rust 要它）：试过 ${candidates.joinToString("、")}。" +
                "用 -Pfeedhub.ndk.dir=<NDK 根目录> 指定（里面要有 toolchains/llvm/prebuilt/linux-x86_64/bin）。",
        )
}

val cargoBuildAndroid = tasks.register<Exec>("cargoBuildAndroid") {
    group = "build"
    description = "交叉编译 libfeedhub_ffi.so（arm64-v8a + x86_64）"
    // 避免与 :rust-bridge:cargoBuildHost 并发争用同一个 rust/target 目录锁
    mustRunAfter(":rust-bridge:cargoBuildHost")
    workingDir = rustDir
    commandLine(
        "cargo", "build", "--release", "--locked", "-j", "4",
        "-p", "feedhub-ffi",
        "--target", "aarch64-linux-android",
        "--target", "x86_64-linux-android",
    )
    environment("CARGO_TERM_COLOR", "never")
    doFirst {
        val bin = File(resolveNdk(), "toolchains/llvm/prebuilt/linux-x86_64/bin")
        val api = libs.versions.android.min.sdk.get()
        environment(
            "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER",
            File(bin, "aarch64-linux-android$api-clang").absolutePath,
        )
        environment(
            "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER",
            File(bin, "x86_64-linux-android$api-clang").absolutePath,
        )
        // cc **不读**
        // `CARGO_TARGET_*_LINKER`，它按 `CC_<target>`（hyphen / underscore 两种写法都认）找 C 编译器；
        // NDK 的 clang 不在 PATH 里，不显式给就会在 "failed to find tool aarch64-linux-android-clang" 上失败。
        // 同一个 clang 也负责链接，口径和上面的 LINKER 一致。
        for (target in listOf("aarch64" to "aarch64-linux-android", "x86_64" to "x86_64-linux-android")) {
            val (key, triple) = target
            val clang = File(bin, "$triple$api-clang").absolutePath
            val clangxx = File(bin, "$triple$api-clang++").absolutePath
            environment("CC_$key-linux-android", clang)
            environment("CC_${key}_linux_android", clang)
            // **C++ 也要给**：`btls-sys` 走 cmake，`CMAKE_C_COMPILER` 认了
            // `CC_<target>` 就对了，但 `CMAKE_CXX_COMPILER` 只认 `CXX` / `CXX_<target>`——不给的话
            // cmake 从 PATH 里挑一个**不带 --target 的 `clang++`**，编出来是一批 x86-64 的 `.o`
            // 混进 arm64 的 rlib，链接期只说 "incompatible with aarch64linux"（不说是哪个编译器干的）。
            environment("CXX_$key-linux-android", clangxx)
            environment("CXX_${key}_linux_android", clangxx)
            environment("AR_${key}_linux_android", File(bin, "llvm-ar").absolutePath)
        }
        // 它的 build.rs **走 cmake**，
        // 只认 `ANDROID_NDK_HOME`（拼 `build/cmake/android.toolchain.cmake`），不认上面那些 CC_*。
        // 不设的话会在 "Please set ANDROID_NDK_HOME for Android build" 上失败；
        // 设错的话 cmake 会用宿主 gcc 编出一批 x86-64 的 `.o` 混进 arm64 库（链接期报
        // "incompatible with aarch64linux"）。另外 `btls-sys` 的 cmake 缓存**会残留**：
        // 换 ABI / 换 NDK 之后要先删 `target/<triple>/release/build/btls-sys-*`。
        environment("ANDROID_NDK_HOME", resolveNdk().absolutePath)
        // 抹掉编译机绝对路径（Rust panic Location 与 C/C++ BoringSSL/SQLite 的 __FILE__），保证产物可复现且不带本机目录名
        val homeDir = System.getProperty("user.home")
        val cargoHome = System.getenv("CARGO_HOME") ?: "$homeDir/.cargo"
        val rustupHome = System.getenv("RUSTUP_HOME") ?: "$homeDir/.rustup"
        val rootPath = rootProject.projectDir.absolutePath
        val remapCFlags = listOf(
            "-ffile-prefix-map=$rootPath=/src",
            "-ffile-prefix-map=$cargoHome=/cargo",
            "-ffile-prefix-map=$homeDir=/home/builder",
        ).joinToString(" ")
        environment("CFLAGS", remapCFlags)
        environment("CXXFLAGS", remapCFlags)
        environment(
            "CARGO_ENCODED_RUSTFLAGS",
            listOf(
                "-C", "link-arg=-Wl,-z,max-page-size=16384",
                "--remap-path-prefix=$rootPath=/src",
                "--remap-path-prefix=$cargoHome=/cargo",
                "--remap-path-prefix=$rustupHome=/rustup",
                "--remap-path-prefix=$homeDir=/home/builder",
            ).joinToString("\u001f"),
        )
    }
    inputs.files(
        fileTree(rustDir.resolve("crates")) {
            include("feedhub-core/**/*.rs", "feedhub-core/**/Cargo.toml")
            include("feedhub-net/**/*.rs", "feedhub-net/**/Cargo.toml")
            include("feedhub-ffi/**/*.rs", "feedhub-ffi/**/Cargo.toml")
        },
        rustDir.resolve("Cargo.toml"),
        rustDir.resolve("Cargo.lock"),
        rustDir.resolve(".cargo/config.toml"),
    )
    outputs.files(
        rustDir.resolve("target/aarch64-linux-android/release/libfeedhub_ffi.so"),
        rustDir.resolve("target/x86_64-linux-android/release/libfeedhub_ffi.so"),
    )
    outputs.upToDateWhen { false }
}

/**
 * 防止 C1 再犯：核对生成 Kotlin 绑定所用的宿主 `libfeedhub_ffi.so` 与安卓两个 ABI 的 `libfeedhub_ffi.so`
 * 导出的 `uniffi_*` 动态符号完全一致（缺一个或多一个都立刻让构建失败，而不是等到真机启动时 `UnsatisfiedLinkError`）。
 */
val verifyRustFfiSymbols = tasks.register("verifyRustFfiSymbols") {
    group = "verification"
    description = "校验安卓 libfeedhub_ffi.so 与宿主绑定生成用 .so 的 uniffi_* 导出符号一致"
    dependsOn(":rust-bridge:cargoBuildHost", cargoBuildAndroid)
    val hostSo = rustDir.resolve("target/release/libfeedhub_ffi.so")
    val arm64So = rustDir.resolve("target/aarch64-linux-android/release/libfeedhub_ffi.so")
    val x64So = rustDir.resolve("target/x86_64-linux-android/release/libfeedhub_ffi.so")
    inputs.files(hostSo, arm64So, x64So)
    doLast {
        val nm = File(resolveNdk(), "toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-nm")
        if (!nm.isFile) throw GradleException("找不到 NDK 的 llvm-nm：${nm.absolutePath}")
        fun exportedUniffiSymbols(so: File): Set<String> {
            val p = ProcessBuilder(nm.absolutePath, "-D", "--defined-only", so.absolutePath)
                .redirectErrorStream(true)
                .start()
            val out = p.inputStream.bufferedReader().use { it.readText() }
            if (p.waitFor() != 0) throw GradleException("llvm-nm 失败（${so.absolutePath}）：$out")
            return out.lineSequence()
                .mapNotNull { line -> line.trim().split(Regex("\\s+")).lastOrNull() }
                .filter { it.startsWith("uniffi_") || it.startsWith("ffi_feedhub_ffi_") }
                .toSortedSet()
        }
        val hostSymbols = exportedUniffiSymbols(hostSo)
        if (hostSymbols.isEmpty()) {
            throw GradleException("宿主 ${hostSo.name} 里没有扫到任何 uniffi_* 动态符号")
        }
        for ((abi, so) in listOf("arm64-v8a" to arm64So, "x86_64" to x64So)) {
            val abiSymbols = exportedUniffiSymbols(so)
            val missingInAndroid = hostSymbols - abiSymbols
            val extraInAndroid = abiSymbols - hostSymbols
            if (missingInAndroid.isNotEmpty() || extraInAndroid.isNotEmpty()) {
                throw GradleException(
                    "安卓 $abi 的 libfeedhub_ffi.so 与 Kotlin 绑定符号不一致！\n" +
                        "  安卓缺少 (${missingInAndroid.size})：${missingInAndroid.take(10)}\n" +
                        "  安卓多出 (${extraInAndroid.size})：${extraInAndroid.take(10)}",
                )
            }
        }
        logger.lifecycle("UniFFI 符号一致性校验通过：${hostSymbols.size} 个导出符号（host == arm64-v8a == x86_64）")
    }
}

tasks.matching { it.name == "check" }.configureEach {
    dependsOn(verifyRustFfiSymbols)
}

val stageRustAndroid = tasks.register<Copy>("stageRustAndroid") {
    group = "build"
    description = "把两个 ABI 的 libfeedhub_ffi.so 放进 jniLibs（Rust 核心）"
    dependsOn(cargoBuildAndroid, verifyRustFfiSymbols)
    from(rustDir.resolve("target/aarch64-linux-android/release/libfeedhub_ffi.so")) { into("arm64-v8a") }
    from(rustDir.resolve("target/x86_64-linux-android/release/libfeedhub_ffi.so")) { into("x86_64") }
    // 它按 NDK 的
    // `CMAKE_ANDROID_STL_TYPE=c++_shared` 链接，于是 `.so` 多了一条 `NEEDED libc++_shared.so`。
    // 安卓系统的 `libc++_shared.so` 在 `/system/lib64` 里**不一定有**（它属于 NDK 运行时，不是公共库），
    // 所以必须自己打进 APK，否则 App 启动时 `dlopen` 就失败（"library ... not found"）。
    // 位置：NDK 的 sysroot；strip 之后每个 ABI 约 1.4 MB（未 strip 9.3 MB）。
    from(ndkStlLib()) { into("arm64-v8a") }
    from(ndkStlLibX86_64()) { into("x86_64") }
    // **strip 一遍**：`libc++_shared.so` 未 strip 9.3 MB / strip 后约 1.4 MB；
    // rust 的 release profile 是 `strip = "debuginfo"`（保留符号表，方便看栈），
    // 但打进 APK 的副本不需要——`--strip-all` 之后动态符号（UniFFI 的 `uniffi_*`）仍在，
    // 每个 ABI 省 4~5 MB。strip 工具从 NDK 里取（和编译用的同一份）。
    doLast {
        val strip = File(
            resolveNdk(),
            "toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip",
        )
        if (!strip.isFile) throw GradleException("找不到 NDK 的 llvm-strip：${strip.absolutePath}")
        rustJniLibs.get().asFile.walkTopDown()
            .filter { it.isFile && it.name.endsWith(".so") }
            .forEach { so ->
                val p = ProcessBuilder(strip.absolutePath, "--strip-all", so.absolutePath)
                    .redirectErrorStream(true)
                    .start()
                val out = p.inputStream.bufferedReader().use { it.readText() }
                if (p.waitFor() != 0) throw GradleException("strip 失败（${so.name}）：$out")
                logger.lifecycle("strip ${so.parentFile.name}/${so.name} → ${so.length()} 字节")
            }
    }
    into(rustJniLibs)
}

/** NDK 里 `libc++_shared.so`（arm64-v8a）。找不到就报错说清楚（不静默出包）。 */
fun ndkStlLib(): File {
    val ndk = resolveNdk()
    val f = File(ndk, "toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so")
    if (!f.isFile) throw GradleException(
        "找不到 NDK 的 libc++_shared.so（BoringSSL 按 c++_shared 链接，打包必须带上它）：${f.absolutePath}\n" +
            "确认 -Pfeedhub.ndk.dir= 指向完整的 NDK r29。",
    )
    return f
}

/** NDK 里 `libc++_shared.so`（x86_64，模拟器）。 */
fun ndkStlLibX86_64(): File {
    val ndk = resolveNdk()
    val f = File(ndk, "toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/x86_64-linux-android/libc++_shared.so")
    if (!f.isFile) throw GradleException(
        "找不到 NDK 的 libc++_shared.so（x86_64）：${f.absolutePath}",
    )
    return f
}

// 打包前必须先编好。只挂在 JniLibFolders 上（**不挂 preBuild**）：`./gradlew test` 也会跑 androidApp 的
// preBuild，挂上去就意味着"跑单元测试也要 NDK + 两个安卓 Rust target"。（sing-box 那个照旧挂 preBuild，
// 它只是复制一个已经在盘上的文件。）
tasks.matching { it.name.contains("JniLibFolders") }.configureEach {
    dependsOn(stageRustAndroid)
}
android.sourceSets["main"].jniLibs.srcDir(rustJniLibs.get().asFile)

// ---------------------------------------------------------------------------
// 手机上的本地翻译（Project Bergamot 进程内轻量英译中引擎 + Firefox enzh 48 MB 量化模型）
// ---------------------------------------------------------------------------
// 默认基础版（未传 -Pfeedhub.translate=true）0 字节不打入 APK；
// 开启 `-Pfeedhub.translate=true` 时打入两样东西（均不进 git）：
// 1. `libfeedhub_bergamot.so`（arm64-v8a 与 x86_64，16 KB 页对齐，由 `tools/translate/build-bergamot.sh` 交叉编译）；
// 2. Firefox `enzh` base-memory 模型四件套 + `nonbreaking_prefix.en`（约 48 MB，SHA-256 校验后放入 `assets/translate/enzh/`）。
val translateModelDir: String = providers.gradleProperty("feedhub.translateModelDir")
    .getOrElse(rootProject.file("deps/translate/bergamot-enzh").path)
val bergamotSrcDir: String = providers.gradleProperty("feedhub.bergamotSrcDir")
    .getOrElse(rootProject.file("deps/bergamot-src").path)
val translateModelSha256 = mapOf(
    "model.enzh.intgemm.alphas.bin" to "4e5accc141373565ddc8fa1565bceaa8d0c3482a82cab8131c719ebcc6c2157c",
    "lex.50.50.enzh.s2t.bin" to "8575d8daa10e2dbff316dcdf8e1ce475357bcc2c92bdc63b736a2d5add22f681",
    "srcvocab.enzh.spm" to "bd9b65504acc6d9726dd281f7defc2adb7c2c22d0688fe2f84697de25197c8c5",
    "trgvocab.enzh.spm" to "aded6993c36e440284d11cec3f6b8aef9c0e43188a772d80be342a713adf223d",
)
val translatePrefixSha256 = "af526c47425bb31b4e7b79ec975c1bb752a34ba21951237d51b965e879d9d51a"

fun sha256Of(file: File): String {
    val digest = MessageDigest.getInstance("SHA-256")
    file.inputStream().use { input ->
        val buf = ByteArray(1 shl 20)
        while (true) {
            val n = input.read(buf)
            if (n <= 0) break
            digest.update(buf, 0, n)
        }
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
}

val translateAssets = layout.buildDirectory.dir("translate-assets")
val bergamotJniDir: String = providers.gradleProperty("feedhub.bergamotAndroidDir")
    .getOrElse("$projectDir/build/bergamot-jniLibs")
// 不要和 `bergamotJniDir` 用同一个目录，防止 Copy 的 from/into 重合截断文件
val bergamotJniLibsStaged = layout.buildDirectory.dir("bergamot-jnilibs-staged")

if (translateEnabled) {
    val stageTranslationModel = tasks.register("stageTranslationModel") {
        group = "build"
        description = "把 Firefox Bergamot enzh 翻译模型拷成 assets/translate/enzh/（校验 sha256；缺失时出空目录照常出包）"
        val srcDir = file(translateModelDir)
        val modelFiles = translateModelSha256.keys.associateWith { File(srcDir, it) }
        val prefixCandidates = listOf(
            File(srcDir, "nonbreaking_prefix.en"),
            file("$bergamotSrcDir/inference/3rd_party/ssplit-cpp/nonbreaking_prefixes/nonbreaking_prefix.en"),
        )
        inputs.property("modelDir", srcDir.absolutePath)
        inputs.property("sha256", translateModelSha256.toSortedMap().toString() + translatePrefixSha256)
        inputs.files(modelFiles.values.filter { it.isFile })
        prefixCandidates.firstOrNull { it.isFile }?.let { inputs.file(it) }
        outputs.dir(translateAssets)
        doLast {
            val out = translateAssets.get().asFile.resolve("translate/enzh")
            translateAssets.get().asFile.resolve("translate").deleteRecursively()
            out.mkdirs()
            val missing = modelFiles.filterValues { !it.isFile }.keys
            if (missing.isNotEmpty()) {
                logger.lifecycle(
                    "翻译：跳过（${srcDir.absolutePath} 缺少 ${missing.joinToString()}）。" +
                        "这次 APK 不带翻译模型，App 里显示「未包含翻译模型」，其它功能不受影响。",
                )
                return@doLast
            }
            var total = 0L
            for ((name, src) in modelFiles.toSortedMap()) {
                val expected = translateModelSha256.getValue(name)
                val actual = sha256Of(src)
                check(actual == expected) {
                    "翻译模型 $name 的 sha256 不对：期望 $expected，实际 $actual（$src）"
                }
                val dst = File(out, name)
                src.copyTo(dst, overwrite = true)
                total += dst.length()
            }
            prefixCandidates.firstOrNull { it.isFile }?.let { prefix ->
                val actual = sha256Of(prefix)
                check(actual == translatePrefixSha256) {
                    "nonbreaking_prefix.en 的 sha256 不对：期望 $translatePrefixSha256，实际 $actual"
                }
                val dst = File(out, "nonbreaking_prefix.en")
                prefix.copyTo(dst, overwrite = true)
                total += dst.length()
            }
            logger.lifecycle(
                "翻译：已放进 assets/translate/enzh/（共 ${total / 1024 / 1024} MB，sha256 校验通过，来自 ${srcDir.absolutePath}）",
            )
        }
    }
    android.sourceSets["main"].assets.srcDir(translateAssets.get().asFile)
    tasks.matching { it.name.startsWith("merge") && it.name.endsWith("Assets") }.configureEach {
        dependsOn(stageTranslationModel)
    }
    tasks.matching { it.name == "preBuild" }.configureEach { dependsOn(stageTranslationModel) }

    val buildBergamotAndroid = tasks.register<Exec>("buildBergamotAndroid") {
        group = "build"
        description = "交叉编译 libfeedhub_bergamot.so（arm64-v8a + x86_64，16 KB 页对齐）"
        workingDir = rootDir
        commandLine("bash", "tools/translate/build-bergamot.sh", "arm64-v8a", "x86_64")
        environment("OUT_ANDROID", file(bergamotJniDir).absolutePath)
        environment("BERGAMOT_SRC", file(bergamotSrcDir).absolutePath)
        doFirst {
            environment("NDK", resolveNdk().absolutePath)
        }
        inputs.files(
            rootDir.resolve("tools/translate/build-bergamot.sh"),
            fileTree(rootDir.resolve("tools/translate/bergamot_jni")),
        )
        outputs.dir(file(bergamotJniDir))
        onlyIf { !providers.gradleProperty("feedhub.bergamotAndroidDir").isPresent }
        outputs.upToDateWhen {
            val newestSrc = (
                rootDir.resolve("tools/translate/bergamot_jni").walkTopDown().filter { it.isFile }.toList() +
                    rootDir.resolve("tools/translate/build-bergamot.sh")
                ).maxOfOrNull { it.lastModified() } ?: 0L
            val arm64 = file("$bergamotJniDir/arm64-v8a/libfeedhub_bergamot.so")
            val x64 = file("$bergamotJniDir/x86_64/libfeedhub_bergamot.so")
            arm64.isFile && x64.isFile && arm64.lastModified() >= newestSrc && x64.lastModified() >= newestSrc
        }
    }

    val stageBergamotAndroid = tasks.register<Copy>("stageBergamotAndroid") {
        group = "build"
        description = "把交叉编译好的 libfeedhub_bergamot.so（arm64-v8a + x86_64）拷进 jniLibs"
        dependsOn(buildBergamotAndroid)
        val root = file(bergamotJniDir)
        inputs.property("dir", root.absolutePath)
        inputs.files(fileTree(root) { include("*/libfeedhub_bergamot.so") })
        outputs.dir(bergamotJniLibsStaged)
        from(root) { include("*/libfeedhub_bergamot.so") }
        into(bergamotJniLibsStaged)
        doLast {
            val out = bergamotJniLibsStaged.get().asFile
            val found = fileTree(out) { include("*/libfeedhub_bergamot.so") }.files
            if (found.isEmpty()) {
                logger.lifecycle(
                    "翻译：这个包里没有 libfeedhub_bergamot.so（$root 为空），App 里翻译将显示不可用。",
                )
            } else {
                found.sortedBy { it.absolutePath }.forEach {
                    logger.lifecycle("翻译：已放进 jniLibs ${it.parentFile.name}/${it.name}（${it.length() / 1024} KB）")
                }
            }
        }
    }
    tasks.matching { it.name.contains("JniLibFolders") }.configureEach {
        dependsOn(stageBergamotAndroid)
    }
    android.sourceSets["main"].jniLibs.srcDir(bergamotJniLibsStaged.get().asFile)
}


dependencies {
    implementation(project(":app"))
    // DebugStartActivity（src/debug）是个 ComponentActivity：`setContent` / `enableEdgeToEdge` 从这里来。
    // :app 的 androidMain 也依赖它，但那是 :app 的 implementation，传不到这个模块的编译类路径上。
    implementation(libs.androidx.activity.compose)
    // DebugStartActivity 里要写 feedhub.core.FeedItem 这个类型（androidDemoItem 的返回值 / App 的 initialDetail），
    // 而 :app 对 :core 的依赖是 implementation，同样传不过来。只是编译期可见性，APK 里本来就带着 core 的类。
    implementation(project(":core"))
    // DebugStartActivity 里"正在取真实内容…"那个等待界面用了 Compose 的 Box / Text（material3）。
    // :app 的 Compose 依赖同样是 implementation，传不过来，这里显式加一份（APK 里本来就有，不额外变大）。
    implementation(libs.compose.material3)
    // Baseline Profile：装上 / 升级后把 APK 里的 baseline profile 交给 ART，
    // 冷启动时先编译启动路径上的类。
    // 它的 ProfileInstallReceiver 由这个库的 manifest 自动合并进来，不用手写。
    implementation(libs.androidx.profileinstaller)
    // 离线单元测试：守"release 不认 --ez demo true"的纯判定，见 src/test 的 DemoFlagTest。
    // 用 androidx.test.ext:junit 是为了拿到它带的 junit 4（不额外引新版本）。
    testImplementation(libs.androidx.test.ext.junit)
    // instrumented 测试：在模拟器上真的建 QuickJS 运行时、跑 yt-dlp 的 EJS
    // 解 n/sig，答案和 JVM 单测逐字对比（见 src/androidTest 的 YoutubeEjsAndroidTest）。
    // - ext.junit 给 AndroidJUnit4（junit 4 的安卓 runner），runner 给 AndroidJUnitRunner（`am instrument` 用）；
    // - :plugin-registry 给测试拿注册表：安卓上"YouTube 在可用名单里、没有灰显的平台"要断言它，签名引擎的自检
    //   也要从 `InstalledPlugins.catalog` 拿到 YouTube 的 Source，再按 **core 的 `ScriptEngineProbe`** 问一句
    //   ——**不能**直接依赖 `:source-youtube`（`PluginBoundaryTest`：从 plugins.toml 删掉一个插件，
    //   别的模块照样编得过；所以 androidTest 也不许 import 插件的类）。
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(project(":plugin-registry"))

    // Rust 核心在安卓上走 JNA 的 aar（里面带各 ABI 的 libjnidispatch.so；普通 jar 只有桌面那几份）。
    // 桌面那边 JNA 是 vlcj 带的 jna-jpms，:app 的 configurations 把普通 jna 排除掉了，所以不冲突。
    implementation("net.java.dev.jna:jna:5.17.0@aar")
    // 探针（DebugStartActivity 的 `--ez rustprobe true`）要直接调 Rust 核心的纯函数
    implementation(project(":rust-bridge"))

    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.androidx.test.runner)
}

// quickjs 的安卓变体
// n/sig 要跑 yt-dlp 的 EJS，引擎是 quickjs-kt。它有两个 JVM 侧的变体：
// - `-jvm`：桌面 jar，里面是 linux / mac / windows 的 .so（JVM 版 `NativeLibraryLoader` 自己解压加载）；
// - `-android`：AAR，`classes.jar` + `jni/<abi>/libquickjs.so`（安卓版走 `System.loadLibrary`）。
// `:source-youtube` 是 kotlin("jvm") 模块、依赖写的是 KMP 的**根坐标**，靠 Gradle 的变体属性挑：
// 安卓运行时类路径（`org.gradle.jvm.environment=android` + `org.jetbrains.kotlin.platform.type=androidJvm`）
// 解析成 AAR，桌面 / cli / 单测是 jar——所以这里**不用** exclude / 换坐标，写一行根坐标就是对的
// （见 source-youtube/build.gradle.kts 的注释）。怎么确认它真的选对了：
// 1. `./gradlew :androidApp:dependencies --configuration releaseRuntimeClasspath` 里 quickjs 那行是
//    `io.github.dokar3:quickjs-kt-android:1.0.5`；
// 2. 打出来的 APK 里有 `lib/arm64-v8a/libquickjs.so` 和 `lib/x86_64/libquickjs.so`（AAR 的 jni 被 AGP 合进 APK）；
// 3. `androidTest` 的 `YoutubeEjsAndroidTest` 在模拟器上真的建运行时解 n/sig（.so 加载不了它一定红），
//    并且断言 APK 里**没有**桌面 jar 的 `jni/linux_x64/libquickjs.so` 资源。
