package feedhub.app.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import feedhub.app.AppGraph
import feedhub.app.LocalPlatforms
import feedhub.app.proxyChoiceFor
import feedhub.app.withProxyChoice
import feedhub.core.Platform
import feedhub.core.ProxyChoice
import feedhub.core.ProxyConfig
import feedhub.core.ProxyType
import feedhub.core.proxy.CoreStatus

/**
 * 设置页的「网络」分组。
 *
 * 从上到下：
 * 1. 「代理」：全局四档 `不用 / 系统 / 指定 / 内置核心`；
 * 2. 「按平台」：每个平台一行 `跟随全局 / 直连 / 指定代理`（**境外平台没有「直连」**，红线）。
 *
 * 不亮 = 直连）。现在每个平台自己选一档，
 * 那一排标签被这一列取代（点亮 = 「跟随全局」、不亮 = 「直连」，存的还是 `AppSettings.proxiedPlatforms`，
 * 见 [feedhub.app.withProxyChoice]）。境外平台一律锁在「跟随全局」或「指定代理」：它不提供「直连」。
 *
 * **这一页只留小白会改的**：
 * 「指定」的协议 / 地址 / 端口（**全局**那一份）、「代理不可用时禁止直连」都挪到了「进阶设置 → 代理（进阶）」；这里只显示一行摘要 /
 * 一行"没填全会直接失败"的后果说明，外加一个**能点进去**的入口（[SettingsJump]）。
 * 按平台「指定代理」的地址 / 端口就地填（那是这一页自己的东西，点进去反而绕）。
 *
 * 选择逻辑见 [feedhub.app.proxyFor]（判定在 `core` 的 `resolveProxyMode`，cli 用的是同一份）。
 * 改了就立刻生效（[feedhub.app.SettingsStore.proxyProvider] 每次请求现读，改了会清一次连接池）；
 * **正在播放的视频要重新打开才换代理**（播放器自己下载流，代理在打开时设一次）。
 */
@Composable
internal fun NetworkSettings(graph: AppGraph, nav: SettingsNav) {
    val s = graph.settings.value
    SettingsCard("网络") {
        SettingBlock("代理", "正在播放的视频要重新打开才换代理") {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                // 「内置核心」要有真的核心才显示（桌面接了 sing-box；安卓要 nativeLibraryDir 里有
                // libsingbox.so 才组装，见 androidProxyCore）——没接就藏起来，选不到就不会 fail-closed
                // 到莫名其妙的错误上。
                val modes = buildList {
                    add(ProxyConfig.Mode.NONE to "不用")
                    add(ProxyConfig.Mode.SYSTEM to "系统")
                    add(ProxyConfig.Mode.CUSTOM to "指定")
                    if (graph.showBuiltinProxy) add(ProxyConfig.Mode.BUILTIN to "内置核心")
                }
                SegmentedChoice(modes, s.proxy.mode) { m -> graph.settings.update { it.copy(proxy = it.proxy.copy(mode = m)) } }
                if (!graph.showBuiltinProxy && s.proxy.mode == ProxyConfig.Mode.BUILTIN) {
                    Hint(
                        "这一档这台设备上没有：换「不用 / 系统 / 指定」。" +
                            if (s.proxyExitLock) "跟随全局的平台会直接失败。" else "出口锁关着，跟随全局的平台会直连。",
                    )
                }
                when (s.proxy.mode) {
                    ProxyConfig.Mode.CUSTOM -> {
                        // 协议 / 地址 / 端口在「进阶设置 → 代理（进阶）」里
                        val where = if (s.proxy.host.isBlank()) "（地址还没填）" else "${s.proxy.host}:${s.proxy.port}"
                        Hint("当前指定：${s.proxy.type.name} $where —— 协议 / 地址 / 端口在「代理（进阶）」里改。")
                        // 地址 / 端口没填全时跟随全局的平台会直接失败
                        if (!s.proxy.valid) Hint("地址和端口填全之前，跟随全局的平台的请求会直接失败（不会悄悄走系统代理）。")
                        ExitLockHint(s, unavailable = !s.proxy.valid)
                        SettingsJump("去「代理（进阶）」改协议 / 地址 / 端口", SettingsRoute.Advanced(AdvancedSection.PROXY), nav)
                    }

                    ProxyConfig.Mode.BUILTIN -> {
                        val core = graph.proxyCore
                        val status: CoreStatus? = if (core != null) core.status.collectAsState().value else null
                        if (core != null) Hint("内置核心：" + builtinSummary(status, hasCore = true)) else BuiltinMissingHint()
                        Hint("订阅 / 节点 / 日志都在「代理（进阶）」里。")
                        ExitLockHint(s, unavailable = status !is CoreStatus.Running)
                        SettingsJump("去「代理（进阶）」看订阅 / 节点 / 日志", SettingsRoute.Advanced(AdvancedSection.PROXY), nav)
                    }

                    else -> Unit
                }
            }
        }
        SettingBlock("按平台", "「跟随全局」用上面那个代理；境外平台没有「直连」") {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                val platforms = LocalPlatforms.current
                for (p in platforms.platforms.filter { graph.available(it) }) {
                    PlatformProxyRow(
                        platform = p,
                        // 境外平台没有「直连」（红线：境外平台绝不直连）：插件描述的 overseas，不按平台名判断
                        overseas = platforms[p]?.overseas == true,
                        choice = s.proxyChoiceFor(p, platforms),
                        global = s.proxy,
                        onChange = { c -> graph.settings.update { it.withProxyChoice(p, c, platforms) } },
                    )
                }
            }
        }
        // 这里只在"代理用不了"的时候留一句后果说明（见上面两条分支的 ExitLockHint），别让常规页变空。
    }
}

/**
 * 「按平台」里的那一行：平台名 + `跟随全局 / 直连 / 指定代理` 三档（境外平台只有两档）。
 * 选「指定代理」时下面就地展开协议 / 地址 / 端口；地址没填全时补一句"这个平台会直接失败"。
 */
@Composable
private fun PlatformProxyRow(
    platform: Platform,
    overseas: Boolean,
    choice: ProxyChoice,
    global: ProxyConfig,
    onChange: (ProxyChoice) -> Unit,
) {
    // 分段按钮按"哪一档"选，不按整个 ProxyChoice 比较（指定代理的地址会变，值不相等也要保持选中）
    val kinds = buildList {
        add(KIND_FOLLOW to "跟随全局")
        if (!overseas) add(KIND_DIRECT to "直连")
        add(KIND_CUSTOM to "指定")
    }
    val current = when (choice) {
        ProxyChoice.Follow -> KIND_FOLLOW
        ProxyChoice.Direct -> KIND_DIRECT
        is ProxyChoice.Custom -> KIND_CUSTOM
    }
    fun pick(kind: Int) {
        when (kind) {
            KIND_FOLLOW -> onChange(ProxyChoice.Follow)
            KIND_DIRECT -> onChange(ProxyChoice.Direct)
            // 「指定」默认填上全局那份地址（多数时候就是同一个代理，改一下就行）；全局不是「指定」时留空
            else -> onChange(choice as? ProxyChoice.Custom ?: initialCustom(global))
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        SettingInline(platform.label, controlWidth = if (overseas) 200.dp else 250.dp) {
            SegmentedChoice(kinds, current) { pick(it) }
        }
        if (choice is ProxyChoice.Custom) {
            ProxyAddressFields(choice) { onChange(it) }
            if (!choice.valid) {
                Hint("地址 / 端口没填全：这个平台的请求会直接失败（不回退直连）。")
            }
        }
    }
}

private const val KIND_FOLLOW = 0
private const val KIND_DIRECT = 1
private const val KIND_CUSTOM = 2

/** 切到「指定代理」时的初值：全局就是「指定」那一档时抄它的地址，否则留空（用户自己填）。 */
private fun initialCustom(global: ProxyConfig): ProxyChoice.Custom =
    if (global.mode == ProxyConfig.Mode.CUSTOM) {
        ProxyChoice.Custom(global.host, global.port, global.type)
    } else {
        ProxyChoice.Custom()
    }

/**
 * 「指定」/「内置核心」两档下、**出口锁关着**时补的那句后果说明（[NetworkSettings] 的两条路都用它）；
 * 出口锁开着时什么都不显示（那两档的"用不了就报错"提示本来就在上面）。
 *
 *
 * [unavailable] 是这一档**现在**用不用得了：现在就用不了要说清"现在直连"，用得了就说清将来会怎样。
 */
@Composable
private fun ExitLockHint(s: feedhub.app.AppSettings, unavailable: Boolean) {
    if (s.proxyExitLock) return
    Hint(
        if (unavailable) "出口锁关着：这一档现在用不了，跟随全局的平台现在直连（不再报错）。"
        else "出口锁关着：这一档将来用不了（核心没跑 / 地址没填全）时，跟随全局的平台会改走直连，不再报错。",
    )
}

/** 「内置核心」那一档在主设置页上的一句话状态（没有核心时用下面那个）。 */
@Composable
private fun BuiltinMissingHint() {
    Hint("内置核心：" + builtinSummary(null, hasCore = false))
}

/** 「内置核心」那一档在主设置页上的一句话状态。 */
internal fun builtinSummary(status: CoreStatus?, hasCore: Boolean): String = when {
    !hasCore -> "这个组装里没有内置核心，跟随全局的平台会直接失败"
    status is CoreStatus.Running -> "运行中（127.0.0.1:${status.port}）"
    status is CoreStatus.Starting -> "正在启动"
    status is CoreStatus.Restarting -> "正在重启（${status.reason}）"
    status is CoreStatus.Failed -> "起不来：${status.message}"
    else -> "已停止"
}

/**
 * 「指定」时那一行：协议分段 + 地址 + 端口。宽屏排一行，窄屏（手机竖屏）分两行，免得地址被挤没。
 *
 * 地址 / 端口一直存在 [ProxyConfig] 里（和模式无关），所以在四档之间切来切去不会丢掉手填的地址。
 * **全局**那一份只有「进阶设置 → 代理（进阶）→ 指定代理的详细项」在用它（常规页只留模式选择）；
 * 按平台那一份用的是下面那个 [ProxyChoice.Custom] 重载。
 */
@Composable
internal fun ProxyAddressFields(proxy: ProxyConfig, onChange: (ProxyConfig) -> Unit) {
    ProxyAddressFields(
        type = proxy.type,
        host = proxy.host,
        port = proxy.port,
        onType = { t -> onChange(proxy.copy(type = t)) },
        onHost = { h -> onChange(proxy.copy(host = h)) },
        onPort = { p -> onChange(proxy.copy(port = p)) },
    )
}

/** 同一个输入行的 [ProxyChoice.Custom] 版本（设置页「按平台」里就地填）。 */
@Composable
internal fun ProxyAddressFields(choice: ProxyChoice.Custom, onChange: (ProxyChoice.Custom) -> Unit) {
    ProxyAddressFields(
        type = choice.type,
        host = choice.host,
        port = choice.port,
        onType = { t -> onChange(choice.copy(type = t)) },
        onHost = { h -> onChange(choice.copy(host = h)) },
        onPort = { p -> onChange(choice.copy(port = p)) },
    )
}

@Composable
private fun ProxyAddressFields(
    type: ProxyType,
    host: String,
    port: Int,
    onType: (ProxyType) -> Unit,
    onHost: (String) -> Unit,
    onPort: (Int) -> Unit,
) {
    val typeChoice: @Composable () -> Unit = {
        Box(Modifier.width(176.dp)) {
            SegmentedChoice(listOf(ProxyType.HTTP to "HTTP", ProxyType.SOCKS5 to "SOCKS5"), type) { onType(it) }
        }
    }
    val address: @Composable (Modifier) -> Unit = { m ->
        ProxyField("地址", host, m) { onHost(it) }
    }
    val portField: @Composable (Modifier) -> Unit = { m ->
        ProxyField("端口", if (port > 0) port.toString() else "", m, digitsOnly = true) { v ->
            onPort(v.toIntOrNull() ?: 0)
        }
    }
    BoxWithConstraints {
        if (maxWidth >= 430.dp) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                typeChoice()
                address(Modifier.weight(1f))
                portField(Modifier.width(88.dp))
            }
        } else {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                typeChoice()
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    address(Modifier.weight(1f))
                    portField(Modifier.width(88.dp))
                }
            }
        }
    }
}

/** 地址 / 端口的小输入框。 */
@Composable
private fun ProxyField(
    label: String,
    value: String,
    modifier: Modifier = Modifier,
    digitsOnly: Boolean = false,
    onChange: (String) -> Unit,
) {
    OutlinedTextField(
        value = value,
        onValueChange = { t -> onChange(if (digitsOnly) t.filter { it.isDigit() }.take(5) else t.take(120)) },
        label = { Text(label) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = if (digitsOnly) KeyboardType.Number else KeyboardType.Text),
        textStyle = MaterialTheme.typography.bodySmall,
        modifier = modifier.fillMaxWidth().padding(vertical = 2.dp),
    )
}
