package feedhub.app.detail

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import feedhub.app.translate.LocalTranslation
import feedhub.app.translate.TranslationEntryState
import feedhub.app.translate.TranslationPhase
import feedhub.app.translate.TranslationState
import feedhub.core.Block
import feedhub.core.RichText
import feedhub.translate.RustTranslate

/**
 * 一段富文本的翻译结果：
 *
 * - [text] 是**现在该画的那一份**（原文 / 译文，由 Rust 的 `showing_blocks` 决定）；
 * - [entry] 是正文下面那一行小字（`翻译` / `正在翻译…` / `由本地模型翻译自 英语 · 显示原文` / 失败 + 重试），
 *   **只在判定为外语时才有**（中文内容不出现入口）；
 * - [showingTranslation] 供调用方在做"折叠时只画前三行纯文字"这类分支时用。
 *
 * 只翻段落 / 小标题 / 引用里的文字；图片 / 视频 / 卡片 / 代码块原样保留，只翻进入组合的块。
 */
class RichTextTranslation internal constructor(
    /** 当前该渲染的富文本（译文或原文）。 */
    val text: RichText,
    /** 下面那一行；null = 不显示（不是外语 / 翻译关着 / 没有本地翻译）。 */
    val entry: TranslationEntryState?,
    /** 现在画的是译文吗。 */
    val showingTranslation: Boolean,
)

/**
 * 组合里记住一段富文本的翻译状态。
 *
 * @param autoTranslate 桌面「点亮平台的自动翻」：进详情就自动翻。
 * 不自动翻"）。
 * @param language 平台给的语言标签（X 的 `legacy.lang`），没有就交给本地识别。
 * @param enabled false = 这块不参与翻译（楼中楼预览里那些本来就不翻的块）。
 *
 * "要不要出入口 / 现在哪一档 / 显示原文还是译文"也在 Rust（`translate::entry`）。这里只剩组合状态。
 */
@Composable
fun rememberRichTextTranslation(
    text: RichText,
    autoTranslate: Boolean = false,
    language: String? = null,
    enabled: Boolean = true,
): RichTextTranslation {
    val store = LocalTranslation.current
    // 每块要翻的原文（不可翻的块是 null，下标和 text.blocks 对齐）。
    // **翻译的单位是块**（分段翻才不会把整段长正文塞进一次请求），但**入口是整段一个**：
    // 判定用整段文字，状态由各块汇总（见 [rememberTranslationEntry] 的 `sources`）。
    val plain = remember(text) { text.textOnly() }
    val plan = remember(text) { RustTranslate.planBlocks(text.blocks) }
    val sources = remember(plan) { plan.filterNotNull().distinct() }
    val entry = if (store == null) {
        null
    } else {
        rememberTranslationEntry(
            text = plain,
            sources = sources,
            language = language,
            autoTranslate = autoTranslate,
            enabled = enabled,
        )
    }
    if (store == null || entry == null) {
        return RichTextTranslation(text = text, entry = null, showingTranslation = false)
    }
    val texts = plan.map { src -> src?.let { store.state(it) as? TranslationState.Done }?.text }
    val anyTranslated = texts.any { it != null }
    val showing = RustTranslate.showingBlocks(entry.showOriginal, texts)
    return RichTextTranslation(
        text = if (showing && anyTranslated) RichText(RustTranslate.applyBlocks(text.blocks, texts)) else text,
        entry = entry,
        showingTranslation = showing && anyTranslated,
    )
}

/**
 * 一段**纯文字**的翻译状态（视频标题 / 卡片摘要这种单段文字）：返回值见 [PlainTextTranslation]。
 *
 * 和 [rememberRichTextTranslation] 同一套口径，只是没有块结构。
 */
@Composable
fun rememberPlainTextTranslation(
    text: String,
    autoTranslate: Boolean = false,
    language: String? = null,
    enabled: Boolean = true,
): PlainTextTranslation {
    val store = LocalTranslation.current
    // `sources` 要 remember 住：每次重组新建一个 list 会让 `LaunchedEffect` 的 key 每帧都变
    // （那会取消 / 重启动那个 effect，自动翻那条路上会一直重新排队）
    val sources = remember(text) { listOf(text) }
    val entry = if (store == null) {
        null
    } else {
        rememberTranslationEntry(
            text = text,
            sources = sources,
            language = language,
            autoTranslate = autoTranslate,
            enabled = enabled,
        )
    }
    if (store == null || entry == null) return PlainTextTranslation(text = text, entry = null)
    val translated = (store.state(text) as? TranslationState.Done)?.text
    return PlainTextTranslation(
        text = RustTranslate.display(text, translated, entry.showOriginal),
        entry = entry,
    )
}

/** [rememberPlainTextTranslation] 的结果。 */
class PlainTextTranslation internal constructor(
    /** 现在该画的那一份。 */
    val text: String,
    /** 下面那一行；null = 不显示。 */
    val entry: TranslationEntryState?,
)

/**
 * 翻译入口的**组合状态**：要不要出现（Rust 判）、点「翻译」、切「显示原文」、重试、「重试」之后清掉失败。
 *
 * 返回值 null = 不出现（不是外语 / 翻译关着 / 没有本地翻译）。**判定在 Rust**，
 * 这里只持有"用户点了显示原文"这一格界面状态（跟内容走，换一条内容就重置）。
 */
@Composable
fun rememberTranslationEntry(
    /** 判定用 / `remember` 的身份：**整段**文字（正文是整篇、评论是整条）。 */
    text: String,
    /**
     * 真正要翻的那些文字（富文本是它的每一块，纯文字就是它自己）。
     *
     * 分开是因为正文按块翻（一次请求一段），而"要不要出入口"必须按整条判断——
     * 只有标题是外语、正文是中文的那种帖子不该出入口。
     */
    sources: List<String> = listOf(text),
    language: String? = null,
    autoTranslate: Boolean = false,
    enabled: Boolean = true,
): TranslationEntryState? {
    val store = LocalTranslation.current ?: return null
    // 判定要读设置（开关 / 目标语言 / 有没有本地翻译），这三样变了要重新判
    val visible = remember(text, language, enabled, store.enabled, store.target, store.available) {
        enabled && store.entryVisible(text, language)
    }
    if (!visible) return null
    // 桌面「点亮平台的自动翻」：进组合就排上（手机上这条不会进来，用户点了才翻）
    LaunchedEffect(sources, autoTranslate, store.enabled, store.target) {
        if (autoTranslate) sources.forEach { store.ensure(it) }
    }
    // "显示原文"跟内容走：换一条内容 / 换一段文字就回到"显示译文"
    var showOriginal by remember(text, language) { mutableStateOf(false) }
    val phase = store.phaseOf(sources, text, language)
    if (phase == TranslationPhase.HIDDEN) return null
    return TranslationEntryState(
        phase = phase,
        language = store.languageLabel(language, text),
        showOriginal = showOriginal,
        failure = sources.firstNotNullOfOrNull { store.failureReason(it) },
        onStart = { sources.forEach(store::start) },
        onToggle = { showOriginal = !showOriginal },
        onRetry = { sources.forEach(store::retry) },
    )
}

/**
 * 详情页的**标题**跟着正文那一行入口走（整篇一个入口，学 X）。
 *
 * - 正文入口不出现（不是外语）→ 标题不翻；
 * - 用户点了「翻译」或者桌面自动翻 → 标题也排上（和正文同一档）；
 * - 翻好了、且没点「显示原文」→ 返回译文；否则返回原标题。
 */
@Composable
fun rememberTitleTranslation(
    title: String,
    language: String? = null,
    autoTranslate: Boolean = false,
    entry: TranslationEntryState?,
): String? {
    val store = LocalTranslation.current ?: return null
    if (entry == null) return null
    val visible = remember(title, language, store.enabled, store.target) {
        store.entryVisible(title, language)
    }
    if (!visible) return null
    // 正文在翻 / 翻好了：标题也排上（点了之后正文先变成「正在翻译…」，标题这时一起去翻）
    val started = autoTranslate || entry.phase != TranslationPhase.IDLE
    LaunchedEffect(started, store.enabled, store.target) {
        if (started) store.ensure(title)
    }
    val done = (store.state(title) as? TranslationState.Done)?.text ?: return null
    return if (entry.showOriginal) null else done
}

/**
 * 长文正文的 Lazy 版本：每个块仍然单独成条，只翻滑到的块。
 *
 * [translated] = 现在画译文（正文上面那一行入口的「显示原文 / 显示译文」管着整段）。
 * 图片 / 视频 / 卡片保持原样。
 */
fun LazyListScope.translatedRichTextItems(
    text: RichText,
    keyPrefix: String,
    style: TextStyle,
    translated: Boolean,
    blockSpacing: Dp = 12.dp,
) {
    val blocks = text.blocks.filterNot { it.isBlank() }
    // 连续的图片是一组（宫格，见 imageLayout）：图片不翻，整组直接画
    val ctx = RichContext(blocks.filterIsInstance<Block.Image>())
    val units = richUnits(blocks)
    // "哪几块能翻"一次算完（下标和 `blocks` 对齐，不是 `text.blocks`）
    val plan = RustTranslate.planBlocks(blocks)
    itemsIndexed(units, key = { _, unit -> "$keyPrefix${unit.firstIndex}" }) { _, unit ->
        if (unit.isGrid) {
            Box(Modifier.padding(bottom = blockSpacing)) { RichUnitView(unit, ctx, style, null, Dp.Unspecified) }
            return@itemsIndexed
        }
        val block = unit.blocks.first()
        val store = LocalTranslation.current
        val src = plan.getOrNull(unit.firstIndex)
        // 只在"现在画译文"时才请求这一块的翻译：手机上用户没点「翻译」之前一个请求都不发
        LaunchedEffect(src, translated, store?.enabled, store?.target) {
            if (translated) src?.let { store?.ensure(it) }
        }
        val done = if (translated) src?.let { store?.state(it) as? TranslationState.Done }?.text else null
        val shown = if (done != null) RustTranslate.applyBlocks(listOf(block), listOf(done)).first() else block
        Box(Modifier.padding(bottom = blockSpacing)) {
            RichBlockView(shown, ctx, style, null, Dp.Unspecified)
        }
    }
}

/**
 * 一块的译文（没有就是原文）：`translated` 为 null 时什么都不做。
 *
 * 单块的入口留给"评论 / 楼中楼"这种小块（一次一两块，不值得攒一批）。
 */
internal fun Block.translatedOrSelf(translated: String?): Block =
    if (translated == null) this else RustTranslate.applyBlocks(listOf(this), listOf(translated)).first()

/**
 * 正文里所有可翻的块（顺序和块顺序一致）。
 *
 * 还在用它的地方：评论的楼中楼预览（要先把几条回复的可翻文字都取出来一起 `ensure`）。
 */
internal fun RichText.translatableTexts(): List<String> = RustTranslate.translatableTexts(blocks)
