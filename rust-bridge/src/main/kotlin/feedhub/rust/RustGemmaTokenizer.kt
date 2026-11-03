package feedhub.rust

import uniffi.feedhub_ffi.FfiTokBatch
import uniffi.feedhub_ffi.FfiTokException
import uniffi.feedhub_ffi.FfiTokTokenizer

/**
 * 本地向量的分词器：`FfiTokTokenizer` 的 Kotlin 门面。
 *
 * 和别的门面（`RustBili` / `RustLibraryBridge`）一个路子：
 * - **加载 `.so`**：第一次真的要用的时候才解压 + 指路（见 [RustNative]）；
 * - **错误折回现有语义**：词表形状不认 / JSON 坏了都抛 `IllegalStateException`——调用方
 *   （`OnnxTextEmbedder.loadTokenizer` 之类）本来就是 `runCatching { … }` 出问题当"没有模型"，
 *   搬进 Rust 之前是 `check()` 抛的 `IllegalStateException`，语义不变；
 * - **按批过边界**：`FfiTokBatch` 是"拼平的 id + 每条的个数"，这里一次 `copyOfRange` 拆成 `IntArray`，
 *   不产生 `List<List<Int>>` 那种一层层装箱的东西。
 *
 * 这一层只做类型转换，没有别的心智负担：分词口径全在 Rust（`feedhub-core/src/embed/tokenizer.rs`）。
 */
class RustGemmaTokenizer(json: ByteArray) {
    private val native: FfiTokTokenizer = try {
        RustNative.ensureLoaded()
        FfiTokTokenizer(json)
    } catch (e: FfiTokException) {
        throw IllegalStateException(e.message ?: "tokenizer.json 读不了（${e::class.simpleName}）", e)
    }

    /** 词表里有多少个 token（只算单码点的那张表，和 Kotlin 版的 `vocabSize` 同口径）。 */
    fun vocabSize(): Int = native.vocabSize()

    /** 一批文本 → 一批 id（含 `<bos>` / `<eos>`）；`maxTokens <= 0` = 不截断。 */
    fun encodeBatch(texts: List<String>, maxTokens: Int): Array<IntArray> =
        toRows(native.encodeBatch(sanitize(texts), maxTokens))

    /** 一批文本 → 一批正文 id（不含特殊符号、不截断）。 */
    fun encodeBodyBatch(texts: List<String>): Array<IntArray> = toRows(native.encodeBodyBatch(sanitize(texts)))

    /** 一批文本各几个 token（含特殊符号、不截断）。 */
    fun tokenLengths(texts: List<String>): IntArray = native.tokenLengths(sanitize(texts)).toIntArray()

    /** 一批文本按 token 截断（返回原文本前缀）。 */
    fun truncateBatch(texts: List<String>, maxTokens: Int): List<String> =
        native.truncateBatch(sanitize(texts), maxTokens)

    /**
     * 把**半个代理对**换成 U+FFFD（`�`）。
     *
     * 为什么要有这一步：Rust 的 `String` 只认合法 UTF-8，而 Kotlin 的 `String` 可以装半个代理对
     * （`String.substring` 正好切在 emoji 中间、或者 JSON 里写了一个孤立的 `\uD83D`）。这种字符串过 UniFFI
     * 时 UTF-8 编码会直接抛 `MalformedInputException`——那是"整批算不出来"，比"这一条不准"糟得多。
     *
     * 换掉之后两边口径仍然一致：HuggingFace 的 `tokenizers` 也是按 UTF-8 读的，半个代理对在它那里同样是 U+FFFD。
     * 完整的代理对（正常 emoji）原样保留。绝大多数文本连一个代理字符都没有，所以先扫一遍、没有就原样返回。
     */
    private fun sanitize(texts: List<String>): List<String> = texts.map { text ->
        var i = 0
        var dirty = false
        while (i < text.length) {
            val c = text[i]
            if (c.isHighSurrogate() && i + 1 < text.length && text[i + 1].isLowSurrogate()) {
                i += 2
                continue
            }
            if (c.isSurrogate()) {
                dirty = true
                break
            }
            i++
        }
        if (!dirty) text else buildString(text.length) {
            var k = 0
            while (k < text.length) {
                val c = text[k]
                if (c.isHighSurrogate() && k + 1 < text.length && text[k + 1].isLowSurrogate()) {
                    append(c).append(text[k + 1])
                    k += 2
                } else if (c.isSurrogate()) {
                    append('\uFFFD')
                    k++
                } else {
                    append(c)
                    k++
                }
            }
        }
    }

    /** 拼平的 id → 每条的 `IntArray`（`lens` 和 `ids` 都是 Rust 给的，长度一定对得上）。 */
    private fun toRows(batch: FfiTokBatch): Array<IntArray> {
        val rows = Array(batch.lens.size) { IntArray(batch.lens[it]) }
        var at = 0
        for (row in rows) {
            for (i in row.indices) row[i] = batch.ids[at++]
        }
        check(at == batch.ids.size) {
            "分词器返回的 ids 长度对不上：lens 合计 $at，ids 有 ${batch.ids.size}"
        }
        return rows
    }
}
