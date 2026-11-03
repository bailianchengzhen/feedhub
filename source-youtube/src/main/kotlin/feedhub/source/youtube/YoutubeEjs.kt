package feedhub.source.youtube

import com.dokar.quickjs.QuickJs
import feedhub.core.text
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.newSingleThreadContext
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * n/sig 求解：在 **QuickJS**（`quickjs-kt`，Apache-2.0）里跑 **yt-dlp 的 EJS**
 *
 * 为什么要它：新版播放器的 n/sig 是 VM/表驱动的，NewPipe 那种"正则抽函数 + Rhino"不可靠；
 * yt-dlp 官方支持 EJS 的运行时就是 Deno / Node / QuickJS。EJS 只需要很小的存根
 * （`document` / `navigator` / `location` / `XMLHttpRequest` / `self=window=globalThis`），
 * **不需要真 DOM**。
 *
 * 流程照 yt-dlp `_builtin/ejs.py::_construct_stdin`：
 * ```js
 * <lib>; Object.assign(globalThis, lib); <core>;
 * jsc({type:"player", player:<播放器 JS>, requests:[{type:"n",challenges:[…]},{type:"sig",challenges:[…]}],
 *      output_preprocessed:true})
 * 返回 `{type:"result", responses:[{type:"result", data:{挑战→答案}}], preprocessed_player:"…"}`。
 * **预处理过的播放器缓存起来**：同一个播放器脚本第一次要编译，之后每次约 1 ms；
 * 用 `type:"preprocessed"` + 上次拿到的 `preprocessed_player` 就跳过重新编译。
 *
 * 资源：独占一个线程（QuickJS 绑定要求同一线程），内存上限 192 MiB；空闲 20 秒释放运行时。
 */
/**
 * **n/sig 求解器的形状**（n = 节流参数、sig = `signatureCipher` 的签名）。
 *
 * 抽出接口只为两件事：
 * 1. `YoutubeCipher.decode` / `YoutubeSource.play` 可以注入一个**假引擎**做对拍
 *    （`YoutubePlayEquivalenceTest` 比"交出去的挑战集合"和"拼进 URL 的位置"，不该为此跑几 MB 的 JS）；
 * 2. `RustYoutubeSource` 把同一份实现塞给 Rust 的 `HostScript` 端口——宿主是唯一的 JS 运行时。
 */
internal interface YoutubeNjs {
    /** 解一组挑战；同一份 `playerJs`（用 [playerKey] 认）只编译一次。 */
    suspend fun solve(playerJs: String, playerKey: String, requests: List<YoutubeEjs.Request>): Map<String, String>
}

internal class YoutubeEjs(
    private val scripts: Scripts = Scripts.fromResources(),
) : YoutubeNjs, AutoCloseable {
    /** 独占线程：`QuickJs` 的 JNI 绑定要在同一个线程上用。 */
    private val dispatcher = newSingleThreadContext("feedhub-youtube-ejs")
    private val scope = CoroutineScope(dispatcher + SupervisorJob())

    private var runtime: QuickJs? = null
    private var playerKey: String? = null
    private var preprocessedPlayer: String? = null
    private var idleCloseJob: Job? = null

    /**
     * 解一组挑战。[requests] 里同一型（`"n"` / `"sig"`）的挑战一次交上去，返回每个挑战的答案。
     * 同一 [playerJs]（用 [playerKey] 认，例如播放器的 hash）只编译一次。
     */
    override suspend fun solve(playerJs: String, playerKey: String, requests: List<Request>): Map<String, String> {
        if (requests.all { it.challenges.isEmpty() }) return emptyMap()
        return withContext(dispatcher) {
            idleCloseJob?.cancel()
            idleCloseJob = null
            try {
                val js = runtime ?: createRuntime().also { runtime = it }
                val useCache = this@YoutubeEjs.playerKey == playerKey && preprocessedPlayer != null
                val data = buildJsonObject {
                    if (useCache) {
                        put("type", "preprocessed")
                        put("preprocessed_player", JsonPrimitive(preprocessedPlayer!!))
                    } else {
                        put("type", "player")
                        put("player", JsonPrimitive(playerJs))
                        put("output_preprocessed", true)
                    }
                    put(
                        "requests",
                        kotlinx.serialization.json.JsonArray(
                            requests.map { r ->
                                buildJsonObject {
                                    put("type", r.type)
                                    put("challenges", kotlinx.serialization.json.JsonArray(r.challenges.map(::JsonPrimitive)))
                                }
                            },
                        ),
                    )
                }
                // 数据走 JSON.parse：播放器 JS 里什么字符都可能有，直接用对象字面量嵌进源码不稳
                val literal = JsonPrimitive(data.toString()).toString()
                val out: String = js.evaluate(
                    "var __d = JSON.parse($literal); JSON.stringify(jsc(__d));",
                    "feedhub_ejs_solve.js",
                )
                val parsed = feedhub.core.parseJsonObject(YOUTUBE, out)
                if (parsed.text("type") == "error") {
                    throw EjsException(parsed.text("error") ?: "EJS 求解失败")
                }
                val preprocessed = parsed.str("preprocessed_player")?.takeIf { it.isNotBlank() }
                if (preprocessed != null) {
                    preprocessedPlayer = preprocessed
                    this@YoutubeEjs.playerKey = playerKey
                } else if (useCache) {
                    // 缓存命中时 EJS 不回传 preprocessed_player
                    this@YoutubeEjs.playerKey = playerKey
                }
                val answers = LinkedHashMap<String, String>()
                parsed.array("responses").orEmpty().forEach { response ->
                    if (response.str("type") == "error") throw EjsException(response.str("error") ?: "EJS 求解失败")
                    response.field("data")?.forEach { (challenge, answer) ->
                        (answer as? JsonPrimitive)?.content?.let { answers[challenge] = it }
                    }
                }
                answers
            } finally {
                scheduleIdleClose()
            }
        }
    }

    /** 起一个 QuickJS 运行时，把 EJS 的 lib + core 装进去（照 yt-dlp 的拼接顺序）。 */
    private suspend fun createRuntime(): QuickJs {
        val js = QuickJs.create(dispatcher)
        js.memoryLimit = MEMORY_LIMIT
        try {
            js.evaluate<String>(
                SHIMS + "\n" + scripts.lib + "\nObject.assign(globalThis, lib);\n" + scripts.core + "\n;\"ok\"",
                "feedhub_ejs_init.js",
            )
        } catch (e: Exception) {
            runCatching { js.close() }
            throw EjsException("EJS 脚本装载失败：${e.message}")
        }
        return js
    }

    private fun scheduleIdleClose() {
        idleCloseJob?.cancel()
        idleCloseJob = scope.launch {
            delay(IDLE_MS)
            closeRuntimeOnly()
        }
    }

    private fun closeRuntimeOnly() {
        runtime?.let { runCatching { it.close() } }
        runtime = null
    }

    override fun close() {
        idleCloseJob?.cancel()
        idleCloseJob = null
        closeRuntimeOnly()
        playerKey = null
        preprocessedPlayer = null
        scope.cancel()
        dispatcher.close()
    }

    /** 一次求解请求：`type` 是 `"n"` 或 `"sig"`。 */
    data class Request(val type: String, val challenges: List<String>)

    /** 两个 EJS 脚本。 */
    data class Scripts(val lib: String, val core: String) {
        companion object {
            fun fromResources(): Scripts = Scripts(
                lib = read("yt_ejs/yt.solver.lib.min.js"),
                core = read("yt_ejs/yt.solver.core.min.js"),
            )

            private fun read(name: String): String =
                checkNotNull(Scripts::class.java.classLoader.getResourceAsStream(name)) {
                    "找不到 EJS 脚本 $name"
                }.use { it.readBytes().decodeToString() }
        }
    }

    private companion object {
        const val MEMORY_LIMIT = 192L * 1024 * 1024
        const val IDLE_MS = 5 * 60_000L

        /**
         * EJS 在 QuickJS 里需要的存根。
         * `Intl.NumberFormat` 只是"有这个东西"：meriyah / astring 不用它，播放器的 n 函数偶尔会碰。
         */
        val SHIMS = """
            globalThis.XMLHttpRequest = { prototype: {} };
            globalThis.location = {
              href: "https://www.youtube.com/watch?v=yt-dlp-wins",
              origin: "https://www.youtube.com",
              protocol: "https:",
              host: "www.youtube.com",
              hostname: "www.youtube.com",
              pathname: "/watch",
              search: "?v=yt-dlp-wins",
            };
            globalThis.document = Object.create(null);
            globalThis.navigator = Object.create(null);
            globalThis.self = globalThis;
            globalThis.window = globalThis;
            if (typeof globalThis.console === "undefined") {
              globalThis.console = { log: function () {}, error: function () {}, warn: function () {}, info: function () {} };
            }
            if (typeof globalThis.Intl === "undefined") globalThis.Intl = {};
            if (typeof globalThis.Intl.NumberFormat === "undefined") {
              globalThis.Intl.NumberFormat = function () {
                return { format: function (n) { return String(n); } };
              };
            }
        """.trimIndent()
    }
}

/** n/sig 求解本身失败（脚本装载不了、EJS 报错）：调用方应当回退到别的客户端，而不是当成风控。 */
class EjsException(message: String) : Exception(message)
