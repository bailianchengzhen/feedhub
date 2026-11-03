package feedhub.app.embed

import feedhub.rust.RustGemmaTokenizer

/**
 * Gemma 分词器（EmbeddingGemma 2 的 `tokenizer.json`）：**BPE + byte fallback**，和 HuggingFace `tokenizers`
 * 逐 token 一致。
 *
 * 这个类只剩一层门面——
 * `tokenizer.json`（32 MB）只在 [fromJson] 时过一次边界，词表在 Rust 里**加载一次常驻**（merge 表约 8 MB），
 * 之后按**批**要 id（[encodeBatch] / [tokenLengths] / [truncateBatch]），不再一条一条过 JNI。
 * Kotlin 原实现退到 `:app` 的 desktopTest 里（`LegacyGemmaTokenizer`），
 * 只当 `TokenizerParityTest` 的对拍参照——搬过去之后行为没变靠它守着。
 *
 * 编码流程（和 HF 的 pipeline 一致，两边的口径一字不差）：
 * 1. 按 `added_tokens`（特殊符号，如 `<bos>`）切段，命中的直接出它的 id；
 * 2. 每段把空格换成 `▁`（U+2581）——normalizer；
 * 3. 每个码点：在词表里就用它的 id，否则按 UTF-8 逐字节换成 `<0xXX>`（byte fallback）；
 * 4. 反复合并"merge 序号最小"的相邻对，直到没有可合并的（`ignore_merges = false`）；
 * 5. 按 `post_processor` 的模板加 `<bos>` / `<eos>`，按需截断（每条最多 [DEFAULT_MAX_TOKENS] 个，含特殊符号）。
 *
 * **不是通用 tokenizer 实现**：只认这一份配置的形状（BPE、空 `continuing_subword_prefix` /
 * `end_of_word_suffix`、`fuse_unk = true`），遇到别的形状加载时直接抛 `IllegalStateException`，不静默算错。
 *
 * 线程：Rust 侧只读、`Send + Sync`，从哪条线程调都行（安卓的调度线程仍然照原样：分词在
 * `Dispatchers.Default` / 算批的那条后台线程上，见 `OnnxTextEmbedder.runBatch`）。
 *
 * **半个代理对**（`substring` 正好切在 emoji 中间、或者 JSON 里写了个孤立的 `\uD83D`）：过边界前换成
 * U+FFFD 再分词（Rust 的字符串只认合法 UTF-8，硬塞会整批抛 `MalformedInputException`；
 * 换掉之后和 HuggingFace `tokenizers` 的口径一致）。详见 `feedhub.rust.RustGemmaTokenizer.sanitize`。
 */
class GemmaTokenizer private constructor(private val native: RustGemmaTokenizer) {
    /** 词表里有多少个 token（只用于日志 / 设置页说明）。 */
    val vocabSize: Int get() = native.vocabSize()

    /**
     * 编码成**含特殊符号**的 token id（`<bos> … <eos>`），最多 [maxTokens] 个。
     *
     * 截断按 HF 的规矩：特殊符号保留，正文从尾部截掉（[maxTokens] = 64 时正文最多 62 个）。
     * [maxTokens] <= 0 表示不截断（测试里对拍整条输出用）。
     *
     * 单条版；批量请用 [encodeBatch]（一次过边界）。
     */
    fun encode(text: String, maxTokens: Int = DEFAULT_MAX_TOKENS): IntArray =
        native.encodeBatch(listOf(text), maxTokens)[0]

    /**
     * 一批文本 → 一批 id（含 `<bos>` / `<eos>`），和 [texts] 等长、等序。
     *
     * [maxTokens] 是这一批**每条**的上限（<= 0 = 不截断）。算批的地方（CPU / NPU 的 `runBatch`、封面图）
     * 一次给一整批，别在循环里调 [encode]。
     */
    fun encodeBatch(texts: List<String>, maxTokens: Int = DEFAULT_MAX_TOKENS): Array<IntArray> =
        native.encodeBatch(texts, maxTokens)

    /** 只要正文（不含 `<bos>` / `<eos>`），给对拍夹具用。 */
    fun encodeBody(text: String): IntArray = native.encodeBodyBatch(listOf(text))[0]

    /** 一批文本 → 一批正文 id（不含特殊符号、不截断）。 */
    fun encodeBodyBatch(texts: List<String>): Array<IntArray> = native.encodeBodyBatch(texts)

    /**
     * 这一批文本各几个 token（含 `<bos>` / `<eos>`、**不截断**的数）：调度器按它分桶挑图
     *
     * 只回个数、不回 id（省一次整批 id 的搬运）。
     */
    fun tokenLengths(texts: List<String>): IntArray = native.tokenLengths(texts)

    /**
     * 把文本截到最多 [maxTokens] 个 token。
     *
     * 返回的是**原文本的前缀**（不重新拼 token），保证 `encode(结果, 0).size <= maxTokens`；
     * 切点不会落在代理对中间（emoji 不会被劈成两个半字符）。[maxTokens] <= 0 或本来就装得下时原样返回。
     */
    fun truncateToTokens(text: String, maxTokens: Int): String =
        native.truncateBatch(listOf(text), maxTokens)[0]

    /** 一批文本按 token 截断（和 [texts] 等长、等序）。 */
    fun truncateBatch(texts: List<String>, maxTokens: Int): List<String> =
        native.truncateBatch(texts, maxTokens)

    companion object {
        /** 一条内容最多算多少个 token。 */
        const val DEFAULT_MAX_TOKENS: Int = 64

        /**
         * 从 `tokenizer.json` 的字节加载（安卓从 assets 读、桌面从 deb 里的文件读，都是这 32 MB 的一份）。
         *
         * 形状不对（换了模型 / 拿错文件）或 JSON 坏了会抛 `IllegalStateException`——和搬进 Rust 之前
         * Kotlin 版 `check()` 的行为一致，调用方的"读不出来就当没有模型"那条路不用改。
         */
        fun fromJson(json: ByteArray): GemmaTokenizer = GemmaTokenizer(RustGemmaTokenizer(json))
    }
}
