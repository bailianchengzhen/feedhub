package feedhub.core.plugin

import feedhub.core.Platform
import feedhub.core.SearchKind
import feedhub.core.SearchSort
import feedhub.core.SearchSource
import feedhub.core.Source
import feedhub.core.SourceContext
import feedhub.core.VideoSource
import feedhub.rust.RustPluginContract

/**
 * 插件描述的自检：每个插件模块的离线测试里调一次 [check]，返回的问题列表必须是空的。
 *
 * 只查"描述自己合不合理"和"描述和它建出来的 Source 对不对得上"，不发任何网络请求
 * （Source 用内存里的 [SourceContext] 构造；构造本身就不该联网）。
 *
 * 这个文件从 `:core` 搬到 `:rust-bridge`、**包名不变**（照 `MinIntervalInterceptor`
 * 的先例）：`:core` 编译期看不见 `:rust-bridge`，而调用方（`plugin-registry` 和各插件模块的测试）
 * 都在能看见它的模块里。剩下的 [check] 里那一半要 `create(ctx)` 造对象、比接口实现，是宿主动作。
 */
object PluginContract {
    /** 只看描述本身（不建 Source）。**判定和文案在 Rust**，这里只做数据投影。 */
    fun checkDescriptor(plugin: PlatformPlugin): List<String> =
        RustPluginContract.checkDescriptor(descriptorOf(plugin))

    /** 描述 + 用它建一个 Source 比对（[ctx] 默认是内存里的，不联网）。 */
    fun check(plugin: PlatformPlugin, ctx: SourceContext = SourceContext.inMemory()): List<String> {
        val problems = checkDescriptor(plugin).toMutableList()
        val source: Source = try {
            plugin.create(ctx)
        } catch (e: Exception) {
            problems += "工厂建 Source 失败：$e"
            return problems
        }
        if (source.platform != plugin.platform) problems += "Source.platform（${source.platform}）和描述（${plugin.platform}）不一致"
        if (source.feeds != plugin.capabilities.feeds) problems += "Source.feeds ${source.feeds} 和描述 ${plugin.capabilities.feeds} 不一致"
        if (source.hosts != plugin.hosts) problems += "Source.hosts ${source.hosts} 和描述 ${plugin.hosts} 不一致"
        if (source.login != plugin.login.web) problems += "Source.login 和描述的 login.web 不一致"
        val caps = plugin.capabilities
        if ((caps.danmaku || caps.playableVideo || caps.relatedVideos) && source !is VideoSource) {
            problems += "声明了视频相关的能力（danmaku / playableVideo / relatedVideos），但 Source 没实现 VideoSource"
        }
        // 扫码登录：描述里声明了就必须真的实现 QrLoginSource，反过来也一样（演示环境按这一项决定显示不显示按钮）
        if (plugin.login.qrLogin && source !is feedhub.core.QrLoginSource) {
            problems += "login.qrLogin 是 true，但 Source 没实现 QrLoginSource"
        }
        if (!plugin.login.qrLogin && source is feedhub.core.QrLoginSource) {
            problems += "Source 实现了 QrLoginSource，但描述里 login.qrLogin 不是 true"
        }
        // 搜索：描述和能力必须成对出现，而且是同一份（照视频能力那条的写法）
        if (caps.search != null && source !is SearchSource) problems += "声明了搜索能力，但 Source 没实现 SearchSource"
        if (source is SearchSource && source.search != caps.search) {
            problems += "Source.search ${source.search} 和描述里的 capabilities.search ${caps.search} 不一致"
        }
        return problems
    }

    /** `PlatformPlugin` 的纯数据 → 过边界的投影（枚举换名字：`SearchKind` / `SearchSort` / `PluginTarget`）。 */
    private fun descriptorOf(plugin: PlatformPlugin): RustPluginContract.Descriptor =
        RustPluginContract.Descriptor(
            id = plugin.platform.key,
            label = plugin.label,
            color = plugin.color,
            hosts = plugin.hosts,
            feedCount = plugin.capabilities.feeds.size,
            targets = plugin.targets.map { it.key },
            allTargets = PluginTarget.entries.size,
            unavailableNote = plugin.unavailableNote,
            proxied = plugin.defaults.proxied,
            overseas = plugin.overseas,
            cookieDomain = plugin.login.web.cookieDomain,
            chromeImport = plugin.login.chromeImport?.let {
                RustPluginContract.ChromeImport(
                    domain = it.domain,
                    suffixes = it.suffixes,
                    exactHosts = it.exactHosts,
                )
            },
            emotes = plugin.emotes?.let { table ->
                RustPluginContract.Emotes(
                    pack = table.pack,
                    // 插入顺序就是表里的顺序（Rust 按这个顺序报问题）
                    codes = table.codes.map { (code, url) -> RustPluginContract.Emote(code, url) },
                )
            },
            search = plugin.capabilities.search?.let {
                RustPluginContract.Search(
                    pageSize = it.pageSize,
                    kinds = it.kinds.map(SearchKind::name),
                    sorts = it.sorts.map(SearchSort::name),
                )
            },
        )
}
