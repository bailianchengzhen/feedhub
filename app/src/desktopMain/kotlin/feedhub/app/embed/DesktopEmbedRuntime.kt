package feedhub.app.embed

import java.io.File

/**
 * 桌面本地向量的文件从哪儿来：**运行库 + 模型都不进 git**，
 * 和安卓那边的 `stageEmbedModel` / `-Pfeedhub.embedModelDir` 一个套路，桌面这边由
 * `app/build.gradle.kts` 的 `stageDesktopEmbed` 在构建时拷进 App 的 resources（`lib/app/resources/embed-ort/`）。
 *
 * 目录里就该有这 5 个文件（打包后全在同一个目录，模型的两份必须在同一层——ONNX 的外部权重是相对路径）：
 * - `libonnxruntime.so.1.31.0`：ONNX Runtime 1.31.0 的 linux-x64 运行库（**和安卓那版同一个版本号**）；
 * - `libfeedhub_onnx_embed.so`：我们自己的 JNI shim（`src/desktopMain/cpp/feedhub_onnx_embed.c`）；
 * - `model_q4_acc4.onnx` + `model_q4.onnx_data`：安卓 CPU 路径用的**同一张 q4 图**（`accuracy_level=4`）；
 * - `tokenizer.json`：Gemma 的词表（`GemmaTokenizer` 读它，安卓 / 桌面共用同一份）。
 *
 * 找目录的顺序（先到先得，**没打包 / 没模型就安静地不可用**）：
 * 1. 系统属性 `feedhub.embed.ort.dir`（`build.gradle.kts` 给截图 / `devMain` / 测试那几个任务设的）；
 * 2. `compose.application.resources.dir/embed-ort`（deb 装好之后就是它）；
 * 3. 环境变量 `FEEDHUB_EMBED_ORT_DIR`（整包目录）；
 * 4. 库和模型分开放（开发 / 测试）：库 `FEEDHUB_ORT_DIR`，模型 `FEEDHUB_EMBED_MODEL_DIR`（和测试共用）。
 */
object DesktopEmbedRuntime {
    /** resources 目录下的子目录名（`common/embed-ort` → `lib/app/resources/embed-ort`）。 */
    const val DIR = "embed-ort"

    const val ORT_LIB = "libonnxruntime.so.1.31.0"
    const val JNI_LIB = "libfeedhub_onnx_embed.so"
    const val MODEL_FILE = "model_q4_acc4.onnx"
    const val WEIGHTS_FILE = "model_q4.onnx_data"
    const val TOKENIZER_FILE = "tokenizer.json"

    /** 整包目录（5 个文件都在）的覆盖。 */
    const val ENV_DIR = "FEEDHUB_EMBED_ORT_DIR"

    /** 只有运行库 + shim 的目录（开发 / 测试）。 */
    const val ENV_ORT_DIR = "FEEDHUB_ORT_DIR"

    /** 模型目录（开发 / 测试；和 `EmbedModelDir.kt` 的测试用同一个名字）。 */
    const val ENV_MODEL_DIR = "FEEDHUB_EMBED_MODEL_DIR"

    /** 显式指定整包目录的系统属性（Gradle 的任务会设它）。 */
    const val PROP_DIR = "feedhub.embed.ort.dir"

    /** 一次解析出来的 5 个文件（可能缺；[complete] 才是"能用"）。 */
    class Assets(
        val ortLib: File?,
        val jniLib: File?,
        val model: File?,
        val weights: File?,
        val tokenizer: File?,
        /** 从哪儿找到的（日志 / 设置页排查用，不含用户内容）。 */
        val source: String,
    ) {
        val complete: Boolean
            get() = listOf(ortLib, jniLib, model, weights, tokenizer).all { it != null && it.isFile }

        val missing: List<String>
            get() = listOf(
                ORT_LIB to ortLib, JNI_LIB to jniLib, MODEL_FILE to model,
                WEIGHTS_FILE to weights, TOKENIZER_FILE to tokenizer,
            ).filter { (_, f) -> f == null || !f.isFile }.map { it.first }

        fun describe(): String = if (complete) {
            fun mb(f: File?): String = "%.1f".format((f?.length() ?: 0L) / 1024.0 / 1024.0)
            "$source（${mb(model)} MB 图 + ${mb(weights)} MB 权重 + ${mb(tokenizer)} MB 词表）"
        } else {
            "缺 ${missing.joinToString(" / ")}（找过：$source）"
        }
    }

    /** 现在的文件够不够 / 在哪儿（每次现算：换目录不用重启；只有很小的 stat 调用）。 */
    fun assets(): Assets {
        val dir = allInOneDir()
        if (dir != null) {
            return Assets(
                ortLib = File(dir, ORT_LIB),
                jniLib = File(dir, JNI_LIB),
                model = File(dir, MODEL_FILE),
                weights = File(dir, WEIGHTS_FILE),
                tokenizer = File(dir, TOKENIZER_FILE),
                source = dir.absolutePath,
            )
        }
        val libs = libsDir()
        val modelDir = modelDir()
        return Assets(
            ortLib = libs?.resolve(ORT_LIB),
            jniLib = libs?.resolve(JNI_LIB),
            model = modelDir?.resolve(MODEL_FILE),
            weights = modelDir?.resolve(WEIGHTS_FILE),
            // 词表和 `onnx/` 同级（模型目录里既有 linux 那份也有别的；词表只在上层有一份）
            tokenizer = modelDir?.parentFile?.resolve(TOKENIZER_FILE) ?: modelDir?.resolve(TOKENIZER_FILE),
            source = "${libs ?: "没有运行库目录"} + ${modelDir ?: "没有模型目录"}",
        )
    }

    /** 5 个文件都在一个目录里：系统属性 → 打包后的 resources → 环境变量。 */
    private fun allInOneDir(): File? = sequenceOf(
        System.getProperty(PROP_DIR)?.takeIf { it.isNotBlank() }?.let { File(it) },
        System.getProperty("compose.application.resources.dir")?.takeIf { it.isNotBlank() }
            ?.let { File(it).resolve(DIR) },
        System.getenv(ENV_DIR)?.takeIf { it.isNotBlank() }?.let { File(it) },
    ).filterNotNull().firstOrNull { dir ->
        File(dir, ORT_LIB).isFile && File(dir, JNI_LIB).isFile && File(dir, MODEL_FILE).isFile
    }

    /** 只有运行库 + shim 的目录（开发 / 测试）。 */
    private fun libsDir(): File? = sequenceOf(
        System.getProperty(PROP_DIR)?.takeIf { it.isNotBlank() }?.let { File(it) },
        System.getProperty("compose.application.resources.dir")?.takeIf { it.isNotBlank() }
            ?.let { File(it).resolve(DIR) },
        System.getenv(ENV_ORT_DIR)?.takeIf { it.isNotBlank() }?.let { File(it) },
    ).filterNotNull().firstOrNull { File(it, ORT_LIB).isFile && File(it, JNI_LIB).isFile }

    /**
     * 模型目录（`.onnx` + `.onnx_data` 所在的那一层）。
     *
     * `FEEDHUB_EMBED_MODEL_DIR` 既可以直接指到 `onnx/` 那一层，也可以指到它的上一层（真正的那份在 `onnx/` 里）。
     */
    private fun modelDir(): File? {
        val base = System.getenv(ENV_MODEL_DIR)?.takeIf { it.isNotBlank() }?.let { File(it) } ?: return null
        return listOf(base, base.resolve("onnx")).firstOrNull { File(it, MODEL_FILE).isFile }
    }

    private var loaded: Result<Unit>? = null

    /**
     * 把两个 `.so` 装进来（**只装一次**，失败也记住，不重试）。返回 null = 成功，否则是给人看的原因。
     *
     * 先装运行库、再装 shim：shim 链接的是 `libonnxruntime.so.1`，早一点装载能在报错时给出说得清的消息。
     */
    @Synchronized
    fun load(assets: Assets = assets()): String? {
        loaded?.let { return it.exceptionOrNull()?.message }
        val ort = assets.ortLib
        val jni = assets.jniLib
        if (ort == null || jni == null || !ort.isFile || !jni.isFile) {
            val reason = "找不到运行库：${assets.describe()}"
            loaded = Result.failure(IllegalStateException(reason))
            return reason
        }
        val result = runCatching {
            System.load(ort.absolutePath)
            System.load(jni.absolutePath)
        }
        loaded = result
        return result.exceptionOrNull()?.let { e -> "${e::class.simpleName}: ${e.message}" }
    }

    /** 运行库版本串（没装载 / 没模型时是 null）。 */
    fun version(): String? = runCatching { if (loaded?.isSuccess == true) OrtNative.nativeVersion() else null }.getOrNull()
}
