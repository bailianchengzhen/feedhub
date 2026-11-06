package feedhub.app.translate

import feedhub.app.SettingsStore
import java.io.File

/**
 * 桌面端 Bergamot 本地翻译运行库与内置模型定位器（对标 `DesktopEmbedRuntime`）：
 * 运行库（`libfeedhub_bergamot.so`）与 Firefox `enzh` 模型均不进 git，开启 `-Pfeedhub.translate=true` 时由
 * `app/build.gradle.kts` 的 `stageDesktopTranslate` 校验 SHA-256 后放入 `build/desktop-translate` 与
 * deb 的 `lib/app/resources/translate-bergamot/`；默认基础版构建不打包任何翻译产物（0 字节）。
 *
 * 查找顺序（先到先得）：
 * 1. 系统属性 `feedhub.translate.bergamot.dir`（Gradle 测试 / 截图 / 开发运行注入）；
 * 2. `compose.application.resources.dir/translate-bergamot`（deb 安装包运行时路径）；
 * 3. 环境变量 `FEEDHUB_TRANSLATE_BERGAMOT_DIR`（整包覆盖）或 `FEEDHUB_BERGAMOT_JNI_DIR` + `FEEDHUB_BERGAMOT_MODEL_DIR`。
 */
object DesktopTranslateRuntime {
    const val DIR = "translate-bergamot"
    const val PROP_DIR = "feedhub.translate.bergamot.dir"
    const val ENV_DIR = "FEEDHUB_TRANSLATE_BERGAMOT_DIR"
    const val ENV_JNI_DIR = "FEEDHUB_BERGAMOT_JNI_DIR"
    const val ENV_MODEL_DIR = "FEEDHUB_BERGAMOT_MODEL_DIR"

    private var loaded: Result<Unit>? = null

    /** 查找 `libfeedhub_bergamot.so`；不存在返回 `null`（不触发加载）。 */
    fun jniLib(): File? = candidateJniDirs()
        .map { File(it, BergamotModelFiles.JNI_LIB) }
        .firstOrNull { it.isFile && it.length() > 0L }

    /** 内置 `enzh` 模型文件集合。 */
    fun builtInModelFiles(): BergamotModelFiles {
        val dir = candidateModelDirs().firstOrNull { BergamotModelFiles.fromDirectory(it).complete }
            ?: candidateModelDirs().firstOrNull()
        return if (dir != null) {
            BergamotModelFiles.fromDirectory(dir, "内置英译中模型（${dir.absolutePath}）")
        } else {
            BergamotModelFiles(null, null, null, null, null, "未打包内置模型")
        }
    }

    /** 是否为旧版 HY-MT GGUF 单文件路径（老 `state.json` 遗留值，自动忽略并回退到内置 Bergamot 模型）。 */
    fun isLegacyGgufPath(raw: String): Boolean {
        val trimmed = raw.trim()
        return trimmed.isNotEmpty() && trimmed.endsWith(".gguf", ignoreCase = true)
    }

    /**
     * 解析当前生效的 Bergamot 模型文件：
     * - 设置项为空或为旧版 `.gguf` 文件路径 → 使用内置 `enzh` 模型；
     * - 设置项填了非空目录路径 → 从该自定义目录读取 Bergamot 模型四件套。
     */
    fun resolveModelFiles(customModelPath: String): BergamotModelFiles {
        val trimmed = customModelPath.trim()
        if (trimmed.isEmpty() || isLegacyGgufPath(trimmed)) {
            return builtInModelFiles()
        }
        val dir = File(trimmed)
        return BergamotModelFiles.fromDirectory(dir, "自定义模型目录（${dir.absolutePath}）")
    }

    /** 当前桌面组装是否具备可用的翻译引擎运行库与模型（供 `AppGraph` 判断 `translation.available`）。 */
    fun isBundledOrConfigured(customModelPath: String = ""): Boolean =
        jniLib() != null && (builtInModelFiles().complete || resolveModelFiles(customModelPath).complete)

    /** 给设置页「翻译（进阶）」展示的一行可读来源说明。 */
    fun describe(customModelPath: String): String {
        val jni = jniLib() ?: return "当前构建未包含翻译运行库（${BergamotModelFiles.JNI_LIB}）。"
        val files = resolveModelFiles(customModelPath)
        val ver = version()?.let { "$it，" }.orEmpty()
        val legacyNote = if (isLegacyGgufPath(customModelPath)) "；已自动忽略旧版 GGUF 路径配置" else ""
        return if (files.complete) {
            val mb = "%.1f".format(files.totalBytes() / 1024.0 / 1024.0)
            "模型与运行库就绪（${ver}共 $mb MB，$jni$legacyNote）"
        } else {
            "缺少模型文件：${files.missing.joinToString(" / ")}（查找位置：${files.source}）"
        }
    }

    /** 按全路径加载 `libfeedhub_bergamot.so`（整个 JVM 生命周期只加载一次）；成功返回 `null`，失败返回原因。 */
    @Synchronized
    fun load(): String? {
        loaded?.let { prev ->
            return prev.exceptionOrNull()?.let { e -> "${e::class.simpleName}: ${e.message}" }
        }
        val jni = jniLib()
        if (jni == null) {
            val msg = "找不到 ${BergamotModelFiles.JNI_LIB}"
            // 不缓存“文件尚不存在”的失败，允许测试稍后放置文件重试
            return msg
        }
        val result = runCatching {
            System.load(jni.absolutePath)
        }
        loaded = result
        return result.exceptionOrNull()?.let { e -> "${e::class.simpleName}: ${e.message}" }
    }

    /** 已加载 `.so` 时返回原生版本串（如 `bergamot-0.6.0`），未加载时返回 `null`。 */
    fun version(): String? = runCatching {
        if (loaded?.isSuccess == true) BergamotNative.nativeVersion() else null
    }.getOrNull()

    private fun candidateJniDirs(): List<File> = listOfNotNull(
        System.getProperty(PROP_DIR)?.takeIf { it.isNotBlank() }?.let(::File),
        System.getProperty("compose.application.resources.dir")?.takeIf { it.isNotBlank() }
            ?.let { File(it, DIR) },
        System.getenv(ENV_DIR)?.takeIf { it.isNotBlank() }?.let(::File),
        System.getenv(ENV_JNI_DIR)?.takeIf { it.isNotBlank() }?.let(::File),
    )

    private fun candidateModelDirs(): List<File> = listOfNotNull(
        System.getProperty(PROP_DIR)?.takeIf { it.isNotBlank() }?.let(::File),
        System.getProperty("compose.application.resources.dir")?.takeIf { it.isNotBlank() }
            ?.let { File(it, DIR) },
        System.getenv(ENV_DIR)?.takeIf { it.isNotBlank() }?.let(::File),
        System.getenv(ENV_MODEL_DIR)?.takeIf { it.isNotBlank() }?.let(::File),
    )
}

/** 构造桌面版进程内 Bergamot 翻译引擎（首次翻译才加载 `.so` 与模型，空闲 5 分钟自动卸载）。 */
fun createDesktopTranslationEngine(settings: SettingsStore): BergamotTranslationEngine =
    BergamotTranslationEngine(
        loadLibrary = { DesktopTranslateRuntime.load() },
        resolveModelFiles = { DesktopTranslateRuntime.resolveModelFiles(settings.value.translationModelPath) },
        idleTimeoutMs = BergamotTranslationEngine.DESKTOP_IDLE_TIMEOUT_MS,
        unloadOnBackground = false,
    )
