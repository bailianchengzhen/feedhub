package feedhub.app.mine

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.runtime.LaunchedEffect
import feedhub.app.AppGraph
import feedhub.app.feed.Avatar
import feedhub.app.login.DeviceCodeLoginDialog
import feedhub.app.login.QrLoginDialog
import feedhub.app.sync.LoginTransferDialog
import feedhub.app.ui.brandColor
import feedhub.core.Platform

/**
 * 账号：各平台的登录状态；登录用扫码（平台支持的话）、**网页登录**（安卓：系统 WebView 打开官方登录页），
 * 桌面版另有"从 Chrome 导入"（酷安目前只能这样）；退出登录、导入都要先确认。
 */
@Composable
fun AccountsPane(graph: AppGraph, state: AccountsState, onBack: (() -> Unit)?) {
    var confirmLogout by remember { mutableStateOf<Platform?>(null) }
    var confirmImport by remember { mutableStateOf<Platform?>(null) }
    // 「登录互传」：出码 / 扫码由 graph.loginSync 说的算（电脑只能出码）
    var syncOpen by remember { mutableStateOf(false) }
    var syncInitialContent by remember { mutableStateOf<String?>(null) }
    // 调试入口（--es syncqr）：带着二维码内容直接把「登录互传」的扫码路径打开
    LaunchedEffect(state.pendingSyncContent) {
        val content = state.pendingSyncContent ?: return@LaunchedEffect
        state.pendingSyncContent = null
        syncInitialContent = content
        syncOpen = true
    }
    LaunchedEffect(state.pendingSyncOffer) {
        if (state.pendingSyncOffer != null) syncOpen = true
    }
    PaneScaffold(
        "账号",
        onBack,
        actions = {
            graph.loginSync?.let { sync ->
                if (sync.canOffer || sync.canScan || sync.manualQrInput) {
                    TextButton(onClick = { syncOpen = true }) { Text("登录互传") }
                }
            }
            IconButton(onClick = state::refresh) { Icon(Icons.Outlined.Refresh, contentDescription = "刷新账号状态") }
        },
    ) {
        Column(
            Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 4.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            for (p in state.platforms) {
                AccountCard(
                    p, state.status[p],
                    canScan = state.qrSource(p) != null,
                    webLoginLabel = if (state.canWebLogin(p)) "网页登录" else null,
                    deviceCodeLabel = if (state.deviceCodeSource(p) != null) "设备码登录" else null,
                    importLabel = state.importer?.takeIf { it.supports(p) }?.let { "从 ${it.browserName} 导入" },
                    importing = state.importing == p,
                    notice = state.notice[p],
                    onScan = { state.scanning = p },
                    onWebLogin = { state.webLogin?.start(p) },
                    onDeviceCode = { state.pairing = p },
                    onImport = { confirmImport = p },
                    onLogout = { confirmLogout = p },
                    onRecheck = { state.check(p) },
                )
            }
            Text(
                // 状态是本机存下来的：进这一页不发请求，只有登录 / 退出 / 导入 / 同步之后问那一个平台，
                // 或者点右上角的刷新。这句要写清楚，不然用户不知道看到的可能是旧的。
                "账号状态存在本机，进这一页不会重新请求" +
                    (state.checkedAt?.let { "（上次查到：${checkedAtLabel(it)}）" } ?: "") +
                    "；点右上角的刷新才会重新问各平台，登录、退出、导入、同步之后只问那一个平台。" +
                    "扫码登录要用对应平台的手机 App 扫。" +
                    (if (state.webLogin != null) "网页登录会打开平台的官方登录页；在页面里自己登录，登录完成后自动把 Cookie 存到本机。" else "") +
                    "设备码登录（YouTube）会显示一组短码，在另一台设备上打开验证页输入即可。" +
                    (state.importer?.let { "也可以从本机 ${it.browserName} 导入登录信息（酷安目前只能这样）；第一次会弹出 KWallet / 钥匙环的授权框。" } ?: "") +
                    (if (graph.loginSync != null) {
                        "「登录互传」让两台设备（电脑 / 手机 / 平板 / 备用机）在同一个局域网里扫码互传登录：出码方选「发出去」或「收进来」，另一台扫码；二维码两分钟内有效、只能成功用一次，只同步 Cookie、不同步设备标识，收的一方会先确认再按平台各自的域覆盖。"
                    } else ""),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp, bottom = 16.dp),
            )
        }
    }
    state.scanning?.let { p ->
        state.qrSource(p)?.let { source ->
            QrLoginDialog(p.label, source, onDismiss = { state.scanning = null }, onSuccess = { state.loggedIn(p) }, foreground = graph.foreground)
        }
    }
    // 网页登录（安卓的系统 WebView）不在这里弹：它在 `App` 的最外层铺成一整屏覆盖层
    // （软键盘 / 返回键 / insets 跟主窗口同一条路，见 App.kt）。桌面没有 webLogin，不会进那一支。
    // 设备码登录（YouTube）：小弹框，只有短码 + 验证地址，不输入任何东西。
    state.pairing?.let { p ->
        state.deviceCodeSource(p)?.let { source ->
            DeviceCodeLoginDialog(
                platformLabel = p.label,
                source = source,
                onDismiss = { state.pairing = null },
                onSuccess = { state.deviceCodeDone(p) },
                foreground = graph.foreground,
            )
        }
    }
    confirmImport?.let { p ->
        ImportConfirmDialog(
            platformLabel = p.label,
            browserName = state.importer?.browserName ?: "",
            onConfirm = { state.importFromBrowser(p); confirmImport = null },
            onDismiss = { confirmImport = null },
        )
    }
    if (syncOpen) {
        val close = {
            syncOpen = false
            syncInitialContent = null
            state.pendingSyncOffer = null
            state.pendingSyncAdvertise = null
            state.pendingSyncBind = null
        }
        LoginTransferDialog(
            graph = graph,
            state = state,
            initialContent = syncInitialContent,
            autoOffer = state.pendingSyncOffer,
            autoConfirm = state.pendingSyncAutoConfirm,
            advertiseHost = state.pendingSyncAdvertise,
            bindHostOverride = state.pendingSyncBind,
            onDismiss = close,
        )
    }
    confirmLogout?.let { p ->
        ConfirmDialog(
            title = "退出${p.label}登录？",
            text = "会清掉本机保存的${p.label}登录信息（Cookie），之后要重新登录。",
            confirmText = "退出登录",
            onConfirm = { state.logout(p); confirmLogout = null },
            onDismiss = { confirmLogout = null },
        )
    }
}

@Composable
private fun AccountCard(
    platform: Platform,
    status: AccountStatus?,
    canScan: Boolean,
    webLoginLabel: String?,
    deviceCodeLabel: String?,
    importLabel: String?,
    importing: Boolean,
    notice: String?,
    onScan: () -> Unit,
    onWebLogin: () -> Unit,
    onDeviceCode: () -> Unit,
    onImport: () -> Unit,
    onLogout: () -> Unit,
    onRecheck: () -> Unit,
) {
    Surface(shape = RoundedCornerShape(14.dp), color = MaterialTheme.colorScheme.surfaceContainerLowest, modifier = Modifier.fillMaxWidth()) {
        Row(Modifier.padding(horizontal = 14.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            val user = (status as? AccountStatus.LoggedIn)?.user
            if (user?.avatar != null) {
                Avatar(user.avatar, 40)
            } else {
                Box(Modifier.size(40.dp).background(platform.brandColor().copy(alpha = 0.16f), CircleShape), contentAlignment = Alignment.Center) {
                    Text(platform.label.take(1), color = platform.brandColor(), fontWeight = FontWeight.Bold)
                }
            }
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(platform.label, style = MaterialTheme.typography.labelMedium, color = platform.brandColor())
                when (status) {
                    null, AccountStatus.Checking -> Row(verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
                        Spacer(Modifier.width(8.dp))
                        Text("正在检查…", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    is AccountStatus.LoggedIn -> {
                        OneLine(status.user.name)
                        OneLine("已登录", muted = true)
                    }
                    is AccountStatus.LoggedOut ->
                        if (status.expired) {
                            Text("登录已失效", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.error)
                            OneLine("需要重新登录", muted = true)
                        } else {
                            Text("未登录", style = MaterialTheme.typography.bodyMedium)
                        }
                    is AccountStatus.Failed -> {
                        Text("检查失败", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.error)
                        Text(status.message, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2)
                    }
                }
                if (importing) {
                    OneLine("正在从浏览器导入…", muted = true)
                } else if (notice != null) {
                    Text(notice, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2)
                }
            }
            Spacer(Modifier.width(8.dp))
            when (status) {
                is AccountStatus.LoggedIn -> Column(horizontalAlignment = Alignment.End) {
                    OutlinedButton(onClick = onLogout) { Text("退出登录") }
                    // 已登录也能重新扫码（换账号、刷新登录）：扫码成功前现有登录不受影响
                    if (canScan) TextButton(onClick = onScan) { Text("重新扫码登录") }
                    if (webLoginLabel != null) TextButton(onClick = onWebLogin) { Text(webLoginLabel) }
                }
                is AccountStatus.LoggedOut -> Column(horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    if (canScan) Button(onClick = onScan) { Text("扫码登录") }
                    if (webLoginLabel != null) {
                        if (canScan) OutlinedButton(onClick = onWebLogin) { Text(webLoginLabel) } else Button(onClick = onWebLogin) { Text(webLoginLabel) }
                    }
                    if (deviceCodeLabel != null) {
                        if (canScan || webLoginLabel != null) OutlinedButton(onClick = onDeviceCode) { Text(deviceCodeLabel) } else Button(onClick = onDeviceCode) { Text(deviceCodeLabel) }
                    }
                    if (importLabel != null) OutlinedButton(onClick = onImport, enabled = !importing) { Text(importLabel) }
                    if (!canScan && webLoginLabel == null && deviceCodeLabel == null && importLabel == null) Text("这个平台在这里还不能登录", style = MaterialTheme.typography.bodySmall)
                    if (status.expired) {
                        TextButton(onClick = onLogout) { Text("清除登录信息") }
                    }
                }
                is AccountStatus.Failed -> OutlinedButton(onClick = onRecheck) { Text("重试") }
                else -> Unit
            }
        }
    }
}
