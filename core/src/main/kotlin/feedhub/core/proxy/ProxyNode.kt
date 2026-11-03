package feedhub.core.proxy

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject

/**
 * 内置代理核心（sing-box 子进程）用到的**数据模型**。
 *
 * **逻辑全在 Rust**（`rust/crates/feedhub-core/src/proxy_core/`）：
 * 订阅 / 分享链接 / Clash YAML 的解析、sing-box 配置生成、`proxy-core.json` 的读写、sha256 校验、
 * 子进程的起停与退避重启、延迟测试、「只在前台跑」的判定表。这个文件只剩**界面要用的形状**
 * （`data class` / 枚举 / 那一两句给人看的文案），因为 `:core` 在依赖顺序上在 `:rust-bridge` 下面，
 * 调不到 Rust；真正干活的类在 `feedhub.proxy.ProxyCore`（`:rust-bridge`）。
 *
 * 这里的 [ProxyNode] / [Subscription] 里带 **uuid、密码、订阅地址**，全是机密：
 * 只存在权限 600 的 JSON 文件里，
 * [ProxyNode.toString] 和 [Subscription.toString] 都手写成了打码版（`outbound = <机密>`），
 * 顺手 `log("$node")` 不会漏——但也别指望它拦得住一切。
 */

/** sing-box 的 `log.level`。默认 `warn`，进阶设置页里可调。 */
val CORE_LOG_LEVELS: List<String> = listOf("trace", "debug", "info", "warn", "error", "fatal", "panic")

/** 一个节点。[outbound] 是 sing-box 的出站对象（不含 `tag`，tag 由我们按 [id] 填）。 */
@Serializable
data class ProxyNode(
    /** 稳定 id：按 [outbound] 的内容算哈希，同一份链接重复导入得到同一个 id（不重复添加）。 */
    val id: String,
    val name: String,
    /** sing-box 出站类型，例如 `vless` / `shadowsocks`。 */
    val kind: String,
    val server: String,
    val port: Int,
    /** 机密：含 uuid / password。 */
    val outbound: JsonObject,
    /** 来自哪个订阅（null = 手动导入）。 */
    val subscriptionId: String? = null,
    /** 上次测速的延迟（毫秒，null = 没测过 / 测不通）。持久化，重开 App 还看得见。 */
    val lastLatencyMs: Int? = null,
    val lastLatencyAt: Long? = null,
) {
    /** 界面上的一行：`vless · hk1.example.com:443`。 */
    val label: String get() = "$kind · $server:$port"

    /**
     * **打码的 `toString`**（出站里有 uuid / 密码）。
     *
     * 用不上了 `data class` 自动生成的那个：那个会把 `outbound` 整个打出来。
     */
    override fun toString(): String =
        "ProxyNode(id=$id, name=$name, kind=$kind, server=$server, port=$port, " +
            "outbound=<机密>, subscriptionId=$subscriptionId, " +
            "lastLatencyMs=$lastLatencyMs, lastLatencyAt=$lastLatencyAt)"
}

/** 一次导入里被跳过的条目：只说第几条、为什么，**不回显原文**（原文里可能有密码）。 */
@Serializable
data class SkippedEntry(val index: Int, val reason: String)

/** 导入结果。 */
data class ImportReport(
    val added: Int,
    val updated: Int,
    val skipped: List<SkippedEntry>,
) {
    val summary: String
        get() = buildString {
            append("新增 $added 条")
            if (updated > 0) append("，更新 $updated 条")
            if (skipped.isNotEmpty()) append("，跳过 ${skipped.size} 条")
        }
}

/** 订阅的流量 / 到期信息（HTTP 响应头 `subscription-userinfo`，机场一般会给）。 */
@Serializable
data class SubscriptionUserInfo(
    val upload: Long? = null,
    val download: Long? = null,
    val total: Long? = null,
    /** Unix 秒。 */
    val expire: Long? = null,
) {
    val used: Long? get() = if (upload == null && download == null) null else (upload ?: 0) + (download ?: 0)
}

/** 一个订阅。**[url] 是机密**：界面上默认打码， */
@Serializable
data class Subscription(
    val id: String,
    val name: String,
    val url: String,
    val addedAt: Long,
    val lastUpdateAt: Long? = null,
    /** 上次更新失败的原因（成功时为 null）。不含机密。 */
    val lastError: String? = null,
    val nodeCount: Int = 0,
    val userInfo: SubscriptionUserInfo? = null,
    /**
     * 这条订阅自己的自动更新间隔（小时，0 = 关）。**每条一份**（和 SFA 的 profile 一致：
     * 参考源码 `compose/screen/profile/EditProfileViewModel.kt` 的 `autoUpdate` / `autoUpdateInterval`）。
     */
    val autoUpdateHours: Int = 0,
) {
    /** **打码的 `toString`**（订阅地址就是凭据）。 */
    override fun toString(): String =
        "Subscription(id=$id, name=$name, url=<机密>, addedAt=$addedAt, lastUpdateAt=$lastUpdateAt, " +
            "lastError=$lastError, nodeCount=$nodeCount, userInfo=$userInfo, " +
            "autoUpdateHours=$autoUpdateHours)"
}

/** 取订阅走哪条路（默认直连）。 */
@Serializable
enum class SubscriptionRoute { DIRECT, CORE, SYSTEM }

/** 延迟测试地址的默认值（用户可改）。Rust 是真相，这里只给界面兜底。 */
const val DEFAULT_TEST_URL: String = "https://www.gstatic.com/generate_204"

/** 取订阅默认的 UA：机场一般按 UA 决定返回什么格式。 */
const val DEFAULT_SUBSCRIPTION_UA: String = "clash.meta"

/** 自动更新间隔可选值（小时，0 = 关）。 */
val AUTO_UPDATE_HOURS: List<Int> = listOf(0, 6, 12, 24)

/**
 * 内置代理核心的全部状态。**整个是机密**（订阅地址 + 节点密码 / uuid）。
 *
 * 界面上的那一档 `不用 / 系统代理 / 指定 / 内置核心`（`feedhub.core.ProxyConfig.Mode`）不在
 * 这里，它在普通设置里；这里放的是「内置核心」自己的东西。
 *
 * **状态的真相在 Rust**（`proxy_core.json` 由 Rust 读写）；这个是过河之后的快照，界面上直接用。
 */
@Serializable
data class ProxyCoreState(
    val subscriptions: List<Subscription> = emptyList(),
    val nodes: List<ProxyNode> = emptyList(),
    /** 选中的节点 id；null = 自动（延迟最低，用 sing-box 的 `urltest` 出站）。 */
    val selectedNodeId: String? = null,
    val testUrl: String = DEFAULT_TEST_URL,
    val logLevel: String = "warn",
    val subscriptionRoute: SubscriptionRoute = SubscriptionRoute.DIRECT,
    val subscriptionUa: String = DEFAULT_SUBSCRIPTION_UA,
) {
    fun nodesOf(subscriptionId: String): List<ProxyNode> = nodes.filter { it.subscriptionId == subscriptionId }

    /** 选中的节点（没选中或者节点已经不在了 → null）。 */
    val selectedNode: ProxyNode? get() = selectedNodeId?.let { id -> nodes.firstOrNull { it.id == id } }
}

/** 内置核心的运行状态（Rust `CoreStatus` 过河之后的形状）。 */
sealed interface CoreStatus {
    /** 没启用（设置里没选「内置核心」）。 */
    data object Stopped : CoreStatus

    /** 正在起（[attempt] 是第几次，0 = 第一次）。 */
    data class Starting(val attempt: Int) : CoreStatus

    data class Running(
        val port: Int,
        /** `sing-box version` 读出来的版本号（读不到就是 null）。 */
        val version: String?,
        val pid: Long,
        val startedAt: Long,
    ) : CoreStatus

    /** 挂了，正在退避重试。 */
    data class Restarting(val attempt: Int, val delayMs: Long, val reason: String) : CoreStatus

    /** 起不来 / 连续重启到上限。 */
    data class Failed(val message: String) : CoreStatus
}

/** 没在运行时给用户看的原因（会出现在 `ProxyConfigGuardInterceptor` 抛出的异常文案里）。 */
fun CoreStatus.unavailableReason(): String = when (this) {
    is CoreStatus.Failed -> message
    is CoreStatus.Starting -> "正在启动（第 ${attempt + 1} 次）"
    is CoreStatus.Restarting -> "正在重启：$reason"
    is CoreStatus.Running -> ""
    CoreStatus.Stopped -> "没有运行（在「设置 → 网络 → 代理」里选「内置核心」后会自动启动）"
}

/** 订阅内容的三种格式（界面 / 导入预览用；解析在 Rust）。 */
enum class NodeSourceFormat {
    /** sing-box JSON（有 `outbounds`）。 */
    SINGBOX_JSON,

    /** Clash / mihomo YAML（有 `proxies:`）。 */
    CLASH_YAML,

    /** 分享链接列表（明文，或者整体是 base64）。 */
    SHARE_LINKS,
}

/** 一次解析的结果：节点 + 跳过原因 + 认出来的格式。 */
data class ParsedNodes(
    val nodes: List<ProxyNode>,
    val skipped: List<SkippedEntry>,
    val format: NodeSourceFormat,
)

/** 一个节点都没有时给用户看的原因（Rust 是真相，这里给界面兜底）。 */
const val NO_NODES_REASON: String = "没有可用节点（先去「进阶设置 → 订阅 / 导入节点」加一个）"

/** 一次延迟测试的结果。[error] 里不含密码 / uuid。 */
data class LatencyResult(val nodeId: String, val ms: Int?, val error: String?)
