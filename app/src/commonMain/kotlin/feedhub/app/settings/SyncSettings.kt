package feedhub.app.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import feedhub.app.AppGraph
import feedhub.app.library.ImportPreview
import feedhub.app.sync.ExportOutcome
import feedhub.app.sync.ImportInspection
import feedhub.app.sync.ImportOutcome
import feedhub.app.sync.LibrarySync
import feedhub.app.sync.PairDeviceDialog
import feedhub.app.sync.ScanPairingDialog
import feedhub.app.sync.SyncPairingDebug
import feedhub.app.sync.SyncPickedFile
import feedhub.app.sync.SyncPreviewDebug
import feedhub.core.sync.LibrarySyncFile
import feedhub.core.sync.SyncCounts
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * 设置 →「跨设备同步」。
 *
 * - **已配对设备**：名字 + 配对时间 + 「移除」。配对之后导出**默认发给它们（含本机自己）**，不用再输密码。
 * - **导出到文件**：有已配对设备就直接按设备加密；一台都没有就退回密码模式，并提示"先配对就不用输密码"。
 * - **改用密码导出**：备用入口，给没配对的设备 / 旧版本；密码最短 12 位。
 * - **从文件导入**：先看文件头，里面有本机的收件条目就用设备密钥直接解（不问密码，预览里写"已用本机密钥解开"）；
 *   密码文件才弹密码框；不是给本机的按设备文件直接说清楚。
 * - **配对新设备**：显示二维码（桌面 / 安卓都可以），另一台扫码或 debug 手动粘贴；两边核对 6 位码后各自确认。
 *
 * 实现分三层：这张卡片 → [LibrarySync]（编排）→ `feedhub.core.sync`（密钥 / 加解密 / 配对 / 合并）。
 * 选文件在 [feedhub.app.sync.SyncFiles]，桌面是 `JFileChooser`、安卓是 SAF。
 *
 * @param initialSyncPreview 截图工具用来直接摆出"导入预览"弹框（平时只有点了导入才会出现）。
 * @param initialPairing 截图工具用来直接摆出"配对二维码 / 校验码"（不监听端口）。
 */
@Composable
fun SyncSettings(
    graph: AppGraph,
    initialSyncPreview: SyncPreviewDebug? = null,
    initialPairing: SyncPairingDebug? = null,
) {
    val sync = remember(graph) {
        LibrarySync(
            library = graph.library,
            files = graph.syncFiles,
            settings = { graph.settings.value },
            deviceKeys = graph.deviceKeys,
            deviceName = { graph.deviceName },
        )
    }
    val scope = rememberCoroutineScope()
    var dialog by remember {
        mutableStateOf<SyncDialogState?>(initialSyncPreview?.let { SyncDialogState.Preview(it.ready) })
    }
    // 配对口令：截图时直接摆出二维码 / 校验码（不监听端口、不发请求）
    var pairingHost by remember { mutableStateOf(initialPairing) }
    var pairingScan by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var status by remember { mutableStateOf<String?>(null) }
    var peers by remember { mutableStateOf(sync.peers()) }
    val pairingUi = graph.pairing

    fun refreshPeers() {
        peers = sync.peers()
    }

    SettingsCard("跨设备同步") {
        Text(
            "在电脑和平板之间同步收藏（含收藏夹）、历史（含播放进度）和屏蔽用户。配对一次之后，导出默认按对方设备加密，" +
                "导入时不用再输密码；密码模式保留给没配对的设备。文件里有你的浏览历史，别随手发出去；" +
                "导入是合并，不会覆盖本机，导入前会先给你看清楚要改几条。",
            style = MaterialTheme.typography.bodySmall,
        )

        Text("已配对设备", style = MaterialTheme.typography.labelLarge)
        if (peers.isEmpty()) {
            Hint("还没有配对的设备。先配对一个：以后导出就不用每次输密码（一台都没有时导出会自动改用密码）。")
        } else {
            for (peer in peers) {
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                    Column(Modifier.weight(1f)) {
                        Text(peer.name.ifBlank { "未命名设备" }, style = MaterialTheme.typography.bodyMedium)
                        Text(
                            "配对于 ${pairedAtText(peer.pairedAt)}",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    TextButton(
                        onClick = {
                            sync.forgetPeer(peer.deviceId)
                            status = "已移除「${peer.name.ifBlank { "未命名设备" }}」"
                            refreshPeers()
                        },
                        enabled = !busy,
                    ) { Text("移除") }
                }
            }
        }

        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedButton(
                onClick = {
                    status = null
                    if (peers.isEmpty()) {
                        // 没有配对设备：默认走密码，并告诉用户怎么省掉这一步
                        dialog = SyncDialogState.ExportPassword(noPeers = true)
                    } else {
                        busy = true
                        scope.launch {
                            val outcome = withContext(Dispatchers.IO) { sync.exportToPairedDevices() }
                            busy = false
                            when (outcome) {
                                is ExportOutcome.Saved -> status = savedText(outcome)
                                ExportOutcome.Cancelled -> Unit
                                is ExportOutcome.Failed -> status = outcome.message
                            }
                        }
                    }
                },
                enabled = sync.available && !busy,
            ) { Text("导出到文件") }
            OutlinedButton(
                onClick = {
                    status = null
                    busy = true
                    scope.launch {
                        val picked = try {
                            withContext(Dispatchers.IO) { sync.pickImportFile() }
                        } catch (e: Exception) {
                            status = "选文件失败：${e.message}"
                            null
                        }
                        if (picked == null) {
                            busy = false
                            return@launch
                        }
                        // 先看文件头：里面有本机的收件条目就直接解（不问密码）；密码文件才弹框
                        val inspection = try {
                            withContext(Dispatchers.IO) { sync.inspect(picked) }
                        } catch (e: Exception) {
                            busy = false
                            status = "读不了这个文件：${e.message}"
                            return@launch
                        }
                        if (inspection.error != null) {
                            busy = false
                            status = inspection.error
                            return@launch
                        }
                        when {
                            inspection.forThisDevice -> {
                                val outcome = withContext(Dispatchers.IO) { sync.preview(picked, null) }
                                busy = false
                                when (outcome) {
                                    is ImportOutcome.Ready -> dialog = SyncDialogState.Preview(outcome)
                                    is ImportOutcome.Failed -> status = outcome.message
                                    ImportOutcome.NeedsPassword -> dialog = SyncDialogState.ImportPassword(picked)
                                    ImportOutcome.Cancelled -> Unit
                                }
                            }
                            inspection.mode == LibrarySyncFile.Mode.PASSWORD -> {
                                busy = false
                                dialog = SyncDialogState.ImportPassword(picked)
                            }
                            else -> {
                                busy = false
                                status = notForThisDeviceText(inspection)
                            }
                        }
                    }
                },
                enabled = sync.available && !busy,
            ) { Text("从文件导入") }
            if (busy) Text("处理中…", style = MaterialTheme.typography.bodySmall)
        }

        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            TextButton(
                onClick = {
                    status = null
                    dialog = SyncDialogState.ExportPassword()
                },
                enabled = sync.available && !busy,
            ) { Text("改用密码导出") }
            if (pairingUi?.canHost == true) {
                TextButton(onClick = { pairingHost = SyncPairingDebug() }, enabled = !busy) { Text("配对新设备") }
            }
            if (pairingUi?.canScan == true) {
                TextButton(onClick = { pairingScan = true }, enabled = !busy) { Text("扫二维码配对") }
            }
        }

        Hint(
            "设备密钥只在本机（桌面是配置目录里一个 600 权限的文件、安卓是 App 私有目录），不导出、不上传，" +
                "任何界面和日志都不显示。已配对设备只在换机 / 重装后需要重新配一次；「历史只保留最近 N 天」仍然是本机策略。",
        )
        if (!sync.devicePairingAvailable && pairingUi != null) {
            Hint("这个版本 / 这台设备上的资料库暂时不能配对（只支持密码模式）。")
        }
        if (!sync.available) {
            Hint("这个版本 / 这台设备上的资料库暂时用不了同步（桌面和安卓的正式构建都可以）。")
        }
        status?.let { Hint(it) }
    }

    // 配对：显示二维码 / 扫码（截图时用的是 SyncPairingDebug，不监听端口）
    pairingHost?.let { demo ->
        PairDeviceDialog(
            graph = graph,
            sync = sync,
            initialDemo = demo,
            onDismiss = { pairingHost = null },
            onPaired = { refreshPeers(); status = "已配对，之后导出会自动发给它" },
        )
    }
    if (pairingScan) {
        ScanPairingDialog(
            graph = graph,
            sync = sync,
            onDismiss = { pairingScan = false },
            onPaired = { refreshPeers(); status = "已配对，之后对方导出会自动发给你" },
        )
    }

    when (val state = dialog) {
        null -> Unit
        is SyncDialogState.ExportPassword -> PasswordDialog(
            title = "用密码导出同步文件",
            hint = (if (state.noPeers) "还没有配对的设备：这次会用密码加密（先配对就不用输密码）。\n" else "给没配对的设备用密码加密。\n") +
                "文件里有你的浏览历史，至少 ${LibrarySync.MIN_PASSWORD_LENGTH} 位；密码不会被保存，丢了就打不开这个文件。",
            confirm = true,
            busy = busy,
            error = state.error,
            onDismiss = { if (!busy) dialog = null },
            onConfirm = { password ->
                busy = true
                scope.launch {
                    val outcome = withContext(Dispatchers.IO) { sync.export(password) }
                    busy = false
                    when (outcome) {
                        is ExportOutcome.Saved -> {
                            status = savedText(outcome)
                            dialog = null
                        }
                        ExportOutcome.Cancelled -> dialog = null
                        is ExportOutcome.Failed -> dialog = SyncDialogState.ExportPassword(outcome.message, noPeers = state.noPeers)
                    }
                }
            },
        )

        is SyncDialogState.ImportPassword -> PasswordDialog(
            title = "打开同步文件",
            hint = "输入导出这个文件时用的同步密码。",
            confirm = false,
            busy = busy,
            error = state.error,
            onDismiss = { if (!busy) dialog = null },
            onConfirm = { password ->
                busy = true
                scope.launch {
                    val outcome = withContext(Dispatchers.IO) { sync.preview(state.file, password) }
                    busy = false
                    when (outcome) {
                        is ImportOutcome.Ready -> dialog = SyncDialogState.Preview(outcome)
                        is ImportOutcome.Failed -> dialog = SyncDialogState.ImportPassword(state.file, outcome.message)
                        ImportOutcome.Cancelled -> dialog = null
                        ImportOutcome.NeedsPassword -> dialog = SyncDialogState.ImportPassword(state.file)
                    }
                }
            },
        )

        is SyncDialogState.Preview -> ImportPreviewDialog(
            ready = state.ready,
            error = state.error,
            busy = busy,
            onDismiss = { if (!busy) dialog = null },
            onApply = { ignoreStale ->
                busy = true
                scope.launch {
                    val result = withContext(Dispatchers.IO) { sync.apply(state.ready, ignoreStale) }
                    busy = false
                    if (result.applied) {
                        status = "已导入：新增 ${result.counts.added} 条 · 更新 ${result.counts.updated} 条 · " +
                            "删除 ${result.counts.deleted} 条" +
                            (if (result.clearedRows > 0) "（按另一台设备的清空历史清掉 ${result.clearedRows} 条）" else "")
                        dialog = null
                    } else {
                        dialog = SyncDialogState.Preview(state.ready, result.rejectedReason ?: "导入没有生效")
                    }
                }
            },
        )
    }

}

private fun savedText(saved: ExportOutcome.Saved): String {
    val how = if (saved.mode == LibrarySyncFile.Mode.RECIPIENTS) {
        "按设备加密，发给 ${saved.recipients} 台设备（含本机）" +
            (if (saved.skippedPeers > 0) "，跳过 ${saved.skippedPeers} 台公钥读不出来的" else "")
    } else {
        "用密码加密"
    }
    return "已导出到「${saved.fileName}」：收藏 ${saved.favorites} 条、历史 ${saved.history} 条，" +
        "共 ${(saved.bytes + 1023) / 1024} KB（$how）"
}

private fun notForThisDeviceText(inspection: ImportInspection): String =
    "这个同步文件不是发给这台设备的（文件里 ${inspection.recipients.size} 台收件设备，没有本机）。" +
        "让导出方把你加进已配对设备，或者改用密码导出"

private fun pairedAtText(at: Long): String =
    if (at <= 0) "未知时间" else SimpleDateFormat("yyyy-MM-dd HH:mm", Locale.getDefault()).format(Date(at))

private sealed interface SyncDialogState {
    data class ExportPassword(val error: String? = null, val noPeers: Boolean = false) : SyncDialogState
    data class ImportPassword(val file: SyncPickedFile, val error: String? = null) : SyncDialogState
    data class Preview(val ready: ImportOutcome.Ready, val error: String? = null) : SyncDialogState
}

/**
 * 同步密码弹框。[confirm] 为 true 时（导出）要输两遍，防止打错之后导出的文件自己也打不开。
 * 密码只在内存里：取消 / 完成后清空输入框。
 */
@Composable
private fun PasswordDialog(
    title: String,
    hint: String,
    confirm: Boolean,
    busy: Boolean,
    error: String?,
    onDismiss: () -> Unit,
    onConfirm: (CharArray) -> Unit,
) {
    var password by remember { mutableStateOf("") }
    var repeat by remember { mutableStateOf("") }
    var localError by remember { mutableStateOf<String?>(null) }
    Dialog(onDismissRequest = { if (!busy) onDismiss() }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(
            shape = androidx.compose.foundation.shape.RoundedCornerShape(20.dp),
            color = MaterialTheme.colorScheme.surfaceContainerLowest,
            modifier = Modifier.padding(16.dp).widthIn(max = 440.dp),
        ) {
            Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(title, style = MaterialTheme.typography.titleMedium)
                Hint(hint)
                OutlinedTextField(
                    value = password,
                    onValueChange = { password = it; localError = null },
                    label = { Text("同步密码") },
                    singleLine = true,
                    visualTransformation = PasswordVisualTransformation(),
                    enabled = !busy,
                    modifier = Modifier.fillMaxWidth(),
                )
                if (confirm) {
                    OutlinedTextField(
                        value = repeat,
                        onValueChange = { repeat = it; localError = null },
                        label = { Text("再输一次") },
                        singleLine = true,
                        visualTransformation = PasswordVisualTransformation(),
                        enabled = !busy,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                (error ?: localError)?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    TextButton(onClick = onDismiss, enabled = !busy) { Text("取消") }
                    Spacer(Modifier.width(8.dp))
                    TextButton(
                        onClick = {
                            val problem = when {
                                password.isEmpty() -> "先输一个同步密码"
                                confirm && password.length < LibrarySync.MIN_PASSWORD_LENGTH ->
                                    "同步密码至少 ${LibrarySync.MIN_PASSWORD_LENGTH} 位"
                                confirm && password != repeat -> "两次输入不一样"
                                else -> null
                            }
                            if (problem != null) {
                                localError = problem
                            } else {
                                onConfirm(password.toCharArray())
                            }
                        },
                        enabled = !busy,
                    ) { Text(if (busy) "处理中…" else "确定") }
                }
            }
        }
    }
}

/**
 * 导入预览：解密之后、写库之前给用户看"会改哪几条"。**默认按钮就是合并**，没有"覆盖"这个选项
 */
@Composable
private fun ImportPreviewDialog(
    ready: ImportOutcome.Ready,
    error: String?,
    busy: Boolean,
    onDismiss: () -> Unit,
    onApply: (ignoreStale: Boolean) -> Unit,
) {
    val preview = ready.preview
    Dialog(onDismissRequest = { if (!busy) onDismiss() }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(
            shape = androidx.compose.foundation.shape.RoundedCornerShape(20.dp),
            color = MaterialTheme.colorScheme.surfaceContainerLowest,
            modifier = Modifier.padding(16.dp).widthIn(max = 480.dp).heightIn(max = 660.dp),
        ) {
            Column(
                Modifier.padding(20.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text("导入预览", style = MaterialTheme.typography.titleMedium)
                Hint("文件：${ready.fileName}")
                Text(
                    if (ready.unlock == LibrarySyncFile.Unlock.DEVICE_KEY) {
                        "已用本机密钥解开（没问密码）：这份文件是发给这台设备的。"
                    } else {
                        "已用同步密码解开。"
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.primary,
                )
                Text(
                    "新增 ${preview.counts.added} 条 · 更新 ${preview.counts.updated} 条 · 删除 ${preview.counts.deleted} 条",
                    style = MaterialTheme.typography.bodyMedium,
                )
                CountLine("收藏", preview.counts.favoritesAdded, preview.counts.favoritesUpdated, preview.counts.favoritesDeleted)
                CountLine("收藏夹", preview.counts.foldersAdded, preview.counts.foldersUpdated, preview.counts.foldersDeleted)
                CountLine("历史", preview.counts.historyAdded, preview.counts.historyUpdated, preview.counts.historyDeleted)
                CountLine("屏蔽的用户", preview.counts.blockedAdded, preview.counts.blockedUpdated, preview.counts.blockedDeleted)
                if (preview.foldersClaimed > 0) {
                    Hint("${preview.foldersClaimed} 个收藏夹会和文件里的同名收藏夹接通，不会变成两个。")
                }
                if (preview.foldersCreated > 0) Hint("会新建 ${preview.foldersCreated} 个收藏夹。")
                if (preview.historyClearedByRemote) {
                    Text(
                        "文件里显示另一台设备清空过历史：导入会按那个时间点清掉本机 ${preview.clearedRows} 条旧历史。",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
                if (preview.skippedItems > 0) Hint("文件里有 ${preview.skippedItems} 条内容读不出来 / 太大，会跳过。")
                if (preview.stale) {
                    Text(
                        "这个文件比上次从同一台设备导入的旧（序号 ${preview.sourceSeq} < ${preview.lastImportedSeq}）：" +
                            "可能是把旧备份放回来了。继续导入会以文件里的旧状态为准，撤销这台设备之后的删除 / 修改。",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
                if (!preview.hasChanges) Hint("和本机一致，没有要改的东西。")
                error?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
                Hint("导入是逐条合并：没存在文件里的收藏 / 历史不会被删掉（除非文件里明确写了是删除）。")
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    TextButton(onClick = onDismiss, enabled = !busy) { Text("取消") }
                    Spacer(Modifier.width(8.dp))
                    TextButton(
                        onClick = { onApply(preview.stale) },
                        enabled = !busy && (preview.hasChanges || preview.stale || error != null),
                    ) { Text(if (busy) "导入中…" else if (preview.stale) "仍要导入" else "合并导入") }
                }
            }
        }
    }
}

@Composable
private fun CountLine(label: String, added: Int, updated: Int, deleted: Int) {
    if (added == 0 && updated == 0 && deleted == 0) return
    Hint("$label：新增 $added · 更新 $updated · 删除 $deleted")
}

/** 预览里那一行"新增 / 更新 / 删除"的纯文本（测试 / 日志用，别再格式化成别的样子）。 */
internal fun syncCountsText(counts: SyncCounts): String =
    "新增 ${counts.added} 条 · 更新 ${counts.updated} 条 · 删除 ${counts.deleted} 条"

/** 让调用方知道文件格式版本（界面上的说明和 core 里的常量是同一个）。 */
internal val syncFileExtension: String = LibrarySyncFile.EXTENSION
