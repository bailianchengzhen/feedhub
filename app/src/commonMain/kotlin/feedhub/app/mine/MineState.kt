package feedhub.app.mine

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.Block
import androidx.compose.material.icons.outlined.History
import androidx.compose.material.icons.outlined.PeopleAlt
import androidx.compose.material.icons.outlined.StarBorder
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.vector.ImageVector
import feedhub.app.ContentType
import feedhub.app.library.FolderFilter
import feedhub.app.login.CookieImporter
import feedhub.app.login.WebLogin
import feedhub.core.FeedCookieJar
import feedhub.core.FollowSource
import feedhub.core.KeyValueStore
import feedhub.core.MemoryKeyValueStore
import feedhub.core.Platform
import feedhub.core.Source
import kotlinx.coroutines.CoroutineScope

/** "我的"里的几个入口。 */
enum class MineSection(val label: String, val icon: ImageVector) {
    ACCOUNTS("账号", Icons.Outlined.AccountCircle),
    FAVORITES("收藏", Icons.Outlined.StarBorder),
    HISTORY("历史记录", Icons.Outlined.History),
    FOLLOWING("关注管理", Icons.Outlined.PeopleAlt),
    BLOCKED("屏蔽的用户", Icons.Outlined.Block),
}

/**
 * "我的"页的状态，挂在 AppGraph 上：点开一条内容进详情再返回时，选中的入口、筛选条件、登录状态都还在
 * （"我的"在详情全屏时会离开组合，状态不能放在组合里）。
 */
class MineState(
    private val scope: CoroutineScope,
    private val platforms: List<Platform>,
    private val sourceOf: (Platform) -> Source,
    cookies: FeedCookieJar,
    importer: CookieImporter? = null,
    webLogin: WebLogin? = null,
    /** 账号状态存哪（`ctx.store`；测试给内存存储）。见 `AccountsState`（编解码在 Rust 的 `AccountEngine`）。 */
    store: KeyValueStore = MemoryKeyValueStore(),
) {
    /** 选中的入口。窄屏 null = 入口列表；宽屏 null 当成"账号"。 */
    var section: MineSection? by mutableStateOf(null)

    val accounts = AccountsState(scope, platforms, sourceOf, cookies, importer = importer, webLogin = webLogin, store = store)

    // 收藏页的筛选
    var favoriteFolder: FolderFilter by mutableStateOf(FolderFilter.All)
    var favoritePlatform: Platform? by mutableStateOf(null)
    var favoriteType: ContentType by mutableStateOf(ContentType.MIXED)

    // 历史页的筛选
    var historyPlatform: Platform? by mutableStateOf(null)
    var historySearch: String by mutableStateOf("")

    /** 关注管理里选中的平台。 */
    var followPlatform: Platform? by mutableStateOf(null)

    private val followStates = mutableMapOf<Platform, FollowingState>()

    /** 支持关注 / 取关的平台（B站、贴吧）。 */
    val followSources: List<FollowSource> by lazy { platforms.mapNotNull { sourceOf(it) as? FollowSource } }

    fun following(source: FollowSource): FollowingState =
        followStates.getOrPut(source.platform) { FollowingState(source, scope) }
}
