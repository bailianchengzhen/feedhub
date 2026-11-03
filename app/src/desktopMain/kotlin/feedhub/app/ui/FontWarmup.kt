package feedhub.app.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.platform.LocalFontFamilyResolver
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.font.createFontFamilyResolver
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlin.concurrent.thread

/**
 * 字体预热。
 *
 * Compose 桌面每第一次用一种（字体族, 字重, 样式），都要在界面线程上经 Skia 的 `FontCollection.findTypefaces` 问一遍
 * fontconfig；这台机器装了 2718 个字体（含 Type1），一次 ≈140 ms，结果只缓存在那个 resolver 自己的 `FontCollection` 里。
 * 所以点开详情（加粗标题）、第一次进"我的"/"设置"都会卡一帧 200~470 ms。
 *
 * 做法：`main()` 一开头在后台线程**新建**一个 resolver（和窗口默认的一样是 [createFontFamilyResolver]），
 * 把 App 用到的组合（[REQUESTS]）先 resolve 一遍；**做完了**再用 [Provide] 把它换进 [LocalFontFamilyResolver]。
 * 预热线程只碰这个新 resolver、窗口在预热完成（[await] 的 join）之前不碰它，所以没有并发访问。字体完全一样，界面不变。
 *
 * **不等它出第一帧**：9 个组合要 ≈1.3 s，第一版在第一次组合时等它做完，冷启动首帧反而晚了 ≈0.5 s
 * （首帧本身只用到两三个组合，默认 resolver 现解析也就 300 ms 左右）。现在首帧用窗口默认的 resolver，
 * 预热做完那一刻换过去：所有文字按新 resolver 重新排一次版（都命中缓存，一帧以内），之后再进新页面就不卡了。
 */
class FontWarmup internal constructor(
    private val resolver: FontFamily.Resolver,
    private val requests: List<Request> = REQUESTS,
    /** 解析一个组合（测试换成假的；`FontFamily.Resolver` 是 sealed，没法整个替身）。 */
    private val resolveOne: (Request) -> Unit = { r -> resolver.resolve(r.family, r.weight, r.style) },
) {
    /** 一个要预热的组合。 */
    data class Request(val family: FontFamily, val weight: FontWeight, val style: FontStyle)

    @Volatile var elapsedMs: Long = -1
        private set

    private val worker: Thread = thread(name = "feedhub-font-warmup", isDaemon = true) {
        val t0 = System.nanoTime()
        for (r in requests) runCatching { resolveOne(r) }
        elapsedMs = (System.nanoTime() - t0) / 1_000_000
    }

    /** 等预热做完（最多 [timeoutMs]）：做完了给出预热好的 resolver，没做完给 null（调用方就用默认的）。 */
    fun await(timeoutMs: Long): FontFamily.Resolver? {
        worker.join(timeoutMs.coerceAtLeast(1))
        return if (worker.isAlive) null else resolver
    }

    /** 预热做完了吗（不等）。 */
    val done: Boolean get() = !worker.isAlive

    /**
     * 预热做完后把 resolver 换进 [LocalFontFamilyResolver]；没做完之前用外面那个（窗口默认的），**不等**。
     * 换的时候组合树结构不变（同一个 CompositionLocalProvider，只是值变了），界面状态不丢。
     */
    @Composable
    fun Provide(content: @Composable () -> Unit) {
        val fallback = LocalFontFamilyResolver.current
        val warmed by produceState(if (done) resolver else null) {
            if (value == null) value = withContext(Dispatchers.IO) { await(Long.MAX_VALUE / 2) }
        }
        CompositionLocalProvider(LocalFontFamilyResolver provides (warmed ?: fallback), content = content)
    }

    companion object {
        /**
         * App 实际用到的组合：默认字体族的 400 / 500（Material 的 title / label）/ 600 / 700，正体和斜体各一份
         * （富文本里有粗斜体），再加等宽 400（代码块、设置页里的路径）。后备字体（表情、日韩文）便宜，不用预热。
         */
        val REQUESTS: List<Request> = buildList {
            for (style in listOf(FontStyle.Normal, FontStyle.Italic)) {
                for (w in listOf(FontWeight.Normal, FontWeight.Medium, FontWeight.SemiBold, FontWeight.Bold)) {
                    add(Request(FontFamily.Default, w, style))
                }
            }
            add(Request(FontFamily.Monospace, FontWeight.Normal, FontStyle.Normal))
        }

        /** 进程一开始就调。 */
        fun start(): FontWarmup = FontWarmup(createFontFamilyResolver())
    }
}
