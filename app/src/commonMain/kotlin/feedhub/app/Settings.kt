package feedhub.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.feed.FeedOrder
import feedhub.app.ui.DarkMode
import feedhub.app.video.DanmakuSettings
import feedhub.core.FeedJson
import feedhub.core.FeedKind
import feedhub.brain.ContentFilter
import feedhub.core.KeyValueStore
import feedhub.core.Platform
import feedhub.core.plugin.PlatformCatalog
import feedhub.core.plugin.PlatformPlugin
import feedhub.plugins.InstalledPlugins
import feedhub.core.ProxyChoice
import feedhub.core.ProxyConfig
import feedhub.core.ProxyMode
import feedhub.core.ProxyProvider
import feedhub.proxy.ProxyRules
import feedhub.settings.RustSettings
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/**
 * 设置改动多久没有新的改动才写回存储（毫秒）。设置页是边打字边存的，见 [SettingsStore.update]。
 *
 * 这个数在 Rust（`feedhub.settings.RustSettings.saveDebounceMs` / `settings_save_debounce_ms`）：
 * 防抖的**时机**仍由 Kotlin 的协程定时器决定，两边的**时长**只有一份。
 */
val SETTINGS_SAVE_DEBOUNCE_MS: Long get() = RustSettings.saveDebounceMs

enum class ContentType { MIXED, VIDEO, TEXT }

/**
 * **默认开着**的平台（首页 / 关注各一份，见 [AppSettings.homePlatforms]）：装了的插件里
 * 描述写了 `defaults.feedOn = true` 的那些。
 *
 * 新插件的 `feedOn` 默认是 false：它**不会**因为"装上了"就出现在首页 / 关注页——签名 / 接口在线验过之前
 * 就先打真实请求，正是当年把抖音从默认里拿掉的原因。各平台为什么默认开，写在各插件的
 * `<平台>PluginTest` 里。
 */
val DEFAULT_FEED_PLATFORMS: Set<Platform> get() = InstalledPlugins.catalog.defaultFeedPlatforms

/**
 * **默认自动翻译**的平台（[AppSettings.translationPlatforms]）：描述写了 `defaults.autoTranslate` 的（境外四家）。
 * 没有需要翻译的平台就完全不启动；国内平台默认不点亮。
 */
val DEFAULT_TRANSLATION_PLATFORMS: Set<Platform> get() = InstalledPlugins.catalog.defaultTranslationPlatforms

/** 默认**走代理**的平台（[AppSettings.proxiedPlatforms]）：描述写了 `defaults.proxied` 的（境外平台）。 */
val DEFAULT_PROXIED_PLATFORMS: Set<Platform> get() = InstalledPlugins.catalog.defaultProxiedPlatforms

/**
 * 这次要请求哪些平台：设置里勾上的 [enabled] 里，内容类型用得上的那些（按插件描述的能力判断）：
 * - 「只看视频」：描述里 `capabilities.playableVideo` 的平台（视频能在我们的播放器里放的；酷安的托管视频有防盗链签名，不算）；
 * - 「只看图文」：不请求 `capabilities.videoOnlyFeeds` 里有这个流的平台（抖音 / YouTube 全是视频，B站只有推荐流全是视频）。
 * 没装插件的平台（老设置里留下的 id）一律不请求。只是名单，Source 支不支持这个流由调用方再筛（见 `AppGraph.feedSources`）。
 * 抽成纯函数是为了能离线测。
 */
fun requestedPlatforms(
    kind: FeedKind,
    contentType: ContentType,
    enabled: Set<Platform>,
    platforms: PlatformCatalog = InstalledPlugins.catalog,
): Set<Platform> {
    val specs = platforms.plugins.map { p ->
        RustSettings.PlatformRequestSpec(
            id = p.id,
            playableVideo = p.capabilities.playableVideo,
            videoOnlyFeeds = p.capabilities.videoOnlyFeeds,
        )
    }
    val rustContentType = when (contentType) {
        ContentType.MIXED -> RustSettings.ContentType.MIXED
        ContentType.VIDEO -> RustSettings.ContentType.VIDEO
        ContentType.TEXT -> RustSettings.ContentType.TEXT
    }
    return RustSettings.requestedPlatforms(kind, rustContentType, enabled, specs)
}

enum class DetailMode { TWO_PANE, FULLSCREEN }

/**
 * 选流 / 封顶 / 倍速
 * 全是 Rust 在判。`DefaultQuality` 的**枚举顺序就是"从高到低"**（`limitQuality` 用 ordinal 比）。
 */
typealias CodecPreference = feedhub.player.CodecPreference

typealias DefaultQuality = feedhub.player.DefaultQuality

/**
 * 本地翻译默认的 GGUF 模型（HY-MT1.5-1.8B Q4_K_M）。桌面打包时构建脚本把仓库 `deps/translate/` 下的那份
 * 写进系统属性 `feedhub.translate.model`（见 `app/build.gradle.kts`）；没有就空着，在设置里自己选。
 */
val DEFAULT_TRANSLATION_MODEL: String = System.getProperty("feedhub.translate.model").orEmpty()

/** 本地翻译默认的 llama-server（llama.cpp Vulkan 构建）。来源同上，系统属性 `feedhub.translate.server`。 */
val DEFAULT_LLAMA_SERVER: String = System.getProperty("feedhub.translate.server").orEmpty()

/**
 * 安卓上 `llama-server` **强制**用哪一档后端。
 *
 * **自动（`null`，默认）才是正常状态**：Rust 的 `translate::device` 按
 * **NPU（Hexagon HTP）→ GPU（Adreno OpenCL）→ NPU+GPU 分层 → CPU** 的顺序试，
 * 每档要过"进程活着 + `/health` 通 + 真翻一句不像坏"，失败就降一档并把结果记进
 * `state.json` 的 `translate.backend.v1`（带 App 版本，升级后重新从 NPU 试）。
 *
 * 这个枚举是**排错 / 测速用的覆盖**（`tools/translate/phone_bench.sh` 四档要逐档量）：
 * - [NPU]：只试 Hexagon（`--device HTP0`）；
 * - [GPU]：只试 Adreno（`--device GPUOpenCL`）；
 * - [NPU_GPU]：只试分层（`--device GPUOpenCL,HTP0 --split-mode layer`）；
 * - [CPU]：只试纯 CPU（`-ngl 0`），**不碰任何加速设备**。
 *
 * 强制的那一档起不来就是真失败（报错给界面），**不会悄悄降级**——不然"我强制了 CPU"变成一句谎话。
 * 桌面上这个字段不生效（桌面永远走 Vulkan AMD 核显 那条红线）。
 */
@Serializable
enum class TranslationAndroidBackend {
    NPU,
    GPU,
    NPU_GPU,

    /**
     * 只试纯 CPU（`-ngl 0`），**一个加速设备都不碰**。
     *
     * 存档里写成 `"CPU_ONLY"`，**不是** `"CPU"`——老版本那个必填字段的名字就是 `CPU`，
     * 而它是产品默认值不是用户选择（见 [AppSettings.translationAndroidBackend] 的迁移口径）。
     * 两个名字要是撞在一起，"强制 CPU" 每次读档都会被当成老值迁回自动，`phone_bench.sh` 的 CPU 那档
     * 就会静默变成"从 NPU 试起"。所以强制档单独起一个名字。
     */
    CPU_ONLY,
}

/**
 * 首页 / 关注的列数怎么定：
 * [AUTO] = 按窗口宽度自动（`feedhub.app.feed.autoColumns`），[FIXED] = 固定 [AppSettings.homeColumns] 列。
 */
@Serializable
enum class HomeColumnsMode { AUTO, FIXED }

/** 老版本首页列数滑轨的默认值；老存档里等于它的当"自动"（用户没主动拖过），见 [decodeSettings]。 */
const val LEGACY_DEFAULT_HOME_COLUMNS = 4

/** 固定首页列数的可选范围。 */
const val MIN_HOME_COLUMNS = 2
const val MAX_HOME_COLUMNS = 6

/**
 * 「每平台一批最多」的候选值：0 = 不限。
 * 默认 [DEFAULT_MAX_PER_PLATFORM] = 15。
 */
val MAX_PER_PLATFORM_CHOICES: List<Int> = listOf(8, 12, 15, 0)

/** 「每平台一批最多」的默认值。 */
const val DEFAULT_MAX_PER_PLATFORM: Int = 15

/** 设置页的全部选项。加字段要给默认值，旧的存档才读得回来。 */
@OptIn(ExperimentalSerializationApi::class)
@Serializable
data class AppSettings(
    val contentType: ContentType = ContentType.MIXED,
    /** 首页请求哪些平台。默认见 [DEFAULT_FEED_PLATFORMS]；老存档没有这个字段时也走它。 */
    val homePlatforms: Set<Platform> = DEFAULT_FEED_PLATFORMS,
    val followingPlatforms: Set<Platform> = DEFAULT_FEED_PLATFORMS,
    /**
     * 首页列数：默认 [HomeColumnsMode.AUTO]。
     *
     * **[EncodeDefault]：这个字段无论是不是默认值都写进存档。** 它是默认值 [HomeColumnsMode.AUTO]，
     * 不写的话 JSON 里就没有它，和"老存档没有这个字段"分不开——那既没法把老存档的默认 4 列迁成自动，
     * 也没法保住用户新选的"固定 4 列"（见 [decodeSettings] 的迁移）。
     */
    @EncodeDefault
    val homeColumnsMode: HomeColumnsMode = HomeColumnsMode.AUTO,
    /** [HomeColumnsMode.FIXED] 时用几列（夹在 [MIN_HOME_COLUMNS]～[MAX_HOME_COLUMNS]）；"自动"时这个值不用，但留着下次切回固定还能用。 */
    val homeColumns: Int = LEGACY_DEFAULT_HOME_COLUMNS,
    /**
     * **每个平台一批最多几条**。
     *
     * 只做**截断**：每个平台取它自己推荐顺序里的前 N 条，最后仍整批按发布时间倒序（用户硬约束：不重排）。
     * 快照实测小红书一批占 43/120，封顶 15 只削掉最肥的三家。改小会让"加载更多"更频繁
     * （每次都是每个平台一个请求），所以设置页那排只给到 8。
     * 老存档没有这个字段 → 默认 15；手改成负数按 0（不限）读回来，见 [decodeSettings]。
     */
    val maxPerPlatform: Int = DEFAULT_MAX_PER_PLATFORM,
    /**
     * **首页排序**：[FeedOrder.INTERLEAVE]（默认，各平台穿插、
     * 每个平台保持它自己推荐流的顺序）/ [FeedOrder.TIME]。关注页**不受它影响**（关注流本身就是时间线，永远按时间）。
     *
     * 老存档没有这个字段 → 默认穿插；手改成认不出的值由 `FeedJson` 的 `coerceInputValues` 收回默认。
     * 改了它会刷新一次首页（顺序只对新的一批生效，旧内容留在下面）。见 [feedhub.app.feed.BatchOrders]。
     */
    val homeOrder: FeedOrder = FeedOrder.INTERLEAVE,
    val detailMode: DetailMode = DetailMode.TWO_PANE,
    /** 双栏时左栏（信息流）占内容区宽度的比例，拖中间的分隔线调，松手时存。默认一半：全宽 4 列时左栏正好 2 列。 */
    val detailSplit: Float = 0.5f,
    val originalImages: Boolean = true,
    val darkMode: DarkMode = DarkMode.SYSTEM,
    /**
     * **「自动旋转」（手机）**：**默认关**。
     *
     * 关着时手机上整个 App 锁竖屏（横屏视频点全屏照样转横屏，退出全屏回竖屏）；开着就跟系统的自动旋转走
     * （= 加这个开关之前的行为）。平板（最短边 ≥ 600dp）不管这个开关，始终跟系统；桌面没有这一项。
     * 方向的计算在 `feedhub.app.video.AppOrientation`（安卓）。老存档没有这个字段 → false。
     */
    val autoRotate: Boolean = false,
    val codec: CodecPreference = CodecPreference.AUTO,
    val quality: DefaultQuality = DefaultQuality.HIGHEST,
    /** 弹幕默认开启；播放器里的开关按钮只管这一次，不改它。 */
    val danmakuOn: Boolean = true,
    /** 弹幕的样式、排布、屏蔽，播放器里的弹幕设置面板和设置页是同一份，改了立刻存。旧存档里的"弹幕密度"见 [decodeSettings]。 */
    val danmaku: DanmakuSettings = DanmakuSettings(),
    val historyLimitOn: Boolean = false,
    val historyDays: Int = 90,
    /**
     * 记不记本地行为数据（曝光 / 停留，见 [feedhub.app.feed.ExposureTracker]）。
     * **默认开**：以后做本地个性化要靠这份数据，文档里写明"越早记录，以后能用的数据越多"。
     * 关掉后不再记录（正在跟踪的那次丢掉），已经记下的用设置页的"清除行为数据"删；保留天数见
     * [feedhub.app.library.EXPOSURE_RETENTION_DAYS]。
     */
    val recordBehavior: Boolean = true,
    /**
     * **「用 NPU 算」（本地向量，实验）**：**默认关**。
     *
     * 打开后本地向量的那些批尽量走手机的 NPU（QNN / Hexagon HTP）；第一次要编图（只在前台、并且充电或空闲时），
     * 准备期间要算的批**先等着**（界面显示"NPU 编译中"）。
     *
     * 所以这个开关在那个包里就是
     * 本地向量的**总开关**（关着 = 不算、不存；NPU 用不了 = 不可用、**不回退 CPU**，设置页显示原因）。
     * 普通包里仍然是"只管在哪儿算"：真失败才回退 CPU。**开关切换不用重启 App**（`AppGraph` 用 `snapshotFlow` 盯它）。
     *
     * 只有带了 `-Pfeedhub.embedNpu=true` 的包才有 NPU 模型；没有模型时设置页那个开关灰显并说明
     * （`feedhub.app.embed.EmbeddingBackendControl.npuBundled` = false）。老存档没有这个字段 → false。
     *
     * **只存本机，不进跨设备同步**（同步只传收藏 / 历史，见 `feedhub.app.sync.LibrarySync`）。
     */
    val embedNpu: Boolean = false,
    // 现在包里带几张图就全编、**按批挑最短够用的那张**（短文本不会被长文本拖进长图），没有"选一张"这件事了；
    // 选项删掉，老存档里的这个字段会被忽略（`ignoreUnknownKeys`），不会报错。
    /**
     * **「封面向量（实验）」**：**默认开**。
     *
     * 打开后：有封面 / 配图的视频和短图文（B站 / 抖音 / 小红书 / YouTube / TikTok / X / Reddit 带图的帖子，
     * 见 `feedhub.app.embed.CoverRules.platformTable`）会**额外**算一份"文字 + 封面"的向量（新的 model id
     * `…-htp-l128-img8x8`），和纯文字那份**并存**。三条纪律：
     * - 前台、纯文字闲着就跑，一次一个"封面窗口"（32 条），排在纯文字补算后面；
     * - 封面图**直接用首页已经缓存的那张缩略图**，缓存里没有才在**不按流量计费**的网络上取一次；
     * - **权重仍然是 0**：只算、只存、只导出，首页顺序一条都不变。
     *
     * 关掉 = 一条都不算（已经在库里的照旧保留，按 30 天保留期过期）。老存档没有这个字段 → true。
     */
    val coverVectors: Boolean = true,
    /**
     * **「性能记录」开关**：App 自己每 10 秒往
     * `/sdcard/Android/data/feedhub.app/files/perf/perf-YYYYMMDD.jsonl` 写一行性能数据（前后台、帧耗时、电流、
     * 本进程 / 各线程 CPU、网络、温度、内存 GC、播放状态 + 事件行），只在前台记、只留 7 天。
     * 用户在设置 →「进阶设置」里开关；**只存本机，不进跨设备同步**（同步只传收藏 / 历史，见
     * `feedhub.app.sync.LibrarySync`），关掉时记录器一个定时器都不起。
     */
    val perfLog: Boolean = false,
    /**
     * **「对外接口（agent）」开关**：**默认关**。
     *
     * 开着时**桌面 App 自己**在本机 unix socket（`$XDG_RUNTIME_DIR/feedhub/api.sock`，没有就是
     * `~/.cache/feedhub/api.sock`）上提供只读 HTTP；关掉、或者 App 没开，就**根本没有这个接口**
     * （不起独立程序、不自动拉起 App）。接口**没有自己的状态**：请求全走这个 App 进程里的 Source / Cookie /
     * 设置 / 资料库（见 `AppGraph.appApi`）。
     *
     * 只有桌面组装有这一档（安卓没有这个接口，设置页也不显示那一页）；老存档没有这个字段时读出来是关。
     */
    val appApi: Boolean = false,
    /** 界面缩放（桌面版），0 = 自动（跟系统）；1.5 = 150%。 */
    val uiScale: Float = 0f,
    /**
     * 网络：**全局**代理配置——一个代理地址，
     * 没有"每平台四档覆盖"。默认 [ProxyConfig.Mode.SYSTEM]（= 加这个功能之前的行为）；
     * 光有它不走代理，还得看 [proxiedPlatforms]。
     */
    val proxy: ProxyConfig = ProxyConfig(),
    /**
     * **走代理**的平台。
     *
     * 国内平台默认**都不点亮**（= 直连，"国内默认不走代理"）；境外平台（X、YouTube、Reddit、TikTok）
     * **接入时默认点亮**。
     * 老存档没有 `proxy` / `proxiedPlatforms` 字段时，读出来就是这里这份默认值，所以升级后境外平台也是点亮的
     * （它们不加代理本来就连不上，默认直连等于用不了）。代理是纯本地设置，默认点亮不会凭空发请求。
     *
     * 这个集合只表示
     * "跟随全局时走不走全局代理"（[ProxyChoice.Follow] / [ProxyChoice.Direct] 就存在这里，见 [withProxyChoice]）。
     */
    val proxiedPlatforms: Set<Platform> = DEFAULT_PROXIED_PLATFORMS,
    /**
     * **按平台指定代理**：这个平台单独怎么连。
     *
     * 键是平台，值是 [ProxyChoice]：没存过 = 跟随全局（默认，= 加这个功能之前的行为，也就是看
     * [proxiedPlatforms] + 全局 [proxy]）；存了 [ProxyChoice.Custom] 就走那个代理（**fail-closed**：
     * 地址没填全时这个平台一个请求都不发，绝不回退直连）；[ProxyChoice.Direct] 等价于把平台从
     * [proxiedPlatforms] 里拿掉（[withProxyChoice] 会一起维护）。境外平台**没有**直连这一档。
     *
     * 这里只存"和默认不一样"的平台：设置页改回「跟随全局」就把这一项删掉（见 [withProxyChoice]）。
     */
    val platformProxy: Map<Platform, ProxyChoice> = emptyMap(),
    /**
     * **「代理不可用时禁止直连」**，默认**开**。
     *
     * - **开**（默认 = 加这个开关之前的行为）：点亮的平台，如果选中的代理**用不了**（内置核心没在跑、
     *   「指定」的地址 / 端口没填全），请求**直接失败**，一个都不发；
     * - **关**：上面这些情况下点亮的平台**改为直连**，不报错（界面上那条提示改成说明"现在直连"，
     *   日志里记一行"出口锁关闭，直连"，都不含地址）。
     *
     * 为什么要留这一档关的余地：用户的一部分安卓设备装了**透明代理模块**（系统层接管流量），
     * 那些设备上直连本来就会被模块接管，"代理用不了就一律不发请求"反而碍事。
     *
     * 「不用」和「系统」两档**不受影响**（它们不是"用不了"，见 `feedhub.proxy.ProxyRules.unavailable`）。
     * 老存档没有这个字段时读出来就是默认的 true（fail-closed）。
     */
    val proxyExitLock: Boolean = true,
    /**
     * 本地翻译总开关。**默认关**：翻译模型常驻约 1.2GB 内存、第一次加载要几秒；用户去设置里开。
     * 开之后只翻"不是目标语言"的标题 / 摘要 / 正文 / 评论，见 [feedhub.app.translate.TranslationStore]。
     */
    val translationOn: Boolean = false,
    /**
     * **自动翻译**点亮的平台。
     *
     * 只有点亮平台的内容才会自动翻；没点亮的平台一律不自动翻，但正文下面那一行手动「翻译」照常能用。
     * **手机上默认一个都不点亮**，见 [SettingsStore] 的 `translationDefault`。
     * 判定在语言识别**之前**（见 `TranslationStore.shouldAutoTranslate`），所以国内平台的内容永远碰不到
     * lingua，也不会去启动 llama-server。**默认点亮 X / YouTube / Reddit / TikTok**（[DEFAULT_TRANSLATION_PLATFORMS]）；
     * 老存档没有这个字段时读出来就是这份默认值。
     */
    val translationPlatforms: Set<Platform> = DEFAULT_TRANSLATION_PLATFORMS,
    /** 翻译目标语言。界面是简体中文，先只做中文；以后加应用语言设置再扩展。 */
    val translationTarget: String = "中文",
    /** llama.cpp 的 GGUF 翻译模型路径（默认指向 HY-MT1.5-1.8B Q4_K_M）。 */
    val translationModelPath: String = DEFAULT_TRANSLATION_MODEL,
    /** llama-server 可执行文件路径。 */
    val translationServerPath: String = DEFAULT_LLAMA_SERVER,
    /**
     * **安卓**上**强制**哪一档 `llama-server` 后端；`null` = **自动降级**（默认）。
     *
     * 自动 = NPU → GPU → NPU+GPU → CPU，失败降一档并记住（见 [TranslationAndroidBackend]）。
     * 这一栏只给 `tools/translate/phone_bench.sh` 和排错用：正常用户不该改它。
     *
     * **老存档迁移**（口径写在 `settings.rs` 的 `normalize` 里，那里是唯一的真相）：
     * 老版本这个字段是**必填**的 `enum class { CPU, OPENCL }`、默认 `CPU`——那个 `CPU`
     * 是"这一版还没有自动降级"的**产品默认值**，不是用户选的。所以读到老值一律**读成 `null`
     * （自动）**，不把它当成"用户选了 CPU"；否则所有人升级完都被钉死在 CPU 上。
     * 于是老存档里 `"CPU"` / `"OPENCL"` 两个值都变成自动。新值 `"NPU"` / `"GPU"` / `"NPU_GPU"` / `"CPU_ONLY"` 只有用户自己写了才认
     * （强制 CPU 故意用 `CPU_ONLY`，和老值 `CPU` 分开——不然每次读档都会被迁回自动）。
     *
     * 桌面上这个字段不生效（桌面固定走 Vulkan AMD 核显 名字匹配）。
     */
    val translationAndroidBackend: TranslationAndroidBackend? = null,
    /**
     * **屏蔽词**：**一直生效**。
     * 标题 / 摘要 / 转发原文里含这些词的整条不进首页 / 关注（整条隐藏，不打码）。
     *
     * **默认空**：涉政词表之类的东西不写死在代码里，
     * 读存档时按 [ContentFilter.normalizeRules] 收干净（去空白、去重、最多 [ContentFilter.MAX_RULES] 条）。
     */
    val blockWords: List<String> = emptyList(),
    /**
     * **外出模式**开关：打开后 [outingKeywords] / [outingCommunities] 生效，命中就整条隐藏；
     * 关掉后"我们自己判出来的"那部分照常显示（不删收藏 / 历史，只是那时不显示）。
     * 平台标成 18+ 的（[FeedItem.adult]）不受这个开关影响，一直隐藏。
     */
    val outingMode: Boolean = false,
    /** 外出模式的关键词，**只在外出模式开着时**生效。默认空。 */
    val outingKeywords: List<String> = emptyList(),
    /**
     * 外出模式的社区名单，**只在外出模式开着时**生效，默认空。
     *
     * 各平台填什么看 `FeedItem.community` 里现成有的字段：贴吧吧名（"原神"）、Reddit 版块（"r/China_irl"）、
     * 知乎问题标题、酷安话题、抖音 / TikTok 话题（第一个 hashtag）、小黑盒社区。B站只有动态的话题有值，
     * 视频 / 专栏没有分区字段。**全名相等**匹配，不分大小写 / 全半角。
     */
    val outingCommunities: List<String> = emptyList(),
    /**
     * 存这份设置时**已经见过**的平台。读设置时，装了但不在这里的插件是**新装的**：
     * 按它描述里的默认值把它加进首页 / 关注 / 走代理 / 自动翻译那几组（见 [decodeSettings]）。
     * 不然一个新装的境外平台，在老用户这里不在「走代理的平台」里，就会直连。
     *
     * 拆掉的插件不从这里删：以后再装回来，用的是用户原来的选择（那几组集合里它的 id 也一直留着，
     * 只是插件不在时界面不显示、也不请求）。
     *
     * **总是写进存档**（[EncodeDefault]）：`FeedJson` 不写等于默认值的字段，而这个字段的默认值就是"装了的全部"——
     * 省掉的话下次读成"老存档"（按老的 11 个算），用户关掉的新插件会被当成新装的重新打开。
     */
    @EncodeDefault(EncodeDefault.Mode.ALWAYS)
    val knownPlatforms: Set<Platform> = InstalledPlugins.catalog.platforms.toSet(),
)

/**
 * 某一组插件下的默认设置（测试 / 演示用自己的插件时用它；正式的 [AppSettings] 默认值就是装好的那组插件的）。
 *
 * **规则在 Rust**：Rust 给出几组平台集合 + 两个 `@EncodeDefault` 字段的 JSON，
 * Kotlin 用 [AppSettings] 的数据类默认值补齐其余字段——`AppSettings` 的默认值是那些数的**唯一**真相，
 * 不在 Rust 里抄第二份。
 *
 * [translationDefault] 不为 null 时用它当「自动翻译的平台」的默认值。做法是把它折进交给 Rust 的
 * 插件描述（`autoTranslate`），所以默认值和"新装的插件自动点亮"两条规则都在 Rust 里按它算。
 */
fun defaultSettings(platforms: PlatformCatalog, translationDefault: Set<Platform>? = null): AppSettings =
    decodeSettings(RustSettings.defaults(platforms.toRustSpecs(translationDefault)), platforms, translationDefault)

/**
 * 装着的插件 → 过边界那一份"设置用得上的几项"（顺序 = 界面顺序）。
 *
 * [translationDefault] 不为 null 时，`autoTranslate` 按"在不在这个集合里"给，而不是插件自己的默认。
 */
internal fun PlatformCatalog.toRustSpecs(translationDefault: Set<Platform>? = null): List<RustSettings.PluginSpec> = plugins.map { p ->
    RustSettings.PluginSpec(
        id = p.id,
        feedOn = p.defaults.feedOn,
        proxied = p.defaults.proxied,
        autoTranslate = translationDefault?.let { p.platform in it } ?: p.defaults.autoTranslate,
        overseas = p.overseas,
    )
}

/**
 * 这次内容过滤用的规则。
 * 信息流每一批现读它（`FeedState` 拿的是 `{ settings.value.contentFilter }`），所以设置页改了立刻生效。
 * 只搬字段、不做判断：匹配规则全在 core 的 [feedhub.brain.ContentFilter] 里。
 */
val AppSettings.contentFilter: ContentFilter.Rules
    get() = ContentFilter.Rules(
        blockWords = blockWords,
        outingMode = outingMode,
        outingKeywords = outingKeywords,
        outingCommunities = outingCommunities,
    )

/**
 * 组装里没接「内置代理核心」时的替身：选了「内置核心」就 fail-closed，
 * 走代理的平台请求直接失败（不会悄悄直连）。
 */
val NO_BUILTIN_CORE: ProxyMode = ProxyMode.BuiltinCore(0, "内置核心没有启动")

/**
 * **这个平台单独选的那一档**：设置页每平台那一行、[proxyFor] 和 cli 都读它。
 *
 * - [ProxyChoice.Custom]（存过）→ 走那个代理；
 * - 境外平台（插件描述 `overseas`）→ 永远 [ProxyChoice.Follow]（**红线：境外平台没有直连**；
 *   老存档里被关掉的境外平台也按这个办，[decodeSettings] 还会把它点回「走代理的平台」里）；
 * - 其余情况：在 [AppSettings.proxiedPlatforms] 里 → [ProxyChoice.Follow]（= 用全局代理），不在 → [ProxyChoice.Direct]。
 *
 * 所以每平台那一行显示的就是**现在真实的样子**：境外平台默认「跟随全局」，国内平台默认「直连」。
 */
fun AppSettings.proxyChoiceFor(platform: Platform, platforms: PlatformCatalog = InstalledPlugins.catalog): ProxyChoice =
    RustSettings.proxyChoiceFor(
        platform = platform,
        platformProxy = platformProxy,
        proxiedPlatforms = proxiedPlatforms,
        overseas = platforms[platform]?.overseas == true,
    )

/**
 * 改一个平台的代理档（设置页用）。三档都写回 [AppSettings.proxiedPlatforms]，让两个字段说的是同一件事
 * （存下来的只有 Custom；Follow / Direct 就是集合的亮 / 不亮）：
 *
 * - [ProxyChoice.Follow] → 点亮（用全局代理）、删掉单独指定的那一项；
 * - [ProxyChoice.Direct] → 不点亮、删掉那一项。**境外平台没有这一档**：传进来也退回 [ProxyChoice.Follow]（红线）；
 * - [ProxyChoice.Custom] → 存下来并点亮（这样再改回「跟随全局」时还是走全局代理，不是悄悄直连）。
 *
 * 没填全的 [ProxyChoice.Custom] 照样存（边打字边存）：这个平台的请求会直接失败，绝不回退直连。
 */
fun AppSettings.withProxyChoice(
    platform: Platform,
    choice: ProxyChoice,
    platforms: PlatformCatalog = InstalledPlugins.catalog,
): AppSettings {
    val (nextProxy, nextProxied) = RustSettings.withProxyChoice(
        platform = platform,
        choice = choice,
        overseas = platforms[platform]?.overseas == true,
        platformProxy = platformProxy,
        proxiedPlatforms = proxiedPlatforms,
    )
    return copy(
        platformProxy = nextProxy,
        proxiedPlatforms = nextProxied,
    )
}

/**
 * 这个平台这次该走什么代理：
 * 1. [proxyChoiceFor] 给的是单独指定 → 就用它（地址没填全 → 这个平台直接失败，**不回退直连**）；
 * 2. 「直连」→ 直连（境外平台没有这一档，按跟随全局办）；
 * 3. 「跟随全局」→ 全局 [AppSettings.proxy]（[proxyChoiceFor] 已经把"在不在
 *    [AppSettings.proxiedPlatforms] 里"折进这一档了：不在集合里就是 [ProxyChoice.Direct]）；
 * 4. 「指定」但地址 / 端口没填全 → 一个 `valid == false` 的 [ProxyMode.Custom]，请求**直接失败**
 * ，不会悄悄退回系统代理；
 * 5. 「内置核心」时用 [builtin]（= `ProxyCore.proxyMode()`）：核心在跑给它本机的 SOCKS5 端口，
 *    没在跑给 `valid == false` 的替身，同样直接失败（绝不悄悄直连）；
 * 6. **[AppSettings.proxyExitLock] 关着**时，第 4 / 5 条那种"全局代理用不了"的情况**改为直连**
 *    （不报错、不算 fail-open：用户主动关掉了出口锁，见那个字段的 KDoc）。**第 1 条不受它影响**。
 *
 * 判定本身在 Rust（`RustSettings.proxyFor`）。
 */
fun AppSettings.proxyFor(
    platform: Platform,
    builtin: ProxyMode = NO_BUILTIN_CORE,
    platforms: PlatformCatalog = InstalledPlugins.catalog,
): ProxyMode = RustSettings.proxyFor(
    platform = platform,
    platformProxy = platformProxy,
    proxiedPlatforms = proxiedPlatforms,
    overseas = platforms[platform]?.overseas == true,
    global = proxy.toProxyMode(builtin),
    exitLock = proxyExitLock,
)

/**
 * 因为这个平台"全局代理用不了、出口锁又关着"而**改成直连**时，给用户 / 日志看的一句话（不含地址）；
 * 没有这种情况就是 null。界面上的提示和 [SettingsStore.proxyProvider] 的日志都用它，
 * 保证两处说的是同一件事。**单独指定了代理的平台永远是 null**（它 fail-closed，不回退）。
 */
fun AppSettings.directFallbackReason(
    platform: Platform,
    builtin: ProxyMode = NO_BUILTIN_CORE,
    platforms: PlatformCatalog = InstalledPlugins.catalog,
): String? = RustSettings.directFallbackReason(
    platform = platform,
    platformProxy = platformProxy,
    proxiedPlatforms = proxiedPlatforms,
    overseas = platforms[platform]?.overseas == true,
    global = proxy.toProxyMode(builtin),
    exitLock = proxyExitLock,
)

/** 设置的读写：存在 [KeyValueStore] 的 `app.settings` 里（JSON），改了立刻生效、稍后落盘。 */
class SettingsStore(
    private val store: KeyValueStore,
    /**
     * 落盘用的协程作用域。给了就**延迟落盘**（见 [SETTINGS_SAVE_DEBOUNCE_MS]）；不给就同步写
     * （测试、以及没有协程环境的地方）。
     */
    private val scope: CoroutineScope? = null,
    /** 装了哪些平台插件：决定默认值，以及读设置时哪些平台是新装的（见 [decodeSettings]）。 */
    private val platforms: PlatformCatalog = InstalledPlugins.catalog,
    /**
     * 「自动翻译的平台」在**存档里没这个字段**时的默认值；null = 插件描述的默认（境外四家）。
     *
     * 不自动翻"）：
     * 默认一个平台都不自动翻，详情里那一行「翻译」点了才翻。用户在设置里点亮之后照旧自动翻。
     * 已经存过这个字段的存档不受影响（用户自己选过就尊重他的选择）。
     */
    private val translationDefault: Set<Platform>? = null,
) {
    var value: AppSettings by mutableStateOf(load())
        private set

    /**
     * 底下那个键值存储（**同一个 `state.json`**）。
     *
     * 给"要往同一个 store 里写自己那几条记录"的组件用——目前只有安卓翻译引擎的**降级记忆**
     * （`translate.backend.v1`，见 Rust `translate::device::Memory`）：它必须和设置落在同一份
     * `state.json` 里，不然升级 / 清缓存之后那条记录对不上。
     */
    internal val rawStore: KeyValueStore get() = store

    private var persist = true
    private var saveJob: Job? = null

    /**
     * 落盘排队：`update()` 会取消上一次的 saveJob，但已经在写的那次取消不掉。
     * 用一把**线程锁**（不是协程 Mutex）保证两次 put 不并发 / 不颠倒顺序，而且 [flush] 也能从
     * 普通线程同步等它写完。
     */
    private val persistLock = ReentrantLock()

    fun update(f: (AppSettings) -> AppSettings) {
        val next = f(value)
        if (next == value) return
        value = next
        if (!persist) return
        val s = scope
        if (s == null) {
            write(next)
            return
        }
        // 设置页是边打字边存的（代理地址 / 端口每按一次键就 update 一次），不必每按一次都写一次盘
        saveJob?.cancel()
        saveJob = s.launch {
            delay(SETTINGS_SAVE_DEBOUNCE_MS)
            // 落盘可能在编码整份 state（关注流缓存也在里面），不要占 UI 线程；锁保证先后顺序。
            withContext(Dispatchers.IO) {
                persistLock.withLock { write(next) }
            }
        }
    }

    /**
     * **立刻**把当前设置落盘。
     *
     * 防抖还没到点就退出的话，最后一次改动以前会丢。这里取消掉排队的那次、同步写一遍当前值；
     * 如果正好有一次写在飞（已经过了防抖、在 IO 线程上），就等它写完再写——锁保证不会旧值覆盖新值。
     * 不走协程：关闭窗口时没有可以挂起的调度器了。截图 / 测试里的 `override` 模式（[persist] false）什么都不做。
     */
    fun flush() {
        saveJob?.cancel()
        saveJob = null
        if (!persist) return
        val snapshot = value
        persistLock.withLock { write(snapshot) }
    }

    private fun write(snapshot: AppSettings) {
        store.put(KEY, FeedJson.encodeToString(AppSettings.serializer(), snapshot))
    }

    /** 之后的修改都只留在内存里、不写回存储（截图工具用，免得改掉用户自己的设置）。 */
    fun override(f: (AppSettings) -> AppSettings) {
        saveJob?.cancel() // 刚排上队的落盘也取消掉，别把 override 之前的设置写回去
        persist = false
        value = f(value)
    }

    /**
     * 设置里的代理配置，交给 core 的 [feedhub.core.SourceContext.withProxy]。
     * 每次请求现读 [value]，所以设置页改了代理**立刻**对新请求生效，不用重建现有的 Source。
     * （OkHttp 的连接池不看这次新选的代理，所以改设置时 AppGraph 会换一份新的连接池 +
     * 重建 Source 缓存，见 `AppGraph.watchProxySettings`。）
     *
     * [builtin] 是「内置核心」那一档现在的走法（`ProxyCore.proxyMode()`）；不传 = 没接核心，fail-closed。
     *
     * **出口锁关着**、代理又用不了（[AppSettings.directFallbackReason] 不为 null）时，走的是直连；
     * 这件事**每种原因只通知一次**（[onDirectFallback]），免得每个请求都刷一行日志——这类"说好的走代理
     * 结果直连了"的状态，用户必须能看见。原因文案不含地址。
     */
    private val fallbackTracker = RustSettings.createFallbackTracker()

    private class ProxyResolveCache(
        val settings: AppSettings,
        val builtin: ProxyMode,
        val byPlatform: java.util.concurrent.ConcurrentHashMap<Platform, ProxyMode> =
            java.util.concurrent.ConcurrentHashMap(),
    )

    fun proxyProvider(
        builtin: () -> ProxyMode = { NO_BUILTIN_CORE },
        onDirectFallback: (String) -> Unit = {},
        /**
         * 内置核心**正在起**时等它起好、返回新的走法（等不到 / 不在起 / 在界面线程上就是 null，照旧 fail-closed）。
         * 只在**这个平台确实走内置核心**、而核心此刻没在跑时才问。
         */
        awaitBuiltin: () -> ProxyMode? = { null },
    ): ProxyProvider {
        val cacheLock = Any()
        var cached: ProxyResolveCache? = null
        return ProxyProvider { platform ->
            val s = value
            var b = builtin()
            if (b is ProxyMode.BuiltinCore && !b.valid &&
                s.proxyFor(platform, ProxyMode.BuiltinCore(1), platforms) is ProxyMode.BuiltinCore
            ) {
                awaitBuiltin()?.let { b = it }
            }
            val hit = cached?.takeIf { it.settings === s && it.builtin == b }
            hit?.byPlatform?.get(platform)?.let { return@ProxyProvider it }
            val (mode, notice) = RustSettings.resolveAndTrackProxy(
                tracker = fallbackTracker,
                platform = platform,
                platformProxy = s.platformProxy,
                proxiedPlatforms = s.proxiedPlatforms,
                overseas = platforms[platform]?.overseas == true,
                global = s.proxy.toProxyMode(b),
                exitLock = s.proxyExitLock,
            )
            notice?.let(onDirectFallback)
            val bucket = synchronized(cacheLock) {
                val cur = cached
                if (cur != null && cur.settings === s && cur.builtin == b) {
                    cur
                } else {
                    ProxyResolveCache(s, b).also { cached = it }
                }
            }
            bucket.byPlatform[platform] = mode
            mode
        }
    }

    private fun load(): AppSettings = store.get(KEY)
        ?.let { runCatching { decodeSettings(it, platforms, translationDefault) }.getOrNull() }
        ?: defaultSettings(platforms, translationDefault)

    private companion object {
        const val KEY = "app.settings"
    }
}

/**
 * 把存下来的设置 JSON 读回 [AppSettings]。多余的字段忽略，缺的字段用默认值，所以加字段不会让旧存档读不出来。
 * 旧版弹幕设置只有一个"弹幕密度"（`danmakuDensity`，0~100），现在拆成了显示区域和智能云屏蔽等级两个选项（见 [DanmakuSettings]）：
 * 存档里没有 `danmaku` 而有 `danmakuDensity` 时，按 [DanmakuSettings.fromLegacyDensity] 换算；下一次保存设置时它就以新的样子写下去，旧字段不再写。
 * 弹幕设置里超出范围的值收回范围（[DanmakuSettings.normalized]）。
 *
 * **旧版首页列数**：老存档里没有 [AppSettings.homeColumnsMode]，只有一个 `homeColumns` 滑轨值。
 * 等于老默认 [LEGACY_DEFAULT_HOME_COLUMNS]（或者压根没存过这个字段）的当成 [HomeColumnsMode.AUTO]——用户没主动拖过滑轨；
 * 别的值保留成 [HomeColumnsMode.FIXED]（用户拖过，尊重他选的那一档）。新存档里一定带着 `homeColumnsMode`，
 * 所以"固定 4 列"不会再被当成老存档迁走。固定值夹回 [MIN_HOME_COLUMNS]～[MAX_HOME_COLUMNS]。
 *
 * **平台**：
 * - 老存档里平台写的是 enum 名（`"BILIBILI"`），现在照样认（[Platform] 的序列化）；
 * - 存档里有**没装的插件**的 id（拆掉了）：读出来留着，不报错；界面只画装了的插件、请求也只发装了的，所以它不起作用；
 * - 缺某一组平台字段：用 [platforms] 的默认值（`defaults.*`）；
 * - **新装的插件**（不在 [AppSettings.knownPlatforms] 里；老存档没有这个字段时按老的 11 个平台算）：按它的默认值
 *   加进首页 / 关注 / 走代理 / 自动翻译那几组。老的 11 个平台因此一个都不会被改动。
 *
 * **内容过滤**：老存档没有 [AppSettings.blockWords] / [AppSettings.outingMode] /
 * [AppSettings.outingKeywords] / [AppSettings.outingCommunities]，读回来就是空名单 + 外出模式关着（默认值），
 * 所以升级之后行为不变、也不多花时间；名单按 [ContentFilter.normalizeRules] 收干净。
 *
 * **首页排序**：老存档没有 [AppSettings.homeOrder]，读回来是 [FeedOrder.INTERLEAVE]（各平台穿插）；
 * 手改成认不出的字符串时，`FeedJson` 的 `coerceInputValues = true` 把它收回默认值（不会因此读不出来整份设置）。
 *
 * 读回来是 **15**；
 * 负数按"不限"（0）收回来。同一个提交里那个「关注页也隐藏已读」的字段（`hideReadOnFollowing`）
 * **已经删掉了**：老存档里还带着这个字段时，靠 `FeedJson` 的
 * `ignoreUnknownKeys` 直接忽略，读得回来、什么都不做（见 `SettingsTest`）。
 *
 * **按平台指定代理**：老存档没有 [AppSettings.platformProxy]，读回来是空表 = 每个平台都是
 * 「跟随全局」，和加这个功能之前**完全一样**（走代理的平台 / 全局代理 / 出口锁照旧）。
 * 现在不行（红线：境外平台不能直连）；想让它不走全局代理只能单独「指定代理」。
 * 2. **境外平台的 [ProxyChoice.Direct] 不认**（手改存档 / 以后的新版本写进来的），按「跟随全局」读。
 *
 * **安卓翻译的强制后端**：老版本[AppSettings.translationAndroidBackend] 是**必填**的
 * `CPU` / `OPENCL`，默认 `CPU`；那个 `CPU` 是"这一版还没有自动降级"的产品默认值，**不是用户选的**。
 * 所以老值一律读成 `null` = 自动降级，新值只认
 * `NPU` / `GPU` / `NPU_GPU` / `CPU_ONLY`。规则在 `settings.rs` 的 `normalize` 第 11 条（Rust 是唯一真相），
 * Kotlin 这边只是把结果解码回来。
 */
fun decodeSettings(
    text: String,
    platforms: PlatformCatalog = InstalledPlugins.catalog,
    /** 存档里没有 `translationPlatforms` 时的默认值；null = 插件描述的默认（安卓传空集合，见 [SettingsStore]）。 */
    translationDefault: Set<Platform>? = null,
): AppSettings {
    // **规则在 Rust**：`feedhub-core/src/settings.rs` 做那十几条迁移 /
    // 收范围 / 插件默认 / 海外红线，这里只把结果用数据类解码回来（`AppSettings` 的默认值是其余字段的
    // 唯一真相，Rust 不写它们）。归一化跑不动（读不出来 / 不是 JSON 对象）时退回"全套默认"。
    val normalized = RustSettings.normalize(text, platforms.toRustSpecs(translationDefault))
        ?: return defaultSettings(platforms, translationDefault)
    return feedhub.core.FeedJson.decodeFromJsonElement(AppSettings.serializer(), FeedJson.parseToJsonElement(normalized))
}
