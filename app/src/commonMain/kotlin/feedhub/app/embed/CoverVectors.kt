package feedhub.app.embed

import feedhub.app.feed.CardThumbnails
import feedhub.core.FeedItem
import feedhub.core.Media
import feedhub.core.Platform
import feedhub.rust.RustEmbed

/**
 * 封面向量：**文字 + 封面图**一起算的向量。
 *
 * 和纯文字向量（[EmbeddingScheduler] 那条）**并存、不替换**：同一条内容可以两种都有，
 * 库里按 `item_embedding.model` 分开（[feedhub.app.library.npuCoverModelId]），画像 / 导出回放也按 model 分开。
 * 当前**权重仍然是 0**：只算、只存、只导出，首页顺序一条都不变。
 *
 * 这个文件只剩三样东西：
 * 1. 图片层的**缩略图地址改写**（[CardThumbnails.of]，图片加载唯一一份口径，不能搬——否则 UI 和向量会用两套）；
 * 2. 设备侧的接口（[CoverEmbedder] / [CoverImageSource] / [TextSessionGate]）；
 * 3. 一些编译期常量。
 */

/**
 * **哪些内容算封面向量：每个平台一行**。
 *
 * 口径是"**有封面 / 配图的视频和短图文**要不要算"，不是"这个平台有没有图"：
 * - **算**：B站 / 抖音 / 小红书 / YouTube / TikTok 的视频封面，X / Reddit 带图的帖子；
 * - **不算**：知乎、酷安（长文为主）、贴吧、小黑盒——文字为主的平台继续只有纯文字向量。
 *
 * 多图（九宫格）**只取第一张**：`[8,128]` 的文本图里图占 64 个 token，第二张就要再挤掉 64 个位置。
 *
 * 表本体在 Rust（`cover::PLATFORM_TABLE`），这里是它的一份只读视图（测试断言"每个已装平台都有一行"）。
 */
object CoverRules {
    /** 平台 id → 算不算封面向量（键是 `Platform.key`，不是显示名、不是插件类型）。 */
    val platformTable: Map<String, Boolean> get() = RustEmbed.coverPlatformTable()

    /** 这个平台算不算封面向量。 */
    fun counts(platform: Platform): Boolean = RustEmbed.coverCounts(platform.key)

    /** 表里所有"算"的平台 id（测试 / 设置页文案用）。 */
    fun coverPlatforms(): Set<String> = RustEmbed.coverPlatforms().toSet()

    /**
     * 封面取哪一档缩略图（首页卡片那一档）。
     *
     * 视觉编码器的原生分辨率是 48×48 像素一个 soft token（8×8 方阵 = 384×384），640 宽的缩略图足够、
     * 也不用为向量多下一张原图（研究：缩略图 vs 原图的向量只差 0.009）。
     */
    const val COVER_THUMB_WIDTH: Int = 640

    /**
     * 试缓存时要试的宽度档（**从大到小**）：首页卡片的宽度是按窗口算的（[CardThumbnails.bucket] 320～1920），
     * 所以同一条内容在缓存里可能是 480 / 640 / 800……哪一档。先按最可能的 640 找，找不到再退几档。
     */
    val CACHE_WIDTHS: List<Int> get() = RustEmbed.coverCacheWidths.toList()
}

/**
 * 这条内容的**第一张**封面 / 配图（原地址，没改成缩略图）；没有就是 null。
 *
 * 顺序（Rust 判）：视频封面（[Media.Video.cover]）优先，然后是正文里的第一张图（[Media.Image]）。
 */
fun coverImageUrl(item: FeedItem): String? = RustEmbed.coverUrl(item)

/**
 * 这条内容试哪些地址（**缩略图优先**，缓存命中率高、流量小）。
 *
 * 每个宽度档一个地址（[CardThumbnails.of] 的写法），去重、保序：先 640，再几档退路。
 * 平台认不得 / 地址已经是服务端缩好的那档时，[CardThumbnails.of] 原样返回，去重之后就一个地址。
 */
fun coverImageUrls(item: FeedItem, widths: List<Int> = CoverRules.CACHE_WIDTHS): List<String> {
    val raw = coverImageUrl(item) ?: return emptyList()
    return coverImageUrlsOf(raw, widths)
}

/**
 * 一个封面原地址 → 各宽度档要试的地址（去重、保序）。
 *
 * 缩略图改写走**图片加载层那唯一一份** [CardThumbnails]（规则本体、连去重都在 Rust `thumb::of_widths`），
 * 所以封面窗口试缓存用的地址和首页卡片上画的是同一个写法——不会漂。
 */
fun coverImageUrlsOf(raw: String, widths: List<Int> = CoverRules.CACHE_WIDTHS): List<String> =
    CardThumbnails.ofWidths(raw, widths)

/**
 * 这条内容值不值得算封面向量：平台在表里 + 有图 + 不是 18+ + 有文字（Rust 判）。
 *
 * **不在这里挡"图片取不取得到"**：那是运行时的事（缓存里没有、又不在 Wi-Fi 上就先跳过，下次再试）。
 */
fun FeedItem.hasCoverVector(): Boolean = RustEmbed.coverHas(this)

/**
 * 封面向量要送的**一段**文字（和纯文字向量同一个口径：`title: … | text: …`）。
 *
 * 为什么不再加"封面"之类的提示词：文本图里图已经占了最前面 64 个 token，文本侧的口径和研究里对拍过的
 * 完全一样——换口径就等于换向量空间。
 */
fun coverEmbeddingText(item: FeedItem): String = RustEmbed.coverText(item)

/**
 * 文本图里**图占多少个 token**（8×8 方阵 = 64 个 soft token）。
 *
 * 这是烘进图里的常量（`tv_b8_l128_k64_*.onnx` 的 K=64），不是可配参数：换 K 要重编图。
 */
const val COVER_IMAGE_TOKENS: Int = 64

/**
 * 文本+图那张图里，**正文**最多放几个 token（含 `<eos>`）。
 *
 * `[8,128]` 的布局是：0=`<bos>`、1=`<|image>`、2..65=64 个图 token、66=`<image|>`、67.. 正文、`<eos>`、0 填充。
 * 所以正文 = 128 − 67 − 1 = **60**。
 */
const val COVER_TEXT_TOKENS: Int = 128 - (2 + COVER_IMAGE_TOKENS + 1) - 1

/**
 * 一条要算封面向量的输入：**送进模型的文字** + **封面图的编码字节**（JPEG / WebP / PNG 原样，设备上去解码）。
 *
 * 为什么传编码字节而不是 Bitmap：commonMain 不认得 `android.graphics.Bitmap`，而解码 / 缩放
 * （[COVER_INPUT_SIDE]×同一侧）本来就是设备侧的事；测试里给一张假 PNG 就能把调度走完。
 */
class CoverInput(
    val text: String,
    val image: ByteArray,
) {
    override fun toString(): String = "CoverInput(text=${text.length} chars, image=${image.size} B)"
}

/** 视觉图的输入边长（384 = 8 个 soft token × 48 像素）。 */
const val COVER_INPUT_SIDE: Int = 384

/** 一个 patch 的边长（16 像素；48 = patch 16 × 池化 3）。 */
const val COVER_PATCH: Int = 16

/**
 * 384×384 的 **ARGB 像素** → `pixel_values [1, 576, 768]`（视觉图唯一的输入）。
 *
 * 口径照 `tools/embed-vision/egv_common.py` 逐行对齐（实现在 Rust `cover::patchify`）：
 * - 像素值**除以 255**、不归一化（`do_normalize=false`）；
 * - patch 是 16×16×3，**行优先**：先按 (patch 行, patch 列) 排 patch，patch 内部再按 (y, x) 排像素，
 *   每个像素按 R、G、B；
 * - 8×8 方阵这条路**不保持长宽比**（拉伸到 384×384）。
 *
 * @param argb 长度必须是 `side * side` 的 ARGB（安卓 `Bitmap.getPixels` 的格式：A<<24|R<<16|G<<8|B）。
 */
fun coverPatchify(argb: IntArray, side: Int = COVER_INPUT_SIDE): FloatArray =
    RustEmbed.coverPatchify(argb, side)

/**
 * 封面向量这一侧的实现（安卓 = 两张 HTP 图；桌面 / 测试 = 没有）。
 *
 * 和 [Embedder] 分开是因为它**不是"每条内容都有"**的东西：只在充电时跑、要图片、会开关两个会话。
 * 所有方法都不抛：失败就 null + 一行日志，纯文字向量那条一点都不受影响。
 */
interface CoverEmbedder : AutoCloseable {
    /** 写进 `item_embedding.model` 的标识（[feedhub.app.library.npuCoverModelId]）。 */
    val modelId: String

    /** 这个构建带没带两张图（视觉 + 文本+图）。 */
    val bundled: Boolean

    /** 设置页那句说明（后端 / 两张图 / context 多大）。 */
    val detail: String

    /** 探头：这台设备能不能走（null = 能，否则一句原因）。 */
    suspend fun probe(): String?

    /**
     * 建好当前要用的会话（**分时**：视觉先，文本+图后；现编图时不许别的 QNN context 在场）。
     *
     * 失败返回一句原因，并且**当前不再重试**（调用方把封面口径标成本次进程不可用，纯文字照旧）。
     */
    suspend fun prepare(): String?

    /**
     * 算一批（一个"封面窗口"，最多 [CoverVectorScheduler.windowSize] 条）：返回和 [items] 等长等序的 768 维向量。
     *
     * 两种 null 不是一个意思，**别混**：
     * - **外层 null** = 这个窗口整体失败（QNN 6001 / 会话建不起来）→ 调用方把封面口径标成本次进程不可用；
     * - **某一条 null** = 那一条算不出来（图解码失败 / 形状怪异）→ 调用方只跳过这一条、不写库。
     */
    suspend fun embed(items: List<CoverInput>): List<FloatArray?>?

    /** 最近一个窗口的账（视觉 / 文本各多少毫秒、切了几次）；没跑过是 null。 */
    val lastWindow: CoverWindowStats?

    /** 窗口结束：关掉两张会话（内存掉回纯文字那份）。 */
    override fun close()
}

/** 一个封面窗口的账（性能记录 / 设置页 / 日志；**不含任何内容**）。 */
data class CoverWindowStats(
    /** 这个窗口算成了几条。 */
    val count: Int,
    /** 视觉那一段：几张图、一共多少毫秒。 */
    val visionImages: Int,
    val visionMs: Long,
    /** 文本+图那一段：几批、一共多少毫秒。 */
    val textBatches: Int,
    val textMs: Long,
    /** 整个过程（含 load / 关会话 / 解码）多少毫秒。 */
    val totalMs: Long,
    /** 这个窗口里**现编**了几张图（0 = 都是 ctx 加载；1 = 只有视觉；2 = 两张都编了）。 */
    val compiled: Int,
) {
    /** 一条摊销多少毫秒。 */
    val msPerItem: Long get() = if (count > 0) totalMs / count else 0
}

/**
 * 封面图的来源：**先看 App 的图片磁盘缓存**（就是首页卡片已经在显示的那张），没有再按缩略图地址取一次。
 *
 * 安卓的实现走 App 自己的 Coil 单例（和界面同一个磁盘缓存 / 同一个 OkHttp 客户端 → 代理 + UA 都是 App 那套），
 * 并且**只在"不按流量计费"的网络上**才真的发请求；测试里换成假的。
 */
interface CoverImageSource {
    /**
     * 按顺序试这些地址：缓存里命中哪个就用哪个（**不发请求**）；都没有再按第一个地址取一次
     * （只有 [unmetered] 为 true 时才取）。拿不到（离线 / 计费网络 / 404 / 解码不了）返回 null。
     */
    suspend fun load(urls: List<String>): ByteArray?

    /** 这个进程里真发出去的封面图片请求数（日志 / 设置页对账；缓存命中不算）。 */
    val fetched: Int

    /** 现在是不是"不按流量计费"的网络（Wi-Fi / 以太网）。 */
    fun unmetered(): Boolean
}

/**
 * 分时用的闸门：算封面前，纯文字那条先把自己的 QNN 会话放掉。
 *
 * 为什么必须有它：真机上"另一张 QNN context 在场时现编文本图"是必挂的
 * ，研究里最后能跑的组合
 * 是"两个都从 ctx 加载"或者"严格串行"。这里选后者——最省事、内存峰值也只有一个会话。
 *
 * 纯文字那条放掉之后不用谁去重建：`RoutingEmbedder.embed` 发现没有就绪的引擎会自己
 * `prepare()` 从 ctx 加载（真机 3 s 量级），调度器的批也不会丢。
 */
interface TextSessionGate {
    /** 放掉所有就绪的 QNN 会话（不关开关、不清 broken；下次 `embed` 会自己从 ctx 重建）。 */
    fun releaseSessions()
}
