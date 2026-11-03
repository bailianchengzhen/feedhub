package feedhub.app.embed

/**
 * 包里带的 NPU 模型 = 一张**固定形状**的图。
 *
 * HTP 上做不了动态形状（npu-research 的结论：原图的 Shape/Range/CumSum 那套推导 QNN 不认），所以
 * "每条能吃多少 token"是**烘进图里的**：一个长度一张图。包里带哪些长度由构建参数决定
 * ，运行时这边只做**发现**，不猜。
 *
 * 所以构建时拆成一份 `model_w8a16_b8.weights`（ONNX external data）+ 每张图几 MB 的图结构；
 * 三张图引用**同一个文件、同一组 offset**。发现规则：`assets/embed-npu/` 里的文件名就是
 * `model_w8a16_b8_l<L>_emb.onnx`（`l` 后面是序列长度），名字不合规则的文件一律忽略——宁可"没有模型"，
 * 也不要拿一张形状对不上的图去建会话。
 *
 * 于是纯文字和封面向量的"文本+图"是**同一张图、同一个 QNN 会话**（以前是三张：
 * 纯文字 L32、L128 吃 `input_ids`，封面另有一张吃 `inputs_embeds`）。只吃 `input_ids` 的老图不再认。
 */
data class NpuModelVariant(
    /** 每条最多几个 token（含 `<bos>` / `<eos>`）。 */
    val seqLen: Int,
    /** assets 里的文件名（`model_w8a16_b8_l<L>_emb.onnx`）。 */
    val assetFile: String,
)

object NpuAssets {
    /** 模型放在 `assets/embed-npu/`。 */
    const val DIR: String = "embed-npu"

    /**
     * 几张图**共用**的那份量化权重（271.16 MB，ONNX external data；三张图里 322 个 initializer 的
     * `location` 都指它，offset 也完全一样）。
     *
     * 为什么要有它：三张图的权重逐字节相同，各带一份 273 MB 就是白占 546 MB 的包体积 / 运行时内存。
     * 拆完之后 APK 只多几 MB。
     */
    const val WEIGHTS_FILE: String = "model_w8a16_b8.weights"

    /**
     * 词表矩阵（u8，262144×512）在 [WEIGHTS_FILE] 里的位置 + 反量化参数（json）。图本身不再带 `Gather`，
     * 宿主按 id 查行拼成 `inputs_embeds`（纯文字直接查；封面在 64 个图位置填视觉特征）。
     */
    const val TABLE_FILE: String = "embed-table.json"

    /** 编译进包的模型名。长度写进文件名（形状是烘进图里的），但**换写法不改名**——见 [tag]。 */
    fun fileFor(seqLen: Int): String = "model_w8a16_b8_l${seqLen}_emb.onnx"

    /**
     * **图结构的指纹**。
     *
     * 为什么需要它：`fileFor` 只带长度，**换写法时文件名不变**——`SimplifiedLayerNormalization` 换成
     * opset 23 的 `RMSNormalization` 就是这种情况。文件名不变，
     * 但编出来的 HTP context binary 结构完全不同：老的 ctx 拿去建会话要么直接失败，要么更糟——
     * 看起来"加载成功"，其实跑的是**旧图**。所以 context 缓存的名字里必须带"这张图是哪一版"。
     *
     * 用**文件内容的摘要**而不是手写的版本常量：常量会忘（忘了就是同一个 bug 再来一次），摘要不会忘——
     * 图一变，ctx 名字就变，老 ctx 走 [ctxFileName] 的清理规则被删掉。
     */
    fun tag(sha256Hex: String): String = sha256Hex.take(8)

    /** context binary 的缓存名：**长度 + 图指纹**。换图 = 换名字 = 不会读到上一版的缓存。 */
    fun ctxFileName(seqLen: Int, tag: String): String = "ctx-htp-l$seqLen-$tag.onnx"

    /** `COPY_OK` 标记里的一行：哪张图 + 它的指纹（指纹变了就得重新拷，哪怕文件大小碰巧一样）。 */
    fun stagedMarker(assetFile: String, tag: String): String = "$assetFile $tag"

    private val FILE_PATTERN = Regex("""^model_w8a16_b8_l(\d+)_emb\.onnx$""")

    /** 从 assets 的文件名列表里认出模型（升序、去重）。 */
    fun variantsIn(names: Collection<String>): List<NpuModelVariant> = names
        .mapNotNull { name ->
            val m = FILE_PATTERN.matchEntire(name) ?: return@mapNotNull null
            val len = m.groupValues[1].toIntOrNull() ?: return@mapNotNull null
            if (len <= 0) null else NpuModelVariant(len, name)
        }
        .distinctBy { it.seqLen }
        .sortedBy { it.seqLen }

    /**
     * 这个包的**向量空间长度** = 包里最长的那个（0 = 没有模型）。
     *
     * `item_embedding.model` 用它（`npuModelId(spaceLen)`）：装得下的文本短图和最长那张算出来是同一个向量
     * ，所以**换图不换 model**，已经算好的向量不用重算。
     */
    fun spaceLen(available: Collection<Int>): Int = available.maxOrNull() ?: 0
}
