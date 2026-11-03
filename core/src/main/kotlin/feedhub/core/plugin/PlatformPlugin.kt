package feedhub.core.plugin

import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.ItemKind
import feedhub.core.LoginSpec
import feedhub.core.Platform
import feedhub.core.SearchCapability
import feedhub.core.Source
import feedhub.core.SourceContext
import feedhub.core.Span
import feedhub.core.UserRef

/**
 * 一个**平台插件**的描述：主程序（app / cli）关于这个平台需要知道的一切，都在这里，而不是散在 app 里按平台 `when`。
 *
 * 每个 `source-*` 模块里有且只有一份（顶层常量，例如 `source-bilibili` 模块里的 `BilibiliPlugin`），
 * 仓库根的 `plugins.toml` 列出启用哪些模块，构建时生成唯一的注册表（`feedhub.plugins.InstalledPlugins`）。
 * 怎么写一个新插件、每一项是什么意思，
 *
 * 这是**编译期**插件：插件需要的库在插件模块的 `build.gradle.kts` 里声明，装上插件就一起编进去；
 * 没有运行时加载，也没有"缺了依赖就降级"的代码。
 */
class PlatformPlugin(
    /** 平台标识 + 显示名，用 [Platform.define] 定义（id 定了就别改：存储、cli 参数、条目 key 都用它）。 */
    val platform: Platform,
    /** 品牌色，`0xAARRGGBB`（平台标签、色点、账号页的头像底色）。 */
    val color: Long,
    /**
     * 这个平台实际会去连的域名后缀（接口、图片、视频 CDN 都算），小写、不带点。
     * 图片加载器只有一个客户端，按域名认出"这张图是哪家的"才能用那家的代理（见 `platformHostTable`）。
     * **只列真的会请求的域名**；Source 的 [Source.hosts] 必须和这里一致（插件模块里有一条测试比对）。
     */
    val hosts: List<String>,
    /** 能力：有哪些流、评论能不能排序、有没有楼中楼 / 弹幕……主程序按能力画界面，不按平台名分支。 */
    val capabilities: PlatformCapabilities,
    /** 设置里几组平台集合的默认值（首页开不开、走不走代理、自动翻译不）。 */
    val defaults: PlatformDefaults = PlatformDefaults(),
    /** 登录：网页登录 / 粘贴 Cookie 用的 [LoginSpec]、从 Chrome 导入哪些 Cookie。 */
    val login: PlatformLogin,
    /** 界面上几处按平台不同的叫法（"UP主" / "想法" / "xx吧"）。 */
    val texts: PlatformTexts = PlatformTexts(),
    /** 境外平台：只走代理；安卓 debug 启动参数只跑境外平台时要求给代理。 */
    val overseas: Boolean = false,
    /** 能跑在哪些端。不在里面的端上，这个平台显示成灰的（附 [unavailableNote]），不构造 Source。 */
    val targets: Set<PluginTarget> = PluginTarget.entries.toSet(),
    /** 某个端跑不了时给用户看的一句话（设置页 / 账号页灰显时的原因）。 */
    val unavailableNote: String? = null,
    /** 随插件带的表情图（文字代码 → `feedhub-emoji:` 地址）；没有就是 null。图片放在插件模块的资源 `feedhub-emoji/<包>/` 下。 */
    val emotes: EmoteTable? = null,
    /**
     * 官方 App 的深链；默认空 = 只给网页链接。
     * 格式写在描述里，主程序只按模板拼地址、**不按平台 id 分支**。
     */
    val appLinks: AppLinks = AppLinks(),
    /** 这个平台的读接口要人机验证时，用户本人完成验证的那张本地页面（只有酷安）。没有就是 null。 */
    val captchaPage: CaptchaPage? = null,
    /** 工厂：拿到 [SourceContext] 建一个 [Source]。构造可以有副作用（写 `ctx.store`），所以只在真要用时才调。 */
    private val factory: (SourceContext) -> Source,
) {
    val id: String get() = platform.key
    val label: String get() = platform.label

    fun create(ctx: SourceContext): Source = factory(ctx)

    fun supports(target: PluginTarget): Boolean = target in targets

    override fun toString(): String = "PlatformPlugin(${platform.key})"
}

/** 插件能跑的端。 */
enum class PluginTarget(val key: String) {
    /** 桌面版 App 和 cli（JVM）。 */
    DESKTOP("desktop"),
    ANDROID("android"),
}

/**
 * 能力。新增一项能力 = 这里加一个带默认值的字段 + 主程序里按它分支（而不是按平台 id）。
 */
data class PlatformCapabilities(
    /** 有哪些流（推荐 / 关注）。[Source.feeds] 必须和它一致。 */
    val feeds: Set<FeedKind>,
    /** 评论能换排序（默认 / 热门 / 最新）。小红书的评论接口只有一种顺序。 */
    val commentSort: Boolean = true,
    /** 楼中楼能点开看。抖音的楼中楼接口被人机验证拒了，关着。 */
    val nestedReplies: Boolean = true,
    /** 视频有弹幕（`VideoSource.danmaku`）。弹幕的渲染在主程序里，插件只负责取。 */
    val danmaku: Boolean = false,
    /** 视频详情有"相关推荐"标签（详情还没回来时先按这个画标签，免得闪一下）。 */
    val relatedVideos: Boolean = false,
    /** 视频能在我们的播放器里放 → 进「只看视频」的请求名单。 */
    val playableVideo: Boolean = false,
    /** 这些流基本全是视频：「只看图文」时不请求（抖音 / YouTube 全部，B站只有推荐流）。 */
    val videoOnlyFeeds: Set<FeedKind> = emptySet(),
    /**
     * 平台侧搜索能力；null = 不支持搜索（老插件 / 还没接的平台）。
     *
     * 非 null 时 Source **必须**实现 [feedhub.core.SearchSource]，而且 `SearchSource.search` 和这里是**同一份**
     * （`PluginContract` 会比对）。主程序按它分支：搜索页只向声明了搜索能力的平台发请求。
     */
    val search: SearchCapability? = null,
)

/** 设置里平台集合的默认值。老存档里没有对应字段时也用这份。 */
data class PlatformDefaults(
    /** 首页 / 关注默认开。**新平台默认不开**：签名 / 接口在线验过再开。 */
    val feedOn: Boolean = false,
    /** 「走代理的平台」默认点亮（境外平台）。 */
    val proxied: Boolean = false,
    /** 「自动翻译的平台」默认点亮（外语内容为主的平台）。 */
    val autoTranslate: Boolean = false,
)

/** 登录相关。扫码 / 设备码登录由 Source 实现 `QrLoginSource` / `DeviceCodeLoginSource` 表示，这里不重复。 */
data class PlatformLogin(
    /** 网页登录（安卓 WebView）和粘贴 / 清除 Cookie 用的那一套。[Source.login] 必须和它一致。 */
    val web: LoginSpec,
    /** 桌面版"从 Chrome 导入 Cookie"收哪些 Cookie；null = 不支持。 */
    val chromeImport: CookieImportSpec? = null,
    /** cli `login` 时没有扫码登录的补一句提示（酷安："用 feedhub cookie import coolapk 粘贴 Cookie"）。 */
    val noQrHint: String? = null,
    /**
     * 能不能扫码登录：有就是 true，[Source] **必须**实现 `QrLoginSource`（`PluginContract` 会比对）。
     *
     * 账号页和 cli 仍然是按 Source 的接口判断（`source as? QrLoginSource`）——这一项是给**没有真的 Source**
     * 的地方用的：演示环境（`app/src/androidMain` 的 `DemoQrSource`）要靠它决定哪个平台显示「扫码登录」，
     * 插件自检也靠它守住"描述和实现一致"。
     */
    val qrLogin: Boolean = false,
)

/**
 * 从浏览器的 Cookie 库里收哪些 Cookie：按 Cookie 的 host（`.bilibili.com` / `www.bilibili.com` 这种）判断。
 *
 * @param domain 导入前先清掉的域（`FeedCookieJar.clearDomain`），一般是主域名。
 * @param suffixes 这些域名本身和它们的子域都收（`bilibili.com` 收 `.bilibili.com`、`www.bilibili.com`）。
 * @param exactHosts 只收这几个 host（百度只要 `.baidu.com` 和 `tieba.baidu.com`，网盘等子域不碰）。
 */
data class CookieImportSpec(
    val domain: String,
    val suffixes: List<String> = listOf(domain),
    val exactHosts: Set<String> = emptySet(),
) {
    fun matches(host: String): Boolean {
        if (host in exactHosts) return true
        val d = host.trimStart('.')
        return suffixes.any { s -> d == s || d.endsWith(".$s") }
    }
}

/** 界面上按平台不同的几处叫法。 */
data class PlatformTexts(
    /** 评论区里"楼主"标签的文字（小红书叫"作者"）。回答 / 文章一律是"作者"，不归这里管。 */
    val posterLabel: String = "楼主",
    /** 视频下面"楼主"标签的文字（B站"UP主"、知乎"作者"）；null = 和 [posterLabel] 一样。 */
    val videoPosterLabel: String? = null,
    /** 动态类条目叫什么（知乎叫"想法"）。 */
    val momentLabel: String = "动态",
    /** 圈子名的后缀（贴吧"吧"：`原神` → `原神吧`；本来就以它结尾的不再加）。 */
    val communitySuffix: String = "",
    /** 信息流卡片上圈子名放在作者前面（Reddit 的 `r/xxx` 比作者名更重要）。 */
    val communityFirst: Boolean = false,
    /** 关注管理页的平台标签（B站"B站 UP 主"）；null = "<平台名> 关注的吧"。 */
    val followTab: String? = null,
    /** 关注管理页"没有关注任何 ___。"里那个词（B站" UP 主"）；null = "吧"。 */
    val followNoun: String? = null,
)

/**
 * 随插件带的表情图（官方没有公开图片地址的那种，比如酷安）。
 *
 * @param pack 表情包名，也是资源目录名：图片在插件模块的 `src/main/resources/feedhub-emoji/<pack>/` 下，
 *   地址是 `feedhub-emoji:<pack>/<文件>`（[feedhub.core.BundledEmote]），由 app 的图片加载器按类路径资源读。
 * @param codes 文字代码 → 图片地址（演示 / 测试用；正文里怎么切由 Source 自己做）。
 * @param spans 一段文字切成文字 + 表情（演示用；和 Source 解析正文用的是同一个函数）。没有就是 null
 *   （知乎的表情走 `FeedItem.emotes` 那张表，不切 [Span.Emote]）。
 */
class EmoteTable(
    val pack: String,
    val codes: Map<String, String>,
    val spans: ((String) -> List<Span>)? = null,
)

/** 人机验证页：用户本人在里面完成验证，页面把结果交到 [submitUrl]（null = 显示出来让用户自己复制）。 */
fun interface CaptchaPage {
    fun html(captchaId: String, submitUrl: String?): String
}

/**
 * 官方 App 的深链模板。
 *
 * 格式写在这里（和颜色 / 名字同一处），主程序只按模板拼地址，**不按平台 id 分支**（`PluginBoundaryTest` 守着）。
 * 模板里的占位符：
 * - `{id}`：条目的平台 id（[FeedItem.id]，B站是 bvid / 动态 id_str，知乎是回答 id）；
 * - `{userId}`：作者的平台 id（[UserRef.id]）。
 *
 * 拼出来是 `http(s)` 的按"网页链接"打开（系统浏览器 / Custom Tabs），别的（`bilibili://…`、`zhihu://…`）
 * 交给官方 App；App 没装 / 认不了这个地址时前端退化成网页地址。
 *
 * **空着 = 这个平台没有拿得准的深链格式**：界面上只给"用浏览器打开"，不猜格式、不拿用户当小白鼠
 */
class AppLinks(
    /** 内容深链，按条目类型给模板（B站视频和动态的地址形状不一样）；这一类没有模板 = 只能用网页打开。 */
    val detail: Map<ItemKind, String> = emptyMap(),
    /** 作者主页深链；null = 作者只有网页地址。 */
    val user: String? = null,
) {
    /** [item] 这一类的深链；这类内容没有模板就是 null。 */
    fun detailLink(item: FeedItem): String? =
        detail[item.kind]?.let { expand(it, item.id, item.author?.id) }

    /** 作者主页的深链；没配就是 null。 */
    fun userLink(author: UserRef): String? = user?.let { expand(it, author.id, author.id) }

    override fun toString(): String = "AppLinks(detail=${detail.size} 类, user=${user != null})"

    companion object {
        /**
         * 把模板里的 `{id}` / `{userId}` 换成实际值。值是 null 时换成空串——拼出来的地址交给系统时
         * 由 `resolveActivity` 决定接不接得住（宁愿让官方 App 打开一个空页面，也不要拼出 `null` 字样）。
         */
        fun expand(template: String, id: String?, userId: String?): String =
            template.replace("{userId}", userId.orEmpty()).replace("{id}", id.orEmpty())
    }
}
