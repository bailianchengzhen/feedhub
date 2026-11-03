package feedhub.app.api

import kotlinx.coroutines.flow.StateFlow

/**
 * 说明书（skill）装在哪：deb 里把仓库的 `skill/feedhub/` 整个装到 `/opt/feedhub/skill/feedhub/`，
 * 进阶设置那一页显示这个路径。
 * 两份是同一份文件。
 */
const val APP_API_SKILL_DIR: String = "/opt/feedhub/skill/feedhub"

/** 说明书的完整路径（deb 里）。 */
const val APP_API_SKILL_FILE: String = "$APP_API_SKILL_DIR/SKILL.md"

/**
 * **桌面 App 的对外接口**。
 *
 * 再配一份说明书，agent 读了说明书自己去 `curl --unix-socket`。
 *
 * 三条硬约束（别在实现里走样）：
 * 1. **不是独立程序**：没有 cli、没有守护进程、没有 systemd 单元。App 开着才有它，App 关掉就什么都不剩；
 * 2. **没有自己的状态**：不自己发请求、不自己读写 Cookie / state.json、不自己限速、不自己记冷却——
 *    请求全部走这个 App 进程里的 `AppGraph`（同一套 Source / OkHttp / Cookie / 设置 / 资料库）；
 * 3. **默认关**：设置 →「进阶设置 → 对外接口（agent）」里打开才在听（[feedhub.app.AppSettings.appApi]）。
 *
 * 实现只有桌面一份（`DesktopAppApi`，HTTP over unix socket）；安卓 / 演示 / 测试的组装不给实现，
 * 那一页在设置里就不出现。
 */
interface AppApi {
    /** 现在在不在听 / socket 在哪 / 被调过几次（设置页那张卡片读它）。 */
    val status: StateFlow<AppApiStatus>

    /**
     * 开关打开时调（在 IO 线程上）：建目录、删掉上次残留的 socket、开始 accept。重复调用是空操作。
     * 起不来（端口 / 路径被别的进程占着、权限不够）时把原因记进 [AppApiStatus.error]，不抛。
     */
    fun start()

    /** 开关关掉时调：停止 accept、删掉 socket 文件、等处理中的请求收尾。重复调用是空操作。 */
    fun stop()

    /** App 收尾（`AppGraph.shutdown`）：和 [stop] 一样，另外不再接受 [start]。 */
    fun close()
}

/**
 * 接口现在的样子。
 *
 * @param enabled 设置里的开关（用户想要的），[listening] 是**实际**在不在听——两者不一致时 [error] 会说明原因
 *   （例如 socket 路径被别的实例占着）。
 * @param socketPath 在听时是 socket 的绝对路径；关着时是"打开后会在哪"（界面照样显示，方便用户去 curl）。
 * @param calls 从打开开关到现在一共被调了几次（**只记次数、时间和有没有出错，不记参数和内容**）。
 * @param lastCallAt 最近一次调用的时刻（Unix 毫秒）；还没被调过就是 null。
 * @param lastError 最近一次失败的错误码（例如 `network`）；`null` = 最近一次是好的。不含请求内容。
 * @param error 起不来的原因（"另一个 feedhub 进程已经在提供这个接口" / 权限不够……）；`null` = 没这个问题。
 */
data class AppApiStatus(
    val enabled: Boolean = false,
    val listening: Boolean = false,
    val socketPath: String? = null,
    val calls: Long = 0,
    val lastCallAt: Long? = null,
    val lastError: String? = null,
    val error: String? = null,
)
