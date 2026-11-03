package feedhub.rust

import uniffi.feedhub_ffi.FfiScriptAnswer
import uniffi.feedhub_ffi.FfiScriptRequest
import uniffi.feedhub_ffi.HostScript

/**
 * **n/sig 的 JS 求解端口**。
 *
 * 为什么是宿主：EJS（yt-dlp 的 `yt.solver.lib.min.js` + `yt.solver.core.min.js`）要在 **QuickJS** 里跑，
 * quickjs-kt 是 Kotlin 侧的依赖（桌面是 JVM 变体、安卓是 AAR 里的 `libquickjs.so`）；`.so` 里既不带
 * JS 引擎也不想带。Rust 只负责"要解什么"（扫 `signatureCipher` 的 `s`、直链里的 `n`）和"答案拼回哪里"。
 *
 * **整批一次**：Rust 把这一份 `player` 里所有要解的挑战按 `sig` / `n` 两组一次交进来，
 * 这边跑**一遍** EJS 拿回全部答案——不是一条挑战过一次边界（一次边界的固定成本 + 一次 EJS 调用）。
 *
 * **同步阻塞**：Rust 的宿主回调都在 `Dispatchers.IO` 的线程上同步回来（和 [RustHttpHost] 一样），
 * 所以这里用 [runBlocking] 把 `suspend` 的 `solveChallenges` 跑完。撑不住的话那是几十毫秒的 JS，
 * 不是网络；而且调用方本来就是 IO 线程。
 *
 * **失败就回空**：解不出来（EJS 报错、脚本读不到、QuickJS 起不来）返回**空列表**——Rust 那边按
 * "这个客户端失败了、换下一个"处理（和 Kotlin 原版 `catch (e: Exception)` 一样）。
 * **绝不在这里抛异常**：跨 UniFFI 边界抛出去的异常会把一次播放打成内部错误，用户看到的是"播放失败"
 * 而不是"换一个客户端再试"。
 */
class RustScriptHost(
    /**
     * 真正干活的实现（`source-youtube` 的 `YoutubeEjs` 包一层）。给 `null` = 这个宿主不提供求解能力，
     * Rust 会把需要解 n/sig 的响应当成这个客户端失败。
     */
    private val solve: (suspend (playerJs: String, playerKey: String, requests: List<Pair<String, List<String>>>) -> Map<String, String>)?,
) : HostScript {
    override fun solve(
        playerJs: String,
        playerKey: String,
        requests: List<FfiScriptRequest>,
    ): List<FfiScriptAnswer> {
        val solver = solve ?: return emptyList()
        if (requests.all { it.challenges.isEmpty() }) return emptyList()
        return try {
            val pairs = requests.map { it.kind to it.challenges }
            val answers = kotlinx.coroutines.runBlocking { solver(playerJs, playerKey, pairs) }
            answers.map { (challenge, answer) -> FfiScriptAnswer(challenge = challenge, answer = answer) }
        } catch (e: kotlinx.coroutines.CancellationException) {
            // 取消照旧往外抛：那次播放本来就不要了
            throw e
        } catch (e: Exception) {
            // 解不出来 = 这个客户端失败（Rust 会换下一个 / 报 all-clients-failed），不是崩溃
            emptyList()
        }
    }
}
