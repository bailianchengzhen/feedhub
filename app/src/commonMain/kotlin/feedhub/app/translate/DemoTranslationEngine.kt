package feedhub.app.translate

import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import java.util.concurrent.atomic.AtomicInteger

/**
 * **"X 式"翻译界面的假引擎**。
 *
 * 界面的四种状态（入口 / 翻译中 / 译文 + 显示原文 / 失败）要截给用户看，而真的 `llama-server`
 * 要 1.1 GB 的 GGUF、几十秒加载、而且截图是在远端跑的（模型不在）。所以演示环境用这一份：
 * 行为（`TranslationEngine` 的契约）和真的那个一模一样，只有"怎么得到译文"是假的。
 *
 * 三种可配的样子（[DemoTranslation.mode]）：
 * - [Mode.OK]：等 [DemoTranslation.delayMs] 之后给一句中文译文（截"正在翻译…"和"译文"）；
 * - [Mode.FAIL]：等一会儿之后抛错（截"失败 + 重试"）；
 * - [Mode.LOADING]：`STARTING` 一直不结束（截"正在加载翻译模型…"）。
 *
 * **不进生产路径**：只有演示 / 截图组装会传它（`desktopGraph(translationEngineOverride = …)` /
 * `androidDemoGraph`），真 App 的工厂仍然是 `LlamaTranslationEngine`。
 */
class DemoTranslationEngine(
    private val mode: DemoTranslation.Mode = DemoTranslation.mode,
) : TranslationEngine {
    override val id: String = "demo-llama"

    private val state = MutableStateFlow(
        when (mode) {
            DemoTranslation.Mode.LOADING -> TranslationStatus.STARTING
            else -> TranslationStatus.READY
        },
    )

    override val status: StateFlow<TranslationStatus> = state

    private val calls = AtomicInteger(0)

    override suspend fun translate(text: String, target: String): String {
        calls.incrementAndGet()
        when (mode) {
            DemoTranslation.Mode.LOADING -> {
                // 模型"一直在加载"：界面上是「正在加载翻译模型…」，永远等不到结果
                while (true) delay(60_000)
            }
            DemoTranslation.Mode.FAIL -> {
                delay(DemoTranslation.delayMs)
                error("模型没有返回译文（演示）")
            }
            DemoTranslation.Mode.OK -> {
                delay(DemoTranslation.delayMs)
                return DemoTranslation.translate(text, target)
            }
        }
    }

    override fun close() {
        state.value = TranslationStatus.STOPPED
    }
}

/**
 * 演示翻译的全局开关（只给截图工具 / 单测改；生产路径永远读默认值）。
 *
 * 放在同一个对象里是为了"截图工具改一下、引擎和界面都看得见"——不用把参数从 Gradle 一路传到界面。
 */
object DemoTranslation {
    enum class Mode {
        /** 等一会儿给译文。 */
        OK,

        /** 等一会儿报错（截失败那一张）。 */
        FAIL,

        /** `STARTING` 不结束（截「正在加载翻译模型…」）。 */
        LOADING,
    }

    @Volatile
    var mode: Mode = Mode.OK

    /** "翻译"要多久（毫秒）：默认 1.5 秒，截图工具等得及、界面上停得住。 */
    @Volatile
    var delayMs: Long = 1_500

    /**
     * 一句"译文"：演示正文里的英文句子有固定的中文，别的就加一个前缀。
     *
     * 不接真模型，但**译文不能等于原文**（`TranslationStore.finish` 会把"译好了但一样"当 `Skipped`，
     * 那样界面上就不显示「显示原文」那一行了——截不到要截的图）。
     */
    fun translate(text: String, target: String): String {
        KNOWN[text.trim()]?.let { return it }
        return "【演示译文】" + text.trim().take(120)
    }

    /** 演示正文 / 标题 / 评论里的几句英文 → 一句像样的中文译文。 */
    private val KNOWN: Map<String, String> = mapOf(
        "Six projects from the last few weeks that caught our attention" to
            "最近几周我们注意到的六个项目",
        // 卡片列表：一条英文推文 + 一条英文视频，标题 / 摘要各有一句像样的译文
        "A quiet but very productive stretch: six open-source projects worth a look, " +
            "from a tiny SQLite wrapper to a full desktop toolkit." to
            "一段安静但很有产出的日子：六个值得一看的开源项目，从一个很小的 SQLite 封装到一个完整的桌面工具箱。",
        "A twenty minute tour of the whole rendering pipeline" to
            "二十分钟走完整条渲染管线",
        "Recorded session: how the frame graph is built, where the tiles come from, " +
            "and why the compositor never blocks on the main thread." to
            "录播：帧图是怎么搭起来的、图块从哪来，以及合成器为什么从不在主线程上阻塞。",
        "A long enough English sentence for detection." to
            "一句足够长的英文句子，用来做语言识别。",
        "ReZygisk" to "ReZygisk",
    )
}
