package feedhub.proxy

import feedhub.core.proxy.ImportReport
import feedhub.core.proxy.NodeSourceFormat
import feedhub.core.proxy.ParsedNodes
import feedhub.core.proxy.ProxyCoreState
import feedhub.core.proxy.ProxyNode
import feedhub.core.proxy.SkippedEntry
import feedhub.core.proxy.Subscription
import feedhub.core.proxy.SubscriptionRoute
import feedhub.core.proxy.SubscriptionUserInfo
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiProxyCore
import uniffi.feedhub_ffi.FfiProxyCoreException
import uniffi.feedhub_ffi.FfiProxyCoreFetchPlan
import uniffi.feedhub_ffi.FfiProxyCoreRoute
import uniffi.feedhub_ffi.FfiProxyCoreMode
import uniffi.feedhub_ffi.FfiProxyCoreNode
import uniffi.feedhub_ffi.FfiProxyCoreNotify
import uniffi.feedhub_ffi.FfiProxyCoreSkip
import uniffi.feedhub_ffi.FfiProxyCoreSourceFormat
import uniffi.feedhub_ffi.FfiProxyCoreState
import uniffi.feedhub_ffi.FfiProxyCoreStatus
import uniffi.feedhub_ffi.FfiProxyCoreSubscription
import uniffi.feedhub_ffi.FfiProxyCoreUpdate
import uniffi.feedhub_ffi.FfiProxyCoreUserInfo
import uniffi.feedhub_ffi.proxyBinaryLocate
import uniffi.feedhub_ffi.proxyBinaryExpectedSha256
import uniffi.feedhub_ffi.proxyBinarySha256
import uniffi.feedhub_ffi.proxyBinaryVerify
import uniffi.feedhub_ffi.proxyBinaryVersion
import uniffi.feedhub_ffi.proxyBuildCoreConfig
import uniffi.feedhub_ffi.proxyBuildLatencyProbeConfig
import uniffi.feedhub_ffi.proxyCoreConfigIssues
import uniffi.feedhub_ffi.proxyCoreDefaults
import uniffi.feedhub_ffi.proxyNetworkReason
import uniffi.feedhub_ffi.proxyParseSubscriptionUserInfo
import java.nio.file.Path
import java.nio.file.Paths

/**
 * **内置代理核心**的 Kotlin 门面。
 *
 * 实现全在 `rust/crates/feedhub-core/src/proxy_core/`；这里只做三件宿主的事：
 *
 * 1. **状态 / 日志推给界面**：实现 [FfiProxyCoreNotify]，接成 [ProxyCoreBridge.status] / [logs]；
 * 2. **定时器**：每 [POLL_INTERVAL_MS] 调一次 [poll]（带回当前时间）；「只在前台跑」的宽限期也在这里
 *    推进（[onForeground] / [onEnabled]）。
 *
 * **取订阅的收发不在宿主了**：Rust 自己用 `feedhub-net` 发；宿主只在建对象时把
 * `System` 那一档解析成具体的一档（`ProxyTarget.of`）。
 *
 * **和 Kotlin 旧实现的关系**：`core/proxy/ProxyCore.kt` 那一套已经挪进 `core` 的 testFixtures
 * （`feedhub.core.proxy.legacy.LegacyProxyCore`），只给对拍当参照。生产路径只有这一份。
 *
 * **机密**：`proxy-core.json` 的内容会过边界（宿主本来就要写它，权限 600）。[ProxyCoreState.toString]
 * 之类**不会打印**出站内容（`ProxyNode` 手写了 `toString`）——但这只拦得住"顺手 log 一个对象"，
 * 别的地方（界面上的"显示完整订阅地址"按钮）是用户主动要看的。
 */
object ProxyCoreBridge {
    /** 宿主定时器该多久调一次 [poll]（毫秒）。和 Rust 的 `POLL_INTERVAL_MS` 一样，但**不依赖 .so**。 */
    const val POLL_INTERVAL_MS: Long = 250

    /** 退到后台之后等多久停核心（毫秒）。Rust 那边用 `proxyCoreDefaults().defaultStopDelayMs` 也行。 */
    const val DEFAULT_STOP_DELAY_MS: Long = 30_000

    /**
     * 建一个核心对象。[stateFile] 为 `null` 时只留在内存里（测试 / 截图用）。
     *
     * [notify] 是状态 / 日志的推送（界面接成 StateFlow）。
     *
     * **取订阅的收发不在宿主**：Rust 自己用 `feedhub-net` 发；`System` 那一档的
     * 代理由 [`FfiProxyCore.setSystemProxy`] 逐条喂（要按订阅地址的主机名问 JVM 的选择器）。
     */
    fun create(
        binary: Path,
        dataDir: Path,
        stateFile: Path?,
        expectedSha256: String?,
        notify: FfiProxyCoreNotify,
        nowMs: Long = System.currentTimeMillis(),
    ): FfiProxyCore = native {
        FfiProxyCore(
            binary = binary.toString(),
            dataDir = dataDir.toString(),
            stateFile = stateFile?.toString(),
            expectedSha256 = expectedSha256,
            nowMs = nowMs,
            notify = notify,
        )
    }

    /**
     * 显式指定二进制的环境变量名（`FEEDHUB_SINGBOX`）。**读环境变量的是宿主**（安卓上 `getenv`
     * 和桌面不是一回事），名字从 Rust 要，免得两边各写一份字符串。
     */
    fun envPath(): String = defaults().envPath

    /** 换一份自己编译的二进制时给哈希的环境变量名（`FEEDHUB_SINGBOX_SHA256`）。 */
    fun envSha256(): String = defaults().envSha256

    /**
     * 期望的哈希：默认按体系结构推；宿主给了 `FEEDHUB_SINGBOX_SHA256` 就用它。
     *
     * 环境变量在这里读（不是在 Rust）：安卓上 `System.getenv` 读的是 App 进程的环境，
     * 桌面读的是 shell 的环境——两边的"环境变量"本来就不是同一件事，就留在宿主。
     */
    fun expectedSha256For(file: Path): String? = expectedSha256(
        arch = System.getProperty("os.arch").orEmpty(),
        overrideSha256 = System.getenv(envSha256())?.takeIf { it.isNotBlank() },
    )

    /** 找 sing-box（宿主按顺序给候选；[extra] 放必须优先的，比如安卓的 `nativeLibraryDir`）。 */
    fun locate(candidates: List<Path>, extra: List<Path> = emptyList()): Path? =
        native { proxyBinaryLocate(candidates.map { it.toString() }, extra.map { it.toString() }) }
            ?.let { Paths.get(it) }

    /** 这份二进制**应该**是哪个哈希（[overrideSha256] = 环境变量给的；`null` = 不校验）。 */
    fun expectedSha256(arch: String = System.getProperty("os.arch").orEmpty(), overrideSha256: String? = null): String? =
        native { proxyBinaryExpectedSha256(arch, overrideSha256) }

    fun sha256(file: Path): String? = native { proxyBinarySha256(file.toString()) }

    /** 校验哈希。`null` = 通过，否则是拒绝运行的原因（**不含机密**）。 */
    fun verify(file: Path, expected: String?): String? = native { proxyBinaryVerify(file.toString(), expected) }

    /** 读 `sing-box version` 的第一行。 */
    fun version(file: Path, timeoutMs: Long = 5_000): String? =
        native { proxyBinaryVersion(file.toString(), timeoutMs.toULong()) }

    /** 造 sing-box 配置的文本（**有机密**）。没有节点时抛 [IllegalArgumentException]。 */
    fun buildCoreConfig(state: ProxyCoreState, port: Int): String = native {
        proxyBuildCoreConfig(state.toFfi(), port)
    }

    /** 造"只测这一个节点"的配置的文本（**有机密**）。 */
    fun buildLatencyProbeConfig(node: ProxyNode): String = native {
        proxyBuildLatencyProbeConfig(node.toFfi())
    }

    /** 起核心之前的自检；空表 = 没问题。 */
    fun configIssues(state: ProxyCoreState): List<String> = native {
        proxyCoreConfigIssues(state.toFfi())
    }

    /**
     * **只解析不合并**（导入预览）：返回解析出来的节点 + 跳过原因 + 认出来的格式。
     *
     * 生产路径里"导入"走 [FfiProxyCore.importText]（解析 + 合并一次过河）；这个入口给"导入前先看
     * 会跳过什么"和对拍用。
     */
    fun parseSource(text: String): ParsedNodes = native {
        val dir = java.nio.file.Files.createTempDirectory("feedhub-proxy-parse")
        try {
            val core = create(
                binary = java.nio.file.Paths.get("/nonexistent/sing-box"),
                dataDir = dir,
                stateFile = null,
                expectedSha256 = null,
                notify = NoProxyCoreNotify,
            )
            try {
                core.parseSource(text).toCore()
            } finally {
                core.close()
            }
        } finally {
            dir.toFile().deleteRecursively()
        }
    }

    /** 认订阅格式（`{` 开头 / 有 `proxies:` / 其余当链接列表）。 */
    fun detectSourceFormat(text: String): NodeSourceFormat = native {
        val dir = java.nio.file.Files.createTempDirectory("feedhub-proxy-detect")
        try {
            val core = create(
                binary = java.nio.file.Paths.get("/nonexistent/sing-box"),
                dataDir = dir,
                stateFile = null,
                expectedSha256 = null,
                notify = NoProxyCoreNotify,
            )
            try {
                core.detectSourceFormat(text).toCore()
            } finally {
                core.close()
            }
        } finally {
            dir.toFile().deleteRecursively()
        }
    }

    /** 解析一段订阅正文（带订阅归属，给"更新订阅"的口径用）。 */
    fun parseSourceForSubscription(text: String, subscriptionId: String): ParsedNodes {
        val parsed = parseSource(text)
        return parsed.copy(nodes = parsed.nodes.map { it.copy(subscriptionId = subscriptionId) })
    }

    /** 解析 `subscription-userinfo` 响应头。 */
    fun parseSubscriptionUserInfo(header: String?): SubscriptionUserInfo? =
        native { proxyParseSubscriptionUserInfo(header)?.toCore() }

    /** 网络异常 → 界面文案（**不含主机名**）。 */
    fun networkReason(kind: String): String = native { proxyNetworkReason(kind) }

    /** 里部的默认值（Rust 是唯一真相；Kotlin 的 `DEFAULT_TEST_URL` 之类只是编译期便利）。 */
    fun defaults(): ProxyCoreDefaults = native {
        val d = proxyCoreDefaults()
        ProxyCoreDefaults(
            testUrl = d.testUrl,
            logLevel = d.logLevel,
            subscriptionUa = d.subscriptionUa,
            logLevels = d.logLevels,
            autoUpdateHours = d.autoUpdateHours,
            noNodesReason = d.noNodesReason,
            maxLogLines = d.maxLogLines,
            singBoxVersion = d.singBoxVersion,
            sha256LinuxAmd64 = d.sha256LinuxAmd64,
            sha256AndroidArm64 = d.sha256AndroidArm64,
            envPath = d.envPath,
            envSha256 = d.envSha256,
            defaultStopDelayMs = d.defaultStopDelayMs,
        )
    }

    internal inline fun <T> native(block: () -> T): T {
        RustNative.ensureLoaded()
        try {
            return block()
        } catch (e: FfiProxyCoreException) {
            throw when (e) {
                is FfiProxyCoreException.Invalid -> IllegalArgumentException(e.detail)
                is FfiProxyCoreException.Io -> IllegalStateException(e.detail)
                is FfiProxyCoreException.Internal -> IllegalStateException(e.detail)
            }
        } catch (e: UnsatisfiedLinkError) {
            throw IllegalStateException("内置代理核心：Rust 核心加载失败（找不到 libfeedhub_ffi.so）：${e.message}", e)
        }
    }
}

// ---------- core 的类型 ↔ FFI 的类型（映射只有这一份） ----------

fun ProxyCoreState.toFfi(): FfiProxyCoreState = FfiProxyCoreState(
    subscriptions = subscriptions.map { it.toFfi() },
    nodes = nodes.map { it.toFfi() },
    selectedNodeId = selectedNodeId,
    testUrl = testUrl,
    logLevel = logLevel,
    subscriptionRoute = subscriptionRoute.toFfi(),
    subscriptionUa = subscriptionUa,
)

fun ProxyNode.toFfi(): FfiProxyCoreNode = FfiProxyCoreNode(
    id = id,
    name = name,
    kind = kind,
    server = server,
    port = port,
    outboundJson = outboundCompactJson(),
    subscriptionId = subscriptionId,
    lastLatencyMs = lastLatencyMs,
    lastLatencyAt = lastLatencyAt,
)

private fun ProxyNode.outboundCompactJson(): String = outbound.toString()

fun Subscription.toFfi(): FfiProxyCoreSubscription = FfiProxyCoreSubscription(
    id = id,
    name = name,
    url = url,
    addedAt = addedAt,
    lastUpdateAt = lastUpdateAt,
    lastError = lastError,
    nodeCount = nodeCount,
    userInfo = userInfo?.toFfi(),
    autoUpdateHours = autoUpdateHours,
)

fun SubscriptionUserInfo.toFfi(): FfiProxyCoreUserInfo = FfiProxyCoreUserInfo(
    upload = upload,
    download = download,
    total = total,
    expire = expire,
)

fun SubscriptionRoute.toFfi(): FfiProxyCoreRoute = when (this) {
    SubscriptionRoute.DIRECT -> FfiProxyCoreRoute.DIRECT
    SubscriptionRoute.CORE -> FfiProxyCoreRoute.CORE
    SubscriptionRoute.SYSTEM -> FfiProxyCoreRoute.SYSTEM
}

fun FfiProxyCoreRoute.toCore(): SubscriptionRoute = when (this) {
    FfiProxyCoreRoute.DIRECT -> SubscriptionRoute.DIRECT
    FfiProxyCoreRoute.CORE -> SubscriptionRoute.CORE
    FfiProxyCoreRoute.SYSTEM -> SubscriptionRoute.SYSTEM
}

fun FfiProxyCoreUserInfo.toCore(): SubscriptionUserInfo = SubscriptionUserInfo(
    upload = upload,
    download = download,
    total = total,
    expire = expire,
)

/** Rust 给的节点。`outboundJson` 是**紧凑 JSON**，解析回 `JsonObject`（保序）。 */
fun FfiProxyCoreNode.toCore(): ProxyNode = ProxyNode(
    id = id,
    name = name,
    kind = kind,
    server = server,
    port = port,
    outbound = feedhub.core.FeedJson.parseToJsonElement(outboundJson) as kotlinx.serialization.json.JsonObject,
    subscriptionId = subscriptionId,
    lastLatencyMs = lastLatencyMs,
    lastLatencyAt = lastLatencyAt,
)

fun FfiProxyCoreState.toCore(): ProxyCoreState = ProxyCoreState(
    subscriptions = subscriptions.map { it.toCore() },
    nodes = nodes.map { it.toCore() },
    selectedNodeId = selectedNodeId,
    testUrl = testUrl,
    logLevel = logLevel,
    subscriptionRoute = subscriptionRoute.toCore(),
    subscriptionUa = subscriptionUa,
)

fun FfiProxyCoreSkip.toCore(): SkippedEntry = SkippedEntry(index, reason)

fun FfiProxyCoreSourceFormat.toCore(): NodeSourceFormat = when (this) {
    FfiProxyCoreSourceFormat.SING_BOX_JSON -> NodeSourceFormat.SINGBOX_JSON
    FfiProxyCoreSourceFormat.CLASH_YAML -> NodeSourceFormat.CLASH_YAML
    FfiProxyCoreSourceFormat.SHARE_LINKS -> NodeSourceFormat.SHARE_LINKS
}

fun FfiProxyCoreMode.toCore(): Pair<Int?, String> = port to reason

/**
 * 里部默认值（Rust 的 `proxy_core_defaults()`）。
 *
 * **为什么不直接用 Kotlin 的常量**：那些值（测速地址、UA、可选间隔……）现在是 Rust 的真相，
 * Kotlin 这边的 `DEFAULT_TEST_URL` 之类只剩"编译期便利 + 给界面兜底"。两边由
 * `ProxyCoreDefaultsParityTest` 对拍。
 */
data class ProxyCoreDefaults(
    val testUrl: String,
    val logLevel: String,
    val subscriptionUa: String,
    val logLevels: List<String>,
    val autoUpdateHours: List<Int>,
    val noNodesReason: String,
    val maxLogLines: Int,
    val singBoxVersion: String,
    val sha256LinuxAmd64: String,
    val sha256AndroidArm64: String,
    val envPath: String,
    val envSha256: String,
    val defaultStopDelayMs: Long,
)

/**
 * 里部一个小工具：把"要不要发 / 发什么"读成人话（**不含 URL**，界面上打日志用）。
 *
 * 为什么在这儿：宿主拿到 [FfiProxyCoreFetchPlan] 之后要决定用哪个客户端，那一层也得说人话。
 */
fun FfiProxyCoreFetchPlan.describe(): String = when (this) {
    is FfiProxyCoreFetchPlan.Fetch -> "发请求（${route.describe()}，UA=$userAgent）"
    is FfiProxyCoreFetchPlan.Refused -> "不发：$reason"
    FfiProxyCoreFetchPlan.NotFound -> "不发：订阅不在列表里了"
}

private fun FfiProxyCoreRoute.describe(): String = when (this) {
    FfiProxyCoreRoute.DIRECT -> "直连"
    FfiProxyCoreRoute.CORE -> "经核心"
    FfiProxyCoreRoute.SYSTEM -> "系统代理"
}

/** 取订阅的结果读成人话（**不含 URL**）。 */
fun FfiProxyCoreUpdate.describe(): String = when (this) {
    is FfiProxyCoreUpdate.Ok -> "成功，解析到 $parsed 条"
    is FfiProxyCoreUpdate.Failed -> "失败：$reason"
}

/** 内部：一个不推任何东西的 [FfiProxyCoreNotify]（测试 / 无界面场景用）。 */
internal object NoProxyCoreNotify : FfiProxyCoreNotify {
    override fun statusChanged(status: FfiProxyCoreStatus) {}
    override fun logsChanged(lines: List<String>) {}
    override fun wake() {}
}

/** 内部：状态读成人话（**不含机密**）。 */
internal fun FfiProxyCoreStatus.describe(): String = when (this) {
    FfiProxyCoreStatus.Stopped -> "已停止"
    is FfiProxyCoreStatus.Starting -> "正在启动（第 ${attempt + 1} 次）"
    is FfiProxyCoreStatus.Running -> "运行中 · 127.0.0.1:$port"
    is FfiProxyCoreStatus.Restarting -> "挂了，${delayMs}ms 后第 $attempt 次自动重启"
    is FfiProxyCoreStatus.Failed -> "起不来"
}

/** `ParsedNodes` 从 FFI 还原（只有测试 / 预览走这条路；生产用 [FfiProxyCore.importText]）。 */
fun uniffi.feedhub_ffi.FfiProxyCoreParsed.toCore(): ParsedNodes = ParsedNodes(
    nodes = nodes.map { it.toCore() },
    skipped = skipped.map { it.toCore() },
    format = format.toCore(),
)

/** 导入结果的还原（同上）。 */
fun uniffi.feedhub_ffi.FfiProxyCoreImportReport.toCore(): ImportReport = ImportReport(
    added = added,
    updated = updated,
    skipped = skipped.map { it.toCore() },
)

/** Rust 的 `FfiProxyCoreStatus` → core 的 `CoreStatus`（界面用）。 */
fun FfiProxyCoreStatus.toCore(): feedhub.core.proxy.CoreStatus = when (this) {
    FfiProxyCoreStatus.Stopped -> feedhub.core.proxy.CoreStatus.Stopped
    is FfiProxyCoreStatus.Starting -> feedhub.core.proxy.CoreStatus.Starting(attempt)
    is FfiProxyCoreStatus.Running ->
        feedhub.core.proxy.CoreStatus.Running(port, version, pid, startedAt)
    is FfiProxyCoreStatus.Restarting ->
        feedhub.core.proxy.CoreStatus.Restarting(attempt, delayMs, reason)
    is FfiProxyCoreStatus.Failed -> feedhub.core.proxy.CoreStatus.Failed(message)
}

/** Rust 的 `FfiProxyCoreSubscription` → core 的 `Subscription`。 */
fun FfiProxyCoreSubscription.toCore(): Subscription = Subscription(
    id = id,
    name = name,
    url = url,
    addedAt = addedAt,
    lastUpdateAt = lastUpdateAt,
    lastError = lastError,
    nodeCount = nodeCount,
    userInfo = userInfo?.toCore(),
    autoUpdateHours = autoUpdateHours,
)
