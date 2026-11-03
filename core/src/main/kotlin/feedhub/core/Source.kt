package feedhub.core

/**
 * 一个平台的数据层。每个平台一个实现，构造时拿到 [SourceContext]。
 *
 * 约定：
 * - 方法都是 suspend，失败抛 [SourceException] 的子类（网络错误原样抛 IOException）。
 * - 只读。唯一的写操作是关注 / 取关，单独在 [FollowSource] 里，只有 B站和贴吧实现；评论、弹幕、点赞一律不做。
 * - 详情、评论在 [NativeDetailSource]，视频播放和弹幕在 [VideoSource]。
 * - 请求频率由 Source 自己的 OkHttpClient 限速（见 [MinIntervalInterceptor]），调用方不用管。
 */
interface Source {
    val platform: Platform

    /** 这个平台支持哪些流。调用不支持的会抛 [UnsupportedFeedException]。 */
    val feeds: Set<FeedKind>

    val login: LoginSpec

    /** `raw` 调试命令里以 "/" 开头的路径相对于这个地址。 */
    val apiBase: String

    /**
     * 这个平台实际会去连的域名后缀（接口、图片、视频 CDN 都算），小写、不带点。
     *
     * 给共用的客户端按域名选代理用（[platformForHost] → [ProxyProvider]）：信息流把六家的图片混在一起，
     * 图片加载器只有一个客户端，只能靠域名判断这张图是哪家的。**只列我们真的会请求的域名**——正文里的站外链接
     * （酷安动态里的 B站视频、小黑盒里的 steam 页面）不算，那是给用户复制链接 / 交给另一个平台打开的。
     * 加了新 CDN 就在这里补一条。
     */
    val hosts: List<String> get() = emptyList()

    /** 当前登录的账号；没登录返回 null（不抛异常）。 */
    suspend fun whoami(): UserRef?

    suspend fun feed(kind: FeedKind, cursor: Cursor? = null): Page<FeedItem>

    /**
     * 调试用：按本平台的规则（Cookie、请求头、签名）发一个任意请求，返回原始响应。
     * cli 的 `raw` 命令就是调它。
     */
    suspend fun raw(request: RawRequest): RawResponse
}

enum class FeedKind(val key: String, val label: String) {
    RECOMMEND("recommend", "推荐"),
    FOLLOWING("following", "关注");

    companion object {
        fun fromKey(key: String): FeedKind? = entries.firstOrNull { it.key == key }
    }
}

/**
 * 怎么登录这个平台。App 里用 WebView 打开 [webLoginUrl]，登录后从 CookieManager 取 [cookieUrl] 下的 Cookie；
 * cli 里则是把浏览器请求头里的 Cookie 粘贴进来，域名统一记成 [cookieDomain]。
 */
data class LoginSpec(
    val webLoginUrl: String,
    /** 取 Cookie 用的地址。 */
    val cookieUrl: String,
    /** 粘贴导入时 Cookie 记到哪个域，例如 ".bilibili.com"。 */
    val cookieDomain: String,
    /** 这些 Cookie 都在才算登录了（只用来提示，不代替 [Source.whoami]）。 */
    val requiredCookies: Set<String>,
)

data class RawRequest(
    /** 完整 URL，或以 "/" 开头、相对于 [Source.apiBase] 的路径（可带 query）。 */
    val url: String,
    val method: String = "GET",
    /** 追加到 URL 上的参数。 */
    val query: List<Pair<String, String>> = emptyList(),
    /** 非空时以 application/x-www-form-urlencoded 发送。 */
    val form: List<Pair<String, String>> = emptyList(),
    /** 非空时以 application/json 发送（InnerTube 这类 JSON 接口用；`form` 和它同时给以它为准）。 */
    val json: String? = null,
    val headers: List<Pair<String, String>> = emptyList(),
    /** 是否按平台规则加签名。 */
    val sign: Boolean = true,
)

data class RawResponse(
    val code: Int,
    val url: String,
    /** 响应头（不含 Set-Cookie，避免在终端里打印出凭据）。 */
    val headers: List<Pair<String, String>>,
    val body: String,
)
