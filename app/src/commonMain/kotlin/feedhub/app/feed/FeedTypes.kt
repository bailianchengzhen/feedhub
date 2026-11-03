package feedhub.app.feed

import feedhub.core.FeedItem
import feedhub.core.NotLoggedInException
import feedhub.core.Platform
import feedhub.core.RiskControlException
import feedhub.core.SourceException
import kotlinx.serialization.Serializable
import java.io.IOException

/**
 * 一条内容是从哪一批、平台原始顺序的第几条来的。
 *
 * [batchId] 是同一次刷新 / 加载更多的一批共用的 id；[platformRank] 是这条在它所属平台**原始返回顺序**里的序号
 * （0 起，每批每平台重新数）。以后离线打分时，"平台自己把它排在第几"是一个直接能用的特征。
 *
 * 这里只是它的 Kotlin 形状（业务代码 / 资料库读得惯的那一份）。
 */
data class ItemOrigin(val batchId: Long, val platformRank: Int)

/**
 * 首页这一批怎么排（设置页「信息流 → 首页排序」）。
 *
 * **关注页不受它影响**：关注流本来就是时间线，永远按发布时间倒序。
 *
 * 这里只是设置里那一档的名字 +
 * `FeedEngine.Batch.order` 的入口；**枚举名不能改**（`state.json` 里存的就是它）。
 */
@Serializable
enum class FeedOrder {
    /** 各平台穿插。 */
    INTERLEAVE,

    /** 整批按发布时间倒序，留着给用户对比用。 */
    TIME,
}

/**
 * 本地向量打分的权重。
 *
 * 接在批内顺序的打分钩子上；当前的任务只搭通路，
 * 所以 [WEIGHT] **固定为 0**：把位置分数和"和用户画像的余弦相似度"加权合成，但权重 0 时合成分数恒等于位置分数，
 * 排序结果和没有向量时**逐个位置完全一样**（`ScoredOrderTest` 守着这条）。
 *
 * 合成本身（`blend` / `blend_order`）在 Rust（`feedhub.brain.Brain`）。
 */
object EmbeddingScore {
    /** 向量相似度在合成排序分里的权重。**当前是 0**：不改变任何顺序。 */
    const val WEIGHT: Float = 0f

    /**
     * 位置分数 [position]（0 = 最前，1 = 最后）和相似度 [similarity]（0～1，越大越相关；null = 这条没有向量）
     * 合成一个**升序排序分**：相似度越高，分数越小、排得越前。
     *
     * 相似度缺省时原样返回位置分数（没有向量的条目不会因此被挤到后面）；权重 0 时也原样返回。
     * **算式在 Rust**（`brain::order::blend`），这里只是过河。
     */
    fun blend(position: Float, similarity: Float?, weight: Float = WEIGHT): Float =
        feedhub.brain.Brain.blend(position, similarity, weight)
}

/**
 * 断网 / 连不上时的那一句（[describe] 给 `IOException` 的文案）。
 * 界面拿它判断"这次失败是不是网络问题"（首页 / 关注标题行的"没有网络，显示的是之前的内容"，
 * 见 `feed/FeedScreen.kt` 的 `feedNoticeFor`），所以文案只有这一处。
 *
 * **留在 Kotlin**：它认的是宿主异常类型（`IOException` / `NotLoggedInException` / …），
 * 那是"收发那一侧"的东西；Rust 只拿算好的那一行字（`PageOutcome.Failed.describe`）。
 */
const val NETWORK_ERROR_TEXT: String = "网络错误"

/** 错误 → 界面上的一行字（留在宿主的唯一理由：它按宿主异常类型分类）。 */
fun describe(e: Throwable): String = when (e) {
    is NotLoggedInException -> "没登录或登录过期"
    is RiskControlException -> "被风控拦了，稍后再试"
    is SourceException -> e.message ?: "接口出错"
    is IOException -> NETWORK_ERROR_TEXT
    else -> e.message ?: e::class.simpleName.orEmpty()
}

/**
 * 失败的技术细节写日志用：异常类名 + message，**URL 的 query 抹掉**（里面可能有签名 / token），
 * 不含 Cookie（异常 message 里本来就没有）。界面上只给 [describe] 那一行字。
 */
fun technicalDetail(e: Throwable): String {
    val message = e.message?.replace(QUERY_IN_URL, "$1?…").orEmpty()
    return "${e::class.simpleName}: $message".trimEnd(':', ' ')
}

private val QUERY_IN_URL = Regex("""(https?://[^\s?#"]+)\?[^\s"]*""")

/** [FeedItem.key] 的简写（重新拼屏幕列表时用，和 `FeedItem.key` 一个算法）。 */
internal fun FeedItem.feedKey(): String = "${platform.key}:$id"

/** 平台 id → 平台（`Platform` 是个字符串包装，读回来照原样）。 */
internal fun platformOf(key: String): Platform = Platform(key)
