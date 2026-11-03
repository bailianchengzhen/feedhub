package feedhub.app.mine

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import feedhub.app.login.CookieImporter
import feedhub.app.login.WebLogin
import feedhub.app.login.WebLoginCookies
import feedhub.core.DeviceCodeLoginSource
import feedhub.core.FeedCookieJar
import feedhub.core.KeyValueStore
import feedhub.core.LoginSpec
import feedhub.core.MemoryKeyValueStore
import feedhub.core.NotLoggedInException
import feedhub.core.Platform
import feedhub.core.QrLoginSource
import feedhub.core.Source
import feedhub.core.UserRef
import feedhub.core.sync.SyncApplyResult
import feedhub.core.sync.SyncApplier
import feedhub.core.sync.SyncBuildResult
import feedhub.core.sync.SyncClient
import feedhub.core.sync.SyncDefaults
import feedhub.core.sync.SyncInbound
import feedhub.core.sync.SyncPackage
import feedhub.core.sync.SyncPackageBuilder
import feedhub.core.sync.SyncPending
import feedhub.core.sync.SyncDirection
import feedhub.core.sync.SyncPushResult
import feedhub.core.sync.SyncScope
import feedhub.core.sync.SyncServer
import feedhub.mine.AccountEngine
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import okhttp3.Cookie
import okhttp3.HttpUrl.Companion.toHttpUrl
import kotlin.coroutines.CoroutineContext

/**
 * "我的 → 账号"的状态：各平台的登录状态（并发问 `source.whoami()`，谁先回来谁先显示）。
 *
 * "该不该问、问哪个、存下来的怎么读写"由引擎决定；
 * 这个类只剩：
 * 1. **Compose 状态容器**（`status` / `checkedAt` / `notice` / `scanning` …）；
 * 2. **协程 + 真正的 `whoami()`**（OkHttp 走平台对象），结果（含异常分类）喂回引擎；
 * 3. **Cookie 罐 / WebView Cookie / 设备码 token 的清写**（退出登录那三处）、浏览器导入、同步包应用、落盘。
 *
 * 原来的行为一条没变（`AccountsCacheTest` / `AccountsLogoutTest` 守着）：进页面**一个请求都不发**、先摆上次的；
 * 登录成功 / 退出 / 导入 / 同步只问那一个平台；手动刷新才问全部；别处报没登录只标过期、不发请求。
 * 平台在别处（刷信息流）报了"没登录"时走 [markExpired]。
 */
class AccountsState(
    private val scope: CoroutineScope,
    val platforms: List<Platform>,
    private val sourceOf: (Platform) -> Source,
    private val cookies: FeedCookieJar,
    private val io: CoroutineContext = Dispatchers.IO,
    val importer: CookieImporter? = null,
    /** 「网页登录」（系统 WebView）：安卓有、桌面为 null。 */
    val webLogin: WebLogin? = null,
    /**
     * 上次查到的状态存这（Rust 的 `AccountEngine` 编解码，`AccountsCache` 包一层）。
     * **只存昵称 / 头像 / id / 状态，不存 Cookie / token。**
     */
    private val store: KeyValueStore = MemoryKeyValueStore(),
) {
    private val engine = AccountEngine(platforms)

    /** 本地存着的"上次查到"时间（毫秒）；界面写"上次查到：今天 14:03"。null = 本地还没存过。 */
    var checkedAt: Long? by mutableStateOf(null)
        private set

    /** 正在从浏览器导入的平台。 */
    var importing: Platform? by mutableStateOf(null)
        private set

    /** 导入结果之类的一行提示，显示在对应平台的卡片里。 */
    var notice: Map<Platform, String> by mutableStateOf(emptyMap())
        private set

    /** 正在扫码登录的平台（弹框开着）；null = 没有。 */
    var scanning: Platform? by mutableStateOf(null)

    /** 正在设备码登录的平台（弹框开着）；null = 没有。YouTube 是设备码登录。 */
    var pairing: Platform? by mutableStateOf(null)

    /** 调试入口（`DebugStartActivity --es syncqr`）。 */
    var pendingSyncContent: String? by mutableStateOf(null)

    /** 调试入口（`--es syncmode offer-send|offer-receive`）：自动选方向并出码。 */
    var pendingSyncOffer: SyncDirection? by mutableStateOf(null)

    /** 调试入口（`--es syncadvertise <host>`）：二维码里写成这个地址（模拟器 A↔B 用 adb forward）。 */
    var pendingSyncAdvertise: String? by mutableStateOf(null)

    /** 调试入口（`--es syncbind <host>`）：真正绑定的地址（模拟器里绑 0.0.0.0 让 adb forward 连得到）。 */
    var pendingSyncBind: String? by mutableStateOf(null)

    /** 调试入口（`--ez syncauto true`）：收到 / 取回后自动点"写入"（只给端到端验收）。 */
    var pendingSyncAutoConfirm: Boolean by mutableStateOf(false)

    /** 没有某个平台的键 = 还没查过。 */
    var status: Map<Platform, AccountStatus> by mutableStateOf(emptyMap())
        private set

    private val jobs = mutableMapOf<Platform, Job>()

    /** 本机有没有这个平台的登录 Cookie（只判存在，不读值）。会构造 Source。 */
    fun hasLoginCookies(p: Platform): Boolean = hasLoginCookies(sourceOf(p).login)

    /** 按插件描述判（**不构造 Source**）：界面默认勾选 / 列表状态用它。 */
    fun hasLoginCookies(login: LoginSpec): Boolean =
        login.requiredCookies.isNotEmpty() && login.requiredCookies.all { cookies.find(login.cookieUrl, it) != null }

    /** 发送方：把这几个平台、这些域里的 Cookie 打成同步包（**不落盘、不打印**）。 */
    suspend fun exportSync(
        platforms: Collection<Platform>,
        scopes: Map<Platform, SyncScope>,
        now: Long = System.currentTimeMillis(),
    ): SyncBuildResult = withContext(io) { SyncPackageBuilder.build(cookies, platforms, scopes, now) }

    /** 接收方：**按平台各自的域**先清后写，然后逐个平台重新查一次登录状态。 */
    suspend fun importSync(pkg: SyncPackage, scopes: Map<Platform, SyncScope>): SyncApplyResult {
        val result = withContext(io) { SyncApplier.apply(cookies, pkg, scopes) }
        finishSync(result)
        return result
    }

    /**
     * 登录互传：把"已经取回 / 已经收到、等确认"的包写进本机罐子，然后逐个平台重查。
     * 写入是"按平台各自的域先清后写"——会顶掉本机同平台原来的登录。
     */
    suspend fun applyLoginTransfer(inbound: SyncInbound, scopes: Map<Platform, SyncScope>): SyncApplyResult {
        val result = withContext(io) { inbound.apply(cookies, scopes) }
        finishSync(result)
        return result
    }

    /** 出码方：起一次性服务（绑定 / 打包 / 加密都在 Rust）。 */
    suspend fun startLoginTransferServer(
        bindHost: String,
        direction: SyncDirection,
        platforms: Collection<Platform>,
        scopes: Map<Platform, SyncScope>,
        advertiseHost: String = bindHost,
    ): SyncServer = withContext(io) {
        SyncServer.start(cookies, bindHost, direction, platforms, scopes, advertiseHost = advertiseHost)
    }

    /** 出码方默认勾哪些平台（Rust 按本机 Cookie 罐 + 各平台地盘算）。 */
    suspend fun defaultSyncPlatforms(scopes: Map<Platform, SyncScope>): List<Platform> =
        withContext(io) { SyncDefaults.platforms(cookies, scopes) }

    /** 扫码方：取回来（解密校验在 Rust；**还没写**，等用户确认）。 */
    suspend fun fetchLoginTransfer(client: SyncClient, offer: feedhub.core.sync.SyncOffer): SyncPending =
        withContext(io) { client.fetch(offer) }

    /** 扫码方：把本机登录推给「收进来」的二维码。 */
    suspend fun pushLoginTransfer(
        client: SyncClient,
        offer: feedhub.core.sync.SyncOffer,
        platforms: Collection<Platform>,
        scopes: Map<Platform, SyncScope>,
    ): SyncPushResult = withContext(io) { client.push(offer, cookies, platforms, scopes) }

    private fun finishSync(result: SyncApplyResult) {
        if (result.applied.isNotEmpty()) {
            notice = notice + result.applied.mapValues { (_, n) -> "已从另一台设备同步 $n 个 Cookie" }
            result.applied.keys.forEach(::check)
        }
    }

    /** 这个平台能不能扫码登录。 */
    fun qrSource(p: Platform): QrLoginSource? = sourceOf(p) as? QrLoginSource

    /** 这个平台能不能用设备码登录（实现了 [DeviceCodeLoginSource]，现在只有 YouTube）。 */
    fun deviceCodeSource(p: Platform): DeviceCodeLoginSource? = sourceOf(p) as? DeviceCodeLoginSource

    /** 设备码登录成功：关掉弹框、清掉旧提示，重新查一次（token 是 Source 自己存的）。 */
    fun deviceCodeDone(p: Platform) {
        pairing = null
        notice = notice - p
        check(p)
    }

    /** 这个平台的登录约定。 */
    fun loginSpec(p: Platform): LoginSpec = sourceOf(p).login

    /** 这个平台能不能用网页登录。 */
    fun canWebLogin(p: Platform): Boolean {
        val login = loginSpec(p)
        return webLogin != null && login.webLoginUrl.isNotBlank() && login.requiredCookies.isNotEmpty()
    }

    /**
     * 网页登录完成：**只动这个平台的域名**（先清旧再写新的），然后重新查状态。
     * 不打印任何 Cookie 值，只报数量。
     */
    fun webLoginDone(p: Platform, imported: List<Cookie>) {
        val login = sourceOf(p).login
        val keep = WebLoginCookies.toStore(login.cookieDomain, imported)
        notice = notice - p
        scope.launch {
            if (keep.isNotEmpty()) {
                withContext(io) {
                    cookies.clearDomain(login.cookieDomain)
                    keep.groupBy { it.domain }.forEach { (domain, list) -> cookies.put("https://$domain/".toHttpUrl(), list) }
                }
                notice = notice + (p to "已从网页登录导入 ${keep.size} 个 Cookie")
            } else {
                notice = notice + (p to "网页登录没有取到${p.label}的 Cookie")
            }
            check(p)
        }
    }

    /**
     * 进"我的 → 账号"时调：把上次存下来的状态**直接摆出来、一个请求都不发**。
     * 本地一条都没存的平台（第一次用、或者新装了个插件）才照旧问一次。这个方法进几次页面都只做一次。
     */
    fun ensureChecked() {
        val plan = engine.ensureChecked(store)
        syncFromEngine()
        plan.queries.forEach(::launchQuery)
    }

    /** 用户点了刷新：重新问所有平台（并发）。这是唯一"问全部"的入口。 */
    fun refresh() {
        val plan = engine.refresh()
        syncFromEngine()
        plan.queries.forEach(::launchQuery)
    }

    fun check(p: Platform) {
        val plan = engine.check(p)
        syncFromEngine()
        plan.queries.forEach(::launchQuery)
    }

    /**
     * 别处（刷信息流的那个平台）报了"没登录"：把这个平台标成**过期**，**不发请求**。
     *
     * 正在查这个平台时不动：查回来的结果更准。本机没有它的登录 Cookie 时标成"未登录"而不是"登录已失效"。
     */
    fun markExpired(p: Platform) {
        if (p !in platforms) return
        // 只判"有没有登录 Cookie"（构造 Source 只为了读它的登录约定），判不出来就当有、让界面给重新登录的按钮
        val expired = runCatching { hasLoginCookies(sourceOf(p).login) }.getOrDefault(true)
        engine.markExpired(p, expired, System.currentTimeMillis())
        syncFromEngine()
        saveStore()
    }

    /** 从浏览器导入这个平台的 Cookie（替换本机原有的），然后重新查。调用前界面要先让用户确认。 */
    fun importFromBrowser(p: Platform) {
        val imp = importer ?: return
        if (importing != null) return
        importing = p
        notice = notice - p
        scope.launch {
            val msg = try {
                val r = imp.import(setOf(p)).getValue(p)
                when {
                    r.imported > 0 -> "从 ${imp.browserName} 导入了 ${r.imported} 个 Cookie" + if (r.failed > 0) "（${r.failed} 个解不开）" else ""
                    r.failed > 0 -> "${imp.browserName} 里的 ${r.failed} 个 Cookie 都解不开"
                    else -> "${imp.browserName} 里没有${p.label}的登录信息，先在 ${imp.browserName} 里登录"
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                e.message ?: "导入失败"
            } finally {
                importing = null
            }
            notice = notice + (p to msg)
            check(p)
        }
    }

    /** 扫码登录成功后调：关掉弹框、清掉旧提示，重新查。 */
    fun loggedIn(p: Platform) {
        scanning = null
        notice = notice - p
        check(p)
    }

    /**
     * 退出登录：清掉这个平台的 Cookie 再重新查。三处都要清，缺一个就会出现"退出了又立刻登录上"：
     * 1. 我们的 Cookie 罐（[FeedCookieJar.clearDomain]）；
     * 2. **系统 WebView 里的**（[WebLogin.clearCookies]）；
     * 3. 设备码登录的 token（[DeviceCodeLoginSource.logout]）。
     */
    fun logout(p: Platform) {
        val source = sourceOf(p)
        val login = source.login
        scope.launch {
            // CookieManager 的写操作在主线程做（AndroidWebCookies 里说明）
            webLogin?.clearCookies(login, source.hosts)
            withContext(io) {
                cookies.clearDomain(login.cookieDomain)
                (source as? DeviceCodeLoginSource)?.logout()
            }
            // 凭据已经清掉了，状态就是确定的"未登录"：**先存下来**，万一接下来那次 whoami 断网失败，
            // 也不会把本地那份"已登录"留着、下次进页面又显示已登录。
            engine.loggedOut(p, System.currentTimeMillis())
            syncFromEngine()
            saveStore()
            check(p)
        }
    }

    // ------------------------------------------------------------------ 内部

    private fun launchQuery(p: Platform) {
        jobs[p]?.cancel()
        jobs[p] = scope.launch {
            val outcome = withContext(io) { runWhoami(p) }
            val hasCookies = runCatching { hasLoginCookies(sourceOf(p).login) }.getOrDefault(false)
            engine.onWhoami(p, outcome, hasCookies, System.currentTimeMillis())
            syncFromEngine()
            saveStore()
        }
    }

    private suspend fun runWhoami(p: Platform): AccountEngine.Whoami = try {
        val user = sourceOf(p).whoami()
        if (user != null) AccountEngine.Whoami.User(user) else AccountEngine.Whoami.NoUser
    } catch (e: CancellationException) {
        throw e
    } catch (e: NotLoggedInException) {
        AccountEngine.Whoami.NotLoggedIn
    } catch (e: Exception) {
        AccountEngine.Whoami.Failed(e.message ?: e::class.simpleName ?: "未知错误")
    }

    private fun syncFromEngine() {
        val next = LinkedHashMap<Platform, AccountStatus>(platforms.size * 2)
        for ((platform, status) in engine.status()) {
            next[platform] = status.toApp()
        }
        if (status != next) status = next
        val at = engine.checkedAt()
        if (checkedAt != at) checkedAt = at
    }

    private fun AccountEngine.Status.toApp(): AccountStatus = when (this) {
        AccountEngine.Status.Checking -> AccountStatus.Checking
        is AccountEngine.Status.LoggedIn -> AccountStatus.LoggedIn(user)
        is AccountEngine.Status.LoggedOut -> AccountStatus.LoggedOut(expired)
        is AccountEngine.Status.Failed -> AccountStatus.Failed(message)
    }

    /** 写盘失败不影响界面（和原来一样）。 */
    private fun saveStore() {
        runCatching { engine.save(store) }
    }
}

/**
 * 一个平台账号现在的状态。
 *
 * **留在界面层**：它是 Compose 读的那个 sealed 类型（界面按它画卡片 / 按钮）；
 * Rust 那份是 `AccountEngine.Status`，一一对应（`AccountsCacheTest` 的字段白名单也靠它）。
 */
sealed interface AccountStatus {
    /** 正在问平台。 */
    data object Checking : AccountStatus

    data class LoggedIn(val user: UserRef) : AccountStatus

    /** [expired]：本机还留着登录用的 Cookie，但平台说没登录（过期了）；false = 压根没有登录过。 */
    data class LoggedOut(val expired: Boolean) : AccountStatus

    /** 查的时候出错了（网络、风控、接口变了……），[message] 是一行原因。 */
    data class Failed(val message: String) : AccountStatus
}
