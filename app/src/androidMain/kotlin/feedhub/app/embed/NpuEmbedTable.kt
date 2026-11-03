package feedhub.app.embed

import org.json.JSONObject
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.channels.FileChannel
import java.nio.file.StandardOpenOption

/**
 * 词表矩阵（u8，262144×512）：**在共用权重文件里按需读行**，反量化成 float32。
 *
 * 图里不再有 `Gather`：纯文字按 id 整行查（[lookup]），封面在 64 个图位置填视觉特征、其余位置也是查这张表
 * （[writeRow]）。位置和参数来自 `assets/embed-npu/embed-table.json`（构建时从纯文字 L128 图的
 * `embed_tokens` 抄出来的：offset / vocab / dim / scale / zero_point）；权重文件只读 mmap，
 * 271 MB 不会整份进 RSS，用到的那几页才进来。
 */
class NpuEmbedTable private constructor(
    private val weights: ByteBuffer,
    val offset: Long,
    val vocab: Int,
    val dim: Int,
    val scale: Float,
    val zeroPoint: Int,
) {
    private val rowBuf = ByteArray(dim)

    /** 第 [id] 行反量化后写进 `dst[at until at + dim]`（越界的 id 夹到词表里，和研究里的对拍口径一致）。 */
    fun writeRow(id: Int, dst: FloatArray, at: Int) {
        val row = id.coerceIn(0, vocab - 1)
        val pos = offset + row.toLong() * dim
        synchronized(this) {
            weights.position(pos.toInt())
            weights.get(rowBuf, 0, dim)
            for (i in 0 until dim) {
                dst[at + i] = ((rowBuf[i].toInt() and 0xFF) - zeroPoint) * scale
            }
        }
    }

    /** 一批 id（行优先，`batch × len`）→ `inputs_embeds`（`batch × len × dim`）。 */
    fun lookup(ids: LongArray): FloatArray {
        val out = FloatArray(ids.size * dim)
        for (i in ids.indices) writeRow(ids[i].toInt(), out, i * dim)
        return out
    }

    companion object {
        /** 读 json + 只读映射权重文件（[weightsFile] 是解到私有目录里的那份）。 */
        fun open(json: String, weightsFile: File): NpuEmbedTable {
            val j = JSONObject(json)
            val map = FileChannel.open(weightsFile.toPath(), StandardOpenOption.READ).use { ch ->
                // channel 关掉映射依然有效（映射是内核那边的引用）
                ch.map(FileChannel.MapMode.READ_ONLY, 0, ch.size()).order(ByteOrder.LITTLE_ENDIAN)
            }
            val t = NpuEmbedTable(
                weights = map,
                offset = j.getLong("offset"),
                vocab = j.getInt("vocab"),
                dim = j.getInt("dim"),
                scale = j.getDouble("scale").toFloat(),
                zeroPoint = j.getInt("zero_point"),
            )
            check(t.offset + t.vocab.toLong() * t.dim <= map.capacity()) {
                "词表矩阵越过了权重文件的末尾（offset=${t.offset}、${t.vocab}×${t.dim}，文件 ${map.capacity()} B）"
            }
            return t
        }
    }
}

/**
 * 封面向量借纯文字那张图算"文本+图"这一步。
 *
 * 实现是那张 128 token 的 [NpuOnnxTextEmbedder]——**和路由手里是同一个实例**（[NpuOnnxEngines] 按长度缓存），
 * 所以会话、词表、词表矩阵都只有一份。
 */
interface CoverTextRunner {
    /** 这张图的序列长度（封面布局烘死在 128，对不上就不能借）。 */
    val seqLen: Int

    /** 这张图的 ctx 编好了吗（编好了封面才能常驻：现编时不能有别的 QNN context 在场）。 */
    fun ctxReady(): Boolean

    /** 词表（和纯文字同一份；读不出来是 null）。 */
    fun tokenizerOrNull(): GemmaTokenizer?

    /** 词表矩阵（要先把模型解到私有目录；失败是 null）。 */
    fun embedTableOrNull(): NpuEmbedTable?

    /**
     * 跑一批 `inputs_embeds [8,L,512]` + `attention_mask [8,L]` → 8 个归一化的 768 维向量。
     * 没会话就先从 ctx 建（没 ctx 就现编）；推理 / 建会话失败**抛出去**（调用方决定退回分时还是停）。
     */
    fun runEmbeds(embeds: FloatArray, mask: LongArray): List<FloatArray>

    /** 会话要是封面这边建的（路由还没认领）就关掉；路由手里的不动。 */
    fun releaseBorrowed()
}
