package feedhub.app.translate

import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.staticCompositionLocalOf
import feedhub.app.SettingsStore
import feedhub.core.Platform
import feedhub.translate.RustTranslate
import feedhub.translate.RustTranslateStore
import feedhub.translate.TrPhaseInput
import feedhub.translate.TranslateDetector
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import java.nio.file.Path
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/** 本地翻译服务的状态；设置页和 [TranslationStore] 的转圈都用它。 */
enum class TranslationStatus {
    /** 还没启动（没开翻译，或者还没有人请求过）。 */
    STOPPED,

    /** llama-server 正在加载模型 / 第一次请求正在等它。 */
    STARTING,

    /** 模型已经加载，可以翻译。 */
    READY,

    /** 启动失败 / 模型路径不对 / 进程挂了；原因在设置页和日志里。 */
    FAILED,
}

/** 一段文字当前的翻译状态。 */
sealed interface TranslationState {
    /** 还没开始。 */
    data object Idle : TranslationState

    /** 正在翻。 */
    data object Loading : TranslationState

    /** 翻好了。 */
    data class Done(val text: String) : TranslationState

    /** 失败（模型没起来 / 返回空 / 超时），保留原文。 */
    data class Failed(val message: String) : TranslationState

    /** 不是外语（或者翻译关着），不需要翻。 */
    data object Skipped : TranslationState
}

/**
 * 一段文字的翻译入口现在长什么样。
 *
 * **档位和优先级都在 Rust**（`feedhub-core/src/translate/entry.rs` 的 `phase`；
 * `RustTranslate.phase`），这里只是把枚举名搬过来给界面 `when` 用；文案由界面定。
 */
enum class TranslationPhase {
    /** 不显示（不是外语 / 译文和原文一样 / 翻译关着 / 没有本地翻译）。 */
    HIDDEN,

    /** 一行可点的「翻译」。 */
    IDLE,

    /** 「正在加载翻译模型…」。 */
    MODEL_LOADING,

    /** 「正在翻译…」。 */
    TRANSLATING,

    /** 有译文了：就地显示译文 + 灰字那一行。 */
    DONE,

    /** 失败：一行灰字原因 +「重试」。 */
    FAILED,
}

/**
 * 本地翻译引擎：桌面端是 llama.cpp 的 `llama-server`（Vulkan AMD 核显）+ HY-MT GGUF；
 *
 * [translate] 失败时抛异常（调用方 [TranslationStore] 会把它变成 [TranslationState.Failed]），
 * 不要静默返回原文——否则界面上分不清"翻完了但和原文一样"和"根本没翻"。
 *
 * 这个接口只剩"宿主怎么把它接到界面上"。
 */
interface TranslationEngine {
    /** 缓存键的一部分；换模型要换 id，旧译文不会串。 */
    val id: String

    val status: StateFlow<TranslationStatus>

    /**
     * 这个引擎同时能处理几个请求（= `llama-server -np`）。[TranslationStore] 的排队器用它设并发上限：
     * 排队在协程里挂起、不占线程，真正同时在跑的不会超过这个数。测试 / 安卓的替身用默认 1。
     */
    val parallelism: Int get() = 1

    suspend fun translate(text: String, target: String): String

    /**
     * App 进 / 退前台。
     *
     * **默认什么都不做**（桌面那条：`llama-server` 只在空闲 5 分钟后停，和窗口在不在前台无关）。
     * 安卓那条要用它：退到后台**立刻**停 `llama-server`（1.1 GB 内存 + 4 个核），回前台下次翻译再起。
     */
    fun onForeground(active: Boolean) {}

    fun close()
}

/**
 * 语言识别（两端都是 Rust 内置的 lingua，只有测试会注入假实现）。返回 BCP 47 语言标签，识别不了返回 null。
 * 翻译门控用它判断"整条内容是不是目标语言"，不再用字符占比之类的规则。
 *
 * 这个接口只剩一个用途：给 [TranslationStore] 注入一个假识别器。Kotlin 原版 `LinguaLanguageIdentifier` 在 `desktopTest` 里当对拍参照。
 */
interface LanguageIdentifier {
    fun detect(text: String): String?
}

/** [tag] 是不是目标语言。[target] 是设置里的显示名（"中文" / "English"……）。 */
internal fun languageMatchesTarget(tag: String, target: String): Boolean =
    RustTranslate.matchesTarget(tag, target)

/**
 * 界面用的翻译门面（组合状态 + 缓存 + 排队）。卡片、正文、评论都从 [LocalTranslation] 拿它：
 *
 * ```
 * val store = LocalTranslation.current
 * val state = store?.state(text)
 * LaunchedEffect(text, store?.enabled) { store?.ensure(text) }
 * ```
 *
 * - 只翻**进入组合、屏幕上看得到**的文字；缓存命中时 [state] 立刻是 [TranslationState.Done]；
 * - 排队交给 `TranslationScheduler`（状态机在 Rust）：不占线程、新的优先、离开组合就取消还没派发的那条；
 * - **引擎（llama-server 的壳）和排队器都是懒建的**：`settings` 里没有任何点亮平台的内容时，
 *   [shouldAutoTranslate] 先看平台、再做语言识别，[ensure] 根本不会被调用，也就不会建引擎、不会加载
 *   lingua、不会启动 server；
 * - 目标语言 / 开关改了以后 [keyFor] 的键会变，新组合会重新翻；已经翻好的旧状态留在内存里，不影响新目标。
 *
 * 这里只剩三样宿主的活：Compose 的 `states`（界面状态容器）、两处防止"每帧都过 FFI"的内存备忘录、
 * 以及一个写缓存的单线程（译文在 Main 上回来，文件 I/O 不能占 UI 线程）。
 */
class TranslationStore(
    private val scope: CoroutineScope,
    /**
     * 引擎工厂（桌面版是 [LlamaTranslationEngine]，安卓是
     * [feedhub.app.translate.AndroidTranslationEngine]）。**懒调用**：第一次真的要翻时才建。
     * null = 这个组装没有本地翻译（测试 / 截图 / 包里缺模型或 llama-server），[available] 为 false。
     */
    private val engineFactory: (() -> TranslationEngine?)?,
    private val settings: SettingsStore,
    cacheDir: Path?,
    /**
     * 语言识别：生产不传（用 Rust 内置的 lingua，见 [RustTranslateStore] 的 `builtin`）；
     * 传了就用它。
     */
    private val identifier: LanguageIdentifier? = null,
) {
    /**
     * Rust 那边的缓存 + 语言识别 + 门控。
     *
     * `builtin` 只在"这个组装真的有本地翻译"（桌面 / 安卓）时打开：截图 / 演示不传引擎，
     * 也就不该加载 lingua 的模型（几 MB，第一次识别还要 20～40 ms）。
     */
    private val core: RustTranslateStore by lazy {
        RustTranslateStore(
            cacheDir = cacheDir,
            detector = identifier?.let { id -> TranslateDetector { text -> id.detect(text) } },
            builtin = identifier == null && engineFactory != null,
        )
    }

    /** 写缓存的单线程（Rust 的 `cache_put` 是同步写文件，不能在 Main 上做）。 */
    private val cacheWriter = Executors.newSingleThreadExecutor { r ->
        Thread(r, "feedhub-translation-cache").apply { isDaemon = true }
    }

    private val states = mutableStateMapOf<String, TranslationState>()
    private val lock = Any()

    init {
        // 缓存文件是在 [core] 第一次被用到时读进内存的；第一次用通常发生在**组合里**（第一张卡片判门控），
        // 别让一次几毫秒的文件读落在 Main 上——启动时先在后台把这份缓存热起来（老代码是在构造函数里同步读的）。
        if (cacheDir != null && engineFactory != null) {
            Thread({ runCatching { core } }, "feedhub-translation-warm").apply { isDaemon = true }.start()
        }
    }

    /**
     * 缓存查询的备忘录（键 → 译文或 null）。
     *
     * **只为了不每帧过 FFI**：`state(text)` 在组合里每帧都会调，每条可见文字一次 JNA 调用太浪费
     * （一次约 6 µs，30 条 × 60 帧就是 10 ms/s）。真缓存和键的算法都在 Rust；这里是宿主的一次性结果缓存，
     * 只在 [finish] / [clearCache] 时更新。
     */
    private val cacheMemo = HashMap<String, String?>()

    /** 引擎自己的状态；[status] 对外一直是它，引擎建出来以后把它的状态镜像过来。 */
    private val ownStatus = MutableStateFlow(TranslationStatus.STOPPED)

    @Volatile
    private var engineRef: TranslationEngine? = null

    @Volatile
    private var schedulerRef: TranslationScheduler? = null

    /** 文本 → SHA-256 键的短缓存：组合里 `state()` 每帧都会调，不必每次都算一次键。 */
    private val keyCache = ConcurrentHashMap<String, String>()

    /**
     * 「要不要出入口」的备忘录（`语言\0正文` → 结论）。
     *
     * **只为了不每帧跑一次语言识别**：一屏十几条评论，每条一次 lingua（中 / 英两张模型，最坏 ~1 ms）
     * 就是每帧十几毫秒。判定本身在 Rust（`translate::entry::visible`），这里只是把"同一段文字 + 同一个
     * 平台语言"的结论记下来；设置（目标语言 / 开关）一变就整份清掉（见 [invalidateEntryMemo]）。
     */
    private val entryMemo = ConcurrentHashMap<String, Boolean>()

    @Volatile
    private var entryMemoKey: String = ""

    /** 设置变了（目标语言 / 开关 / 可用性）：入口判定的备忘录整份作废。 */
    private fun invalidateEntryMemo() {
        val key = "$enabled|$target|$available"
        if (key != entryMemoKey) {
            entryMemoKey = key
            entryMemo.clear()
        }
    }

    val status: StateFlow<TranslationStatus> = ownStatus
    val available: Boolean get() = engineFactory != null
    val enabled: Boolean get() = settings.value.translationOn
    val target: String get() = settings.value.translationTarget

    /** 自动翻译点亮的平台。手动那一行「翻译」（[entryVisible]）不看它。 */
    val autoPlatforms: Set<Platform> get() = settings.value.translationPlatforms

    /** 建引擎（只建一次）；没配引擎返回 null。 */
    private fun engineOrNull(): TranslationEngine? {
        engineRef?.let { return it }
        val factory = engineFactory ?: return null
        return synchronized(lock) {
            engineRef ?: factory()?.also { engine ->
                engineRef = engine
                // 引擎状态镜像到自己的 StateFlow：settings 卡片读 status 时不会把引擎提前建出来
                scope.launch { engine.status.collect { ownStatus.value = it } }
            }
        }
    }

    /** 排队器（只建一次）；并发上限 = 引擎的 `-np`。 */
    private fun scheduler(): TranslationScheduler {
        schedulerRef?.let { return it }
        return synchronized(lock) {
            schedulerRef ?: TranslationScheduler(engineOrNull()?.parallelism ?: 1, scope) { text, target ->
                val engine = engineOrNull() ?: error("本地翻译只在桌面版可用")
                engine.translate(text, target)
            }.also { schedulerRef = it }
        }
    }

    /** 当前状态；纯读，不启动翻译。组合里直接调。 */
    fun state(text: String): TranslationState {
        if (!enabled || text.isBlank()) return TranslationState.Skipped
        val key = keyFor(text)
        states[key]?.let { return it }
        cached(key)?.let { return TranslationState.Done(it) }
        return TranslationState.Idle
    }

    /**
     * 这条内容要不要**自动**翻译。判定单位是**整条**（卡片=标题+摘要，详情=标题+正文，评论=整条评论）：
     *
     * 0. **[platform] 没在 [autoPlatforms] 里就一律不自动翻**：国内平台默认不点亮，
     *    这里在**语言识别之前**就返回，保证它们的内容永远碰不到 lingua、也不会去启动 llama-server；
     * 1. 平台给了语言（X 的 `legacy.lang`）就信平台，不再猜；
     * 2. 没有平台语言时，短于 25 **字节**的不自动翻（Chrome 的语言识别也是短于 25 字节就当不可靠），
     *    交给正文下面那一行手动「翻译」；
     * 3. 其余交给语言识别：识别成目标语言就不翻，识别不了（UNKNOWN）按外语处理。
     *
     * **规则本身在 Rust**，这里只是把本机设置现读出来传过去。
     */
    fun shouldAutoTranslate(text: String, language: String? = null, platform: Platform? = null): Boolean =
        core.shouldAutoTranslate(
            text = text,
            language = language,
            platform = platform,
            enabled = enabled,
            target = target,
            autoPlatforms = autoPlatforms,
        )

    /**
     * 这条内容要不要出**手动**的「翻译」入口。
     *
     * 和 [shouldAutoTranslate] 的区别（两条规则都在 Rust 里，共用同一份识别器）：
     * - **不看平台**：手动翻哪家都能用（自动翻那半边仍然只翻点亮的平台）；
     * - **不看 25 字节门槛**：用户主动点的那一下不该被"短文本识别不可靠"挡住；
     * - 但**照样判"是不是外语"**：中文内容**不出入口**。
     *
     * 总开关关着 / 这个组装没有本地翻译（[available] 为 false）时一律 false——界面上什么都不出现。
     */
    fun entryVisible(text: String, language: String? = null): Boolean {
        if (!enabled || !available || text.isBlank()) return false
        invalidateEntryMemo()
        val memoKey = "${language.orEmpty()}\u0000$text"
        entryMemo[memoKey]?.let { return it }
        val visible = core.entryVisible(text = text, language = language, target = target)
        if (entryMemo.size > 4096) entryMemo.clear()
        entryMemo[memoKey] = visible
        return visible
    }

    /**
     * 这段文字现在该显示哪一档入口（判断在 Rust 的 `translate::entry::phase`）。
     *
     * 界面每一帧现算，所以这里**不缓存**：一次 FFI，几微秒。
     */
    fun phase(text: String, language: String? = null, visible: Boolean = entryVisible(text, language)): TranslationPhase {
        if (!visible) return TranslationPhase.HIDDEN
        return phaseOf(listOf(text), text, language)
    }

    /**
     * 一个**入口**对应几段文字（正文按块翻）时的那一档：把每块的状态**汇总**成整条的一个结论。
     *
     * 汇总规则：
     * - 有任何一块失败 → 失败（那一块的原因显示在灰字里）；
     * - 还有块在翻 → 「正在翻译…」（**哪怕已经有几块翻好了**：入口只有一个，正文有的还是原文时
     *   不能显示「由本地模型翻译自 …」，不然用户会以为整篇都翻完了）；
     * - 全部翻完（或本来不用翻）而且至少有一块翻好了 → 有译文；
     * - 全部"跳过了"（译文和原文一样）→ 隐藏。
     *
     * 所以和单条的 [`phase`] 有一处刻意的差别：单条是"有译文就显示译文"（那里只有一段），
     * 这里是"在翻的一律还显示正在翻译"。
     */
    fun phaseOf(sources: List<String>, text: String, language: String? = null): TranslationPhase {
        if (sources.isEmpty()) return TranslationPhase.HIDDEN
        val each = sources.map { states[keyFor(it)] }
        val failed = each.any { it is TranslationState.Failed }
        val anyDone = each.any { it is TranslationState.Done }
        // "还在翻"包含两种：真的在翻（Loading），和这个入口已经点下去了、但这一块还没轮到的
        // （`ensure` 会给每一块都先写 Loading，所以这两者在界面上是同一档）
        val pending = each.any { it is TranslationState.Loading }
        val skipped = each.all { it is TranslationState.Skipped }
        return RustTranslate.phase(
            TrPhaseInput(
                visible = true,
                skipped = skipped,
                translating = pending,
                translated = anyDone && !pending,
                failed = failed,
                // 「模型还在加载」的判定也在 Rust（`tr_model_loading`）：界面这半边只把状态搬过去
                modelLoading = RustTranslate.modelLoading(engineStatus.toEngineStatus()),
            ),
        ).toPhase()
    }

    /** 灰字里那个语言名（`由本地模型翻译自 <语言> · 显示原文`）。识别不出来写"外语"。 */
    fun languageLabel(language: String? = null, text: String = ""): String {
        val tag = language?.takeIf { it.isNotBlank() } ?: runCatching { core.detect(text) }.getOrNull()
        return RustTranslate.languageLabel(tag)
    }

    /** 失败原因（[TranslationState.Failed] 里那句话）；没有失败就是 null。 */
    fun failureReason(text: String): String? =
        (states[keyFor(text)] as? TranslationState.Failed)?.message

    /** 引擎现在是什么状态（引擎还没建出来 = [TranslationStatus.STOPPED]）。 */
    val engineStatus: TranslationStatus get() = ownStatus.value

    /**
     * **点「翻译」**：在 store 自己的 scope 里开始翻（非挂起，给 `onClick` 用）。
     *
     * 和 [ensure] 的区别只是"谁持有协程"：`ensure` 由 `LaunchedEffect` 调、界面走了能取消排队；
     * 这里是用户亲手点的那一下，**不该因为手指抬起后重组一下就没了**——所以挂在 store 的 scope 上，
     * 翻完照样写缓存（下一次进这条直接命中）。
     */
    fun start(text: String) {
        if (!enabled || text.isBlank()) return
        scope.launch { ensure(text) }
    }
    /**
     * 请求翻译；已翻 / 正在翻 / 不需要翻都会直接返回。由 `LaunchedEffect` 调。
     *
     * **是 suspend 的**：卡片 / 评论离开组合时这个协程被取消，排队器就把还没派发的那条从队列里拿掉；
     * 已经派发的照常跑完、写缓存（见 [TranslationScheduler]）。所以调用方不需要自己等，也不该包一层
     * `launch` 到别的 scope 去——那样界面走了也取消不掉。
     */
    suspend fun ensure(text: String) {
        if (!enabled || text.isBlank()) return
        val key = keyFor(text)
        cached(key)?.let {
            states[key] = TranslationState.Done(it)
            return
        }
        if (states[key] is TranslationState.Done) return
        states[key] = TranslationState.Loading
        try {
            scheduler().submit(key, text, target) { result ->
                result.onSuccess { out -> finish(key, text, out) }
                    .onFailure { e -> fail(key, e) }
            }
        } catch (e: CancellationException) {
            // 排队中被取消 / 界面走了：`fail` 已经把状态清掉了（不是失败，下次进组合重新排）。
            // 取消要往外传，不然调用方的协程以为自己还在跑。
            throw e
        } catch (e: Throwable) {
            // 引擎那条异常**已经**通过 `onDone` 落成 [TranslationState.Failed] 了（`fail` 里写的），
            // 这里再抛一次只会变成"未捕获异常"打在 Main 上（桌面截图 / 真机上都是一屏红字），
            // 界面要的是那一行灰字 +「重试」，所以在这里收住。
            if (states[key] == null) states[key] = TranslationState.Failed(e.message ?: "翻译失败")
        }
    }

    /** 一条请求落地：原样返回（专名、缩写、zsbd 这类）不算翻好，不写缓存、不显示「原文」按钮。 */
    private fun finish(key: String, text: String, out: String) {
        if (out.isBlank()) {
            states[key] = TranslationState.Failed("模型没有返回译文")
            return
        }
        val translated = out.trim()
        if (translated == text.trim()) {
            states[key] = TranslationState.Skipped
        } else {
            states[key] = TranslationState.Done(translated)
            synchronized(cacheMemo) { cacheMemo[key] = translated }
            cacheWriter.execute { runCatching { core.cachePut(key, translated) } }
        }
    }

    private fun fail(key: String, e: Throwable) {
        // 排队中被取消（界面走了 / 关翻译）不是失败，把状态清掉，下次进组合重新排
        if (e is CancellationException) states.remove(key) else states[key] = TranslationState.Failed(e.message ?: "翻译失败")
    }

    /** 失败后手动重试（设置页 / 卡片上以后可以加按钮）。 */
    fun retry(text: String) {
        if (!enabled || text.isBlank()) return
        val key = keyFor(text)
        if (states[key] is TranslationState.Failed) states.remove(key)
        scope.launch { ensure(text) }
    }

    /** 清掉内存和磁盘缓存；已经显示在界面上的译文会在下次重组时回到原文。 */
    fun clearCache() {
        states.clear()
        synchronized(cacheMemo) { cacheMemo.clear() }
        cacheWriter.execute { runCatching { core.clearCache() } }
    }

    /**
     * App 进 / 退前台（安卓由 `AppGraph.onForeground` 喂）。
     *
     * 引擎**还没建出来**时什么都不做——没建就没有进程要停（`llama-server` 是第一次翻译才起的）。
     */
    fun onForeground(active: Boolean) {
        engineRef?.onForeground(active)
    }

    /** 退出前收尾：排队中的请求取消掉、缓存写线程等一会儿、引擎（llama-server）停掉。 */
    fun close() {
        schedulerRef?.cancelPending()
        cacheWriter.shutdown()
        runCatching { cacheWriter.awaitTermination(CACHE_FLUSH_TIMEOUT_MS, TimeUnit.MILLISECONDS) }
        engineRef?.close()
        engineRef = null
    }

    /** 缓存里有没有这条（命中才进 [cacheMemo]）。 */
    private fun cached(key: String): String? {
        synchronized(cacheMemo) {
            if (cacheMemo.containsKey(key)) return cacheMemo[key]
        }
        val value = core.cacheGet(key)
        synchronized(cacheMemo) { cacheMemo[key] = value }
        return value
    }

    private fun keyFor(text: String): String {
        // CACHE_VERSION 换了就整体失效：旧缓存里可能有判定规则误翻的中文改写，不能再显示。
        // 模型路径也进键（`lid2` 起取代原来的引擎 id）：引擎是懒建的，键不能等它建出来才稳定，
        // 换个 GGUF 就换一批键，旧译文不会串（第一次会把旧缓存当作未命中，重新翻一遍，纯性能代价）。
        // 这里只是备忘录。
        val model = settings.value.translationModelPath
        val target = target
        val raw = "$model\u0000$target\u0000$text"
        keyCache[raw]?.let { return it }
        val key = core.keyFor(model, target, text)
        if (keyCache.size > 2048) keyCache.clear()
        keyCache[raw] = key
        return key
    }

    private companion object {
        /** 退出时最多等这么久把排队的缓存追加写完（App 关闭的总预算 3 秒，见 `AppGraph.shutdown`）。 */
        const val CACHE_FLUSH_TIMEOUT_MS = 2_000L
    }
}

/** 组合里拿 [TranslationStore]；截图 / 测试 / 安卓没有翻译时为 null。 */
val LocalTranslation = staticCompositionLocalOf<TranslationStore?> { null }

/**
 * [TranslationStatus] → [feedhub.translate.TrEngineStatus]（同一个引擎状态的两种枚举名）。
 *
 * 引擎状态本身是 Rust 推上来的（`FfiTrStatusHost`），这里只是搬名字：界面用 [TranslationStatus]
 * （设置页那些文案早就按它写），Rust 的纯函数认 [feedhub.translate.TrEngineStatus]。
 */
internal fun TranslationStatus.toEngineStatus(): feedhub.translate.TrEngineStatus = when (this) {
    TranslationStatus.STOPPED -> feedhub.translate.TrEngineStatus.STOPPED
    TranslationStatus.STARTING -> feedhub.translate.TrEngineStatus.STARTING
    TranslationStatus.READY -> feedhub.translate.TrEngineStatus.READY
    TranslationStatus.FAILED -> feedhub.translate.TrEngineStatus.FAILED
}

/** Rust 的 `FfiTrPhase` → 界面的 [TranslationPhase]（同一个枚举名的两半）。 */
internal fun uniffi.feedhub_ffi.FfiTrPhase.toPhase(): TranslationPhase = when (this) {
    uniffi.feedhub_ffi.FfiTrPhase.HIDDEN -> TranslationPhase.HIDDEN
    uniffi.feedhub_ffi.FfiTrPhase.IDLE -> TranslationPhase.IDLE
    uniffi.feedhub_ffi.FfiTrPhase.MODEL_LOADING -> TranslationPhase.MODEL_LOADING
    uniffi.feedhub_ffi.FfiTrPhase.TRANSLATING -> TranslationPhase.TRANSLATING
    uniffi.feedhub_ffi.FfiTrPhase.DONE -> TranslationPhase.DONE
    uniffi.feedhub_ffi.FfiTrPhase.FAILED -> TranslationPhase.FAILED
}
