package feedhub.cli

import okhttp3.Interceptor
import okhttp3.Response
import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.nio.file.Files
import java.nio.file.Path

/**
 * `--record DIR`：把每个响应的 body 存成文件，拿去当单测的 fixtures。
 * 文件名 = 域名首段 + 路径，例如 `api_x_web-interface_wbi_index_top_feed_rcmd.json`；
 * 同一次运行里重复请求同一个接口（翻页）时依次加 `_2`、`_3`……
 * 不是 UTF-8 文本的 body（例如贴吧的 protobuf）原样存成 `.bin`。
 * 只存 body，不存请求头（Cookie 不会落进去）。
 */
class RecordingInterceptor(private val dir: Path) : Interceptor {
    private val seen = mutableMapOf<String, Int>()

    override fun intercept(chain: Interceptor.Chain): Response {
        val response = chain.proceed(chain.request())
        val url = response.request.url
        val name = (listOf(url.host.substringBefore('.')) + url.pathSegments.filter { it.isNotEmpty() })
            .joinToString("_") { it.replace(Regex("[^A-Za-z0-9._-]"), "-") }
        val bytes = response.peekBody(Long.MAX_VALUE).bytes()
        val body = utf8OrNull(bytes)
        val ext = when {
            body == null -> "bin"
            body.trimStart().startsWith("{") || body.trimStart().startsWith("[") -> "json"
            else -> "txt"
        }
        val n = synchronized(seen) { seen.merge(name, 1, Int::plus)!! }
        val file = dir.resolve(if (n == 1) "$name.$ext" else "${name}_$n.$ext")
        Files.createDirectories(dir)
        Files.write(file, bytes)
        System.err.println("  已录制 $file")
        return response
    }
}

/** 合法 UTF-8 且没有除 \t \n \r 以外的控制字符就当文本，否则 null（二进制）。 */
private fun utf8OrNull(bytes: ByteArray): String? {
    val text = try {
        Charsets.UTF_8.newDecoder().decode(ByteBuffer.wrap(bytes)).toString()
    } catch (_: CharacterCodingException) {
        return null
    }
    return text.takeIf { t -> t.none { it < ' ' && it != '\t' && it != '\n' && it != '\r' } }
}
