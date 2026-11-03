package feedhub.app.embed

/**
 * NPU 模型的输入形状：批固定 **8**，每条长度跟着**包里那张图**走（`[NpuModelVariant.seqLen]`）。
 *
 * 批为什么还是 8：HTP 的 batch 也是烘进图里的，
 * 当前只动长度、不动批。
 *
 * **口径和 npu-research 的夹具一致**：
 * - 分词：`encode(text, L)` = `<bos>` + 最多 L-2 个正文 + `<eos>`（HF `enable_truncation(max_length=L)` 的规矩，
 *   特殊符号保留、正文从尾部截）；
 * - 填充：**id 用 0、mask 用 0**；`mask = (id != 0)` 和 `npu-research/scripts/eg2_common.py` 的 `build_inputs`
 *   一模一样（真 0 号 token 只出现在填充位置）；
 * - 填充行整行是 0（因果注意力下不会影响别的行）。
 *
 * 注意它和 CPU 那份的**不同**：CPU 每条最多 [GemmaTokenizer.DEFAULT_MAX_TOKENS] 个 token（形状按批内最长的那条
 * 动态给）；NPU 这条是**定长**、长度由图决定。长度不同的两份 NPU 向量也不是同一个空间
 * （见 `feedhub.app.library.npuModelId`）。
 */
const val NPU_BATCH: Int = 8

/** 一批定长输入：[ids] / [mask] 都是 `[batch * len]` 的行优先数组，前 [rows] 行是真的内容。 */
class NpuInputBatch(
    val ids: LongArray,
    val mask: LongArray,
    val rows: Int,
    val batch: Int = NPU_BATCH,
    val len: Int,
) {
    init {
        require(batch > 0 && len > 0) { "形状必须是正的（$batch × $len）" }
        require(ids.size == batch * len) { "ids 长度 ${ids.size} 对不上 $batch × $len" }
        require(mask.size == batch * len) { "mask 长度 ${mask.size} 对不上 $batch × $len" }
        require(rows in 0..batch) { "rows=$rows 不在 0..$batch 里" }
    }
}

/**
 * 把 [texts] 切成一批一批的定长输入（批 8；一次给多于 8 条就自己切）。
 *
 * [len] 是**必填**的：就是包里那张图的序列长度（[NpuModelVariant.seqLen]）。
 * 那种"忘了传就悄悄用错长度"的事情不能再有（长度对不上建会话直接失败）。
 *
 * 顺序和条数和 [texts] 一一对应：第 i 条在 `i / batch` 这一批的第 `i % batch` 行。
 */
fun npuInputs(
    tokenizer: GemmaTokenizer,
    texts: List<String>,
    len: Int,
    batch: Int = NPU_BATCH,
): List<NpuInputBatch> {
    if (texts.isEmpty()) return emptyList()
    require(batch > 0 && len > 0) { "形状必须是正的（$batch × $len）" }
    val out = ArrayList<NpuInputBatch>((texts.size + batch - 1) / batch)
    for (chunk in texts.chunked(batch)) {
        val ids = LongArray(batch * len)
        val mask = LongArray(batch * len)
        // 整批一次过边界：分词在 Rust 里，别一条一条调
        val encoded = tokenizer.encodeBatch(chunk, len)
        for (row in chunk.indices) {
            val tokens = encoded[row]
            for (i in tokens.indices) {
                val id = tokens[i]
                ids[row * len + i] = id.toLong()
                // 和 npu-research 的 build_inputs 同一口径：mask 就是"不是填充的 0"
                mask[row * len + i] = if (id != 0) 1L else 0L
            }
        }
        out.add(NpuInputBatch(ids, mask, chunk.size, batch, len))
    }
    return out
}
