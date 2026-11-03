package feedhub.app.settings

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Movie
import androidx.compose.material.icons.outlined.QueryStats
import androidx.compose.material.icons.outlined.Router
import androidx.compose.material.icons.outlined.Speed
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material.icons.outlined.Translate
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import feedhub.app.AppGraph
import feedhub.app.CodecPreference
import feedhub.app.feed.FeedOrder
import feedhub.app.video.DecodeLedger
import feedhub.core.proxy.AUTO_UPDATE_HOURS
import feedhub.core.proxy.CORE_LOG_LEVELS
import feedhub.core.proxy.CoreStatus
import feedhub.core.proxy.ImportReport
import feedhub.proxy.ProxyCore
import feedhub.core.proxy.ProxyNode
import feedhub.core.proxy.Subscription
import feedhub.core.proxy.SubscriptionRoute
import feedhub.core.proxy.unavailableReason
import kotlinx.coroutines.launch
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * 「进阶设置」下的子页。
 *
 * 顺序就是进阶列表里的顺序。[key] 是截图工具 / debug 启动参数里的短名
 * （桌面 `adv=proxy`、安卓 `screen=advanced:proxy`）。
 *
 * **加一个新的进阶子页**：在这里加一项，再在 [AdvancedSectionContent] 里加一个分支。列表（`SettingsScreen` 的
 * [AdvancedMenu]）和窄 / 宽两套摆放都是枚举驱动的，别另写布局。
 */
internal enum class AdvancedSection(val key: String, val title: String, val summary: String, val icon: ImageVector) {
    /** 内置代理核心（状态 / 日志）、订阅、节点、指定代理的明细、出口锁。 */
    PROXY("proxy", "代理（进阶）", "内置代理核心、订阅、节点、指定代理明细、出口锁", Icons.Outlined.Router),

    /** 从「信息流」挪出来的两项。 */
    FEED("feed", "信息流（进阶）", "首页排序、每平台一批最多", Icons.Outlined.Tune),

    VIDEO("video", "视频（进阶）", "编码偏好、已记下的解码上限", Icons.Outlined.Movie),

    /** 翻译引擎的两个文件路径（开关和平台标签留在「翻译（本地）」）。 */
    TRANSLATION("translation", "翻译（进阶）", "翻译模型（GGUF）与 llama-server 的路径", Icons.Outlined.Translate),

    BEHAVIOR("behavior", "行为数据", "看看记了多少、本地向量（实验）、导出", Icons.Outlined.QueryStats),

    PERF("perf", "性能记录", "性能日志开关、记录文件", Icons.Outlined.Speed),

    /**
     * 桌面 App 的对外接口：本机 unix socket 上的只读 HTTP，给 agent 用。
     * **桌面才有这一页**（安卓组装没有 `AppGraph.appApi`，进阶列表里就不列它）。
     */
    API("api", "对外接口（agent）", "本机 unix socket 上的只读 HTTP、skill 路径、调用次数", Icons.Outlined.Terminal),

    ;

    companion object {
        /** 短名 → 子页；认不出（没给）就是 null。 */
        fun of(key: String?): AdvancedSection? {
            val k = key?.trim()?.takeIf { it.isNotEmpty() } ?: return null
            return entries.firstOrNull { it.key.equals(k, ignoreCase = true) }
        }

        /** 报错 / 截图工具提示里用的一行短名清单。 */
        fun keysText(): String = entries.joinToString(" / ") { it.key }
    }
}

/** 可以先进设置页的弹框（截图工具用 [initialDialog] 指定，平时都是点了才开；两个都在「代理（进阶）」页）。 */
enum class AdvancedDialog(val key: String) {
    IMPORT("import"),
    SUBSCRIBE("subscribe");

    companion object {
        fun of(key: String?): AdvancedDialog? = entries.firstOrNull { it.key == key }
    }
}

/**
 * 一个进阶子页的内容：卡片和拆之前**一模一样**，只是按子页分开了——
 * **每个设置项的存储键、默认值、行为都没变，只是挪了位置**。
 *
 * 卡片之间留空靠外面那层 `Column(verticalArrangement = spacedBy(12.dp))`（`SettingsScreen` 的 `SettingsPage`）。
 */
@Composable
internal fun AdvancedSectionContent(graph: AppGraph, section: AdvancedSection, initialDialog: String? = null) {
    // 弹框状态按子页各记一份（只有「代理（进阶）」有两个弹框；别的子页不会用到）
    var dialog by remember { mutableStateOf(AdvancedDialog.of(initialDialog)) }
    val core = graph.proxyCore
    when (section) {
        AdvancedSection.PROXY -> {
            CoreStatusCard(graph)
            SubscriptionCard(graph, onAdd = { dialog = AdvancedDialog.SUBSCRIBE })
            NodeCard(graph, onImport = { dialog = AdvancedDialog.IMPORT })
            CustomProxyCard(graph)
            ExitLockCard(graph)
        }

        AdvancedSection.FEED -> FeedAdvancedCard(graph)
        AdvancedSection.VIDEO -> VideoAdvancedCard(graph)
        AdvancedSection.TRANSLATION -> TranslationAdvancedSettings(graph)
        AdvancedSection.BEHAVIOR -> BehaviorSettings(graph)
        AdvancedSection.PERF -> PerfLogSettings(graph)
        AdvancedSection.API -> AppApiSettings(graph)
    }
    when (dialog) {
        AdvancedDialog.IMPORT -> if (core != null) ImportNodesDialog(core) { dialog = null }
        AdvancedDialog.SUBSCRIBE -> if (core != null) AddSubscriptionDialog(core) { dialog = null }
        null -> Unit
    }
}

// ---------------------------------------------------------------------------
// 核心状态
// ---------------------------------------------------------------------------

@Composable
private fun CoreStatusCard(graph: AppGraph) {
    val core = graph.proxyCore
    SettingsCard("内置代理核心") {
        if (core == null) {
            Hint("这个组装里没有内置代理核心（它是桌面的 sing-box 子进程；安卓这一轮没接）。设置 → 网络 里选「内置核心」时，跟随全局的平台会直接失败，不会悄悄直连。")
            return@SettingsCard
        }
        val status by core.status.collectAsState()
        val state by core.state.collectAsState()
        val logs by core.logs.collectAsState()
        var showAllLogs by remember { mutableStateOf(false) }

        SettingBlock("运行状态") {
            Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(Modifier.size(8.dp).background(statusColor(status), RoundedCornerShape(4.dp)))
                    Spacer(Modifier.width(8.dp))
                    Text(statusTitle(status), style = MaterialTheme.typography.bodyMedium)
                }
                Hint(statusDetail(status))
                if (status is CoreStatus.Failed || status is CoreStatus.Restarting) {
                    Hint(status.unavailableReason())
                }
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(onClick = { core.restart() }) { Text("重启核心") }
            OutlinedButton(onClick = { core.clearLogs() }) { Text("清空日志") }
        }
        Hint("「设置 → 网络 → 代理」选「内置核心」时核心会自动启动；选了别的就停掉。")
        SettingBlock("数据目录", "生成的 sing-box 配置（含节点机密，权限 600）就放在这里") {
            Text(maskPath(core.dataDir.toString()), style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
        }
        SettingBlock("当前节点") {
            Text(state.selectedNode?.name ?: "自动（延迟最低）", style = MaterialTheme.typography.bodyMedium)
        }
        SettingBlock("日志级别", "改完会重启核心（很快就回来）") {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                for (level in CORE_LOG_LEVELS) {
                    FilterChip(
                        selected = state.logLevel == level,
                        onClick = { core.updatePrefs { it.copy(logLevel = level) } },
                        label = { Text(level) },
                    )
                }
            }
        }
        // 日志：默认只显示最后十几行（卡片别太长），「看全部」展开到最近 200 行
        val logScroll = rememberScrollState()
        val shown = if (showAllLogs) logs else logs.takeLast(LOG_PREVIEW_LINES)
        LaunchedEffect(logs.size, showAllLogs) { logScroll.animateScrollTo(logScroll.maxValue) }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Hint("最近 ${logs.size} 行（最多 200 行）", Modifier.weight(1f))
            if (logs.size > LOG_PREVIEW_LINES) {
                TextButton(onClick = { showAllLogs = !showAllLogs }) { Text(if (showAllLogs) "收起" else "看全部") }
            }
        }
        Box(
            Modifier.fillMaxWidth().heightIn(max = if (showAllLogs) 260.dp else 110.dp)
                .background(MaterialTheme.colorScheme.surfaceContainer, RoundedCornerShape(8.dp))
                .padding(8.dp)
                .verticalScroll(logScroll),
        ) {
            Text(
                if (shown.isEmpty()) "（还没有日志）" else shown.joinToString("\n"),
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
                fontSize = 11.sp,
            )
        }
    }
}

private fun statusColor(status: CoreStatus): Color = when (status) {
    is CoreStatus.Running -> Color(0xFF2E7D32)
    is CoreStatus.Failed -> Color(0xFFC62828)
    is CoreStatus.Restarting, is CoreStatus.Starting -> Color(0xFFEF6C00)
    CoreStatus.Stopped -> Color(0xFF757575)
}

private fun statusTitle(status: CoreStatus): String = when (status) {
    is CoreStatus.Running -> "运行中 · 127.0.0.1:${status.port}"
    is CoreStatus.Starting -> "正在启动（第 ${status.attempt + 1} 次）"
    is CoreStatus.Restarting -> "挂了，${status.delayMs}ms 后第 ${status.attempt} 次自动重启"
    is CoreStatus.Failed -> "起不来"
    CoreStatus.Stopped -> "已停止"
}

private fun statusDetail(status: CoreStatus): String = when (status) {
    is CoreStatus.Running ->
        "sing-box ${status.version ?: "（版本号没读出来）"} · pid ${status.pid} · 混入站同时收 HTTP 和 SOCKS5，只监听本机、不当系统代理"

    is CoreStatus.Restarting -> status.reason
    is CoreStatus.Failed -> status.message
    is CoreStatus.Starting -> "sing-box 子进程"
    CoreStatus.Stopped -> "在「设置 → 网络 → 代理」里选「内置核心」后会自动启动"
}

/** 路径太长时中间省略（日志 / 截图里不要整条又长又没用的路径）。 */
internal fun maskPath(path: String, keepStart: Int = 28, keepEnd: Int = 12): String =
    if (path.length <= keepStart + keepEnd + 1) path else path.take(keepStart) + "…" + path.takeLast(keepEnd)

// ---------------------------------------------------------------------------
// 订阅
// ---------------------------------------------------------------------------

@Composable
private fun SubscriptionCard(graph: AppGraph, onAdd: () -> Unit) {
    val core = graph.proxyCore
    SettingsCard("订阅") {
        if (core == null) {
            Hint("没有内置代理核心，订阅也用不了。")
            return@SettingsCard
        }
        val state by core.state.collectAsState()
        SettingBlock("取订阅走哪条路", "默认直连，不经过任何代理") {
            SegmentedChoice(
                listOf(
                    SubscriptionRoute.DIRECT to "直连",
                    SubscriptionRoute.CORE to "经核心",
                    SubscriptionRoute.SYSTEM to "系统代理",
                ),
                state.subscriptionRoute,
            ) { route -> core.updatePrefs { it.copy(subscriptionRoute = route) } }
        }
        SettingInline("取订阅的 UA", "机场常按 UA 决定返回 Clash 还是别的格式") {
            UaField(state.subscriptionUa) { ua -> core.updatePrefs { it.copy(subscriptionUa = ua) } }
        }
        Hint("自动更新是每条订阅各自一份（关 / 6 / 12 / 24 小时），在下面每条订阅那一行上改。")
        if (state.subscriptions.isEmpty()) {
            Hint("还没有订阅。点下面的「添加订阅」，或者直接在「节点」里导入分享链接。")
        } else {
            for (sub in state.subscriptions) SubscriptionRow(core, sub)
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(onClick = onAdd) { Text("添加订阅") }
        }
    }
}

@Composable
private fun UaField(ua: String, onChange: (String) -> Unit) {
    OutlinedTextField(
        value = ua,
        onValueChange = { onChange(it.take(60)) },
        singleLine = true,
        textStyle = MaterialTheme.typography.bodySmall,
        modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp),
    )
}

@Composable
private fun SubscriptionRow(core: ProxyCore, sub: Subscription) {
    val scope = rememberCoroutineScope()
    var reveal by remember(sub.id) { mutableStateOf(false) }
    var busy by remember(sub.id) { mutableStateOf(false) }
    var message by remember(sub.id) { mutableStateOf<String?>(null) }
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(sub.name, style = MaterialTheme.typography.bodyMedium)
                Text(
                    if (reveal) sub.url else maskSubscriptionUrl(sub.url),
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.clickable { reveal = !reveal },
                )
            }
            Spacer(Modifier.width(8.dp))
            OutlinedButton(
                enabled = !busy,
                onClick = {
                    busy = true
                    message = null
                    scope.launch {
                        val r = core.updateSubscription(sub.id)
                        message = r.fold({ "更新好了：$it 个节点" }, { it.message })
                        busy = false
                    }
                },
            ) { Text(if (busy) "更新中…" else "更新") }
            Spacer(Modifier.width(6.dp))
            TextButton(onClick = { core.removeSubscription(sub.id) }) { Text("删除") }
        }
        Hint(subscriptionSummary(sub))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Hint("自动更新", Modifier.padding(end = 8.dp))
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (h in AUTO_UPDATE_HOURS) {
                    FilterChip(
                        selected = sub.autoUpdateHours == h,
                        onClick = { core.setSubscriptionAutoUpdate(sub.id, h) },
                        label = { Text(if (h == 0) "关" else "${h}h") },
                    )
                }
            }
        }
        sub.lastError?.let { Hint("上次更新失败：$it") }
        message?.let { Hint(it) }
    }
}

internal fun subscriptionSummary(sub: Subscription): String = buildList {
    add("上次更新 " + formatTime(sub.lastUpdateAt))
    add("${sub.nodeCount} 个节点")
    sub.userInfo?.let { info ->
        val used = info.used
        val total = info.total
        if (used != null && total != null) add("已用 ${formatBytes(used)} / ${formatBytes(total)}")
        info.expire?.let { add("到期 " + formatDate(it)) }
    }
}.joinToString(" · ")

/**
 * 订阅地址默认打码：只留协议 + 主机（token / 路径全部藏掉），点一下才显示全。
 */
internal fun maskSubscriptionUrl(url: String): String {
    val parsed = url.toHttpUrlOrNull() ?: return "（订阅地址已隐藏，点一下显示完整）"
    val port = if (parsed.port == defaultPortOf(parsed.scheme)) "" else ":${parsed.port}"
    val path = if (parsed.encodedPath.length > 1) "/…" else ""
    return "${parsed.scheme}://${parsed.host}$port$path（点一下显示完整）"
}

private fun defaultPortOf(scheme: String) = if (scheme == "https") 443 else 80

/** 毫秒时间戳 → `MM-dd HH:mm`；没有就是「还没更新过」。 */
internal fun formatTime(ms: Long?): String = ms?.let {
    SimpleDateFormat("MM-dd HH:mm", Locale.getDefault()).format(Date(it))
} ?: "还没更新过"

/** Unix 秒 → `yyyy-MM-dd`。 */
internal fun formatDate(unixSeconds: Long): String =
    SimpleDateFormat("yyyy-MM-dd", Locale.getDefault()).format(Date(unixSeconds * 1000))

// ---------------------------------------------------------------------------
// 节点
// ---------------------------------------------------------------------------

@Composable
private fun NodeCard(graph: AppGraph, onImport: () -> Unit) {
    val core = graph.proxyCore
    SettingsCard("节点") {
        if (core == null) {
            Hint("没有内置代理核心，节点管理也用不了。")
            return@SettingsCard
        }
        val state by core.state.collectAsState()
        val scope = rememberCoroutineScope()
        var testing by remember { mutableStateOf(false) }
        var testAllResult by remember { mutableStateOf<String?>(null) }

        SettingBlock("选中哪个", "「自动」用 sing-box 的 urltest 出站，每 3 分钟自己挑一次延迟最低的") {
            Row(
                Modifier.fillMaxWidth().clickable { core.selectNode(null) },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RadioButton(selected = state.selectedNodeId == null, onClick = { core.selectNode(null) })
                Text("自动（延迟最低）", style = MaterialTheme.typography.bodyMedium)
            }
        }
        if (state.nodes.isEmpty()) {
            Hint("还没有节点：可以从订阅更新下来，也可以直接导入分享链接。")
        } else {
            Hint("共 ${state.nodes.size} 个节点" + if (state.nodes.size > MAX_NODE_ROWS) "（下面只列前 $MAX_NODE_ROWS 个）" else "")
            Box(Modifier.fillMaxWidth().heightIn(max = 320.dp).verticalScroll(rememberScrollState())) {
                Column {
                    for (node in state.nodes.take(MAX_NODE_ROWS)) {
                        NodeRow(core, node, selected = state.selectedNodeId == node.id, testing = testing)
                    }
                }
            }
        }
        SettingBlock("测速", "测速地址默认 https://www.gstatic.com/generate_204，可以改成自己的；测速是另起一个一次性的 sing-box 去取这个地址") {
            TestUrlField(state.testUrl) { url -> core.updatePrefs { it.copy(testUrl = url) } }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(
                enabled = state.nodes.isNotEmpty() && !testing,
                onClick = {
                    testing = true
                    testAllResult = null
                    scope.launch {
                        val n = core.testAllLatencies()
                        testAllResult = "测完 $n 个"
                        testing = false
                    }
                },
            ) { Text(if (testing) "测速中…" else "全部测速") }
            OutlinedButton(onClick = onImport) { Text("导入节点") }
        }
        testAllResult?.let { Hint(it) }
    }
}

private const val MAX_NODE_ROWS = 40

/** 日志默认显示最后几行（点「看全部」看最近 200 行）。 */
private const val LOG_PREVIEW_LINES = 14

@Composable
private fun NodeRow(core: ProxyCore, node: ProxyNode, selected: Boolean, testing: Boolean) {
    val scope = rememberCoroutineScope()
    var busy by remember(node.id) { mutableStateOf(false) }
    Row(Modifier.fillMaxWidth().clickable { core.selectNode(node.id) }, verticalAlignment = Alignment.CenterVertically) {
        RadioButton(selected = selected, onClick = { core.selectNode(node.id) })
        Column(Modifier.weight(1f)) {
            Text(node.name, style = MaterialTheme.typography.bodyMedium)
            Hint("${node.kind} · ${node.server}:${node.port} · " + latencyLabel(node))
        }
        OutlinedButton(
            enabled = !busy && !testing,
            onClick = {
                busy = true
                scope.launch {
                    core.testLatency(node.id)
                    busy = false
                }
            },
        ) { Text(if (busy) "…" else "测速") }
        TextButton(onClick = { core.removeNode(node.id) }) { Text("删除") }
    }
}

/** 节点那一行的延迟文案：没测过 / 123 ms / 测不通。 */
internal fun latencyLabel(node: ProxyNode): String = when {
    node.lastLatencyAt == null -> "还没测过"
    node.lastLatencyMs == null -> "测不通"
    else -> "${node.lastLatencyMs} ms"
}

@Composable
private fun TestUrlField(url: String, onChange: (String) -> Unit) {
    OutlinedTextField(
        value = url,
        onValueChange = { onChange(it.take(160)) },
        singleLine = true,
        textStyle = MaterialTheme.typography.bodySmall,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
        modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp),
    )
}

// 从「网络与代理」「信息流」挪过来的几块

/**
 *
 * 为什么放这一页：它管的是"代理用不了的时候怎么办"，和上面的「指定代理（没填全）」「内置代理核心（没在跑）」
 * 是同一件事；「网络与代理」只留小白会改的四档选择和平台开关。存储键还是 `proxyExitLock`，行为一个字没动。
 */
@Composable
private fun ExitLockCard(graph: AppGraph) {
    val s = graph.settings.value
    SettingsCard("出口锁") {
        SwitchRow(
            label = "代理不可用时禁止直连",
            hint = if (s.proxyExitLock) "开（默认）：代理用不了时，点亮的平台直接失败" else "关：代理用不了时，点亮的平台改为直连",
            checked = s.proxyExitLock,
        ) { v -> graph.settings.update { it.copy(proxyExitLock = v) } }
        // 出口锁的口径说明（为什么默认开、什么时候该关）压成一行 + 「?」
        Hint(
            "代理用不了 = 内置核心没跑 /「指定」没填全",
            detail = if (s.proxyExitLock) {
                "开（默认）：代理用不了（内置核心没跑 /「指定」没填全）时，点亮的平台请求直接失败，一个都不发。"
            } else {
                "关：代理用不了时，点亮的平台改为直连（装了透明代理模块的机器用得上）；日志里会记一行“出口锁关闭，直连”。"
            },
        )
    }
}

/**
 * 「信息流（进阶）」：从「信息流」挪过来的两项。
 */
@Composable
private fun FeedAdvancedCard(graph: AppGraph) {
    val s = graph.settings.value
    val update = graph.settings::update
    SettingsCard("信息流（进阶）") {
        // 每平台一批最多：只削掉最肥的一两家，不改排序
        SettingBlock(
            "每平台一批最多",
            "从每个平台自己的推荐顺序里取前几条",
            detail = "从每个平台自己的推荐顺序里取前几条；调小会让「加载更多」更频繁（每次都是每个平台一个请求），所以选项最小到 8。",
        ) {
            PerPlatformCapChoice(s.maxPerPlatform) { n -> update { it.copy(maxPerPlatform = n) } }
        }
        // 首页排序：默认各平台穿插，平台内保持它自己的推荐顺序；
        // 留着对比用。关注页不受影响，一直按时间。
        SettingBlock(
            "首页排序",
            if (s.homeOrder == FeedOrder.INTERLEAVE) "各平台穿插，平台内保持它自己的推荐顺序" else "老行为：整批按发布时间倒序",
            detail = if (s.homeOrder == FeedOrder.INTERLEAVE) {
                "每个平台一条队列，按它这一批自己的顺序均匀铺开（条数多的平台会分散在整批里，而不是挤在批尾）。关注页不受影响，仍按发布时间。"
            } else {
                "老行为：整批按发布时间倒序。各平台推荐内容的新旧差很多，按时间排会让 B站这类平台沉到每一批的最后。"
            },
        ) {
            SegmentedChoice(
                listOf(FeedOrder.INTERLEAVE to "穿插", FeedOrder.TIME to "按时间"),
                s.homeOrder,
            ) { v -> update { it.copy(homeOrder = v) } }
        }
    }
}

// ---------------------------------------------------------------------------
// 从主页挪过来的两块
// ---------------------------------------------------------------------------

/** 指定代理的详细项（协议 / 地址 / 端口）：主页只留四档选择，明细挪到这里。 */
@Composable
private fun CustomProxyCard(graph: AppGraph) {
    val s = graph.settings.value
    SettingsCard("指定代理的详细项") {
        Hint("「网络与代理」页的「代理」选「指定」时用这里的协议 / 地址 / 端口。地址没填全时点亮的平台会直接失败（不会悄悄直连）。")
        if (s.proxy.mode != feedhub.core.ProxyConfig.Mode.CUSTOM) {
            Hint("现在选的不是「指定」（当前：${proxyModeLabel(s.proxy.mode)}），改完模式这边就生效。")
        }
        ProxyAddressFields(s.proxy) { c -> graph.settings.update { it.copy(proxy = c) } }
        if (!s.proxy.valid) Hint("地址和端口填全之前，点亮的平台的请求会直接失败。")
    }
}

internal fun proxyModeLabel(mode: feedhub.core.ProxyConfig.Mode): String = when (mode) {
    feedhub.core.ProxyConfig.Mode.NONE -> "不用"
    feedhub.core.ProxyConfig.Mode.SYSTEM -> "系统代理"
    feedhub.core.ProxyConfig.Mode.CUSTOM -> "指定"
    feedhub.core.ProxyConfig.Mode.BUILTIN -> "内置核心"
}

/** 视频的编码偏好（硬解 / 解码器选择）+ 落盘的解码能力账本：普通用户用不上，默认「自动」就够。 */
@Composable
private fun VideoAdvancedCard(graph: AppGraph) {
    val s = graph.settings.value
    // 账本（[DecodeLedger]）不是 Compose 状态：清掉之后靠这个计数重新读一次
    var ledgerVersion by remember { mutableIntStateOf(0) }
    SettingsCard("视频（进阶）") {
        SettingBlock("编码偏好", "自动：优先本机能硬件解码的（能硬解的里面 AV1 > HEVC > AVC）") {
            SegmentedChoice(
                listOf(
                    CodecPreference.AUTO to "自动",
                    CodecPreference.AVC to "AVC",
                    CodecPreference.HEVC to "HEVC",
                    CodecPreference.AV1 to "AV1",
                ),
                s.codec,
            ) { v -> graph.settings.update { it.copy(codec = v) } }
        }
        Hint("不确定就别改：选错了可能变成软解，费 CPU、还可能放不动。")
        val upperLimit = remember(ledgerVersion) { DecodeLedger.summaryText() }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(
                    "已记下的解码上限：" + (upperLimit ?: "还没有（自动模式只按能不能硬解排）"),
                    style = MaterialTheme.typography.bodyMedium,
                )
                Hint(
                    "播放中实测出来的，自动模式按它避开跟不上的档位（手动选编码不受影响）。" +
                        "记录只对这台机器的这套解码环境有效：换显卡 / 升级驱动 / 升级 GStreamer 之后自动作废，" +
                        "${DecodeLedger.TTL_MS / 86_400_000} 天没有新的实测也作废重测。",
                )
            }
            Spacer(Modifier.width(12.dp))
            OutlinedButton(
                enabled = upperLimit != null,
                onClick = {
                    DecodeLedger.clear()
                    ledgerVersion++
                },
            ) { Text("清除，重新测") }
        }
    }
}

// ---------------------------------------------------------------------------
// 两个弹框
// ---------------------------------------------------------------------------

@Composable
private fun AddSubscriptionDialog(core: ProxyCore, onClose: () -> Unit) {
    var name by remember { mutableStateOf("") }
    var url by remember { mutableStateOf("") }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        // imePadding + 可滚动：手机上这个弹框里有两个输入框，键盘弹出来时不能被挡
        Box(Modifier.padding(16.dp).widthIn(max = 520.dp).imePadding().verticalScroll(rememberScrollState())) {
            SettingsCard("添加订阅") {
                Hint("名字随便起；地址是机密，列表里默认打码。")
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it.take(40) },
                    label = { Text("名字") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = url,
                    onValueChange = { url = it.take(500) },
                    label = { Text("订阅地址") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                    modifier = Modifier.fillMaxWidth(),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(
                        enabled = url.isNotBlank() && url.startsWith("http"),
                        onClick = {
                            core.addSubscription(name, url)
                            onClose()
                        },
                    ) { Text("添加") }
                    TextButton(onClick = onClose) { Text("取消") }
                }
                Hint("加完在列表里点「更新」才会去取（不会偷偷发请求）。")
            }
        }
    }
}

@Composable
private fun ImportNodesDialog(core: ProxyCore, onClose: () -> Unit) {
    var text by remember { mutableStateOf("") }
    var report by remember { mutableStateOf<ImportReport?>(null) }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        // imePadding + 可滚动：粘贴分享链接 / JSON 的大输入框
        Box(Modifier.padding(16.dp).widthIn(max = 560.dp).imePadding().verticalScroll(rememberScrollState())) {
            SettingsCard("导入节点") {
                Hint("粘贴一条或多条分享链接（vless / vmess / trojan / ss / hysteria2 / tuic），或者一段 sing-box 出站 JSON。一行一条。")
                OutlinedTextField(
                    value = text,
                    onValueChange = { text = it.take(20_000) },
                    label = { Text("分享链接 / 出站 JSON") },
                    minLines = 4,
                    maxLines = 8,
                    textStyle = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.fillMaxWidth(),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(
                        enabled = text.isNotBlank() && !busy,
                        onClick = {
                            busy = true
                            scope.launch {
                                report = core.importText(text)
                                busy = false
                            }
                        },
                    ) { Text(if (busy) "导入中…" else "导入") }
                    TextButton(onClick = onClose) { Text("关闭") }
                }
                report?.let { r ->
                    Hint(r.summary)
                    for (s in r.skipped.take(6)) Hint("第 ${s.index} 条跳过了：${s.reason}")
                    if (r.skipped.size > 6) Hint("还有 ${r.skipped.size - 6} 条被跳过（原因同上）")
                }
                Hint("跳过条目的原因只说第几条、为什么，不会把你粘的内容回显出来（里面可能有密码）。")
            }
        }
    }
}
