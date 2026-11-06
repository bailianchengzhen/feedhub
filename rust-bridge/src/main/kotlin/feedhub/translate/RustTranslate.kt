package feedhub.translate

import feedhub.core.Block
import feedhub.core.DetailTarget
import feedhub.core.Platform
import feedhub.core.Span
import feedhub.core.UserRef
import feedhub.rust.HostedCall
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiDetailBlock
import uniffi.feedhub_ffi.FfiDetailSpan
import uniffi.feedhub_ffi.FfiDetailTarget
import uniffi.feedhub_ffi.FfiException
import uniffi.feedhub_ffi.FfiTrBackend
import uniffi.feedhub_ffi.FfiTrBackendDevice
import uniffi.feedhub_ffi.FfiTrConfig
import uniffi.feedhub_ffi.FfiTrDevice
import uniffi.feedhub_ffi.FfiTrDetector
import uniffi.feedhub_ffi.FfiTrEngine
import uniffi.feedhub_ffi.FfiTrException
import uniffi.feedhub_ffi.FfiTrGate
import uniffi.feedhub_ffi.FfiTrJob
import uniffi.feedhub_ffi.FfiTrPhase
import uniffi.feedhub_ffi.FfiTrPhaseInput
import uniffi.feedhub_ffi.FfiTrQueue
import uniffi.feedhub_ffi.FfiTrStatus
import uniffi.feedhub_ffi.FfiTrStatusHost
import uniffi.feedhub_ffi.FfiTrStore
import uniffi.feedhub_ffi.FfiTrSubmit
import uniffi.feedhub_ffi.FfiTrTimeouts
import uniffi.feedhub_ffi.FfiUserRef
import uniffi.feedhub_ffi.HostStore
import java.nio.file.Path

/**
 * **翻译的 Rust 门面**。
 *
 * 一个包把三样东西分开给出去（边界见 `rust/crates/feedhub-core/src/translate/mod.rs`）：
 *
 * | 类 | 干什么 | 对应 Rust |
 * |---|---|---|
 * | [RustTranslate] | 纯函数：语言标签、门控用的 `--list-devices` 解析 / 挑 AMD 核显 / 环境变量、chat 请求体与响应、富文本的"翻哪几段 / 怎么拼回" | `translate::lang` / `server` / `engine` / `text` |
 * | [RustTranslateStore] | 磁盘缓存 + 语言识别 + 门控判定 | `translate::cache` / `lang` |
 * | [RustTranslateQueue] | 排队状态机（新的优先 / 并发上限 / 去重 / 取消） | `translate::queue` |
 * | [RustTranslateEngine] | `llama-server` 的启停 + 一次 chat 请求 | `translate::engine` |
 *
 * **不留开关**：生产路径直接用这几份，Kotlin 原来的实现（`LlamaServerPolicy` / `VulkanDevices` /
 * `LinguaLanguageIdentifier` / `TranslationCache`）只剩 `app/src/desktopTest` 里的对拍参照。
 *
 * 三条口径：
 * - **`.so` 加载失败就抛**（不悄悄退回 Kotlin 实现）——和 `RustBili` 那些门面一样；
 * - **错误文案原样带过去**（`FfiTrError.detail`），界面直接显示"找不到模型：…"这种话；
 * - **不发请求的地方绝不多发**：门控 / 缓存 / 排队一次 HTTP 都不发。
 */
object RustTranslate {
    /** [`tag`] 是不是目标语言（`languageMatchesTarget`，Rust 侧 `translate::lang::matches_target`）。 */
    fun matchesTarget(tag: String, target: String): Boolean = guard {
        uniffi.feedhub_ffi.trMatchesTarget(tag, target)
    }

    /** 解析 `llama-server --list-devices` 的输出（只认 `VulkanN: 名字` 这种行）。 */
    fun parseDevices(output: String): List<LlamaDevice> = guard {
        uniffi.feedhub_ffi.trParseDevices(output).map { it.toModel() }
    }

    /** 按名字挑 AMD 核显（含 `RADV` 或 `Radeon`）；挑不到是 null。 */
    fun chooseDevice(devices: List<LlamaDevice>): LlamaDevice? = guard {
        uniffi.feedhub_ffi.trChooseDevice(devices.map { it.toFfi() })?.toModel()
    }

    /** 启动 `llama-server` 的 Vulkan 环境变量（有序；**不带** `GGML_VK_VISIBLE_DEVICES`）。 */
    fun vkDriverEnv(server: String, icdFile: String? = null): Map<String, String> = guard {
        uniffi.feedhub_ffi.trVkDriverEnv(server, icdFile).associate { it.name to it.value }
    }

    /**
     * 解析 `--list-devices` 的**完整**输出（`HTP0` / `GPUOpenCL` / `VulkanN` 都认，日志行不算）。
     *
     * 和 [parseDevices] 的分工：那个只认 `VulkanN`（桌面红线），这个给手机挑后端用。
     */
    fun parseBackendDevices(output: String): List<TrBackendDevice> = guard {
        uniffi.feedhub_ffi.trParseBackendDevices(output).map { it.toModel() }
    }

    /** 这台机器是手机（有 HTP / GPUOpenCL）还是桌面（只有 Vulkan）。 */
    fun isMobile(devices: List<TrBackendDevice>): Boolean = guard {
        uniffi.feedhub_ffi.trIsMobile(devices.map { it.toFfi() })
    }

    /** 这台机器上**能用**的后端档位（顺序固定：NPU → GPU → NPU+GPU → CPU；CPU 一定在）。 */
    fun usableBackends(devices: List<TrBackendDevice>): List<TrBackend> = guard {
        uniffi.feedhub_ffi.trUsableBackends(devices.map { it.toFfi() }).map { it.toModel() }
    }

    /** 设置页那一行显示的字（`NPU（Hexagon HTP）` / …）。 */
    fun backendLabel(backend: TrBackend): String = guard {
        uniffi.feedhub_ffi.trBackendLabel(backend.toFfi())
    }

    /** "上次降到哪一档"那个记忆的键（清缓存 / 排障用）。 */
    fun backendKey(): String = guard { uniffi.feedhub_ffi.trBackendKey() }

    /** 一句话的输出看起来是不是坏的（空串 / 一大半是替换字符）。降级判据之一。 */
    fun looksBroken(text: String): Boolean = guard { uniffi.feedhub_ffi.trLooksBroken(text) }

    /** 一次 chat 请求的 body（逐字节和 Kotlin 原版一样）。 */
    fun chatPayload(text: String, target: String): String = guard {
        uniffi.feedhub_ffi.trChatPayload(text, target)
    }

    /** 解析 chat 响应（`choices[0].message.content`）。 */
    fun parseChatResponse(body: String): String = guard {
        uniffi.feedhub_ffi.trParseChatResponse(body)
    }

    /** 每一块要翻的原文（不可翻的给 null）。 */
    fun planBlocks(blocks: List<Block>): List<String?> = guard {
        uniffi.feedhub_ffi.trPlanBlocks(blocks.map { it.toFfiBlock() })
    }

    /** 把译文拼回富文本（`texts[i]` 是第 `i` 块的译文；null = 不翻 / 没译）。 */
    fun applyBlocks(blocks: List<Block>, texts: List<String?>): List<Block> = guard {
        uniffi.feedhub_ffi.trApplyBlocks(blocks.map { it.toFfiBlock() }, texts).map { it.toBlock() }
    }

    /** 整段富文本里所有可翻的文字。 */
    fun translatableTexts(blocks: List<Block>): List<String> = guard {
        uniffi.feedhub_ffi.trTranslatableTexts(blocks.map { it.toFfiBlock() })
    }

    // 入口的判定与状态

    /** 这段文字现在该显示译文吗（`false` = 原文：点了「显示原文」/ 没译文 / 译文本就是原文）。 */
    fun showingTranslation(showOriginal: Boolean, translated: String?, original: String): Boolean = guard {
        uniffi.feedhub_ffi.trShowingTranslation(showOriginal, translated, original)
    }

    /** 现在要画哪一份文字（原文 / 译文）。 */
    fun display(original: String, translated: String?, showOriginal: Boolean): String = guard {
        uniffi.feedhub_ffi.trDisplay(original, translated, showOriginal)
    }

    /**
     * 富文本版：现在该画译文吗（`texts[i]` 是第 `i` 块的译文，null = 这块不翻 / 没译）。
     *
     * 和 [showingTranslation] 的区别：拆出来的块是句子片段，"译文 == 原文"没有意义，
     * 所以这里只要求"有任意一块真翻好了"。
     */
    fun showingBlocks(showOriginal: Boolean, texts: List<String?>): Boolean = guard {
        uniffi.feedhub_ffi.trShowingBlocks(showOriginal, texts)
    }

    /** 灰字里那个语言名（`en` → "英语"；认不出的写"外语"）。 */
    fun languageLabel(tag: String?): String = guard {
        uniffi.feedhub_ffi.trLanguageLabel(tag)
    }

    /** 引擎状态 → "模型还在加载"（「正在翻译…」/「正在加载翻译模型…」那一档）。 */
    fun modelLoading(status: TrEngineStatus): Boolean = guard {
        uniffi.feedhub_ffi.trModelLoading(status.toFfi())
    }

    /** 入口的状态（[FfiTrPhase]；界面文案映射见 `feedhub.app.translate.TranslationPhase`）。 */
    fun phase(input: TrPhaseInput): FfiTrPhase = guard {
        uniffi.feedhub_ffi.trPhase(
            FfiTrPhaseInput(
                visible = input.visible,
                skipped = input.skipped,
                translating = input.translating,
                translated = input.translated,
                failed = input.failed,
                modelLoading = input.modelLoading,
            ),
        )
    }

    /**
     * 统一出口：加载 `.so`，把 `FfiTrError` 折成 `IllegalStateException`（Kotlin 原版就是 `error("…")`，
     * 上层 `TranslationStore` 拿 `e.message` 直接显示）。
     */
    internal inline fun <T> guard(block: () -> T): T {
        try {
            RustNative.ensureLoaded()
            return block()
        } catch (e: FfiTrException) {
            val detail = when (e) {
                is FfiTrException.Failed -> e.detail
            }
            throw IllegalStateException(detail, e)
        } catch (e: FfiException) {
            throw IllegalStateException(e.toString(), e)
        } catch (e: UnsatisfiedLinkError) {
            throw IllegalStateException("Rust 核心加载失败（找不到 libfeedhub_ffi.so）：${e.message}", e)
        } catch (e: IllegalStateException) {
            throw e
        }
    }
}

/** 一个 Vulkan 设备（`--list-devices` 的一行）。 */
/**
 * 手机上翻译实际用的后端。**声明顺序就是候选顺序**：NPU → GPU → NPU+GPU → CPU。
 */
enum class TrBackend {
    NPU,
    GPU,
    NPU_GPU,
    CPU,
}

/** `--list-devices` 里的一行（手机上挑后端用；不限 `VulkanN`）。 */
data class TrBackendDevice(val name: String, val description: String)

internal fun FfiTrBackend.toModel(): TrBackend = when (this) {
    FfiTrBackend.NPU -> TrBackend.NPU
    FfiTrBackend.GPU -> TrBackend.GPU
    FfiTrBackend.NPU_GPU -> TrBackend.NPU_GPU
    FfiTrBackend.CPU -> TrBackend.CPU
}

internal fun TrBackend.toFfi(): FfiTrBackend = when (this) {
    TrBackend.NPU -> FfiTrBackend.NPU
    TrBackend.GPU -> FfiTrBackend.GPU
    TrBackend.NPU_GPU -> FfiTrBackend.NPU_GPU
    TrBackend.CPU -> FfiTrBackend.CPU
}

internal fun FfiTrBackendDevice.toModel(): TrBackendDevice = TrBackendDevice(name, description)

internal fun TrBackendDevice.toFfi(): FfiTrBackendDevice =
    FfiTrBackendDevice(name = name, description = description)

data class LlamaDevice(val index: Int, val name: String) {
    /** `llama-server -dev` 参数里的设备名。 */
    val argument: String get() = "Vulkan$index"
}

/**
 * [`RustTranslate.phase`] 的输入：界面现成的布尔量（对应 Rust `translate::entry::Input`）。
 *
 * 这一组判断**不进 Rust 的状态**：界面每一帧现算，Rust 只回答"该显示哪一档"。
 */
data class TrPhaseInput(
    val visible: Boolean = false,
    val skipped: Boolean = false,
    val translating: Boolean = false,
    val translated: Boolean = false,
    val failed: Boolean = false,
    val modelLoading: Boolean = false,
)

internal fun FfiTrDevice.toModel(): LlamaDevice = LlamaDevice(index, name)

internal fun LlamaDevice.toFfi(): FfiTrDevice = FfiTrDevice(index = index, name = name)

// ===========================================================================
// Block / Span ⇄ FfiDetailBlock / FfiDetailSpan
// ===========================================================================
//
// 富文本的翻译是"整段进去、整段出来"（Rust 那边要看到块结构才知道哪些能翻），所以需要
// Kotlin → FFI 这一向。反过来的映射早就有（`RustModel.toBlock`），这里只补去的那一半。
// 两个方向都是逐字段 1:1（`detail::Block` 和 Kotlin `Block` 的字段一一对应），
// 对拍 `TranslateTextParityTest` 拿十几种块（段落 / 标题 / 引用 / 代码 / 图片 / 卡片 / 视频 / 分割线 +
// 七种 Span）走一圈，逐字段比对"原样回来"。

internal fun Block.toFfiBlock(): FfiDetailBlock = when (this) {
    is Block.Paragraph -> FfiDetailBlock.Paragraph(spans.map { it.toFfiSpan() })
    is Block.Heading -> FfiDetailBlock.Heading(spans.map { it.toFfiSpan() })
    is Block.Quote -> FfiDetailBlock.Quote(spans.map { it.toFfiSpan() })
    is Block.Code -> FfiDetailBlock.Code(text = text, language = language)
    is Block.Image -> FfiDetailBlock.Image(
        url = url,
        width = width,
        height = height,
        original = original,
        caption = caption,
    )
    is Block.Card -> FfiDetailBlock.Card(
        title = title,
        subtitle = subtitle,
        cover = cover,
        url = url,
        target = target?.toFfiTarget(),
    )
    is Block.Video -> FfiDetailBlock.Video(
        id = id,
        partId = partId,
        cover = cover,
        durationSec = durationSec,
        width = width,
        height = height,
        title = title,
        url = url,
    )
    Block.Divider -> FfiDetailBlock.Divider
}

internal fun Span.toFfiSpan(): FfiDetailSpan = when (this) {
    is Span.Text -> FfiDetailSpan.Text(text = text, bold = bold, italic = italic)
    is Span.Link -> FfiDetailSpan.Link(text = text, url = url, target = target?.toFfiTarget())
    is Span.Mention -> FfiDetailSpan.Mention(text = text, user = user.toFfiUser())
    is Span.Emote -> FfiDetailSpan.Emote(text = text, url = url)
}

internal fun DetailTarget.toFfiTarget(): FfiDetailTarget = when (this) {
    is DetailTarget.Web -> FfiDetailTarget.Web(url = url)
    is DetailTarget.Native -> FfiDetailTarget.Native(id = id, platform = platform?.key)
}

internal fun UserRef.toFfiUser(): FfiUserRef = FfiUserRef(
    platform = platform.key,
    id = id,
    name = name,
    avatar = avatar,
    url = url,
)

/** `FfiDetailBlock` → Kotlin `Block`（和 `RustModel.FfiDetailBlock.toBlock()` 同一份口径）。 */
internal fun FfiDetailBlock.toBlock(): Block = when (this) {
    is FfiDetailBlock.Paragraph -> Block.Paragraph(spans.map { it.toSpan() })
    is FfiDetailBlock.Heading -> Block.Heading(spans.map { it.toSpan() })
    is FfiDetailBlock.Quote -> Block.Quote(spans.map { it.toSpan() })
    is FfiDetailBlock.Code -> Block.Code(text, language)
    is FfiDetailBlock.Image -> Block.Image(url, width, height, original, caption)
    is FfiDetailBlock.Card -> Block.Card(title, subtitle, cover, url, target?.toTarget())
    is FfiDetailBlock.Video -> Block.Video(
        id = id,
        partId = partId,
        cover = cover,
        durationSec = durationSec,
        width = width,
        height = height,
        title = title,
        url = url,
    )
    is FfiDetailBlock.Divider -> Block.Divider
}

internal fun FfiDetailSpan.toSpan(): Span = when (this) {
    is FfiDetailSpan.Text -> Span.Text(text, bold, italic)
    is FfiDetailSpan.Link -> Span.Link(text, url, target?.toTarget())
    is FfiDetailSpan.Mention -> Span.Mention(text, UserRef(Platform(user.platform), user.id, user.name, user.avatar, user.url))
    is FfiDetailSpan.Emote -> Span.Emote(text, url)
}

internal fun FfiDetailTarget.toTarget(): DetailTarget = when (this) {
    is FfiDetailTarget.Native -> DetailTarget.Native(id, platform?.let { Platform(it) })
    is FfiDetailTarget.Web -> DetailTarget.Web(url)
}

// ===========================================================================
// 缓存 + 语言识别 + 门控
// ===========================================================================

/** 语言识别器（Rust 侧 `FfiTrDetector`）：生产用 Rust 内置的 lingua，测试注假实现。 */
fun interface TranslateDetector {
    /** BCP 47 标签（`zh` / `en`）；识别不出来是 null（上层按外语处理）。 */
    fun detect(text: String): String?
}

/**
 * 翻译的**缓存 + 语言识别 + 门控**。
 *
 * @param cacheDir `~/.cache/feedhub`；null = 只放内存（安卓 / 测试 / 截图）。
 * @param detector 注一个识别器；null 时看 [builtin]：true = 用 Rust 版 lingua，
 *   false = 不识别（没有平台语言就直接翻——安卓 / 没有本地翻译的组装）。
 */
class RustTranslateStore(
    cacheDir: Path?,
    detector: TranslateDetector? = null,
    builtin: Boolean = false,
) {
    private val native: FfiTrStore by lazy {
        RustNative.ensureLoaded()
        FfiTrStore(
            cacheDir = cacheDir?.toString(),
            detector = detector?.let { d ->
                object : FfiTrDetector {
                    override fun detect(text: String): String? = d.detect(text)
                }
            },
            builtin = builtin,
        )
    }

    /** 缓存键（`SHA-256("lid2\0模型\0目标\0正文")` 的小写十六进制）。 */
    fun keyFor(modelPath: String, target: String, text: String): String = RustTranslate.guard {
        native.keyFor(modelPath, target, text)
    }

    fun cacheGet(key: String): String? = RustTranslate.guard { native.cacheGet(key) }

    fun cachePut(key: String, text: String) = RustTranslate.guard { native.cachePut(key, text) }

    fun clearCache() = RustTranslate.guard { native.cacheClear() }

    fun cacheSize(): Int = RustTranslate.guard { native.cacheLen().toInt() }

    /** 一次语言识别（`zh` / `en` / null）；生产走 [shouldAutoTranslate]。 */
    fun detect(text: String): String? = RustTranslate.guard { native.detect(text) }

    /** 这条内容要不要自动翻译（平台门控在语言识别之前）。 */
    fun shouldAutoTranslate(
        text: String,
        language: String?,
        platform: Platform?,
        enabled: Boolean,
        target: String,
        autoPlatforms: Collection<Platform>,
        sourceLanguages: Collection<String> = emptyList(),
    ): Boolean = RustTranslate.guard {
        native.shouldAutoTranslate(
            text = text,
            language = language,
            platform = platform?.key,
            gate = FfiTrGate(
                enabled = enabled,
                target = target,
                autoPlatforms = autoPlatforms.map { it.key },
                sourceLanguages = sourceLanguages.toList(),
            ),
        )
    }

    /**
     * 这条内容要不要出**手动**的「翻译」入口。
     *
     * 和 [shouldAutoTranslate] 的区别：**不看平台门控、不看 25 字节门槛**，只看"这本来就不是目标语言"
     * （中文内容不出现入口）；若指定了 [sourceLanguages]（如 Bergamot 英译中传 `["en"]`），则仅对匹配的源语言出入口。
     */
    fun entryVisible(
        text: String,
        language: String?,
        target: String,
        sourceLanguages: Collection<String> = emptyList(),
    ): Boolean = RustTranslate.guard {
        native.entryVisibleForSources(
            text = text,
            language = language,
            target = target,
            sourceLanguages = sourceLanguages.toList(),
        )
    }
}

// ===========================================================================
// 排队
// ===========================================================================

/** 排队中的一条请求（Rust `translate::queue::Job`）。 */
data class TranslateJob(val id: Long, val key: String, val text: String, val target: String)

/** [`RustTranslateQueue.submit`] 的答复。 */
data class TranslateSubmit(val created: Boolean, val id: Long)

/**
 * 翻译请求的排队器（状态机在 Rust，协程挂起在 `feedhub.app.translate.TranslationScheduler`）。
 *
 * 用法：`submit` → 新键就自己 `pump()` → 拿到 `TranslateJob` 就起协程跑 → 跑完 `finish`；
 * 调用方被取消时 `abandon`（还没派发的就被拿掉）。
 */
class RustTranslateQueue(parallelism: Int) {
    private val native: FfiTrQueue by lazy {
        RustNative.ensureLoaded()
        FfiTrQueue(parallelism.coerceAtLeast(1).toUInt())
    }

    val parallelism: Int get() = RustTranslate.guard { native.parallelism().toInt() }

    fun running(): Int = RustTranslate.guard { native.running().toInt() }

    fun pending(): Int = RustTranslate.guard { native.pending().toInt() }

    fun submit(key: String, text: String, target: String): TranslateSubmit = RustTranslate.guard {
        val s: FfiTrSubmit = native.submit(key, text, target)
        TranslateSubmit(s.created, s.id.toLong())
    }

    fun pump(): List<TranslateJob> = RustTranslate.guard {
        native.pump().map { j: FfiTrJob -> TranslateJob(j.id.toLong(), j.key, j.text, j.target) }
    }

    fun finish(id: Long) = RustTranslate.guard { native.finish(id.toULong()) }

    fun abandon(id: Long): Boolean = RustTranslate.guard { native.abandon(id.toULong()) }

    fun cancelPending(): List<Long> = RustTranslate.guard {
        native.cancelPending().map { it.toLong() }
    }
}

// ===========================================================================
// 引擎（llama-server）
// ===========================================================================

/**
 * 一次翻译的引擎配置（`FfiTrConfig` 的 Kotlin 版）。
 *
 * [mobile] 是**宿主的事实**（安卓 = `true`），不让 Rust 从设备列表猜：x86_64 模拟器上一个加速设备
 * 都没有，但仍然是手机形态（走候选档位 + CPU 兜底），而不是桌面的"没有 AMD 核显 就不启动"。
 */
data class TrEngineConfig(
    val enabled: Boolean,
    val modelPath: String,
    val serverPath: String,
    val cacheDir: Path? = null,
    val icdFile: String? = null,
    /** 当前 App 版本：手机上那条"降到哪一档"的记录按它判失效（升级后重新从 NPU 试）。 */
    val appVersion: String = "",
    /** 宿主是不是跑在手机上（安卓 = `true`）。**桌面永远 `false`**（AMD 核显 红线那条路）。 */
    val mobile: Boolean = false,
    /**
     * **强制只用这一档**（`null` = 自动降级，默认）：`"NPU"` / `"GPU"` / `"NPU_GPU"` / `"CPU_ONLY"`，
     * 大小写不敏感，认不出来 = 自动。只给排错和 `tools/translate/phone_bench.sh` 用。
     *
     * 老版本的 `"CPU"` **故意不认**（那时的字段必填、默认就是 CPU，是产品默认值不是用户选择）。
     */
    val forcedBackend: String? = null,
)

/**
 * 并发槽数 / 空闲回收 / 重启阈值（`FfiTrTimeouts` 的 Kotlin 版）。
 *
 * [desktop] = 桌面口径（并发 2 / 空闲 5 分钟 / 150 次重启 / 线程交给 llama.cpp）；
 * [android] = 手机口径（并发 1 / 空闲 2 分钟 / 30 次重启；`-t` 由 Rust 的 device 层按档位给）。
 */
data class TrTimeouts(
    val parallelism: Int,
    val idleMs: Long,
    val maxRequests: Int,
    val threads: Int? = null,
) {
    companion object {
        /** 桌面。 */
        fun desktop(): TrTimeouts = RustTranslate.guard {
            uniffi.feedhub_ffi.trTimeoutsDesktop().toModel()
        }

        /** 安卓：并发 1 / 空闲 2 分钟 / 30 次重启。 */
        fun android(): TrTimeouts = RustTranslate.guard {
            uniffi.feedhub_ffi.trTimeoutsAndroid().toModel()
        }
    }
}

internal fun uniffi.feedhub_ffi.FfiTrTimeouts.toModel(): TrTimeouts = TrTimeouts(
    parallelism = parallelism.toInt(),
    idleMs = idleMs,
    maxRequests = maxRequests.toInt(),
    threads = threads?.toInt(),
)

internal fun TrTimeouts.toFfi(): FfiTrTimeouts = FfiTrTimeouts(
    parallelism = parallelism.toUInt(),
    idleMs = idleMs,
    maxRequests = maxRequests.toUInt(),
    threads = threads?.toUInt(),
)

/** 引擎状态（Kotlin `TranslationStatus` 的来源）。 */
enum class TrEngineStatus { STOPPED, STARTING, READY, FAILED }


/**
 * `llama-server` 的启停 + 一次 chat 请求。
 *
 * **到 `llama-server` 的 HTTP 也不在宿主**：之前这里接一个
 * `Proxy.NO_PROXY` 的 OkHttp 客户端、再过 `HostHttp` 回调出去；现在是 Rust 里的一份专用
 * `NetClient`（直连 + 2 秒连接超时 + 120 秒读超时，见 `feedhub-ffi/src/translate.rs` 的
 * `local_translate_client`）。所以这个类**不再收客户端**，也不再需要释放连接池。
 *
 * 所有会阻塞的方法都是 `suspend`，内部走 `HostedCall.blocking`（`Dispatchers.IO`；取消时 Rust 侧
 * 的 `CancelToken` 会把在飞的请求掐断）；模型加载时最长会阻塞 120 秒。
 */
class RustTranslateEngine(
    private val statusSink: (TrEngineStatus) -> Unit,
    private val now: () -> Long = { System.currentTimeMillis() },
    /**
     * `ctx.store`：**手机上必须给**——Rust 用它记"上次降到哪一档"（NPU 起不来 / 自检失败时降级并记住，
     * App 升级后自动重新从 NPU 试）。桌面也可以给（写一份排障记录，不参与任何判断）。
     */
    private val store: HostStore? = null,
    /**
     * 并发槽数 / 空闲回收 / 重启阈值。**默认桌面口径**；
     * 安卓传 [TrTimeouts.android]（并发 1 / 空闲 2 分钟 / 30 次重启）。
     */
    private val timeouts: TrTimeouts? = null,
) {
    private val native: FfiTrEngine by lazy {
        RustNative.ensureLoaded()
        val status = object : FfiTrStatusHost {
            override fun status(status: FfiTrStatus) {
                statusSink(status.toModel())
            }
        }
        // 四种组合都走 Rust 的对应构造：手机上 `store` + `timeouts` 都要（降级记忆 + 手机的时间常量）。
        when {
            store != null && timeouts != null ->
                FfiTrEngine.withStoreAndTimeouts(status, store, timeouts.toFfi())
            store != null -> FfiTrEngine.withStore(status, store)
            timeouts != null -> FfiTrEngine.withTimeouts(status, timeouts.toFfi())
            else -> FfiTrEngine(status)
        }
    }

    suspend fun translate(config: TrEngineConfig, text: String, target: String): String =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            RustTranslate.guard {
                native.translate(
                    config = FfiTrConfig(
                        enabled = config.enabled,
                        modelPath = config.modelPath,
                        serverPath = config.serverPath,
                        cacheDir = config.cacheDir?.toString(),
                        icdFile = config.icdFile,
                        appVersion = config.appVersion,
                        mobile = config.mobile,
                        forcedBackend = config.forcedBackend,
                    ),
                    text = text,
                    target = target,
                    nowMs = now(),
                )
            }
        }

    /** 对已经在跑的端口发一次 chat 请求（请求等价的验收门用；不碰进程）。 */
    suspend fun chat(port: Int, text: String, target: String): String =
        HostedCall.blocking(cancelInFlight = ::cancelInFlight) {
            RustTranslate.guard { native.chat(port.toUShort(), text, target) }
        }

    /**
     * 取消这一跳正在飞的请求（`HostedCall.blocking` 在协程被取消时调）。
     *
     * 切到 `feedhub-net` 之后宿主没有 `Call` 可掐了（请求在 Rust 里发），只能从外面把 Rust 的
     * `CancelToken` 置上——和 11 家平台的 `cancelInFlight = ::cancelInFlight` 同一个口径。
     */
    internal fun cancelInFlight() {
        runCatching { native.cancel() }
    }

    fun status(): TrEngineStatus = RustTranslate.guard { native.status().toModel() }

    fun running(): Boolean = RustTranslate.guard { native.running() }

    fun port(): Int = RustTranslate.guard { native.port().toInt() }

    fun parallelism(): Int = RustTranslate.guard { native.parallelism().toInt() }

    /** 当前**实际**在用的后端（手机上降级之后会变）；null = 桌面（只有 AMD 核显）或者还没起过。 */
    fun backend(): TrBackend? = RustTranslate.guard { native.backend()?.toModel() }

    /** 宿主每 60 秒问一次：该不该停（空闲 5 分钟 / 累计 150 次请求）。返回 true = 这次停掉了。 */
    suspend fun reap(): Boolean = HostedCall.blocking { reapBlocking() }

    /**
     * 同步版的心跳（宿主的守护线程用，不建协程）：停进程最多阻塞 3 秒。
     */
    fun reapBlocking(): Boolean = RustTranslate.guard { native.reap(now()) }

    /**
     * **只停 `llama-server`，不关引擎**（安卓退到后台时用：下次回前台还要接着翻）。
     *
     * 和 [close] 的区别：`close` 之后这个引擎永远报"翻译已关闭"（App 退出用）；
     * 这个只是把进程收掉，下一次 [translate] 会自己再起一个。
     */
    fun stopServerBlocking() {
        runCatching { RustTranslate.guard { native.stopServer() } }
    }

    /** 收进程（最多等 300 ms，然后 SIGKILL）。同步：退出钩子里不再挂协程。 */
    fun close() {
        runCatching { RustTranslate.guard { native.shutdown() } }
    }
}

private fun FfiTrStatus.toModel(): TrEngineStatus = when (this) {
    FfiTrStatus.STOPPED -> TrEngineStatus.STOPPED
    FfiTrStatus.STARTING -> TrEngineStatus.STARTING
    FfiTrStatus.READY -> TrEngineStatus.READY
    FfiTrStatus.FAILED -> TrEngineStatus.FAILED
}

/** [TrEngineStatus] → FFI（纯函数 `tr_model_loading` 要用）。 */
internal fun TrEngineStatus.toFfi(): FfiTrStatus = when (this) {
    TrEngineStatus.STOPPED -> FfiTrStatus.STOPPED
    TrEngineStatus.STARTING -> FfiTrStatus.STARTING
    TrEngineStatus.READY -> FfiTrStatus.READY
    TrEngineStatus.FAILED -> FfiTrStatus.FAILED
}

// ===========================================================================
// 重启策略（纯状态机）
// ===========================================================================

/**
 * `llama-server` 什么时候该停 / 该重启（Kotlin `LlamaServerPolicy` 的搬迁）：
 * 空闲 5 分钟、累计 150 次请求。
 *
 * 生产路径上这套判断在 [RustTranslateEngine] 里（`reap`）；这个类单独露出来是给对拍
 * （`TranslatePolicyParityTest`）和排障用的。
 */
class RustTranslatePolicy(maxRequests: Int, idleMs: Long) {
    private val native: uniffi.feedhub_ffi.FfiTrPolicy by lazy {
        RustNative.ensureLoaded()
        uniffi.feedhub_ffi.FfiTrPolicy(maxRequests.toUInt(), idleMs)
    }

    fun markStarted(nowMs: Long) = RustTranslate.guard { native.markStarted(nowMs) }

    fun noteRequest(nowMs: Long) = RustTranslate.guard { native.noteRequest(nowMs) }

    fun shouldRestart(): Boolean = RustTranslate.guard { native.shouldRestart() }

    fun idleTooLong(nowMs: Long): Boolean = RustTranslate.guard { native.idleTooLong(nowMs) }

    fun requestCount(): Int = RustTranslate.guard { native.requestCount().toInt() }
}
