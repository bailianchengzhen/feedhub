package feedhub.source.bilibili

import feedhub.core.Cursor
import feedhub.core.FollowKind
import feedhub.core.FollowTarget
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.arr
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject

/** 关注列表（`/x/relation/followings`）→ [FollowTarget]。 */
internal object BiliFollowParser {
    private val P = BILIBILI

    /** 页码翻页，`data.total` 是关注总数；已取条数 ≥ total 或这页是空的就到头了。 */
    fun followings(json: JsonObject, pn: Int, ps: Int): Page<FollowTarget> {
        val data = json.obj("data") ?: throw ParseException(P, "关注列表缺少 data")
        val items = data.arr("list").orEmpty().mapNotNull { it.obj()?.let(::target) }
        val total = data.long("total") ?: 0L
        return Page(items, if (items.isNotEmpty() && pn.toLong() * ps < total) Cursor((pn + 1).toString()) else null)
    }

    private fun target(o: JsonObject): FollowTarget? {
        val mid = o.string("mid")?.takeIf { it.isNotEmpty() && it != "0" } ?: return null
        return FollowTarget(
            platform = P,
            kind = FollowKind.USER,
            id = mid,
            name = o.text("uname") ?: mid,
            avatar = o.text("face")?.toHttps(),
            description = o.text("sign"),
            url = BiliParser.spaceUrl(mid),
        )
    }
}
