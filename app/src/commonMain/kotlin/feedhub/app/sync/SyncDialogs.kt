package feedhub.app.sync

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import feedhub.app.AppGraph
import feedhub.app.login.QrCode
import feedhub.app.mine.AccountsState
import feedhub.app.mine.ImportConfirmDialog
import feedhub.app.ui.brandColor
import feedhub.core.Platform
import feedhub.core.sync.LanAddresses
import feedhub.core.sync.SyncApplyResult
import feedhub.core.sync.SyncClient
import feedhub.core.sync.SyncDirection
import feedhub.core.sync.SyncException
import feedhub.core.sync.SyncInbound
import feedhub.core.sync.SyncOffer
import feedhub.core.sync.SyncPushResult
import feedhub.core.sync.SyncScope
import feedhub.core.sync.SyncServer
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/*
 * 「登录互传」的界面：任意两台客户端扫码互传登录。
 *
 * 出码方选方向（发出去 / 收进来）、选平台（发出去时）、起一次性服务；扫码方扫过去，
 * 按二维码里的方向**取走**或**推上来**。收之前一定先列出"会覆盖本机哪些平台"让用户确认。
 *
 * 安全约定（两端都一样）：明文 HTTP 只传 AES-256-GCM 的密文；二维码里只有局域网地址、端口、
 * 会话 id、一次性 256 位密钥、协议版本和方向，**没有 Cookie**；二维码两分钟内有效、只能成功
 * 一次；不打印、不落盘（二维码是画在 Canvas 上的，不是文件）。调试日志最多出现平台名和 Cookie 个数。
 */

private enum class Stage { HOME, SCANNING, OFFER, CONFIRM, PUSH_CONFIRM, DONE, FAILED }

/** 这个端能按 Cookie 互传的平台：没有关键 Cookie 的平台（YouTube 是设备码 token）不算。 */
private fun cookieSyncPlatforms(graph: AppGraph, state: AccountsState): List<Platform> =
    state.platforms.filter { p ->
        val login = graph.platforms[p]?.login ?: return@filter false
        login.web.requiredCookies.isNotEmpty()
    }

private fun scopesFor(graph: AppGraph, state: AccountsState): Map<Platform, SyncScope> =
    SyncScope.of(graph.platforms).filterKeys { it in state.platforms }

private fun countText(counts: Map<Platform, Int>): String =
    counts.entries.joinToString("、") { (p, n) -> "${p.label} $n 个" }

private fun mmss(ms: Long): String {
    val total = (ms / 1000).coerceAtLeast(0)
    return "${total / 60}:${(total % 60).toString().padStart(2, '0')}"
}

/**
 * 两端账号页同一个入口「登录互传」。
 *
 * @param initialContent 调试入口直接灌进来的二维码内容（`--es syncqr`），自动走和扫码一样的取回路径
 * @param autoOffer 调试入口直接选好方向并自动出码（`--es syncmode offer-send|offer-receive`）
 * @param autoConfirm 调试入口自动点"写入"（`--ez syncauto true`；只用于端到端验收）
 * @param advertiseHost 调试入口把二维码里的地址改成这个（模拟器 A↔B 用 `adb forward` 时）
 */
@Composable
fun LoginTransferDialog(
    graph: AppGraph,
    state: AccountsState,
    initialContent: String? = null,
    autoOffer: SyncDirection? = null,
    autoConfirm: Boolean = false,
    advertiseHost: String? = null,
    bindHostOverride: String? = null,
    onDismiss: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val client = remember { SyncClient() }
    val scopes = remember(graph, state) { scopesFor(graph, state) }
    val supported = remember(graph, state) { cookieSyncPlatforms(graph, state).filter { it in scopes }.toSet() }
    val addresses = remember { LanAddresses.list() }
    var addressIndex by remember { mutableIntStateOf(0) }
    fun loggedIn(p: Platform): Boolean = graph.platforms[p]?.let { state.hasLoginCookies(it.login.web) } == true

    var direction by remember { mutableStateOf(SyncDirection.SEND) }
    var selected by remember { mutableStateOf<Set<Platform>>(emptySet()) }
    var defaultsLoaded by remember { mutableStateOf(false) }
    var stage by remember { mutableStateOf(if (initialContent != null) Stage.SCANNING else Stage.HOME) }
    var server by remember { mutableStateOf<SyncServer?>(null) }
    var qrText by remember { mutableStateOf<String?>(null) }
    var inbound by remember { mutableStateOf<SyncInbound?>(null) }
    var applied by remember { mutableStateOf<SyncApplyResult?>(null) }
    var pushed by remember { mutableStateOf<SyncPushResult?>(null) }
    /** 扫到"收进来"方向的码：要推给对方的地址和平台，等用户点"发送"。 */
    var outbound by remember { mutableStateOf<Pair<SyncOffer, Set<Platform>>?>(null) }
    var sentCounts by remember { mutableStateOf<Map<Platform, Int>>(emptyMap()) }
    var error by remember { mutableStateOf<String?>(null) }
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    var working by remember { mutableStateOf(false) }
    var appliedByServer by remember { mutableStateOf(false) }
    var confirmImport by remember { mutableStateOf<Platform?>(null) }
    var importTarget by remember { mutableStateOf<Platform?>(null) }
    var addressMenu by remember { mutableStateOf(false) }
    var manualOpen by remember { mutableStateOf(initialContent != null) }
    var manualText by remember { mutableStateOf(initialContent.orEmpty()) }
    var cameraError by remember { mutableStateOf<String?>(null) }

    DisposableEffect(Unit) { onDispose { server?.stop() } }

    // 默认勾上"本机已有该平台 Cookie"的；Rust 按 Cookie 罐 + 各平台地盘算。
    LaunchedEffect(scopes) {
        val defaults = runCatching { state.defaultSyncPlatforms(scopes) }.getOrDefault(emptyList())
        selected = defaults.filter { it in supported }.toSet()
        defaultsLoaded = true
    }

    // 从 Chrome 导入完成后自动勾上
    LaunchedEffect(state.importing, state.notice, importTarget) {
        val target = importTarget ?: return@LaunchedEffect
        if (state.importing != null) return@LaunchedEffect
        if (target in supported && loggedIn(target)) selected = selected + target
        importTarget = null
    }

    fun applyInbound(inValue: SyncInbound) {
        if (working) return
        working = true
        error = null
        scope.launch {
            try {
                val result = state.applyLoginTransfer(inValue, scopes)
                if (result.applied.isEmpty()) {
                    throw SyncException(
                        result.skipped.values.firstOrNull()?.let { "对方发来的平台这台设备都用不了：$it" }
                            ?: "没有写入任何平台",
                    )
                }
                applied = result
                stage = Stage.DONE
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                error = (e as? SyncException)?.message ?: "写入失败（${e::class.simpleName}）"
                stage = Stage.FAILED
            } finally {
                working = false
            }
        }
    }

    fun pushOutbound(offer: SyncOffer, plats: Set<Platform>) {
        if (working) return
        working = true
        error = null
        scope.launch {
            try {
                pushed = state.pushLoginTransfer(client, offer, plats, scopes)
                outbound = null
                stage = Stage.DONE
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                error = (e as? SyncException)?.message ?: "同步失败（${e::class.simpleName}）"
                stage = Stage.FAILED
            } finally {
                working = false
            }
        }
    }

    fun startOffer() {
        if (direction == SyncDirection.SEND && selected.isEmpty()) {
            error = "先勾选至少一个平台"
            return
        }
        val address = addresses.getOrNull(addressIndex)
        if (address == null && bindHostOverride == null) {
            error = "没找到能用的局域网地址：请先连上 Wi-Fi / 网线"
            return
        }
        server?.stop()
        server = null
        qrText = null
        error = null
        sentCounts = emptyMap()
        applied = null
        pushed = null
        inbound = null
        appliedByServer = false
        working = true
        stage = Stage.OFFER
        scope.launch {
            try {
                val s = state.startLoginTransferServer(
                    bindHost = bindHostOverride ?: address!!.host,
                    direction = direction,
                    platforms = if (direction == SyncDirection.SEND) selected else emptySet(),
                    scopes = scopes,
                    advertiseHost = advertiseHost ?: address!!.host,
                )
                server = s
                qrText = s.offer.content
                sentCounts = if (direction == SyncDirection.SEND) s.counts else emptyMap()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                error = (e as? SyncException)?.message ?: "生成二维码失败（${e::class.simpleName}）"
                stage = Stage.FAILED
            } finally {
                working = false
            }
        }
    }

    fun receive(text: String) {
        if (working) return
        working = true
        error = null
        stage = Stage.SCANNING
        scope.launch {
            try {
                val offer = SyncOffer.parse(text)
                    ?: throw SyncException("这不是 feedhub 的登录互传二维码。让对方重新生成一个")
                when (offer.direction) {
                    SyncDirection.SEND -> {
                        val pending = state.fetchLoginTransfer(client, offer)
                        inbound = pending
                        stage = Stage.CONFIRM
                        if (autoConfirm) {
                            // 验收入口直接点"写入"；这里先把 working 放掉，否则 applyInbound 会以为
                            // 自己还在同一个任务里（它的防重入判断）而什么都不做。
                            working = false
                            applyInbound(pending)
                        }
                    }
                    SyncDirection.RECEIVE -> {
                        // 对方要收：本机把登录推上去（选中的平台；没勾就按本机有 Cookie 的来）。
                        // 先让用户看清"推给谁、推哪些"再发：扫一下就把所有登录交出去太危险。
                        val plats = selected.ifEmpty { state.defaultSyncPlatforms(scopes).toSet() }
                        if (plats.isEmpty()) throw SyncException("本机没有可以推给对方的登录")
                        outbound = offer to plats
                        stage = Stage.PUSH_CONFIRM
                        if (autoConfirm) {
                            working = false
                            pushOutbound(offer, plats)
                        }
                    }
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                error = (e as? SyncException)?.message ?: "同步失败（${e::class.simpleName}）"
                stage = Stage.FAILED
            } finally {
                working = false
            }
        }
    }

    // 出码之后：倒计时 + 服务状态轮询（只在这一个弹框活着的时候跑）
    LaunchedEffect(server) {
        val s = server ?: return@LaunchedEffect
        while (true) {
            now = System.currentTimeMillis()
            when (s.state) {
                SyncServer.State.DELIVERED -> {
                    if (s.direction == SyncDirection.SEND) {
                        sentCounts = s.counts
                        stage = Stage.DONE
                    }
                    // 收进来模式不会出现 DELIVERED（成功是 RECEIVED）
                    return@LaunchedEffect
                }
                SyncServer.State.RECEIVED -> {
                    inbound = s
                    stage = Stage.CONFIRM
                    if (autoConfirm && !appliedByServer) {
                        appliedByServer = true
                        applyInbound(s)
                    }
                    return@LaunchedEffect
                }
                SyncServer.State.EXPIRED -> {
                    error = "二维码过期了（两分钟），服务已经关了"
                    stage = Stage.FAILED
                    return@LaunchedEffect
                }
                SyncServer.State.FAILED -> {
                    error = s.failure ?: "服务起不来"
                    stage = Stage.FAILED
                    return@LaunchedEffect
                }
                else -> Unit
            }
            delay(200)
        }
    }

    // 调试入口：自动开始出码（等默认平台算完）
    LaunchedEffect(autoOffer, defaultsLoaded) {
        if (autoOffer == null || !defaultsLoaded || server != null) return@LaunchedEffect
        direction = autoOffer
        startOffer()
    }

    // 调试入口：直接把二维码内容拿去走和扫码一样的路径
    LaunchedEffect(initialContent) {
        if (!initialContent.isNullOrBlank()) receive(initialContent)
    }

    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Surface(
            shape = RoundedCornerShape(20.dp),
            color = MaterialTheme.colorScheme.surfaceContainerLowest,
            modifier = Modifier.widthIn(max = 420.dp).width(380.dp).heightIn(max = 720.dp),
        ) {
            Column(
                Modifier.padding(20.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("登录互传", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                    IconButton(onClick = onDismiss) { Icon(Icons.Outlined.Close, contentDescription = "关闭") }
                }
                Text(
                    "两台设备连同一个局域网（电脑 / 手机 / 平板 / 备用机都行），一台出二维码、另一台扫。" +
                        "二维码两分钟内有效、只能成功用一次。只同步 Cookie，不同步设备标识；" +
                        "收的一方会按平台各自的域先清后写，会顶掉本机同平台的登录。" +
                        "二维码里有一次性密钥，别截图外发。",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )

                when (stage) {
                    Stage.HOME -> {
                        if (graph.loginSync?.canOffer == true) {
                            Text("我来出码", style = MaterialTheme.typography.labelLarge)
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                FilterChip(
                                    selected = direction == SyncDirection.SEND,
                                    onClick = { direction = SyncDirection.SEND },
                                    label = { Text("发出去") },
                                )
                                FilterChip(
                                    selected = direction == SyncDirection.RECEIVE,
                                    onClick = { direction = SyncDirection.RECEIVE },
                                    label = { Text("收进来") },
                                )
                            }
                            Text(
                                if (direction == SyncDirection.SEND) {
                                    "发出去：对方扫码取走这台设备选中的登录。"
                                } else {
                                    "收进来：对方扫码把它的登录推上来，写进来之前会先让你确认。"
                                },
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )

                            Text("局域网地址", style = MaterialTheme.typography.labelLarge)
                            Box {
                                OutlinedButton(
                                    onClick = { addressMenu = true },
                                    enabled = addresses.isNotEmpty(),
                                ) {
                                    Text(
                                        addresses.getOrNull(addressIndex)?.label ?: "没有可用的局域网地址",
                                        maxLines = 1,
                                        overflow = TextOverflow.Ellipsis,
                                    )
                                }
                                DropdownMenu(expanded = addressMenu, onDismissRequest = { addressMenu = false }) {
                                    addresses.forEachIndexed { i, a ->
                                        DropdownMenuItem(
                                            text = { Text(a.label) },
                                            onClick = { addressIndex = i; addressMenu = false },
                                        )
                                    }
                                }
                            }
                            if (addresses.size > 1) {
                                Text(
                                    "有多个网卡：选和对方同一个 Wi-Fi / 路由器的那个地址。",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }

                            if (direction == SyncDirection.SEND) {
                                Text("要发出去哪些平台", style = MaterialTheme.typography.labelLarge)
                                Column(
                                    Modifier.heightIn(max = 260.dp).verticalScroll(rememberScrollState()),
                                    verticalArrangement = Arrangement.spacedBy(2.dp),
                                ) {
                                    for (p in state.platforms) {
                                        val plugin = graph.platforms[p]
                                        val canSync = p in supported
                                        val hasCookies = canSync && loggedIn(p)
                                        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                                            Checkbox(
                                                checked = p in selected,
                                                enabled = canSync,
                                                onCheckedChange = { on ->
                                                    selected = if (on) selected + p else selected - p
                                                },
                                            )
                                            Column(Modifier.weight(1f)) {
                                                Text(p.label, style = MaterialTheme.typography.bodyMedium)
                                                Text(
                                                    when {
                                                        !canSync && plugin != null && plugin.login.web.requiredCookies.isEmpty() ->
                                                            "设备码登录（token 存在本机，不是 Cookie），不能这样同步"
                                                        !canSync -> "这个平台在这里不能同步"
                                                        hasCookies -> "已登录"
                                                        else -> "未登录"
                                                    },
                                                    style = MaterialTheme.typography.bodySmall,
                                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                                    maxLines = 2,
                                                )
                                            }
                                            if (canSync && !hasCookies) {
                                                val importer = state.importer
                                                if (importer != null && importer.supports(p)) {
                                                    TextButton(onClick = { confirmImport = p }) {
                                                        Text("从 ${importer.browserName} 导入")
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            error?.let {
                                Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                            }
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                Button(
                                    onClick = ::startOffer,
                                    enabled = !working && addresses.isNotEmpty() &&
                                        (direction == SyncDirection.RECEIVE || selected.isNotEmpty()),
                                ) {
                                    Text(if (working) "正在生成…" else "生成二维码")
                                }
                                if (working) CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                                TextButton(onClick = onDismiss) { Text("取消") }
                            }
                        }

                        if (graph.loginSync?.canScan == true || graph.loginSync?.manualQrInput == true) {
                            if (graph.loginSync.canOffer == true) {
                                Text("对方出码，我来扫", style = MaterialTheme.typography.labelLarge)
                            }
                            Text(
                                "扫「发出去」的码 = 取走对方的登录；扫「收进来」的码 = 把本机登录推给对方。" +
                                    "取回来的内容会先列出来让你确认再写。",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                if (graph.loginSync.canScan == true) {
                                    Button(onClick = { stage = Stage.SCANNING; error = null }) { Text("扫码") }
                                }
                                if (graph.loginSync.manualQrInput == true) {
                                    OutlinedButton(onClick = {
                                        manualOpen = true
                                        stage = Stage.SCANNING
                                        error = null
                                    }) { Text("粘贴二维码内容（调试）") }
                                }
                                TextButton(onClick = onDismiss) { Text("关闭") }
                            }
                        }
                    }

                    Stage.SCANNING -> {
                        Text("扫码", style = MaterialTheme.typography.labelLarge)
                        if (initialContent == null && graph.loginSync?.canScan == true) {
                            Box(
                                Modifier.fillMaxWidth().height(280.dp)
                                    .background(MaterialTheme.colorScheme.surfaceContainerHighest, RoundedCornerShape(16.dp)),
                            ) {
                                QrScannerView(
                                    onCode = { code -> if (SyncOffer.looksLikeSync(code)) receive(code) },
                                    onError = { cameraError = it },
                                    modifier = Modifier.fillMaxWidth().height(280.dp),
                                )
                            }
                            cameraError?.let {
                                Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                            }
                            Text("把另一台设备屏幕上的二维码放进框里。", style = MaterialTheme.typography.bodySmall)
                        }
                        if (graph.loginSync?.manualQrInput == true) {
                            if (!manualOpen && initialContent == null) {
                                TextButton(onClick = { manualOpen = true }) {
                                    Text("没有摄像头？手动输入二维码内容（调试）")
                                }
                            }
                            if (manualOpen) {
                                Text(
                                    "含一次性密钥，别截图外发。",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.error,
                                )
                                OutlinedTextField(
                                    value = manualText,
                                    onValueChange = { manualText = it },
                                    label = { Text("二维码内容") },
                                    singleLine = false,
                                    maxLines = 4,
                                    modifier = Modifier.fillMaxWidth(),
                                )
                                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    Button(onClick = { receive(manualText) }, enabled = !working && manualText.isNotBlank()) {
                                        Text("开始同步")
                                    }
                                    TextButton(onClick = { manualOpen = false }) { Text("收起") }
                                }
                            }
                        }
                        if (working) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                CircularProgressIndicator(Modifier.size(22.dp), strokeWidth = 2.dp)
                                Spacer(Modifier.width(10.dp))
                                Text("正在和对方同步…")
                            }
                        }
                        error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            TextButton(onClick = { stage = Stage.HOME; error = null }) { Text("返回") }
                            TextButton(onClick = onDismiss) { Text("关闭") }
                        }
                    }

                    Stage.OFFER -> {
                        val s = server
                        val left = ((s?.expiresAtMs ?: now) - now).coerceAtLeast(0)
                        Text(
                            if (direction == SyncDirection.SEND) "等对方扫码取走…" else "等对方扫码把登录推上来…",
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                            qrText?.let { QrCode(it, 232.dp, dimmed = false) }
                        }
                        Text(
                            "还有 ${mmss(left)}（到点自动关闭）",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.align(Alignment.CenterHorizontally),
                        )
                        if (direction == SyncDirection.SEND) {
                            Text(
                                "会传：" + countText(sentCounts).ifEmpty { "（正在打包…）" },
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        // 调试构建把二维码内容也显示出来（模拟器 / 天机 pick 不到屏时用 uiautomator 取；
                        // 内容里只有一次性会话和密钥，没有 Cookie）
                        if (graph.loginSync?.manualQrInput == true && qrText != null) {
                            Text(
                                "调试用二维码内容（别外发）：",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.error,
                            )
                            SelectionContainer {
                                Text(
                                    qrText.orEmpty(),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                        error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlinedButton(onClick = ::startOffer) { Text("重新生成") }
                            TextButton(onClick = { server?.stop(); stage = Stage.HOME }) { Text("返回") }
                            TextButton(onClick = onDismiss) { Text("关闭") }
                        }
                    }

                    Stage.CONFIRM -> {
                        val inValue = inbound
                        Text("确认要写入这台设备吗？", style = MaterialTheme.typography.titleMedium)
                        Text(
                            "会用对方发来的登录顶掉本机这些平台原来的登录；不在列表里的平台不动。",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        val list = inValue?.platforms.orEmpty()
                        if (list.isEmpty()) {
                            Text("对方发来的内容里没有平台。", style = MaterialTheme.typography.bodyMedium)
                        } else {
                            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                for (entry in list) {
                                    val known = entry.platform in graph.platforms.platforms
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        Box(Modifier.size(9.dp).background(entry.platform.brandColor(), CircleShape))
                                        Spacer(Modifier.width(8.dp))
                                        Text(
                                            "${entry.platform.label}：${entry.count} 个 Cookie" +
                                                if (known) "" else "（这台设备没有这个平台，会跳过）",
                                            style = MaterialTheme.typography.bodyMedium,
                                            color = if (known) MaterialTheme.colorScheme.onSurface
                                            else MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    }
                                }
                            }
                        }
                        error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(onClick = { inValue?.let(::applyInbound) }, enabled = inValue != null && !working) {
                                Text(if (working) "正在写入…" else "写入并完成")
                            }
                            OutlinedButton(onClick = { server?.stop(); inbound = null; stage = Stage.HOME }) { Text("取消") }
                            if (working) CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                        }
                    }

                    Stage.PUSH_CONFIRM -> {
                        val out = outbound
                        Text("要把登录发给对方吗？", style = MaterialTheme.typography.titleMedium)
                        Text(
                            "会把这台设备下面这些平台的登录发给 ${out?.first?.host ?: "?"} 上的设备。" +
                                "只在确认那是你自己的设备时才发送。",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            for (platform in out?.second.orEmpty().sortedBy { it.label }) {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Box(Modifier.size(9.dp).background(platform.brandColor(), CircleShape))
                                    Spacer(Modifier.width(8.dp))
                                    Text(platform.label, style = MaterialTheme.typography.bodyMedium)
                                }
                            }
                        }
                        error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(onClick = { out?.let { pushOutbound(it.first, it.second) } }, enabled = out != null && !working) {
                                Text(if (working) "正在发送…" else "发送（${out?.second?.size ?: 0} 个平台）")
                            }
                            OutlinedButton(onClick = { outbound = null; stage = Stage.HOME }) { Text("取消") }
                            if (working) CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                        }
                    }

                    Stage.DONE -> {
                        Text("同步完成", style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.primary)
                        val r = applied
                        when {
                            r != null -> {
                                for ((platform, n) in r.applied) {
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        Box(Modifier.size(9.dp).background(platform.brandColor(), CircleShape))
                                        Spacer(Modifier.width(8.dp))
                                        Text("${platform.label}：$n 个 Cookie", style = MaterialTheme.typography.bodyMedium)
                                    }
                                }
                                val skipped = r.skipped.filterKeys { id -> r.applied.keys.none { it.key == id } }
                                if (skipped.isNotEmpty()) {
                                    Text(
                                        "跳过：" + skipped.entries.joinToString("、") { (id, why) ->
                                            (graph.platforms.find(id)?.label ?: id) + "（$why）"
                                        },
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                                Text(
                                    "正在逐个平台重新检查登录状态，账号卡片会自动更新。",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            pushed != null -> {
                                val p = pushed!!
                                Text("已把这台设备的登录推给对方。", style = MaterialTheme.typography.bodyMedium)
                                Text(
                                    "推送的平台：" + countText(p.counts).ifEmpty { "（没有）" } +
                                        "。对方确认写入后才会生效。",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            else -> {
                                Text(
                                    "已发送：" + countText(sentCounts).ifEmpty { "（没有）" },
                                    style = MaterialTheme.typography.bodyMedium,
                                )
                                Text(
                                    "服务已经关了，这条二维码也不能再用。对方会先确认再写入。",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(onClick = onDismiss) { Text("好") }
                            OutlinedButton(onClick = {
                                server?.stop(); server = null; qrText = null
                                inbound = null; applied = null; pushed = null; error = null
                                stage = Stage.HOME
                            }) { Text("再传一次") }
                        }
                    }

                    Stage.FAILED -> {
                        Text("没有同步成功", style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.error)
                        error?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(onClick = {
                                server?.stop(); server = null; qrText = null
                                inbound = null; error = null
                                stage = if (initialContent != null) Stage.HOME else Stage.SCANNING
                            }) { Text("重试") }
                            TextButton(onClick = { stage = Stage.HOME; error = null }) { Text("返回") }
                            TextButton(onClick = onDismiss) { Text("关闭") }
                        }
                    }
                }
            }
        }
    }

    confirmImport?.let { p ->
        ImportConfirmDialog(
            platformLabel = p.label,
            browserName = state.importer?.browserName ?: "",
            onConfirm = {
                importTarget = p
                state.importFromBrowser(p)
                confirmImport = null
            },
            onDismiss = { confirmImport = null },
        )
    }
}

/** 旧名字（账号页 / 文档里还提到）；现在两端是同一个弹框。 */
@Composable
fun SyncToPhoneDialog(graph: AppGraph, state: AccountsState, onDismiss: () -> Unit) {
    LoginTransferDialog(graph, state, onDismiss = onDismiss)
}
