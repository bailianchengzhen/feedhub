package feedhub.cli

import com.github.ajalt.clikt.core.CliktCommand
import com.github.ajalt.clikt.core.Context
import com.github.ajalt.clikt.core.requireObject
import com.github.ajalt.clikt.parameters.arguments.argument
import com.github.ajalt.clikt.parameters.options.flag
import com.github.ajalt.clikt.parameters.options.option
import feedhub.core.Platform
import feedhub.core.plugin.PluginTarget
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

/**
 * `capabilities`：让 agent **自己发现**能用什么。
 *
 * 列出：所有命令和参数、错误码表、每个平台支持什么（搜索 / 详情 / 评论 / 楼中楼）、限速和冷却规则。
 * 完全离线：不读 Cookie、不建 Source、不发请求。
 *
 * `--json` 时是**一行**大 JSON（`type=capabilities`）；不加 `--json` 打印一份人也能看懂的清单。
 */
class CapabilitiesCmd : JsonCmd("capabilities") {
    private val session by requireObject<Session>(SESSION_KEY)

    override fun help(context: Context) = "列出命令、参数、错误码、各平台能力、限速和冷却规则（给 agent 自己发现用；离线）"

    override fun run() {
        configure(session)
        val platforms = installed.plugins.map { plugin ->
            buildJsonObject {
                put("id", JsonPrimitive(plugin.id))
                put("label", JsonPrimitive(plugin.label))
                put("feeds", jsonArray(plugin.capabilities.feeds.map { it.key }))
                searchCapabilityJson(plugin.capabilities.search)?.let { put("search", it) }
                put("commentSort", JsonPrimitive(plugin.capabilities.commentSort))
                put("nestedReplies", JsonPrimitive(plugin.capabilities.nestedReplies))
                put("playableVideo", JsonPrimitive(plugin.capabilities.playableVideo))
                put("overseas", JsonPrimitive(plugin.overseas))
                put("desktop", JsonPrimitive(plugin.supports(PluginTarget.DESKTOP)))
            }
        }
        if (session.json) {
            session.emit(
                "capabilities",
                buildJsonObject {
                    put("schema", JsonPrimitive(SCHEMA_VERSION))
                    put("commands", commandsJson())
                    put("errorCodes", errorCodesJson())
                    put("platforms", kotlinx.serialization.json.JsonArray(platforms))
                    put(
                        "limits",
                        buildJsonObject {
                            put(
                                "rateLimitMs",
                                buildJsonObject {
                                    put("default", JsonPrimitive(CrossProcessRateLimit.DEFAULT_INTERVAL_MS))
                                    CrossProcessRateLimit.DEFAULT_INTERVALS_MS.forEach { (k, v) -> put(k, JsonPrimitive(v)) }
                                },
                            )
                            put("cooldownMs", JsonPrimitive(CooldownStore.DEFAULT_TTL_MS))
                            put("agentMaxRequests", JsonPrimitive(DEFAULT_MAX_REQUESTS))
                            put("searchMaxPages", JsonPrimitive(feedhub.brain.SearchPlan.PAGE_BUDGET))
                            put("libraryMaxLimit", JsonPrimitive(500))
                        },
                    )
                },
            )
            return
        }
        session.say("错误码（退出码）：")
        ErrorCode.entries.forEach { e ->
            session.say("  ${e.key.padEnd(18)} exit ${e.exitCode}  retryable=${e.retryable}")
        }
        session.say("\n命令：")
        for (c in COMMANDS) session.say("  ${c.name.padEnd(18)} ${c.usage}")
        session.say("\n平台：")
        for (p in installed.plugins) {
            val search = p.capabilities.search?.let { "搜索(${it.pageSize}/页)" } ?: "无搜索"
            session.say(
                "  ${p.id.padEnd(9)} ${p.label.padEnd(8)} ${p.capabilities.feeds.joinToString("/") { it.key }}" +
                    "  $search  楼中楼=${p.capabilities.nestedReplies}  $search",
            )
        }
        session.say("\n限速：同平台最小间隔 ${CrossProcessRateLimit.DEFAULT_INTERVAL_MS} ms（X / 小红书 / 抖音 / TikTok 更长）；" +
            "风控冷却 ${CooldownStore.DEFAULT_TTL_MS / 60000} 分钟；agent 模式每次运行最多 $DEFAULT_MAX_REQUESTS 个请求。")
    }

    private fun searchCapabilityJson(cap: feedhub.core.SearchCapability?): JsonObject? = cap?.let {
        buildJsonObject {
            put("kinds", jsonArray(it.kinds.map { k -> k.name.lowercase() }))
            put("sorts", jsonArray(it.sorts.map { s -> s.name.lowercase() }))
            put("pageSize", JsonPrimitive(it.pageSize))
            put("needsLogin", JsonPrimitive(it.needsLogin))
        }
    }

    private fun commandsJson() = kotlinx.serialization.json.JsonArray(
        COMMANDS.map { c ->
            buildJsonObject {
                put("name", JsonPrimitive(c.name))
                put("usage", JsonPrimitive(c.usage))
                put("args", jsonArray(c.args))
                put("options", jsonArray(c.options))
                put("readsNetwork", JsonPrimitive(c.readsNetwork))
                put("writes", JsonPrimitive(c.writes))
                put("json", JsonPrimitive(c.json))
            }
        },
    )

    private fun errorCodesJson() = kotlinx.serialization.json.JsonArray(
        ErrorCode.entries.map { e ->
            buildJsonObject {
                put("error", JsonPrimitive(e.key))
                put("exitCode", JsonPrimitive(e.exitCode))
                put("retryable", JsonPrimitive(e.retryable))
            }
        },
    )

    private class Command(
        val name: String,
        val usage: String,
        val args: List<String>,
        val options: List<String>,
        val readsNetwork: Boolean,
        val writes: Boolean = false,
        val json: Boolean = true,
    )

    companion object {
        private val COMMANDS = listOf(
            Command(
                "search", "search <平台|all> <词>", listOf("<平台|all>", "<词>"),
                listOf("--json", "--only", "--limit", "--page", "--cursor", "-n/--pages", "--kind", "--sort", "--community"),
                readsNetwork = true,
            ),
            Command("detail", "detail <平台> <id>", listOf("<平台>", "<id>"), listOf("--json"), readsNetwork = true),
            Command("comments", "comments <平台> <id>", listOf("<平台>", "<id>"), listOf("--json", "-n/--pages", "--limit", "--cursor", "--sort"), readsNetwork = true),
            Command("replies", "replies <平台> <id> <评论 id>", listOf("<平台>", "<id>", "<评论 id>"), listOf("--json", "-n/--pages", "--limit", "--cursor"), readsNetwork = true),
            Command("whoami", "whoami [平台…]", listOf("[平台…]"), listOf("--json"), readsNetwork = true),
            Command("platforms", "platforms", emptyList(), listOf("--json"), readsNetwork = false),
            Command("feed", "feed <平台> [recommend|following]", listOf("<平台>", "[流]"), listOf("--json", "-n/--pages", "--limit", "--cursor"), readsNetwork = true),
            Command(
                "library favorites", "library favorites", emptyList(),
                listOf("--json", "--platform", "--since", "--query", "--limit"), readsNetwork = false,
            ),
            Command(
                "library history", "library history", emptyList(),
                listOf("--json", "--platform", "--since", "--query", "--limit", "--failed-only"), readsNetwork = false,
            ),
            Command("cooldown list", "cooldown list", emptyList(), listOf("--json"), readsNetwork = false),
            Command("cooldown clear", "cooldown clear <平台>", listOf("<平台>"), listOf("--json"), readsNetwork = false),
            Command("capabilities", "capabilities", emptyList(), listOf("--json"), readsNetwork = false, json = false),
            // 写操作：agent 模式下会被拒绝（bad_input）
            Command("follow", "follow <平台> <用户 id>", listOf("<平台>", "<用户 id>"), emptyList(), readsNetwork = true, writes = true),
            Command("unfollow", "unfollow <平台> <用户 id>", listOf("<平台>", "<用户 id>"), emptyList(), readsNetwork = true, writes = true),
            Command("cookie import", "cookie import <平台>", listOf("<平台>"), listOf("--replace"), readsNetwork = false, writes = true),
            Command("cookie clear", "cookie clear <平台>", listOf("<平台>"), emptyList(), readsNetwork = false, writes = true),
            Command("cookie remove", "cookie remove <平台> <名字…>", listOf("<平台>", "<名字…>"), emptyList(), readsNetwork = false, writes = true),
            Command("login", "login <平台>", listOf("<平台>"), listOf("--qr", "--device-code"), readsNetwork = true, writes = true),
            Command("plugin add", "plugin add <模块>", listOf("<模块>"), emptyList(), readsNetwork = false, writes = true),
            Command("plugin remove", "plugin remove <平台 id>", listOf("<平台 id>"), emptyList(), readsNetwork = false, writes = true),
            Command("plugin new", "plugin new <平台 id>", listOf("<平台 id>"), emptyList(), readsNetwork = false, writes = true),
            Command("raw", "raw <平台> <url>", listOf("<平台>", "<url>"), listOf("-X", "-q", "-d", "-H", "--no-sign", "--json"), readsNetwork = true),
        )
    }
}

/**
 * `cooldown list` / `cooldown clear <平台>`：给用户（和 agent）看 / 清 cli 的风控冷却。
 * 只动 `~/.cache/feedhub/cooldown/`，**不碰 App**。
 */
class CooldownCmd : CliktCommand(name = "cooldown") {
    override fun help(context: Context) = "cli 的风控冷却：list 看、clear <平台> 清（只影响 cli，不影响 App）"
    override fun run() = Unit
}

class CooldownListCmd : JsonCmd("list") {
    private val session by requireObject<Session>(SESSION_KEY)

    override fun help(context: Context) = "现在正在冷却的平台（冷却期内 cli 对它们一个请求都不发）"

    override fun run() {
        configure(session)
        val entries = session.cooldown.list()
        if (session.json) {
            val now = System.currentTimeMillis()
            for (e in entries) {
                session.emit(
                    "cooldown",
                    extra = buildMap {
                        put("platform", JsonPrimitive(e.platform))
                        put("until", JsonPrimitive(e.until))
                        put("retryAfterMs", JsonPrimitive((e.until - now).coerceAtLeast(0)))
                        put("reason", JsonPrimitive(e.reason))
                    },
                )
            }
            session.emit(
                "done",
                extra = mapOf("count" to JsonPrimitive(entries.size), "requests" to JsonPrimitive(session.requests.get())),
            )
            return
        }
        if (entries.isEmpty()) {
            session.say("没有平台在冷却。")
            return
        }
        val now = System.currentTimeMillis()
        for (e in entries) {
            session.say(
                "${e.platform.padEnd(9)} 还剩 ${(e.until - now + 999) / 1000} 秒（原因 ${e.reason}，到 ${timeText(e.until)}）",
            )
        }
    }
}

class CooldownClearCmd : JsonCmd("clear") {
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "平台，如 bili")

    override fun help(context: Context) = "清掉某个平台的冷却（用户确认风控已经过去之后再清）"

    override fun run() {
        configure(session)
        val p = platformOf(platform)
        val removed = session.cooldown.clear(p)
        if (session.json) {
            session.emit(
                "cleared",
                extra = buildMap {
                    put("platform", JsonPrimitive(p.key))
                    put("removed", JsonPrimitive(removed))
                },
            )
            return
        }
        session.say(if (removed) "已清掉 ${p.label} 的冷却。" else "${p.label} 本来就不在冷却里。")
    }
}

/** 平台对象给别的文件用（避免 import 未使用告警）。 */
internal fun platformKeyOf(p: Platform): String = p.key
