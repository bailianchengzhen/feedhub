package feedhub.cli

import com.github.ajalt.clikt.core.CliktCommand
import com.github.ajalt.clikt.core.CliktError
import com.github.ajalt.clikt.core.Context
import com.github.ajalt.clikt.core.requireObject
import com.github.ajalt.clikt.parameters.arguments.argument
import com.github.ajalt.clikt.parameters.arguments.optional
import com.github.ajalt.clikt.parameters.options.default
import com.github.ajalt.clikt.parameters.options.flag
import com.github.ajalt.clikt.parameters.options.option
import com.github.ajalt.clikt.parameters.types.choice
import com.github.ajalt.clikt.parameters.types.int
import feedhub.core.Cursor
import feedhub.core.FeedJson
import feedhub.core.FollowKind
import feedhub.core.FollowSource
import feedhub.core.FollowTarget
import kotlinx.coroutines.runBlocking

// 关注管理：feedhub followings / follow / unfollow。follow 和 unfollow 是写操作，不加 --confirm 只打印不发请求。

private fun Env.followSource(platformKey: String): FollowSource {
    val p = platformOf(platformKey)
    return source(p) as? FollowSource
        ?: throw CliFailure(ErrorCode.NOT_SUPPORTED, "${p.label}不支持关注（只有 B站和贴吧）", p)
}

class FollowingsCmd : JsonCmd("followings") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "bili 或 tieba")
    private val pages by option("-n", "--pages", help = "翻几页").int().default(1)
    private val cursor by option("--cursor", help = "从这个游标开始")

    override fun help(context: Context) = "列出当前账号关注的 UP 主 / 吧"

    override fun run() = runBlocking {
        configure(session)
        val source = env.followSource(platform)
        var next = cursor?.let(::Cursor)
        var n = 0
        try {
            for (i in 0 until pages) {
                val page = source.followings(next)
                for (t in page.items) {
                    n++
                    if (session.json) {
                        session.emit("follow", FeedJson.encodeToJsonElement(FollowTarget.serializer(), t))
                    } else {
                        session.say("#$n ${t.name} · ${t.kind} ${t.id}" + (t.description?.let { " · " + it.replace(Regex("\\s+"), " ").take(40) } ?: ""))
                    }
                }
                next = page.next
                if (next == null) break
            }
        } catch (e: Exception) {
            throw session.classify(e)
        }
        if (session.json) {
            session.emit(
                "done",
                extra = mapOf(
                    "count" to kotlinx.serialization.json.JsonPrimitive(n),
                    "next" to (next?.let { kotlinx.serialization.json.JsonPrimitive(it.value) } ?: kotlinx.serialization.json.JsonNull),
                    "requests" to kotlinx.serialization.json.JsonPrimitive(session.requests.get()),
                ),
            )
        } else {
            session.say("共 $n 个；下一页游标：${next ?: "（没有了）"}")
        }
    }
}

/** follow / unfollow 共用。 */
abstract class SetFollowCmd(name: String, private val follow: Boolean) : CliktCommand(name = name) {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "bili 或 tieba")
    private val kind by argument(help = "user（B站 UP 主）或 forum（贴吧的吧）").choice("user" to FollowKind.USER, "forum" to FollowKind.FORUM)
    private val id by argument(help = "B站 mid，或贴吧 fid")
    private val name by argument(help = "名字；贴吧必须给吧名").optional()
    private val confirm by option("--confirm", help = "真的发请求（这是写操作，在线测试前要征得用户同意）").flag()

    override fun run() = runBlocking {
        // 写操作：agent 模式下一律拒绝
        session.rejectInAgentMode(if (follow) "关注" else "取关")
        val source = env.followSource(platform)
        val target = FollowTarget(source.platform, kind, id, name ?: id)
        val action = if (follow) "关注" else "取关"
        if (!confirm) {
            echo("将要在${source.platform.label}$action ${target.name}（${target.kind} ${target.id}）。这是写操作，加 --confirm 才会真的发请求。")
            return@runBlocking
        }
        try {
            source.setFollowing(target, follow)
        } catch (e: Exception) {
            throw session.classify(e)
        }
        echo("已$action ${target.name}")
    }
}

class FollowCmd : SetFollowCmd("follow", true) {
    override fun help(context: Context) = "关注（写操作，要 --confirm）"
}

class UnfollowCmd : SetFollowCmd("unfollow", false) {
    override fun help(context: Context) = "取关（写操作，要 --confirm）"
}
