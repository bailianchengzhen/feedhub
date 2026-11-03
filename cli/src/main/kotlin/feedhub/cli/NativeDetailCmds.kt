package feedhub.cli

import com.github.ajalt.clikt.core.CliktCommand
import com.github.ajalt.clikt.core.CliktError
import com.github.ajalt.clikt.core.Context
import com.github.ajalt.clikt.core.requireObject
import com.github.ajalt.clikt.parameters.arguments.argument
import com.github.ajalt.clikt.parameters.options.default
import com.github.ajalt.clikt.parameters.options.flag
import com.github.ajalt.clikt.parameters.options.option
import com.github.ajalt.clikt.parameters.types.choice
import com.github.ajalt.clikt.parameters.types.int
import feedhub.brain.AdultFilter
import feedhub.core.Block
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedDetail
import feedhub.core.FeedJson
import feedhub.core.NativeDetailSource
import feedhub.core.Page
import feedhub.core.PrettyJson
import feedhub.core.RichText
import feedhub.core.Span
import kotlinx.coroutines.runBlocking
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

// 原生详情、评论、楼中楼：feedhub detail / comments / replies。

/** 取实现了 [NativeDetailSource] 的平台。 */
fun Env.nativeSource(platformKey: String): NativeDetailSource {
    val p = platformOf(platformKey)
    return source(p) as? NativeDetailSource ?: throw CliktError("${p.label}还没实现原生详情和评论")
}

private val commentTimeFmt = DateTimeFormatter.ofPattern("MM-dd HH:mm").withZone(ZoneId.of("Asia/Shanghai"))

private fun spansText(spans: List<Span>): String = spans.joinToString("") { span ->
    when (span) {
        // 站内链接（能原生打开的）只显示文字，站外链接在后面补上地址
        is Span.Link -> if (span.target != null || "coolapk.com" in span.url || span.text == span.url) span.text else "${span.text}（${span.url}）"
        is Span.Text -> if (span.bold) "**${span.text}**" else span.text
        else -> span.text
    }
}

/** 富文本 → 终端里看的几行。图片、卡片单独一行。 */
fun formatRichText(text: RichText, indent: String = ""): String = text.blocks.joinToString("\n") { block ->
    when (block) {
        is Block.Paragraph -> indent + spansText(block.spans)
        is Block.Heading -> indent + "## " + spansText(block.spans)
        is Block.Quote -> indent + "> " + spansText(block.spans)
        is Block.Code -> block.text.lines().joinToString("\n") { "$indent  | $it" }
        is Block.Image -> indent + "[图片" + (if (block.width != null && block.height != null) " ${block.width}x${block.height}" else "") + "] ${block.url}" +
            (block.original?.let { "（原图 $it）" } ?: "") + (block.caption?.let { " $it" } ?: "")
        is Block.Card -> indent + "[卡片] " + listOfNotNull(block.title, block.subtitle).joinToString(" — ") +
            (block.target?.let { " → $it" } ?: block.url?.let { " $it" } ?: "")
        is Block.Video -> indent + "[视频] " + listOfNotNull(
            block.title,
            block.durationSec?.let { "%d:%02d".format(it / 60, it % 60) },
            if (block.width != null && block.height != null) "${block.width}x${block.height}" else null,
        ).joinToString(" · ") + "  play ${block.id} --part ${block.partId}"
        Block.Divider -> indent + "-".repeat(20)
    }
}

fun formatComment(c: Comment, index: Int?, indent: String = ""): String = buildString {
    val head = (index?.let { "#$it " } ?: "") + listOfNotNull(
        "置顶".takeIf { c.pinned },
        c.floor?.let { "${it}楼" },
        (c.author?.name ?: "（匿名）") + (c.replyTo?.let { " 回复 ${it.name}" } ?: ""),
        c.publishedAt?.let { commentTimeFmt.format(Instant.ofEpochSecond(it)) },
        c.likes?.let { "赞 $it" },
        c.replyCount?.takeIf { it > 0 }?.let { "回复 $it" },
        c.ipLocation,
        "id ${c.id}",
    ).joinToString(" · ")
    append(indent).append(head).append('\n')
    val body = formatRichText(c.content, "$indent    ")
    if (body.isNotBlank()) append(body).append('\n')
    for (r in c.previewReplies) {
        val who = (r.author?.name ?: "（匿名）") + (r.replyTo?.let { " 回复 ${it.name}" } ?: "")
        append(indent).append("    ↳ ").append(who).append("：")
            .append(r.content.plainText().replace(Regex("\\s+"), " ").let { if (it.length > 80) it.take(80) + "…" else it })
            .append('\n')
    }
    val more = (c.replyCount ?: 0) - c.previewReplies.size
    if (c.previewReplies.isNotEmpty() && more > 0) append(indent).append("    ↳ 还有 $more 条，用 replies 命令看\n")
}

class DetailCmd : JsonCmd("detail") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "平台，如 coolapk")
    private val id by argument(help = "内容 id（DetailTarget.Native 的 id，search / feed --json 里能看到）")

    override fun help(context: Context) = "打印一条内容的详情正文（--json 时一行一条 JSON）"

    override fun run() = runBlocking {
        configure(session)
        val p = platformOf(platform)
        session.cooldown.check(p) // 冷却期一个请求都不发
        val source = env.nativeSource(platform)
        val d: FeedDetail = try {
            source.detail(id)
        } catch (e: Exception) {
            throw session.classify(e)
        }
        // 平台标成 18+ 的整条隐藏：JSON 里也只给一行"隐藏了"，不给正文 / 媒体 / 相关推荐
        if (AdultFilter.hidden(d.item)) {
            if (session.json) {
                session.emit(
                    "hidden",
                    extra = buildMap {
                        put("reason", kotlinx.serialization.json.JsonPrimitive("adult"))
                        put("platform", kotlinx.serialization.json.JsonPrimitive(p.key))
                        put("id", kotlinx.serialization.json.JsonPrimitive(id))
                        put("message", kotlinx.serialization.json.JsonPrimitive(AdultFilter.NOTICE))
                        put("requests", kotlinx.serialization.json.JsonPrimitive(session.requests.get()))
                    },
                )
            } else {
                session.say(AdultFilter.NOTICE)
            }
            return@runBlocking
        }
        if (session.json) {
            session.emit("detail", FeedJson.encodeToJsonElement(FeedDetail.serializer(), d))
            session.emit(
                "done",
                extra = mapOf(
                    "count" to kotlinx.serialization.json.JsonPrimitive(1),
                    "requests" to kotlinx.serialization.json.JsonPrimitive(session.requests.get()),
                ),
            )
            return@runBlocking
        }
        session.say(formatItem(d.item).trimEnd())
        d.follow?.let { f ->
            val state = when (f.following) {
                true -> "已关注"
                false -> "未关注"
                null -> "关注状态未知"
            }
            session.say("    关注按钮：${f.target.name}（${f.target.kind} ${f.target.id}，$state）")
        }
        d.video?.let { v ->
            v.parts.forEachIndexed { i, part ->
                val dur = part.durationSec?.let { "%d:%02d".format(it / 60, it % 60) }
                session.say("    P${i + 1} part=${part.id} " + listOfNotNull(part.title, dur, part.width?.let { "${it}x${part.height}" }).joinToString(" · "))
            }
            v.danmakuSegmentMs?.let { session.say("    弹幕每段 ${it / 1000} 秒") }
        }
        session.say("—".repeat(30))
        session.say(formatRichText(d.body))
        if (d.related.isNotEmpty()) {
            // 相关推荐里被平台标成 18+ 的也不列
            val related = AdultFilter.visible(d.related)
            session.say("—".repeat(30))
            session.say("相关推荐 ${related.size} 条：")
            related.take(5).forEachIndexed { i, item -> session.say(formatItem(item, i + 1, "  ").trimEnd()) }
        }
    }
}

/** comments 和 replies 共用：翻页、打印、最后报游标。 */
private suspend fun printComments(
    session: Session,
    pages: Int,
    limit: Int?,
    start: Cursor?,
    load: suspend (Cursor?) -> Page<Comment>,
) {
    var next = start
    var n = 0
    // `--limit` 落在某一页中间时，把**这一页的起始游标**报回去（`truncated=true`）：
    // 再跑一次会重取这一页，前面已经打过的几条会重复（按 id 去重即可），但不会漏。
    var truncated = false
    // 酷安这类按"最近回复"排的列表在翻页期间有新回复会整体位移，下一页可能重复上一页的条目；
    // 和 App 的 PagedState 一样按 id 去重（同一 id 保留第一次看到的）。
    val seen = HashSet<String>()
    try {
        for (i in 0 until pages) {
            val pageStart = next
            val page = load(pageStart)
            var fresh = 0
            for (c in page.items) {
                if (!seen.add(c.id)) continue
                if (limit != null && n >= limit) {
                    truncated = true
                    break
                }
                fresh++
                n++
                if (session.json) {
                    session.emit("comment", FeedJson.encodeToJsonElement(Comment.serializer(), c))
                } else {
                    session.say(formatComment(c, n))
                }
            }
            if (truncated) {
                next = pageStart
                break
            }
            next = page.next
            // 整页都是重复的：再翻也只会重复，当到底了（和 PagedState 一致）
            if (next == null || fresh == 0) break
        }
    } catch (e: Exception) {
        throw session.classify(e)
    }
    if (session.json) {
        session.emit(
            "done",
            extra = buildMap {
                put("count", kotlinx.serialization.json.JsonPrimitive(n))
                // 下一页游标；null = 没有了（`--limit` 在第一页截断时也是 null：这一页还没看完，
                // 再跑一次同样的命令会重新取这一页，配 `truncated=true` 一起看）
                put("next", next?.let { kotlinx.serialization.json.JsonPrimitive(it.value) } ?: kotlinx.serialization.json.JsonNull)
                put("truncated", kotlinx.serialization.json.JsonPrimitive(truncated))
                put("requests", kotlinx.serialization.json.JsonPrimitive(session.requests.get()))
            },
        )
    } else {
        session.say("共 $n 条；下一页游标：${next ?: "（没有了）"}" + if (truncated) "（到 --limit 停的，这一页还没看完）" else "")
    }
}

class CommentsCmd : JsonCmd("comments") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "平台，如 coolapk")
    private val id by argument(help = "内容 id")
    private val pages by option("-n", "--pages", help = "翻几页").int().default(1)
    private val limit by option("--limit", help = "最多输出几条（到量就停，游标报回去）").int()
    private val cursor by option("--cursor", help = "从这个游标开始")
    private val sort by option("--sort", help = "default（平台默认）/ hot / newest").choice(
        "default" to CommentSort.DEFAULT, "hot" to CommentSort.HOT, "newest" to CommentSort.NEWEST,
    ).default(CommentSort.DEFAULT)

    override fun help(context: Context) = "打印一条内容的评论（含楼中楼预览；贴吧是楼层）"

    override fun run() = runBlocking {
        configure(session)
        limit?.let { if (it < 1) throw CliFailure(ErrorCode.BAD_INPUT, "--limit 要 ≥ 1") }
        val p = platformOf(platform)
        session.cooldown.check(p)
        val source = env.nativeSource(platform)
        printComments(session, pages, limit, cursor?.let(::Cursor)) { source.comments(id, it, sort) }
    }
}

class RepliesCmd : JsonCmd("replies") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "平台，如 coolapk")
    private val id by argument(help = "内容 id")
    private val commentId by argument(help = "一级评论 id")
    private val pages by option("-n", "--pages", help = "翻几页").int().default(1)
    private val limit by option("--limit", help = "最多输出几条（到量就停，游标报回去）").int()
    private val cursor by option("--cursor", help = "从这个游标开始")

    override fun help(context: Context) = "打印某条评论下的楼中楼"

    override fun run() = runBlocking {
        configure(session)
        limit?.let { if (it < 1) throw CliFailure(ErrorCode.BAD_INPUT, "--limit 要 ≥ 1") }
        val p = platformOf(platform)
        session.cooldown.check(p)
        val source = env.nativeSource(platform)
        printComments(session, pages, limit, cursor?.let(::Cursor)) { source.replies(id, commentId, it) }
    }
}
