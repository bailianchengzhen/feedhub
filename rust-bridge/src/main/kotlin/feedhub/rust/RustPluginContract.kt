package feedhub.rust

import uniffi.feedhub_ffi.FfiPluginChromeImport
import uniffi.feedhub_ffi.FfiPluginDescriptor
import uniffi.feedhub_ffi.FfiPluginEmote
import uniffi.feedhub_ffi.FfiPluginEmotes
import uniffi.feedhub_ffi.FfiPluginSearch
import uniffi.feedhub_ffi.pluginContractCheckDescriptor

/**
 * 插件描述自检的 Rust 门面。
 *
 * 宿主把 `PlatformPlugin` 的**纯数据**投影成 [Descriptor]，Rust 返回问题列表（空 = 通过）。
 * "描述和它建出来的 Source 对不对得上"那一半留在 `feedhub.core.plugin.PluginContract.check`（要造对象）。
 *
 * 只在插件模块的离线测试 / 启动自检里调，**不在界面的热路径上**。
 */
internal object RustPluginContract {
    class ChromeImport(
        val domain: String,
        val suffixes: List<String>,
        val exactHosts: Set<String>,
    )

    class Emote(val code: String, val url: String)

    class Emotes(val pack: String, val codes: List<Emote>)

    class Search(val pageSize: Int, val kinds: List<String>, val sorts: List<String>)

    /** `PlatformPlugin` 里自检要看的那部分（枚举已经换成名字：`SearchKind` / `SearchSort`）。 */
    class Descriptor(
        val id: String,
        val label: String,
        val color: Long,
        val hosts: List<String>,
        val feedCount: Int,
        val targets: List<String>,
        val allTargets: Int,
        val unavailableNote: String?,
        val proxied: Boolean,
        val overseas: Boolean,
        val cookieDomain: String,
        val chromeImport: ChromeImport?,
        val emotes: Emotes?,
        val search: Search?,
    )

    /** 只看描述本身的检查（`PluginContract.checkDescriptor`）：问题列表，空 = 通过。 */
    fun checkDescriptor(descriptor: Descriptor): List<String> = load {
        pluginContractCheckDescriptor(descriptor.toFfi())
    }

    private fun Descriptor.toFfi(): FfiPluginDescriptor = FfiPluginDescriptor(
        id = id,
        label = label,
        color = color,
        hosts = hosts,
        feedCount = feedCount.toUInt(),
        targets = targets,
        allTargets = allTargets.toUInt(),
        unavailableNote = unavailableNote,
        proxied = proxied,
        overseas = overseas,
        cookieDomain = cookieDomain,
        chromeImport = chromeImport?.let {
            FfiPluginChromeImport(
                domain = it.domain,
                suffixes = it.suffixes,
                exactHosts = it.exactHosts.toList(),
            )
        },
        emotes = emotes?.let { table ->
            FfiPluginEmotes(
                pack = table.pack,
                codes = table.codes.map { FfiPluginEmote(code = it.code, url = it.url) },
            )
        },
        search = search?.let {
            FfiPluginSearch(pageSize = it.pageSize, kinds = it.kinds, sorts = it.sorts)
        },
    )

    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }
}
