package feedhub.source.tieba

import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.rust.RustTieba
import kotlinx.serialization.json.JsonObject

/**
 * "拿到 body / 参数之后谁来解析 / 签名"这一个接缝（照 `source-bilibili` 的 `BiliParse`）。
 *
 * - [RustTiebaParse] / [RustTiebaSigner]：Rust 里的实现（`rust/crates/feedhub-core/src/tieba/`），经 UniFFI 过来；
 * - [KotlinTiebaSigner]：`TiebaSign` 的 Kotlin 版。Kotlin 的解析（`KotlinTiebaParse`）只在测试里当对拍参照。
 *
 * 生产路径上 [RustTiebaSource] 只用到签名（`raw` 调试命令）；解析接缝留给对拍比对 Kotlin / Rust 两份解析。
 */
internal interface TiebaParse {
    /** 推荐流一页（`pn` 是页码，下一页永远是 `pn + 1`）。 */
    fun personalized(bytes: ByteArray, pn: Int): Page<FeedItem>

    /** 吧内帖子列表一页。平台错误**不抛**，带回去让 `FrsPager` 按原来的规则归类（见 [FrsParsed]）。 */
    fun frsPage(bytes: ByteArray, forumName: String): FrsParsed

    /** 关注的吧（`forumGuide` 响应的 `like_forum`）。 */
    fun followedForums(json: JsonObject): List<FollowedForum>
}

/**
 * 吧内帖子列表的解析结果：要么有一页（[page]），要么带一个平台错误码（[errorCode] 非 0，[page] 为 null）。
 *
 * 为什么不像 `personalized` 那样直接抛：`FrsPager` 对"这个吧读不了"（例如 340001）要包一层
 * [ForumClosedException] 让合并器把这一个吧标成暂时关闭，其余错误（风控 / 未登录 / 签名）原样抛。
 * 这个分类在 Kotlin，所以错误码要能带回来。
 */
internal class FrsParsed(val page: ForumPage?, val errorCode: Int, val errorMessage: String)

/** 贴吧 App 签名：`md5(排序后的 "k=v" 拼接 + salt)`。 */
internal interface TiebaSigner {
    fun compute(params: List<Pair<String, String>>, salt: String = TiebaSign.APP_SALT): String

    /** 去掉 `sign`、排好序、末尾补上 `sign`（返回值就是请求里参数的顺序）。 */
    fun signed(params: List<Pair<String, String>>, salt: String = TiebaSign.APP_SALT): List<Pair<String, String>>
}

/**
 * Rust 解析。解析失败的异常由 Rust 带类型回来（`FfiException.Parse` → `ParseException`）；
 * **平台错误码的归类仍在 Kotlin**（[TiebaErrors.mapError]），和 Kotlin 实现走同一份规则，不会漂。
 */
internal class RustTiebaParse(private val platform: Platform = TIEBA) : TiebaParse {
    override fun personalized(bytes: ByteArray, pn: Int): Page<FeedItem> {
        val feed = RustTieba.parsePersonalized(bytes, pn, platform)
        if (feed.errorCode != 0) throw TiebaErrors.mapError(feed.errorCode, feed.errorMessage)
        return feed.page
    }

    override fun frsPage(bytes: ByteArray, forumName: String): FrsParsed {
        val feed = RustTieba.parseFrsPage(bytes, forumName, platform)
        if (feed.errorCode != 0) return FrsParsed(null, feed.errorCode, feed.errorMessage)
        return FrsParsed(ForumPage(feed.page.items, feed.last), 0, "")
    }

    override fun followedForums(json: JsonObject): List<FollowedForum> =
        RustTieba.followedForums(json.toString(), platform).map {
            FollowedForum(it.id, it.name, it.dayThreads, it.avatar, it.memberCount, it.threadCount)
        }
}

/** Kotlin 自己的签名。 */
internal object KotlinTiebaSigner : TiebaSigner {
    override fun compute(params: List<Pair<String, String>>, salt: String): String = TiebaSign.compute(params, salt)

    override fun signed(params: List<Pair<String, String>>, salt: String): List<Pair<String, String>> =
        TiebaSign.signed(params, salt)
}

/** Rust 签名。 */
internal class RustTiebaSigner(private val platform: Platform = TIEBA) : TiebaSigner {
    override fun compute(params: List<Pair<String, String>>, salt: String): String =
        RustTieba.sign(params, salt, platform)

    override fun signed(params: List<Pair<String, String>>, salt: String): List<Pair<String, String>> =
        RustTieba.signed(params, salt, platform)
}
