package feedhub.proxy

import feedhub.core.ProxyMode
import feedhub.rust.ProxyTarget
import feedhub.core.proxy.CoreStatus
import feedhub.core.proxy.ImportReport
import feedhub.core.proxy.LatencyResult
import feedhub.core.proxy.ProxyCoreState
import feedhub.core.proxy.Subscription
import feedhub.core.proxy.SubscriptionRoute
import feedhub.core.proxy.SubscriptionUserInfo
import feedhub.core.proxy.unavailableReason
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.yield
import uniffi.feedhub_ffi.FfiProxyCore
import uniffi.feedhub_ffi.FfiProxyCoreRoute
import java.nio.file.Path

/**
 * **内置代理核心**：把 `sing-box` 当子进程跑，只给 App 自己用。
 *
 * 实现全在 Rust（`rust/crates/feedhub-core/src/proxy_core/`）：订阅 / 分享链接 / Clash YAML 的解析、
 * sing-box 配置生成、`proxy-core.json` 的读写、sha256 校验、子进程的起停与退避重启、延迟测试、
 * 「只在前台跑」的判定表。这个类只做三件**宿主**的事：
 *
 * 1. **状态 / 日志**：两条 `StateFlow`，界面直接 `collectAsState`；
 * 2. **定时器**：[start] 之后每 [ProxyCoreBridge.POLL_INTERVAL_MS] 调一次 Rust 的 `poll`
 *    （喂当前时间），并推进前台宽限期；[autoUpdateCheckMs] > 0 时定期查自动更新。
 *
 * **取订阅的收发也不在宿主了**：以前是 [ProxyClients] 那三个 OkHttp 客户端 +
 * `RustProxyFetchHost`，现在是 Rust 里的 [`feedhub_net`] 客户端。宿主只剩"建对象时把 `System` 那一档
 * 解析成具体的一档"（[systemProxy]，口径和 `ProxyTarget.of` 一样）。
 *
 * 原来的 Kotlin 实现挪进了 `core` 的 testFixtures（`feedhub.core.proxy.legacy.LegacyProxyCore`），
 * 只给对拍当参照；生产路径只有 Rust 这一份，**没有开关**。
 *
 * @param binary sing-box 可执行文件。
 * @param dataDir 放生成的配置 / 探针配置的目录（App 数据目录，权限 600 的文件写在里面）。
 * @param stateFile `proxy-core.json` 的路径（null = 只在内存里，测试 / 截图用）。
 * @param scope 长命的协程作用域（App 活多久它活多久）。
 * @param systemProxy `System` 那一档取订阅时用哪一档代理：**按订阅地址的主机名**解析（App 里传
 *   `{ host -> ProxyTarget.of(ProxyMode.System, listOf(host)) }`，口径和平台对象那条完全一样）。
 * @param expectedSha256 二进制的 sha256 期望值（默认按体系结构推，见 [ProxyCoreBridge.expectedSha256]）。
 * @param autoUpdateCheckMs 多久查一次"有没有订阅该自动更新了"（0 = 不查，测试 / 截图用）。
 */
class ProxyCore(
    /** sing-box 可执行文件。 */
    val binary: Path,
    /** 放生成的配置 / 探针配置的目录（App 数据目录，权限 600 的文件写在里面）。 */
    val dataDir: Path,
    scope: CoroutineScope,
    private val systemProxy: (host: String) -> ProxyTarget = { ProxyTarget.Direct },
    private val stateFile: Path? = null,
    expectedSha256: String? = DEFAULT_EXPECTED_SHA256,
    private val autoUpdateCheckMs: Long = 600_000,
    private val expectedSha256Provider: (() -> String?)? = null,
) : AutoCloseable {

    private val job: Job = SupervisorJob(scope.coroutineContext[Job])
    private val core = CoroutineScope(scope.coroutineContext + job)

    private val _status = MutableStateFlow<CoreStatus>(CoreStatus.Stopped)
    val status: StateFlow<CoreStatus> = _status.asStateFlow()

    private val _logs = MutableStateFlow<List<String>>(emptyList())
    val logs: StateFlow<List<String>>
        get() {
            ensureLogsSubscription()
            return _logs.asStateFlow()
        }

    private val _state = MutableStateFlow(ProxyCoreState())

    /** 现在的整份状态（Rust 是真相，这是过河之后的快照；首次读时懒加载 `proxy-core.json`）。 */
    val state: StateFlow<ProxyCoreState>
        get() {
            native
            return _state.asStateFlow()
        }

    private val wakeSignal = Channel<Unit>(Channel.CONFLATED)

    @Volatile
    private var isForeground: Boolean = true

    @Volatile
    private var logsSubscribed: Boolean = false

    /** 生成的 sing-box 配置（**有机密**，权限 600）。 */
    val configPath: Path get() = java.nio.file.Paths.get(native.configPath())

    @Volatile
    private var nativeRef: FfiProxyCore? = null

    /**
     * Rust 核心实例：**懒建**。
     * 用户没选「内置核心」、也没点进代理设置页时，冷启动不读 `proxy-core.json`、不解析节点 JSON、不过 FFI、不起协程。
     */
    private val native: FfiProxyCore by lazy(LazyThreadSafetyMode.SYNCHRONIZED) {
        val sha256 = when {
            expectedSha256Provider != null -> expectedSha256Provider.invoke()
            expectedSha256 === DEFAULT_EXPECTED_SHA256 -> ProxyCoreBridge.expectedSha256()
            else -> expectedSha256
        }
        val nowMs = System.currentTimeMillis()
        ProxyCoreBridge.create(
            binary = binary,
            dataDir = dataDir,
            stateFile = stateFile,
            expectedSha256 = sha256,
            notify = NotifyTarget(this),
            nowMs = nowMs,
        ).also { created ->
            nativeRef = created
            created.setClock(nowMs)
            created.setForeground(isForeground)
            created.setLogsSubscribed(logsSubscribed)
            _state.value = created.state().toCore()
            syncAutoUpdateJob()
            ensureEventLoop()
            wakeSignal.trySend(Unit)
        }
    }

    private var pollJob: Job? = null
    private var logsSubscriptionJob: Job? = null
    private var autoUpdateJob: Job? = null

    // -----------------------------------------------------------------------
    // 给 App 用的一档：内置核心
    // -----------------------------------------------------------------------

    /**
     * 「内置核心」这一档的请求走法：在跑就给 `127.0.0.1:<端口>` 的 SOCKS5；
     * 没在跑给一个 `valid == false` 的替身，请求直接失败（fail-closed，绝不悄悄直连）。
     *
     * 读的是 [status] 这份镜像（Rust 起停 / 轮询时经 `statusChanged` 推过来），**不过 FFI**：
     * 镜像最多比 Rust 晚一个轮询；晚的那一下顶多是"端口连不上、请求失败"，仍然 fail-closed。
     */
    fun proxyMode(): ProxyMode = when (val s = _status.value) {
        is CoreStatus.Running -> ProxyMode.BuiltinCore(s.port)
        else -> ProxyMode.BuiltinCore(0, s.unavailableReason())
    }

    /**
     * 核心**正在起**（[CoreStatus.Starting]）时阻塞等它有结论，返回那时的 [proxyMode]；不在起就是 null。
     *
     * 给请求那条路用：回前台后核心要几百毫秒才 bind 好，这期间的请求
     * 不该按"没在跑"直接失败。**界面线程上不等**（安卓主线程 / 桌面 AWT 事件线程，按线程名认）——
     * 返回 null，调用方照旧 fail-closed。最多等 [START_WAIT_MS]（和 Rust 的健康检查期限一样）。
     */
    fun awaitStarting(): ProxyMode? {
        if (_status.value !is CoreStatus.Starting) return null
        val name = Thread.currentThread().name
        if (name == "main" || name.startsWith("AWT-EventQueue")) return null
        runBlocking { withTimeoutOrNull(START_WAIT_MS) { status.first { it !is CoreStatus.Starting } } }
        return proxyMode()
    }

    /** 现在能不能用（在跑）。同 [proxyMode]：读镜像，不碰 Rust 的锁。 */
    val running: Boolean get() = _status.value is CoreStatus.Running

    val port: Int? get() = (_status.value as? CoreStatus.Running)?.port

    // -----------------------------------------------------------------------
    // 进程生命周期
    // -----------------------------------------------------------------------

    /**
     * App 里"设置 → 网络 → 代理"选了「内置核心」时调用；选了别的就停掉（不占内存）。
     *
     * **不阻塞调用方**：调用方是 `MainActivity.onStart → AppGraph.applyForeground`（主线程）。
     * 11-02 起 Rust 只起进程、
     * 探活交给事件循环的 `poll`，这里仍放在一条单线程的 IO 通道上按顺序执行（sha256 校验 / 读版本号第一次也要几百毫秒）；
     * 结果经 [status] 推出去。
     */
    fun setEnabled(enabled: Boolean) {
        if (!enabled && nativeRef == null) return
        // 先把镜像标成"正在起"：真正的起停排在 IO 通道上，这几毫秒里进来的请求也该等（[awaitStarting]），
        // 而不是看到 Stopped 直接失败。Rust 一回话（Starting / Running / Failed）就以它为准。
        if (enabled && _status.value == CoreStatus.Stopped) _status.value = CoreStatus.Starting(0)
        core.launch(lifecycle) {
            native.setClock(System.currentTimeMillis())
            applyStatus(native.setDesiredRunning(enabled))
            wakeSignal.trySend(Unit)
        }
    }

    /** 手动重启（进阶设置页的按钮；挂了以后自动重启到了上限也用它）。同 [setEnabled]，不阻塞调用方。 */
    fun restart() {
        core.launch(lifecycle) {
            native.setClock(System.currentTimeMillis())
            applyStatus(native.restart())
            wakeSignal.trySend(Unit)
        }
    }

    /** 起停 / 重启排队用的单线程通道（保证顺序，且不占调用方线程）。 */
    private val lifecycle = Dispatchers.IO.limitedParallelism(1)

    // -----------------------------------------------------------------------
    // 「只在前台跑」（安卓；桌面没有"退到后台"这回事）
    // -----------------------------------------------------------------------

    /**
     * 设置里选了「内置核心」/ 换成别的档位。返回"现在该不该跑"（`null` = 没变）。
     *
     * 安卓上**不要**直接调 [setEnabled]：走这个（前台判定 + 30 秒宽限期在 Rust 的判定表里，
     * 宿主只负责把结果转给 [setEnabled]）。
     */
    fun setEnabledGate(enabled: Boolean): Boolean? {
        if (!enabled && nativeRef == null) return null
        native.setClock(System.currentTimeMillis())
        val wanted = native.setEnabled(enabled)
        if (wanted != null) applyStatus(native.status())
        wakeSignal.trySend(Unit)
        return wanted
    }

    /** App 进前台 / 退到后台（安卓侧从 Activity 生命周期喂）。返回"现在该不该跑"（`null` = 没变）。 */
    fun setForeground(foreground: Boolean): Boolean? {
        isForeground = foreground
        val cur = nativeRef ?: return null
        syncAutoUpdateJob()
        cur.setClock(System.currentTimeMillis())
        val wanted = cur.setForeground(foreground)
        if (wanted != null) applyStatus(cur.status())
        wakeSignal.trySend(Unit)
        return wanted
    }

    /** 现在是不是"核心该跑"（宽限期里退到后台仍算真；测试 / 排查用）。 */
    fun shouldRun(): Boolean = status.value != CoreStatus.Stopped && status.value !is CoreStatus.Failed

    // -----------------------------------------------------------------------
    // 订阅 / 节点 / 导入
    // -----------------------------------------------------------------------

    /** 加一个订阅（同 URL 重复加不会重复）。[autoUpdateHours] 是这条订阅自己的自动更新间隔（0 = 关）。 */
    fun addSubscription(name: String, url: String, autoUpdateHours: Int = 0): Subscription {
        native.setClock(System.currentTimeMillis())
        val sub = native.addSubscription(name, url, autoUpdateHours).toCore()
        refreshState()
        return sub
    }

    /** 改某条订阅的自动更新间隔（小时，0 = 关）。 */
    fun setSubscriptionAutoUpdate(id: String, hours: Int) {
        native.setSubscriptionAutoUpdate(id, hours)
        refreshState()
    }

    fun renameSubscription(id: String, name: String) {
        native.renameSubscription(id, name)
        refreshState()
    }

    fun removeSubscription(id: String) {
        native.removeSubscription(id)
        refreshState()
    }

    fun removeNode(id: String) {
        native.removeNode(id)
        refreshState()
    }

    /**
     * 整份替换状态（截图 / 演示用：灌一份假数据进去，`ProxyDemo` 就是这么做的）。
     *
     * **不是给生产用的**：生产路径是下面那些逐个方法（每个都只改一处，Rust 那边只算一次）。
     */
    fun replaceState(state: ProxyCoreState) {
        native.setClock(System.currentTimeMillis())
        native.setState(state.toFfi())
        refreshState()
    }

    /**
     * `proxy-core.json` 的**文本**（Rust 写出来的那一份，逐字节等于 Kotlin 原版）。
     *
     * 生产路径不需要它（[replaceState] 和各改状态的方法自己落盘）；它是给"对拍比字节"用的。
     */
    fun stateJsonText(): String = native.stateJsonText()

    /** 从一段 `proxy-core.json` 文本读状态（坏了就是空状态，和 Kotlin 的 `ProxyCoreStore.load` 一样）。 */
    fun parseStateJson(text: String): ProxyCoreState = native.parseStateJson(text).toCore()

    /** 手动选节点；null = 自动（延迟最低，`urltest`）。 */
    fun selectNode(id: String?) {
        native.selectNode(id)
        refreshState()
    }

    /** 改测试地址 / 日志级别 / UA / 取订阅路线（订阅自己的自动更新间隔用 [setSubscriptionAutoUpdate]）。 */
    fun updatePrefs(f: (ProxyCoreState) -> ProxyCoreState) {
        val cur = native
        cur.setClock(System.currentTimeMillis())
        val before = _state.value
        val after = f(before)
        if (after == before) return
        cur.updatePrefs(
            testUrl = after.testUrl.takeIf { it != before.testUrl },
            logLevel = after.logLevel.takeIf { it != before.logLevel },
            subscriptionRoute = after.subscriptionRoute
                .takeIf { it != before.subscriptionRoute }
                ?.let { route -> FfiProxyCoreRoute.valueOf(route.name) },
            subscriptionUa = after.subscriptionUa.takeIf { it != before.subscriptionUa },
        )
        refreshState()
    }

    /** 粘贴一条或多条分享链接、或一段 sing-box 出站 JSON。 */
    suspend fun importText(text: String): ImportReport = withContext(Dispatchers.IO) {
        native.setClock(System.currentTimeMillis())
        val report = native.importText(text).toCore()
        refreshState()
        report
    }

    /**
     * 更新一个订阅：Rust 决定"发什么请求 / 走哪条路"、也自己发（`feedhub-net`），
     * 回来之后判状态码 / 解析 / 合并 / 记账。宿主只是等这一趟返回。
     */
    suspend fun updateSubscription(id: String): Result<Int> = withContext(Dispatchers.IO) {
        native.setClock(System.currentTimeMillis())
        feedSystemProxy(id)
        try {
            when (val update = native.updateSubscription(id)) {
                is uniffi.feedhub_ffi.FfiProxyCoreUpdate.Ok -> {
                    refreshState()
                    Result.success(update.parsed)
                }
                is uniffi.feedhub_ffi.FfiProxyCoreUpdate.Failed -> {
                    refreshState()
                    Result.failure(IllegalStateException(update.reason))
                }
            }
        } catch (e: IllegalArgumentException) {
            Result.failure(e)
        }
    }

    /**
     * 有没有该自动更新的订阅，有就更新。**每条订阅按自己的间隔**（[Subscription.autoUpdateHours]，0 = 关）。
     * 返回更新成功的订阅 id 表。
     */
    suspend fun updateDueSubscriptions(): List<String> = withContext(Dispatchers.IO) {
        native.setClock(System.currentTimeMillis())
        // **逐条调**（不用 Rust 的 `update_due_subscriptions`）：每条订阅的主机可能不同，而 `System`
        // 那一档的代理是**按主机**解析的，得逐条喂（见 [feedSystemProxy]）。走的还是同一个
        // `native.updateSubscription`，和 Rust 那个批量方法做的事逐条一样。
        val done = mutableListOf<String>()
        for (id in native.dueSubscriptions()) {
            feedSystemProxy(id)
            val update = runCatching { native.updateSubscription(id) }.getOrNull()
            if (update is uniffi.feedhub_ffi.FfiProxyCoreUpdate.Ok) done += id
        }
        refreshState()
        done
    }

    /**
     * 把 `System` 那一档的代理喂给 Rust（**按这条订阅地址的主机名**解析，和 OkHttp 那条路的
     * `PlatformProxySelector.select(uri)` 同一个口径）。
     *
     * 为什么逐条喂而不是建对象时定死：订阅地址是用户随时能改的，而"系统代理"是**按主机**问的
     * （`PlatformProxySelector.select(uri)` 拿到的就是请求的 URL）。所以每次更新之前喂一次。
     */
    private fun feedSystemProxy(id: String) {
        val url = state.value.subscriptions.firstOrNull { it.id == id }?.url ?: return
        val host = runCatching { java.net.URI(url).host }.getOrNull().orEmpty()
        if (host.isBlank()) return
        runCatching { native.setSystemProxy(systemProxy(host).toFfi()) }
    }

    // -----------------------------------------------------------------------
    // 延迟测试
    // -----------------------------------------------------------------------

    /** 测一个节点的延迟（另起一个一次性 sing-box，量墙钟时间）。 */
    suspend fun testLatency(nodeId: String): LatencyResult = withContext(Dispatchers.IO) {
        native.setClock(System.currentTimeMillis())
        val result = native.testLatency(nodeId)
        refreshState()
        LatencyResult(result.nodeId, result.ms, result.error)
    }

    /** 全部测一遍（最多 [concurrency] 个同时测）。返回测了几个。 */
    suspend fun testAllLatencies(concurrency: Int = 4): Int = withContext(Dispatchers.IO) {
        native.setClock(System.currentTimeMillis())
        val count = native.testAllLatencies(concurrency)
        refreshState()
        count
    }

    // -----------------------------------------------------------------------
    // 日志
    // -----------------------------------------------------------------------

    fun clearLogs() {
        native.clearLogs()
        _logs.value = native.logs()
    }

    // -----------------------------------------------------------------------
    // 内部
    // -----------------------------------------------------------------------

    private fun refreshState() {
        _state.value = native.state().toCore()
        syncAutoUpdateJob()
        wakeSignal.trySend(Unit)
    }

    private fun applyStatus(status: uniffi.feedhub_ffi.FfiProxyCoreStatus) {
        val mapped = status.toCore()
        if (_status.value != mapped) _status.value = mapped
    }

    private fun syncAutoUpdateJob() {
        val needed = autoUpdateCheckMs > 0 &&
            isForeground &&
            _state.value.subscriptions.any { it.autoUpdateHours > 0 }
        if (needed) {
            if (autoUpdateJob?.isActive != true) {
                autoUpdateJob = core.launch(Dispatchers.IO) {
                    while (isActive) {
                        delay(autoUpdateCheckMs)
                        runCatching { updateDueSubscriptions() }
                    }
                }
            }
        } else {
            autoUpdateJob?.cancel()
            autoUpdateJob = null
        }
    }

    private fun ensureLogsSubscription() {
        if (logsSubscriptionJob?.isActive == true) return
        logsSubscriptionJob = core.launch(Dispatchers.Default) {
            _logs.subscriptionCount
                .map { it > 0 }
                .distinctUntilChanged()
                .collect { subscribed ->
                    logsSubscribed = subscribed
                    val cur = if (subscribed) native else nativeRef
                    cur?.setLogsSubscribed(subscribed)
                    if (subscribed && cur != null) {
                        val current = cur.logs()
                        if (_logs.value != current) _logs.value = current
                    }
                }
        }
    }

    private fun ensureEventLoop() {
        ensureLogsSubscription()
        if (pollJob?.isActive == true) return
        pollJob = core.launch(Dispatchers.Default) {
            // 未选内置核心 / 稳定运行无待到期事件时，挂在 wakeSignal.receive() 上，零定时器唤醒。
            while (isActive) {
                wakeSignal.receive()
                while (isActive) {
                    val now = System.currentTimeMillis()
                    native.setClock(now)
                    native.poll()
                    // 宽限期（只在前台跑）：到点了 Rust 会说"该停了"
                    native.foregroundPoll()?.let { wanted ->
                        native.setClock(System.currentTimeMillis())
                        applyStatus(native.setDesiredRunning(wanted))
                    }
                    applyStatus(native.status())
                    if (logsSubscribed) {
                        val lines = native.logs()
                        if (_logs.value != lines) _logs.value = lines
                    }
                    val delayMs = native.nextWakeDelayMs(System.currentTimeMillis()) ?: break
                    if (delayMs <= 0L) {
                        yield()
                    } else {
                        withTimeoutOrNull(delayMs) {
                            wakeSignal.receive()
                        }
                    }
                }
            }
        }
    }

    internal fun onStatus(status: uniffi.feedhub_ffi.FfiProxyCoreStatus) = applyStatus(status)

    internal fun onLogs(lines: List<String>) {
        if (_logs.value != lines) _logs.value = lines
    }

    internal fun onWake() {
        wakeSignal.trySend(Unit)
    }

    override fun close() {
        pollJob?.cancel()
        pollJob = null
        logsSubscriptionJob?.cancel()
        logsSubscriptionJob = null
        autoUpdateJob?.cancel()
        autoUpdateJob = null
        wakeSignal.close()
        val created = nativeRef
        nativeRef = null
        if (created != null) {
            runCatching { created.shutdown() }
            runCatching { created.close() }
        }
        job.cancel()
    }

    private companion object {
        /** 请求等"正在起"的核心最多等多久（毫秒）：和 Rust `CoreOptions.health_timeout_ms` 的默认值一样。 */
        const val START_WAIT_MS = 15_000L
        val DEFAULT_EXPECTED_SHA256 = String(charArrayOf('_', 'D', 'E', 'F', 'A', 'U', 'L', 'T', '_'))
    }
}

/** 状态 / 日志推送的落点（Rust 在 `poll` / 起停时回调）；这里只是把值放进 StateFlow。 */
private class NotifyTarget(private val core: ProxyCore) :
    uniffi.feedhub_ffi.FfiProxyCoreNotify {
    override fun statusChanged(status: uniffi.feedhub_ffi.FfiProxyCoreStatus) {
        core.onStatus(status)
    }

    override fun logsChanged(lines: List<String>) {
        core.onLogs(lines)
    }

    override fun wake() {
        core.onWake()
    }
}

