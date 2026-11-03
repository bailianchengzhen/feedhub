package feedhub.app.sync

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.background
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import feedhub.app.AppGraph
import feedhub.app.login.QrCode
import feedhub.core.sync.LanAddresses
import feedhub.core.sync.PairProtocol
import feedhub.core.sync.PairingClient
import feedhub.core.sync.PairingCodec
import feedhub.core.sync.PairingException
import feedhub.core.sync.PairingOffer
import feedhub.core.sync.PairingServer
import feedhub.core.sync.PairingServers
import feedhub.core.sync.PairingSession
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/*
 * 「配对新设备」的界面。
 *
 * 一次配对 = 一条和「登录同步」同一套形状的一次性局域网通道（二维码里只有地址 / 端口 / 会话 id / 一次性密钥），
 * 双方在这条通道上**交换设备公钥 + 设备 id + 设备名**，然后各自算一个 6 位短校验码。
 * 用户在两台设备上看到同一个数字、点「确认」之后才写进 `sync_peer`——扫错码 / 局域网中间人会看到不一样的数字。
 *
 * 私钥永远不出本机；二维码、弹框里都不出现私钥。演示（截图）用 [SyncPairingDebug] 直接摆出二维码 / 校验码，
 * 不监听端口、不发请求。
 */

private enum class HostPhase { Idle, Working, Showing, Verifying, Done, Failed }

private enum class ScanPhase { Scanning, Working, Verifying, Done, Failed }

/** 电脑 / 手机这边显示二维码：等对面扫码，收到之后弹出 6 位校验码。 */
@Composable
fun PairDeviceDialog(
    graph: AppGraph,
    sync: LibrarySync,
    initialDemo: SyncPairingDebug? = null,
    onDismiss: () -> Unit,
    onPaired: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val addresses = remember { LanAddresses.list() }
    // 只有真的带了二维码内容 / 校验码才算"截图演示状态"；空对象 = 正常打开（选地址、生成二维码）
    val demoState = remember(initialDemo) { initialDemo?.takeIf { it.qrContent.isNotBlank() || it.code.isNotBlank() } }
    var addressIndex by remember { mutableIntStateOf(0) }
    var addressMenu by remember { mutableStateOf(false) }
    var phase by remember {
        mutableStateOf(
            when {
                demoState == null -> HostPhase.Idle
                demoState.qrContent.isNotBlank() -> HostPhase.Showing
                else -> HostPhase.Verifying
            },
        )
    }
    var session by remember { mutableStateOf<PairingSession?>(null) }
    var qrContent by remember { mutableStateOf(demoState?.qrContent.orEmpty()) }
    var demo by remember { mutableStateOf(demoState) }
    var peerName by remember { mutableStateOf(demoState?.peerName.orEmpty()) }
    var peerDevice by remember { mutableStateOf("") }
    var peerKey by remember { mutableStateOf<ByteArray?>(null) }
    var code by remember { mutableStateOf(demoState?.code.orEmpty()) }
    var error by remember { mutableStateOf<String?>(null) }
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }

    DisposableEffect(Unit) { onDispose { session?.server?.stop() } }

    // 倒计时 + 服务状态轮询（只在弹框活着、且真有服务的时候跑）
    LaunchedEffect(session) {
        val s = session?.server ?: return@LaunchedEffect
        while (true) {
            now = System.currentTimeMillis()
            when (s.state) {
                PairingServer.State.PAIRED -> {
                    val p = s.peer
                    if (p != null) {
                        peerName = p.name
                        peerDevice = p.device
                        peerKey = p.publicKey
                        code = s.code.orEmpty()
                        phase = HostPhase.Verifying
                    } else {
                        error = "收到了配对请求但读不出对方的信息"
                        phase = HostPhase.Failed
                    }
                    return@LaunchedEffect
                }
                PairingServer.State.EXPIRED -> {
                    error = "二维码过期了（三分钟）。重新生成一个再让对方扫"
                    phase = HostPhase.Failed
                    session = null
                    return@LaunchedEffect
                }
                PairingServer.State.FAILED -> {
                    error = s.failure ?: "配对服务起不来"
                    phase = HostPhase.Failed
                    session = null
                    return@LaunchedEffect
                }
                else -> Unit
            }
            delay(200)
        }
    }

    fun startServer() {
        val address = addresses.getOrNull(addressIndex)
        if (address == null) {
            error = "没找到能用的局域网地址：请先连上 Wi-Fi / 网线"
            phase = HostPhase.Failed
            return
        }
        session?.server?.stop()
        session = null
        error = null
        code = ""
        peerKey = null
        phase = HostPhase.Working
        scope.launch {
            try {
                val started = withContext(Dispatchers.IO) {
                    val key = sync.deviceKey() ?: throw PairingException("这台设备的资料库不支持设备密钥")
                    val own = PairingCodec.own(key.deviceId, sync.pairingName(), key.publicKey)
                    PairingServers.start(address.host, own)
                }
                session = started
                qrContent = started.offer.content
                phase = HostPhase.Showing
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                error = (e as? PairingException)?.message ?: "生成配对二维码失败（${e::class.simpleName}）"
                phase = HostPhase.Failed
            }
        }
    }

    fun confirm() {
        val device = peerDevice
        val key = peerKey
        if (device.isBlank() || key == null) {
            // 截图 / 演示状态：没有真的对端，直接关掉
            onDismiss()
            return
        }
        sync.rememberPeer(device, peerName, key)
        phase = HostPhase.Done
        onPaired()
    }

    Dialog(onDismissRequest = onDismiss) {
        Surface(
            shape = RoundedCornerShape(20.dp),
            color = MaterialTheme.colorScheme.surfaceContainerLowest,
            modifier = Modifier.width(380.dp).heightIn(max = 720.dp),
        ) {
            Column(
                Modifier.padding(20.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("配对新设备", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                    IconButton(onClick = onDismiss) { Icon(Icons.Outlined.Close, contentDescription = "关闭") }
                }
                Text(
                    "在这台设备上显示二维码，用另一台设备扫一下。两边的通道是一次性加密的，只交换设备公钥、设备名和设备 id；" +
                        "私钥永远不出本机。扫完两边会显示同一个 6 位数字，核对一致再点确认。二维码三分钟内有效、只能用一次。",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )

                when (phase) {
                    HostPhase.Idle, HostPhase.Working -> {
                        Text("局域网地址", style = MaterialTheme.typography.labelLarge)
                        Box {
                            OutlinedButton(onClick = { addressMenu = true }, enabled = addresses.isNotEmpty() && phase == HostPhase.Idle) {
                                Text(
                                    addresses.getOrNull(addressIndex)?.label ?: "没有可用的局域网地址",
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                            DropdownMenu(expanded = addressMenu, onDismissRequest = { addressMenu = false }) {
                                addresses.forEachIndexed { i, a ->
                                    DropdownMenuItem(text = { Text(a.label) }, onClick = { addressIndex = i; addressMenu = false })
                                }
                            }
                        }
                        if (addresses.size > 1) {
                            Text(
                                "有多个网卡：选和另一台设备同一个 Wi-Fi / 路由器的那个地址。",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                            Button(onClick = ::startServer, enabled = phase == HostPhase.Idle && addresses.isNotEmpty()) {
                                Text(if (phase == HostPhase.Working) "正在生成…" else "生成配对二维码")
                            }
                            if (phase == HostPhase.Working) CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                            TextButton(onClick = onDismiss) { Text("取消") }
                        }
                    }

                    HostPhase.Showing -> {
                        Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                            if (qrContent.isNotBlank()) QrCode(qrContent, 232.dp, dimmed = false)
                        }
                        val left = ((session?.server?.expiresAtMs ?: now) - now).coerceAtLeast(0)
                        Text(
                            if (session == null) "（演示二维码，不会监听端口）" else "还有 ${mmss(left)}（到点自动关闭）",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.align(Alignment.CenterHorizontally),
                        )
                        Text("在另一台设备上点「扫二维码配对」，扫这个码…", style = MaterialTheme.typography.bodySmall)
                        error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlinedButton(onClick = ::startServer, enabled = session != null) { Text("重新生成") }
                            TextButton(onClick = onDismiss) { Text("取消") }
                        }
                    }

                    HostPhase.Verifying -> {
                        PairingCodePane(peerName = peerName, code = code) {
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                Button(onClick = ::confirm) { Text("确认配对") }
                                TextButton(onClick = onDismiss) { Text("取消") }
                            }
                        }
                        if (session == null && demo != null) {
                            Text("（演示：没有真的对端）", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }

                    HostPhase.Done -> {
                        Text("已配对：$peerName", style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.primary)
                        Text("以后导出会默认发给它（不用输密码）。", style = MaterialTheme.typography.bodySmall)
                        Button(onClick = onDismiss) { Text("好") }
                    }

                    HostPhase.Failed -> {
                        Text("没有配上", style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.error)
                        error?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(onClick = { phase = HostPhase.Idle; error = null }) { Text("重新开始") }
                            TextButton(onClick = onDismiss) { Text("关闭") }
                        }
                    }
                }
            }
        }
    }
}

/** 手机 / 另一台设备这边：扫码（或 debug 手动粘贴）→ 交换 → 核对 6 位码 → 确认。 */
@Composable
fun ScanPairingDialog(
    graph: AppGraph,
    sync: LibrarySync,
    initialContent: String? = null,
    onDismiss: () -> Unit,
    onPaired: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    var phase by remember { mutableStateOf(if (initialContent.isNullOrBlank()) ScanPhase.Scanning else ScanPhase.Working) }
    var error by remember { mutableStateOf<String?>(null) }
    var cameraError by remember { mutableStateOf<String?>(null) }
    var manualOpen by remember { mutableStateOf(initialContent != null) }
    var manualText by remember { mutableStateOf(initialContent.orEmpty()) }
    var peerName by remember { mutableStateOf("") }
    var peerDevice by remember { mutableStateOf("") }
    var peerKey by remember { mutableStateOf<ByteArray?>(null) }
    var code by remember { mutableStateOf("") }

    fun receive(text: String) {
        if (phase == ScanPhase.Working || phase == ScanPhase.Done) return
        error = null
        phase = ScanPhase.Working
        scope.launch {
            try {
                val offer = PairingOffer.parse(text)
                    ?: throw PairingException("这不是 feedhub 的配对二维码。在另一台设备上点「配对新设备」重新生成一个")
                val result = withContext(Dispatchers.IO) {
                    val key = sync.deviceKey() ?: throw PairingException("这台设备的资料库不支持设备密钥")
                    val own = PairingCodec.own(key.deviceId, sync.pairingName(), key.publicKey)
                    PairingClient().exchange(offer, own)
                }
                peerName = result.peer.name
                peerDevice = result.peer.device
                peerKey = result.peer.publicKey
                code = result.code
                phase = ScanPhase.Verifying
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                error = (e as? PairingException)?.message ?: "配对失败（${e::class.simpleName}）"
                phase = ScanPhase.Failed
            }
        }
    }

    LaunchedEffect(initialContent) {
        if (!initialContent.isNullOrBlank()) receive(initialContent)
    }

    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(
            color = MaterialTheme.colorScheme.background,
            modifier = Modifier.fillMaxSize().windowInsetsPadding(androidx.compose.foundation.layout.WindowInsets.safeDrawing),
        ) {
            Column(Modifier.fillMaxSize().imePadding().padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("扫二维码配对", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                    IconButton(onClick = onDismiss) { Icon(Icons.Outlined.Close, contentDescription = "关闭") }
                }
                Text(
                    "在另一台设备的「设置 → 跨设备同步 → 配对新设备」里生成二维码，用这台设备扫。" +
                        "只交换设备公钥 / 设备名 / 设备 id，不传收藏、历史和 Cookie。",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )

                when (phase) {
                    ScanPhase.Scanning -> {
                        if (initialContent == null && graph.pairing?.canScan == true) {
                            Box(
                                Modifier.fillMaxWidth().height(300.dp)
                                    .background(MaterialTheme.colorScheme.surfaceContainerHighest, RoundedCornerShape(16.dp)),
                            ) {
                                QrScannerView(
                                    onCode = { c -> if (PairingOffer.looksLikePairing(c)) receive(c) },
                                    onError = { cameraError = it },
                                    modifier = Modifier.fillMaxSize(),
                                )
                            }
                            cameraError?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
                            Text("把另一台设备屏幕上的二维码放进框里。", style = MaterialTheme.typography.bodySmall)
                        }
                        if (graph.pairing?.manualQrInput == true) {
                            if (!manualOpen) {
                                TextButton(onClick = { manualOpen = true }) { Text("没有摄像头？手动输入二维码内容（调试）") }
                            } else {
                                Text(
                                    "含一次性密钥，别截图外发。只在两台设备自己之间用。",
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
                                    Button(onClick = { receive(manualText) }, enabled = manualText.isNotBlank()) { Text("开始配对") }
                                    TextButton(onClick = { manualOpen = false }) { Text("收起") }
                                }
                            }
                        }
                        if (initialContent == null && graph.pairing?.canScan != true && graph.pairing?.manualQrInput != true) {
                            Text("这个版本不能扫码，也不能手动输入二维码内容。", style = MaterialTheme.typography.bodyMedium)
                        }
                    }

                    ScanPhase.Working -> Row(verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(Modifier.size(22.dp), strokeWidth = 2.dp)
                        Spacer(Modifier.width(10.dp))
                        Text("正在和另一台设备交换公钥…")
                    }

                    ScanPhase.Verifying -> {
                        PairingCodePane(peerName = peerName, code = code) {
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                Button(
                                    onClick = {
                                        val key = peerKey
                                        if (key != null) sync.rememberPeer(peerDevice, peerName, key)
                                        phase = ScanPhase.Done
                                        onPaired()
                                    },
                                ) { Text("确认配对") }
                                TextButton(onClick = onDismiss) { Text("取消") }
                            }
                        }
                    }

                    ScanPhase.Done -> {
                        Text("已配对：$peerName", style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.primary)
                        Text("以后对方导出会默认发给你（不用输密码）。", style = MaterialTheme.typography.bodySmall)
                        Button(onClick = onDismiss) { Text("好") }
                    }

                    ScanPhase.Failed -> {
                        Text("没有配上", style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.error)
                        error?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(onClick = { phase = ScanPhase.Scanning; error = null }) { Text("重新扫") }
                            TextButton(onClick = onDismiss) { Text("关闭") }
                        }
                    }
                }
            }
        }
    }
}

/** 6 位校验码 + 对方的设备名；两边要对得上才确认。 */
@Composable
private fun PairingCodePane(peerName: String, code: String, buttons: @Composable () -> Unit) {
    Text(
        "核对两台设备上的数字是否一样",
        style = MaterialTheme.typography.titleSmall,
    )
    Text(
        code.ifBlank { "------" },
        style = MaterialTheme.typography.displayMedium,
        fontFamily = FontFamily.Monospace,
        fontWeight = FontWeight.Bold,
        letterSpacing = 8.sp,
        textAlign = TextAlign.Center,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.fillMaxWidth(),
    )
    if (peerName.isNotBlank()) {
        Text("对方设备：$peerName", style = MaterialTheme.typography.bodyMedium)
    }
    Text(
        "如果两台设备上的数字不一样，说明中间有别人插进来（或者扫错了码）：点取消，重新配对。",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    buttons()
}

private fun mmss(ms: Long): String {
    val total = (ms / 1000).coerceAtLeast(0)
    return "${total / 60}:${(total % 60).toString().padStart(2, '0')}"
}
