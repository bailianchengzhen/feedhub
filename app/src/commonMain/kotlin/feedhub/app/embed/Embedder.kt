package feedhub.app.embed

import feedhub.app.library.EMBEDDING_DIM
import feedhub.app.library.EMBEDDING_MODEL_ID
import feedhub.core.FeedItem
import feedhub.rust.RustEmbed

/**
 * 文本向量模型。
 *
 * 两个实现：安卓的 [feedhub.app.embed.OnnxTextEmbedder]（ONNX Runtime + 打进安装包的 EmbeddingGemma 2 q4 图），
 * 桌面的 [feedhub.app.embed.DesktopOnnxTextEmbedder]。没有模型时整条通路安静地不可用：[available] = false，[embed] 直接返回 null，不抛、不崩。
 */
interface Embedder : AutoCloseable {
    /** 写进 `item_embedding.model` 的标识（换模型就换它，老向量自然失效）。 */
    val modelId: String

    /** 这个构建 / 这台机器上模型能不能用（打包时没带模型就是 false）。 */
    val available: Boolean

    /**
     * 一次算一批文本，返回和 [texts] 等长、等序的向量（**模型原图维数**，这个模型是 768；截断 + 归一化由调用方
     * 用 [embeddingMRL] 做，因为那属于存储策略，不是模型的事）。
     *
     * 算不出来（没模型 / 会话建不起来 / 输入被拒）返回 null，**不抛异常**：向量是"有了更好"的东西，
     * 不能把界面或后台拖崩。调用方每批最多给 8 条。
     */
    suspend fun embed(texts: List<String>): List<FloatArray>?

    /**
     * 和 [embed] 一样，但**告诉实现这批最长的那条几个 token**。
     *
     * NPU 那条路包里带了多张**固定形状**的图（`[8,32]` / `[8,128]`），路由实现按 [maxTokens] 挑
     * **能装下这批的最短的那张**；装得下的文本短图和长图算出来是同一个向量。
     * [maxTokens] = "这批里最长的一条有几个 token（含 `<bos>` / `<eos>`）"；[Int.MAX_VALUE] = 不知道 / 不分组，
     * 用最长的那张图。
     *
     * 默认实现忽略它、直接走 [embed]（CPU / 桌面 / 测试都不需要分组）。
     */
    suspend fun embed(texts: List<String>, maxTokens: Int): List<FloatArray>? = embed(texts)

    /**
     * 这一批文本各几个 token（含 `<bos>` / `<eos>`，**不截断**的数）。调度器按它分桶。
     *
     * null = 这个实现不数（没有分词器 / 还没准备好）：调度器就退回"这批全用最长的那张图"，
     * 不会因为数不出来就把长文本塞进短图（那样正文被截掉、向量就错了）。
     */
    suspend fun tokenLengths(texts: List<String>): IntArray? = null

    /**
     * 把文本截到最多 [maxTokens] 个 token（**在 token 边界上切**，不切半个字）。
     *
     * 只给"点开过的条目带正文算"用：正文先折叠空白，再按 token 截到
     * 128 token 装得下的长度——按字数估会切多或切少，这条是硬要求。
     * null = 这个实现不会切（没有分词器），调用方按自己的字符上限兜底。
     */
    suspend fun truncateToTokens(text: String, maxTokens: Int): String? = null

    /**
     * 最近一批**实际**用的后端：[embed] 返回之后才准。
     *
     * 路由实现（[RoutingEmbedder]）在这里报告"这一批走了 NPU"或"NPU 失败、这批回退成了 CPU"；
     * 后台调度器拿它写性能记录（每批的后端 / 条数 / 耗时）。默认 = CPU（只有 CPU 实现的那些）。
     */
    val lastEmbedBackend: EmbedBackend get() = EmbedBackend.CPU

    /**
     * 最近一批**实际**写进 `item_embedding.model` 的标识（和 [lastEmbedBackend] 一起看）。
     *
     * CPU / NPU 是两个向量空间，所以路由实现会在这里给出**真正**用的那个 id；
     * 默认 = [modelId]（单后端的实现）。
     */
    val lastEmbedModelId: String get() = modelId

    /**
     * 最近一批**实际**用的是哪张固定长度的图（token 数；0 = 不是 NPU / 不知道）。
     * 汇总时按长度分开看。
     */
    val lastEmbedSeqLen: Int get() = 0

    /** 释放会话占用的内存（约 300 MB）；下次 [embed] 会重新懒建。 */
    override fun close() {}
}

/**
 * 只说"在哪儿算"的实现：设置页「当前实际用的后端」那行拿它显示
 * "ONNX Runtime 1.31.0 · CPU · 3 线程"这种话。安卓走 [NpuStatus]（[EmbeddingBackendControl]），不需要这个。
 */
interface EmbeddingRuntimeInfo {
    /** 一行说明（**不含**模型内容 / 路径里的用户名之类）。 */
    val runtimeLabel: String
}

/** 没有向量模型的组装（桌面 / 测试 / 打包时没带模型）：安静地"不可用"。 */
object UnavailableEmbedder : Embedder {
    override val modelId: String = EMBEDDING_MODEL_ID
    override val available: Boolean = false
    override suspend fun embed(texts: List<String>): List<FloatArray>? = null
}

/**
 * 卡片 → 送去算向量的那段文字。
 *
 * 格式照模型 README 的文档模板：`title: {标题} | text: {正文摘要}`；没有标题时是 `title: none | text: …`
 * 。几条自己定的规矩，理由都写在下面：
 *
 * - **摘要最多取 [maxSummaryChars] 个字**（默认 200）：手机上一批 8 条、每条最多 64 个 token，
 *   200 个中文字符足够撑满 64 个 token（模型的前缀已经占了十几个），再长只是白算；
 * - **空白折叠**成单个空格：标题里换行 / 连续空格会让同一个意思的词分成两个 token；
 * - **没标题的卡片**（很多小红书 / 抖音）：`title: none`，正文用摘要；摘要也没有就用所属圈子（贴吧吧名、
 *   知乎问题）或推荐理由，都没有才写 `text: none`；
 * - **视频卡片不加作者名**：模型的模板是 `title: … | text: …` 两段，作者名塞进去会偏离它训练时的格式；
 *   视频作者对"这条讲什么"的信息量也不如标题 / 简介，宁可保持和图文卡片同一种形状（离线回放时向量可比）。
 */
fun embeddingText(item: FeedItem, maxSummaryChars: Int = RustEmbed.MAX_SUMMARY_CHARS): String =
    RustEmbed.embeddingText(item, maxSummaryChars)

/** 一条卡片值不值得算向量：18+ 不算，标题和正文都没有的也不算（算出来也是噪声）。 */
fun FeedItem.hasEmbeddingText(): Boolean = RustEmbed.hasEmbeddingText(this)

/**
 * 详情正文最多算到几个 token。
 *
 * 128 = 包里最长那张图能装下的长度。正文按这个截断之后，
 * **所有长度 ≥128 的图看到的是同一段正文**，向量才稳定；这也让"点开过的条目"和"首页卡片"用同一种形状的文本
 * （`title: … | text: …`），只是 text 换成了正文开头。
 */
const val DETAIL_MAX_TOKENS: Int = 128

/**
 * 详情正文的字符上限（兜底用）：128 个中文 token 大约 100～160 个字，正文再长也没用，
 * 先按字符砍一刀是为了**不让一条几万字的专栏在内存里来回拷贝 / 分词**。真正的截断按 token 做
 * （[Embedder.truncateToTokens]），这里只是"肯定够"的上限。
 */
const val DETAIL_MAX_CHARS: Int = 1200

/**
 * 点开过的条目用它自己的正文重新拼一段文字。
 *
 * 格式和 [embeddingText] 一样（`title: … | text: …`），只是 `text` 从"卡片摘要"换成"正文开头"：
 * - 正文先**折叠空白**（正文里的换行 / 连续空格会让同一个词分成两个 token），再按 [maxChars] 砍一刀；
 * - 正文截断到 [DETAIL_MAX_TOKENS] 个 token 由调用方做（那要分词器，见 [Embedder.truncateToTokens]）；
 * - 正文是空的（纯图片帖、视频没简介）就退回 [embeddingText]：**不要**拿空 text 去算，那和卡片文字不是一回事。
 *
 * 正文**只用来算向量**：不存库、不导出、不进任何表。
 */
fun detailEmbeddingText(item: FeedItem, body: String?, maxChars: Int = DETAIL_MAX_CHARS): String =
    RustEmbed.detailEmbeddingText(item, body, maxChars)

/**
 * MRL（Matryoshka）：EmbeddingGemma 2 原生支持 768 → 128 / 256 / 512 维截断。当前存 **256 维**
 * （[EMBEDDING_DIM]）：MTEB 多语言只掉约 1 个百分点，存储是 1/3。
 *
 * 截前 [dim] 维之后**重新做 L2 归一化**：不归一化的话截断后的向量长度会变小，
 * 点积就不再等于余弦相似度了。
 */
fun embeddingMRL(vec: FloatArray, dim: Int = EMBEDDING_DIM): FloatArray {
    require(dim > 0) { "dim 必须 > 0（收到 $dim）" }
    return RustEmbed.mrl(vec, dim)
}

/** L2 归一化（原地改并返回）；全零向量原样返回（不产生 NaN）。 */
fun l2NormalizeInPlace(vec: FloatArray): FloatArray {
    val out = RustEmbed.l2Normalize(vec)
    out.copyInto(vec)
    return vec
}

/** 两个**已归一化**向量的余弦相似度（= 点积）；长度不一致时取公共前缀（离线工具里对齐用）。 */
fun embeddingCosine(a: FloatArray, b: FloatArray): Float = RustEmbed.cosine(a, b)
