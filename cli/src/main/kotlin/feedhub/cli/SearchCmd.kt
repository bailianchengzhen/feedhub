package feedhub.cli

import com.github.ajalt.clikt.core.Context
import com.github.ajalt.clikt.core.requireObject
import com.github.ajalt.clikt.parameters.arguments.argument
import com.github.ajalt.clikt.parameters.options.default
import com.github.ajalt.clikt.parameters.options.option
import com.github.ajalt.clikt.parameters.options.split
import com.github.ajalt.clikt.parameters.types.int
import feedhub.core.Cursor
import feedhub.core.FeedItem
import feedhub.core.FeedJson
import feedhub.core.Platform
import feedhub.core.SearchKind
import feedhub.core.SearchRequest
import feedhub.core.SearchSort
import feedhub.core.SearchSource
import feedhub.core.plugin.PluginTarget
import feedhub.brain.SearchMerge
import feedhub.brain.SearchPlan
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

/**
 * `search <平台|all> <词>`。
 *
 * `all` 和 App 搜索页**同一套规则**（规则本身在 core 的 [SearchPlan] / [SearchMerge]，不在这里抄一份）：
 * - 只搜「首页点亮 + 支持搜索 + 这一端能用」的平台，顺序 = `plugins.toml`；
 * - 每家一页只取配额条，按平台顺序轮转混排；
 * - 每家最多 [SearchPlan.PAGE_BUDGET] 页；
 * - 18+ 整条隐藏、内容过滤（屏蔽词 / 外出模式）在进列表之前过。
 *
 * 另外这里是串行的（一次只打一家），比 App 的并发温和得多；每次请求另外过冷却 / 限速闸。
 */
class SearchCmd : JsonCmd("search") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "平台，如 bili；all = 首页点亮且支持搜索的平台")
    private val text by argument(help = "搜索词")
    private val kindText by option("--kind", help = "搜什么：all（默认）/ video / post").default("all")
    private val sortText by option("--sort", help = "排序：relevance（默认）/ latest / hot").default("relevance")
    private val community by option("--community", help = "贴吧吧名：给了就是吧内搜索（默认全吧搜索）")
    private val only by option("--only", help = "只搜这些平台，逗号分隔（只有 all 能配；必须在首页点亮）")
        .split(",").default(emptyList())
    private val limit by option("--limit", help = "最多输出 N 条（默认：all 一整批 / 单平台一整页）").int()
    private val page by option("--page", help = "第几页，1 起（all 时每个平台各翻到这个页；最多 ${SearchPlan.PAGE_BUDGET}）").int().default(1)
    private val cursor by option("--cursor", help = "从这个平台游标开始（只能指定单个平台）")
    private val pages by option("-n", "--pages", help = "连续取 N 页（老参数，等价于 --page 1..N）").int()

    override fun help(context: Context) = """
        搜索（只读；风控信号立刻停，不重试）。

        all：和 App 搜索页同一套规则——只搜首页点亮且支持搜索的平台，
        每家的第一页只取配额条、按平台顺序轮转混排，18+ / 内容过滤在进列表之前过。
        单平台：直接问这一家，--page / --cursor 翻页。
        --kind / --sort 按各平台的 capabilities.search 校验，不支持就报错，不静默退化。

        每个结果都带平台和 id（`data.platform` / `data.id`），detail / comments 原样接：
          feedhub search all "词" --json
          feedhub detail bili BV1xx --json
    """.trimIndent()

    override fun run() = runBlocking {
        configure(session)
        val query = text.trim()
        if (query.isEmpty()) throw CliFailure(ErrorCode.BAD_INPUT, "搜索词不能为空")
        val kind = enumOf<SearchKind>(kindText, "kind")
        val sort = enumOf<SearchSort>(sortText, "sort")
        val isAll = platform == "all"
        if (only.isNotEmpty() && !isAll) throw CliFailure(ErrorCode.BAD_INPUT, "--only 只能和 all 一起用")
        if (cursor != null && isAll) throw CliFailure(ErrorCode.BAD_INPUT, "--cursor 只能和单个平台一起用（all 是好几家的游标）")
        limit?.let { if (it < 1) throw CliFailure(ErrorCode.BAD_INPUT, "--limit 要 ≥ 1") }
        if (page < 1) throw CliFailure(ErrorCode.BAD_INPUT, "--page 从 1 开始")
        val pagesFlag = pages
        if (pagesFlag != null && pagesFlag < 1) throw CliFailure(ErrorCode.BAD_INPUT, "-n/--pages 要 ≥ 1")

        // 这次翻哪几页：-n 给的是"从第 1 页连续 N 页"，否则只取 --page 那一页
        val wanted = if (pagesFlag != null) (1..pagesFlag).toList() else listOf(page)
        val deepest = wanted.max()
        if (deepest > SearchPlan.PAGE_BUDGET) {
            throw CliFailure(
                ErrorCode.BAD_INPUT,
                "一次搜索每家最多 ${SearchPlan.PAGE_BUDGET} 页（和 App 搜索页一致），--page $deepest 超了",
            )
        }

        val settings = env.feedSettings
        val chosen: List<Platform> = if (isAll) {
            val allow = if (only.isEmpty()) {
                settings.homePlatforms
            } else {
                // 显式点名的平台也必须在首页点亮：all 的规则是"只搜点亮的"
                for (key in only) {
                    val p = platformOf(key)
                    if (p !in settings.homePlatforms) {
                        throw CliFailure(
                            ErrorCode.BAD_INPUT,
                            "${p.label}（${p.key}）不在首页点亮的平台里（all 只搜点亮的）；" +
                                "要么在 App 里点亮它，要么直接 feedhub search ${p.key} …",
                        )
                    }
                }
                only.map(::platformOf).toSet()
            }
            SearchPlan.platforms(installed, allow, installed.forTarget(PluginTarget.DESKTOP).platforms.toSet())
        } else {
            listOf(platformOf(platform))
        }
        if (chosen.isEmpty()) {
            throw CliFailure(
                ErrorCode.BAD_INPUT,
                "没有可搜的平台：首页亮着的平台里没有一家接了搜索（用 feedhub platforms --json 看）",
            )
        }

        // 先按描述里的能力筛一遍（不支持搜索的直接报 / 跳过），再建 Source
        val candidates = mutableListOf<Pair<Platform, SearchSource>>()
        for (p in chosen) {
            val plugin = installed[p]
                ?: throw CliFailure(ErrorCode.NOT_SUPPORTED, "${p.label}（${p.key}）的插件没装", p)
            val cap = plugin.capabilities.search
            if (cap == null) {
                if (!isAll) {
                    throw CliFailure(ErrorCode.NOT_SUPPORTED, "${p.label} 没有接搜索（描述里 capabilities.search 是 null）", p)
                }
                session.reportError(CliFailure(ErrorCode.NOT_SUPPORTED, "${p.label}：没有接搜索，跳过", p))
                continue
            }
            if (kind !in cap.kinds) {
                if (!isAll) throw CliFailure(ErrorCode.NOT_SUPPORTED, "${p.label} 不支持 --kind $kindText", p)
                session.reportError(CliFailure(ErrorCode.NOT_SUPPORTED, "${p.label}：不支持 --kind $kindText，跳过", p))
                continue
            }
            if (sort !in cap.sorts) {
                if (!isAll) throw CliFailure(ErrorCode.NOT_SUPPORTED, "${p.label} 不支持 --sort $sortText", p)
                session.reportError(CliFailure(ErrorCode.NOT_SUPPORTED, "${p.label}：不支持 --sort $sortText，跳过", p))
                continue
            }
            val source = env.source(p) as? SearchSource
                ?: throw CliFailure(ErrorCode.NOT_SUPPORTED, "${p.label} 的描述里有搜索能力，但 Source 没实现 SearchSource", p)
            candidates += p to source
        }
        if (candidates.isEmpty()) throw CliFailure(ErrorCode.BAD_INPUT, "没有可搜索的平台")

        val request = SearchRequest(query, kind, sort, community)
        val order = candidates.map { it.first }
        val outcome = collectMixedSearch(
            candidates = candidates,
            request = request,
            wanted = wanted,
            cursor = cursor?.let(::Cursor),
            rules = settings.contentFilter,
            limit = limit,
            strict = !isAll,
        ) { p -> session.cooldown.check(p) }
        val failures = LinkedHashMap<Platform, ErrorCode>()
        for ((p, e) in outcome.failures) failures[p] = session.reportPlatformFailure(e)
        val shown = outcome.items

        if (session.json) {
            for (item in shown) session.emit("item", FeedJson.encodeToJsonElement(FeedItem.serializer(), item))
            session.emit(
                "done",
                extra = buildMap {
                    put("count", JsonPrimitive(shown.size))
                    put("hidden", JsonPrimitive(outcome.hidden))
                    put("answered", JsonPrimitive(outcome.answered))
                    put("platforms", jsonArray(order.map { it.key }))
                    put("page", JsonPrimitive(deepest))
                    put("requests", JsonPrimitive(session.requests.get()))
                    put("next", nextCursorJson(order, outcome.next))
                    if (failures.isNotEmpty()) put("failed", platformCodesJson(failures))
                },
            )
        } else {
            shown.forEachIndexed { i, item -> session.say(formatItem(item, i + 1)) }
            session.say(
                "共 ${shown.size} 条，${outcome.answered} / ${candidates.size} 家有结果" +
                    (if (outcome.hidden > 0) "（另有 ${outcome.hidden} 条被 18+ / 内容过滤藏掉）" else ""),
            )
        }
        if (failures.isNotEmpty() && failures.size == candidates.size) {
            throw CliFailure(failures.values.first(), "所有平台都失败了（每一家的原因见 stderr）")
        }
    }

    private fun nextCursorJson(order: List<Platform>, next: Map<Platform, Cursor?>): JsonElement =
        buildJsonObject {
            for (p in order) {
                val c = next[p]
                if (c == null) put(p.key, JsonPrimitive(null as String?)) else put(p.key, JsonPrimitive(c.value))
            }
        }

    private fun platformCodesJson(failures: Map<Platform, ErrorCode>): JsonElement = buildJsonObject {
        failures.forEach { (p, e) -> put(p.key, JsonPrimitive(e.key)) }
    }

    private inline fun <reified T : Enum<T>> enumOf(text: String, what: String): T =
        enumValues<T>().firstOrNull { it.name.equals(text, ignoreCase = true) }
            ?: throw CliFailure(
                ErrorCode.BAD_INPUT,
                "未知的 $what「$text」，可选：${enumValues<T>().joinToString { it.name.lowercase() }}",
            )
}
