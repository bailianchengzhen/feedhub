package feedhub.app.login

import androidx.compose.runtime.snapshotFlow
import feedhub.app.AppForeground
import feedhub.core.SourceException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import java.io.IOException

/**
 * 扫码 / 设备码登录共用的轮询循环。
 *
 * 切回来弹框就是「网络错误」、再也不问了——
 * 后台时 国产 ROM 冻结 / 断网、内置核心 30 秒后也停了，这期间的一次轮询失败就把整个流程判死。所以：
 * - **退到后台不问**，等回到前台再问；回来先等 [RESUME_SETTLE_MS] 让网络 / 内置核心缓过来，不再等满一个间隔；
 * - 轮询时的**纯网络错误**（`IOException`，不是 [SourceException]）不算失败：界面照旧显示码，按原间隔再问；
 *   连续 [MAX_NETWORK_FAILURES] 次（只数前台的）才报错，短码 / 二维码到期（[expiresAtMs]）就当过期；
 * - 平台明确的错误（风控、接口错）照旧**立刻停**，原样抛出去，不重试。
 *
 * 不会比以前多发请求：间隔还是平台给的，只是后台那段不发了。
 *
 * @param isFinal 这个状态是不是终点（成功 / 过期 / 失败）。
 * @param onState 每次拿到状态；`networkTrouble` = 刚才那次网络不通、正在等下一次。
 * @return 终点状态；本地判断到期时返回 [expired]。
 */
internal suspend fun <S> pollLogin(
    intervalMs: Long,
    expiresAtMs: Long?,
    gate: ForegroundGate,
    expired: S,
    poll: suspend () -> S,
    isFinal: (S) -> Boolean,
    onState: (state: S?, networkTrouble: Boolean) -> Unit,
    now: () -> Long = System::currentTimeMillis,
): S {
    var failures = 0
    while (true) {
        delay(intervalMs)
        if (gate.awaitForeground()) delay(RESUME_SETTLE_MS)
        if (expiresAtMs != null && now() >= expiresAtMs) return expired
        val state = try {
            poll()
        } catch (e: IOException) {
            // 请求发出去之后才退到后台的那次不算（失败本来就是后台造成的）
            if (gate.isForeground() && ++failures >= MAX_NETWORK_FAILURES) throw e
            onState(null, true)
            continue
        }
        failures = 0
        onState(state, false)
        if (isFinal(state)) return state
    }
}

/** 轮询看的"App 在不在前台"。桌面一直在前台（[AppForeground] 没人调 `set`）。 */
internal interface ForegroundGate {
    fun isForeground(): Boolean

    /** 在前台就马上返回 false；不在就挂起到回前台，返回 true。 */
    suspend fun awaitForeground(): Boolean

    companion object {
        val Always = object : ForegroundGate {
            override fun isForeground() = true
            override suspend fun awaitForeground() = false
        }

        fun of(foreground: AppForeground) = object : ForegroundGate {
            override fun isForeground() = foreground.isForeground

            override suspend fun awaitForeground(): Boolean {
                if (foreground.isForeground) return false
                snapshotFlow { foreground.isForeground }.first { it }
                return true
            }
        }
    }
}

/** 回前台后先等一下再问：内置核心重启 + 健康检查、冻结后的网络恢复，都在这一两秒里。 */
internal const val RESUME_SETTLE_MS = 1500L

/** 前台连续这么多次网络不通才报错。 */
internal const val MAX_NETWORK_FAILURES = 5

/** 轮询间隔里网络不通时的提示。 */
internal const val NETWORK_RETRY_HINT = "网络暂时不通，稍后自动再试…"
