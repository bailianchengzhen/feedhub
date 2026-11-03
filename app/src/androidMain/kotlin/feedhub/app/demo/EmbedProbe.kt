package feedhub.app.demo

import android.content.Context
import android.content.Intent
import android.util.Log
import feedhub.app.embed.OnnxTextEmbedder
import feedhub.app.embed.embeddingMRL
import java.io.File
import kotlinx.coroutines.runBlocking

/**
 * **演示 / 自检用**的向量探针：`--ez demo true --es embedprobe "中文|English"`
 * 时，用**和正式通路同一个** [OnnxTextEmbedder] 给这几条文本算向量，写到
 * `getExternalFilesDir("embedprobe")/vectors.json`（一行一条：768 维原图 + 256 维 MRL）。
 *
 * 用途只有一个：给"Kotlin（安卓 ONNX 实现）算的向量 vs Python（onnxruntime + 同一个图）算的向量"的余弦对拍
 * （`tools/embed/check_vectors.py`）提供一个中文 + 一个英文的样本。**不发任何平台请求**，也不把文本打进 logcat。
 *
 * 和 `--ez demo true` 一样**只在允许演示数据的构建里生效**（debug / benchmark；release 的
 * `feedhub.demoBuild = false`，所以 release 装了也不会跑这条路）——见 `MainActivity.isDemoRequested`。
 */
fun runEmbedProbeIfRequested(context: Context, intent: Intent?) {
    val raw = intent?.getStringExtra("embedprobe")?.trim().orEmpty()
    if (raw.isEmpty()) return
    val app = context.applicationContext
    Thread({
        val embedder = OnnxTextEmbedder(app, log = { Log.i(TAG, it) })
        val texts = raw.split('|').map { it.trim() }.filter { it.isNotEmpty() }
        val vectors = runBlocking { embedder.embed(texts) }
        if (vectors == null) {
            Log.w(TAG, "embedprobe：模型不可用（bundled=${embedder.bundled}）")
            return@Thread
        }
        val dir = app.getExternalFilesDir("embedprobe") ?: app.filesDir
        dir.mkdirs()
        val file = File(dir, "vectors.json")
        file.bufferedWriter().use { w ->
            for (i in texts.indices) {
                val full = vectors[i]
                val mrl = embeddingMRL(full)
                w.write(
                    "{\"text\":${jsonString(texts[i])},\"model\":${jsonString(embedder.modelId)}," +
                        "\"dim\":${full.size},\"full\":${jsonArray(full)},\"mrl\":${jsonArray(mrl)}}",
                )
                w.write("\n")
            }
        }
        Log.i(TAG, "embedprobe：写了 ${texts.size} 条向量（${file.length()} 字节）→ ${file.absolutePath}")
        embedder.close()
    }, "feedhub-embedprobe").start()
}

private const val TAG = "feedhub-embed"

private fun jsonString(text: String): String {
    val sb = StringBuilder(text.length + 2)
    sb.append('"')
    for (c in text) {
        when {
            c == '"' -> sb.append("\\\"")
            c == '\\' -> sb.append("\\\\")
            c < ' ' -> sb.append(String.format("\\u%04x", c.code))
            else -> sb.append(c)
        }
    }
    sb.append('"')
    return sb.toString()
}

private fun jsonArray(values: FloatArray): String =
    values.joinToString(prefix = "[", postfix = "]", separator = ",") { it.toString() }
