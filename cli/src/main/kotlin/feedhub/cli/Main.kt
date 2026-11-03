package feedhub.cli

import com.github.ajalt.clikt.core.CliktCommand
import com.github.ajalt.clikt.core.CliktError
import com.github.ajalt.clikt.core.Context
import com.github.ajalt.clikt.core.PrintCompletionMessage
import com.github.ajalt.clikt.core.PrintHelpMessage
import com.github.ajalt.clikt.core.PrintMessage
import com.github.ajalt.clikt.core.findOrSetObject
import com.github.ajalt.clikt.core.parse
import com.github.ajalt.clikt.core.requireObject
import com.github.ajalt.clikt.core.subcommands
import com.github.ajalt.clikt.parameters.arguments.argument
import com.github.ajalt.clikt.parameters.arguments.default
import com.github.ajalt.clikt.parameters.arguments.multiple
import com.github.ajalt.clikt.parameters.options.default
import com.github.ajalt.clikt.parameters.options.flag
import com.github.ajalt.clikt.parameters.options.multiple
import com.github.ajalt.clikt.parameters.options.option
import com.github.ajalt.clikt.parameters.options.split
import com.github.ajalt.clikt.parameters.types.int
import com.github.ajalt.clikt.parameters.types.path
import feedhub.brain.AdultFilter
import feedhub.core.Cursor
import feedhub.core.FeedItem
import feedhub.core.FeedJson
import feedhub.core.FeedKind
import feedhub.core.Platform
import feedhub.core.PrettyJson
import feedhub.core.RawRequest
import feedhub.core.Source
import feedhub.core.SourceContext
import feedhub.core.SourceException
import feedhub.core.local.LocalFiles
import feedhub.core.parseCookieHeader
import feedhub.core.platformHostTable
import feedhub.core.platformForHost
import feedhub.core.plugin.PlatformCatalog
import feedhub.core.plugin.PluginTarget
import feedhub.plugins.InstalledPlugins
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.Cookie
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.io.IOException
import java.io.PrintStream
import java.nio.file.Path
import kotlin.system.exitProcess

/**
 * 所有子命令共享：Cookie / 存储 / HTTP 客户端，以及按需创建的各平台 Source。
 *
 * **代理**：clikt 的 `--proxy` 覆盖 → [CliProxyPlan]（读 App 的同一份设置）→ 套到 [SourceContext] 上。
 * 点亮的平台但设置里是「不用」时，[CliProxyGuardInterceptor] 会在发请求前直接报错（fail-closed，
 * 一个请求都不发），不会悄悄直连。规则见 `CliProxy.kt`。
 *
 * **安全闸**：[CliGateInterceptor] 在真正发请求前做三件事——风控冷却
 * （冷却期一个请求都不发）、agent 模式的请求预算、跨进程限速；403 / 412 / 429 回来时写冷却并抛风控错误。
 */
class Env(record: Path?, proxyOverride: String? = null, private val session: Session) {
    /** 读 App 的 `app.settings`：Cookie / 存储先建好（`LocalFiles.context` 同时建了基础客户端）。 */
    private val base: SourceContext = LocalFiles.context(listOfNotNull(record?.let(::RecordingInterceptor)))

    /** 这次运行怎么走代理（懒：`--proxy` 写错 / 读设置的时机都在第一次真要用 ctx 的时候）。 */
    val proxyPlan: CliProxyPlan by lazy { CliProxyPlan.of(base.store, proxyOverride) }

    /** 这次读到的 App 设置（搜索规则用；只解析 cli 需要的几个字段）。 */
    val feedSettings: CliFeedSettings by lazy {
        base.store.get(CliFeedSettings.KEY)?.let { runCatching { CliFeedSettings.parse(it) }.getOrNull() }
            ?: CliFeedSettings()
    }

    /** "域名 → 平台"表：给代理守卫 / 限速闸按域名认平台用（和 app 是同一份：插件描述里的 `hosts`）。 */
    private val hostTable: Map<String, Platform> = platformHostTable(installed.hosts)

    /** 验证码：agent 模式 / 没有终端时**不替用户做**，只记下来、由 Source 抛回 `captcha_required`。 */
    private fun captchaHandler(): feedhub.core.CaptchaHandler? =
        if (session.agent || System.console() == null) NonInteractiveCaptchaHandler(session) else ConsoleCaptchaHandler()

    val ctx: SourceContext by lazy {
        val budget = if (session.agent) RequestBudget(session.maxRequests) else null
        base.withProxy(proxyPlan.provider, captchaHandler())
            .withExtraInterceptor(CliProxyGuardInterceptor(hostTable) { proxyPlan })
            .withExtraInterceptor(CliGateInterceptor(session, { host -> hostTable.platformForHost(host) }, budget))
    }

    private val sources = mutableMapOf<Platform, Source>()

    fun source(p: Platform): Source = sources.getOrPut(p) {
        val plugin = installed[p] ?: throw CliFailure(ErrorCode.NOT_SUPPORTED, "${p.label}（${p.key}）的插件没装")
        plugin.create(ctx)
    }

    fun platforms(keys: List<String>): List<Platform> =
        if (keys.isEmpty() || keys == listOf("all")) installed.platforms else keys.map(::platformOf)
}

/** 装了哪些平台插件。 */
val installed: PlatformCatalog get() = InstalledPlugins.catalog

/**
 * 平台参数 → 平台。**id 和老的 enum 名都认**（`bili` / `BILIBILI`）：
 * `--json` 里 `FeedItem.platform` 是按存储格式写的（老 11 个平台照旧写 enum 名，见 `PlatformSerializer`），
 * 直接把它喂回 `detail` / `comments` 要能用。
 */
fun platformOf(key: String): Platform =
    installed.find(key)
        ?: installed.find(Platform.fromStored(key).key)
        ?: throw CliFailure(ErrorCode.BAD_INPUT, "未知平台 $key，可选：${installed.keysText(", ")}")

/** 导入 / 清除 Cookie 用的域名：插件描述里的 `login.web.cookieDomain`（建 Source 之前就能拿到）。 */
fun cookieDomain(p: Platform): String = installed[p]!!.login.web.cookieDomain

/** 把业务错误打成一行给人看的字（只有人类模式用；JSON 模式走 [Session.reportError]）。 */
fun describe(e: Throwable): String = when (e) {
    is SourceException -> e.message.orEmpty()
    is IOException -> "网络错误：${e.message}"
    else -> "${e::class.simpleName}: ${e.message}"
}

/**
 * 支持 `--json` / `--agent` / `--max-requests` 的命令的基类。
 *
 * 三个开关**在子命令前后都能写**：根命令上也有一份（`feedhub --json search …`），
 * 子命令自己这一份让 `feedhub search … --json` 也能用。两边是"或"的关系。
 */
abstract class JsonCmd(name: String) : CliktCommand(name) {
    private val jsonFlag by option("--json", help = "每行一条 JSON（JSON Lines，schema 1；见 docs/cli.md）").flag()
    private val agentFlag by option("--agent", help = "agent 模式：拒绝写操作 / raw，限制本次运行的请求数").flag()
    private val maxRequestsFlag by option("--max-requests", help = "agent 模式每次运行的请求上限（只能往低调，默认 20）").int()

    /** 每个命令的第一行：把三个开关合进这次运行的 [Session]。 */
    protected fun configure(session: Session) = session.configure(jsonFlag, agentFlag, maxRequestsFlag)
}

/** 这次运行里 clikt 解析出来的 Session（`main` 兜底输出错误时要用）。 */
internal object CliRun {
    var session: Session? = null
}

class Feedhub(
    private val out: PrintStream = System.out,
    private val err: PrintStream = System.err,
) : CliktCommand(name = "feedhub") {
    private val record by option("--record", metavar = "DIR", help = "把响应 body 存到 DIR（录 fixtures 用）").path()
    private val proxy by option(
        "--proxy",
        metavar = "URL",
        help = "这次运行的代理：socks5://host:port、http://host:port 或 none（覆盖设置，不写回设置）",
    ).default("")
    private val json by option("--json", help = "所有命令都按 JSON Lines 输出（schema 1）").flag()
    private val agent by option("--agent", help = "agent 模式：拒绝写操作 / raw，限制每次运行的请求数").flag()
    private val maxRequests by option("--max-requests", help = "agent 模式每次运行的请求上限（只能往低调，默认 20）").int()

    override fun help(context: Context) = """
        聚合信息流的命令行工具。Cookie 存在 ${LocalFiles.cookiesFile}。

        给 agent 用：加 --json（或看 docs/cli.md）。`feedhub capabilities --json` 列出会用的命令、参数和支持的平台。

        平台：${installed.platforms.joinToString { "${it.key}=${it.label}" }}

        代理：默认按 App 的设置（${LocalFiles.stateFile} 的 ${CliProxySettings.KEY}：「代理」+「走代理的平台」）；
        境外平台没配代理时会直接报错，不会悄悄直连（fail-closed）。用 --proxy 可以覆盖这一次运行。
    """.trimIndent()

    override fun run() {
        // plugin / capabilities / cooldown / library 只读本地文件，不建 Env（不读 Cookie / 设置）
        val session = Session(
            json = json,
            agent = agent || System.getenv("FEEDHUB_AGENT").orEmpty().trim() in setOf("1", "true", "yes", "on"),
            maxRequests = maxRequests ?: DEFAULT_MAX_REQUESTS,
            out = out,
            err = err,
        )
        if (maxRequests != null) session.configure(jsonFlag = false, agentFlag = false, maxRequestsFlag = maxRequests)
        currentContext.findOrSetObject(SESSION_KEY) { session }
        CliRun.session = session
        when (currentContext.invokedSubcommand) {
            is PluginCmd, is CapabilitiesCmd, is CooldownCmd, is LibraryCmd -> return
            else -> Unit
        }
        currentContext.findOrSetObject { Env(record, proxy.takeIf { it.isNotBlank() }, session) }
    }
}

class PlatformsCmd : JsonCmd("platforms") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)

    override fun help(context: Context) = "列出已接入的平台和支持的流"

    override fun run() {
        configure(session)
        if (session.json) {
            // JSON 只回答"有哪些平台、能干什么"：不构造 Source（构造有副作用），也不发请求
            for (p in installed.platforms) {
                val plugin = installed[p] ?: continue
                session.emit(
                    "platform",
                    buildJsonObject {
                        put("id", JsonPrimitive(p.key))
                        put("label", JsonPrimitive(p.label))
                        put("feeds", jsonArray(plugin.capabilities.feeds.map { it.key }))
                        plugin.capabilities.search?.let { cap ->
                            put(
                                "search",
                                buildJsonObject {
                                    put("kinds", jsonArray(cap.kinds.map { it.name.lowercase() }))
                                    put("sorts", jsonArray(cap.sorts.map { it.name.lowercase() }))
                                    put("pageSize", JsonPrimitive(cap.pageSize))
                                    put("needsLogin", JsonPrimitive(cap.needsLogin))
                                },
                            )
                        }
                        put("commentSort", JsonPrimitive(plugin.capabilities.commentSort))
                        put("nestedReplies", JsonPrimitive(plugin.capabilities.nestedReplies))
                        put("overseas", JsonPrimitive(plugin.overseas))
                        put("desktop", JsonPrimitive(plugin.supports(PluginTarget.DESKTOP)))
                        // 故意不构造 Source（构造有副作用）：JSON 里不写 apiBase，要调试用不带 --json 的 platforms
                    },
                )
            }
            session.emit(
                "done",
                extra = mapOf("count" to JsonPrimitive(installed.platforms.size), "requests" to JsonPrimitive(session.requests.get())),
            )
            return
        }
        for (p in installed.platforms) {
            val s = env.source(p)
            out("${p.key.padEnd(8)}${p.label}\t${s.feeds.joinToString(" ") { it.key }}\t${s.apiBase}")
        }
    }

    private fun out(text: String) = println(text)
}

class FeedCmd : JsonCmd("feed") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "平台，如 bili")
    private val kind by argument(help = "recommend（默认）或 following").default("recommend")
    private val pages by option("-n", "--pages", help = "翻几页").int().default(1)
    private val cursor by option("--cursor", help = "从这个游标开始")

    override fun help(context: Context) = "打印某个平台的信息流（平台标成 18+ 的整条隐藏，见 AdultFilter）"

    override fun run() = runBlocking {
        configure(session)
        val p = platformOf(platform)
        val k = FeedKind.fromKey(kind) ?: throw CliFailure(ErrorCode.BAD_INPUT, "未知的流 $kind，可选：${FeedKind.entries.joinToString { it.key }}")
        val source = env.source(p)
        var next: Cursor? = cursor?.let(::Cursor)
        var n = 0
        var hidden = 0
        try {
            repeat(pages) {
                val page = source.feed(k, next)
                // 平台标成 18+ 的整条隐藏：不打印、不计数，
                // 只在结尾提一下有多少条被藏了，免得调试时以为接口没返回
                val shown = AdultFilter.visible(page.items)
                hidden += page.items.size - shown.size
                for (item in shown) {
                    n++
                    if (session.json) {
                        session.emit("item", FeedJson.encodeToJsonElement(FeedItem.serializer(), item))
                    } else {
                        session.say(formatItem(item, n))
                    }
                }
                next = page.next
                if (next == null) return@repeat
            }
        } catch (e: Exception) {
            throw session.classify(e)
        }
        if (session.json) {
            session.emit(
                "done",
                extra = buildMap {
                    put("count", JsonPrimitive(n))
                    put("hidden", JsonPrimitive(hidden))
                    put("command", JsonPrimitive("feed"))
                    put("requests", JsonPrimitive(session.requests.get()))
                    next?.let { put("next", JsonPrimitive(it.value)) }
                },
            )
        } else {
            session.say("共 $n 条；下一页游标：${next ?: "（没有了）"}" + if (hidden > 0) "（另有 $hidden 条被平台标为 18+，已隐藏）" else "")
        }
    }
}

class MixCmd : CliktCommand(name = "mix") {
    private val env by requireObject<Env>()
    private val only by option("--only", help = "只看这些平台，逗号分隔").split(",").default(emptyList())
    private val following by option("--following", help = "看关注流（默认推荐流）").flag()
    private val pages by option("-n", "--pages", help = "每个平台翻几页").int().default(1)

    override fun help(context: Context) = "把各平台的信息流交错合并后打印（统一信息流的雏形；平台标成 18+ 的整条隐藏）"

    override fun run() = runBlocking {
        val kind = if (following) FeedKind.FOLLOWING else FeedKind.RECOMMEND
        val lists = mutableListOf<List<FeedItem>>()
        var hidden = 0
        for (p in env.platforms(only)) {
            val s = env.source(p)
            if (kind !in s.feeds) {
                println("⚠ ${p.label}：不支持${kind.label}流"); continue
            }
            try {
                val items = mutableListOf<FeedItem>()
                var next: Cursor? = null
                repeat(pages) {
                    val page = s.feed(kind, next)
                    // 平台标成 18+ 的整条隐藏
                    val shown = AdultFilter.visible(page.items)
                    hidden += page.items.size - shown.size
                    items += shown
                    next = page.next ?: return@repeat
                }
                lists += items
            } catch (e: Exception) {
                println("⚠ ${describe(e)}")
            }
        }
        // 轮流各取一条；同一个 key 只出现一次
        val seen = mutableSetOf<String>()
        var n = 0
        val max = lists.maxOfOrNull { it.size } ?: 0
        for (i in 0 until max) for (l in lists) {
            val item = l.getOrNull(i) ?: continue
            if (seen.add(item.key)) println(formatItem(item, ++n))
        }
        println("共 $n 条" + if (hidden > 0) "（另有 $hidden 条被平台标为 18+，已隐藏）" else "")
    }
}

class WhoamiCmd : JsonCmd("whoami") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platforms by argument(help = "平台，不填就是全部").multiple()

    override fun help(context: Context) = "检查各平台的登录态"

    override fun run() = runBlocking {
        configure(session)
        var worst: ErrorCode? = null
        var failed = 0
        var total = 0
        for (p in env.platforms(platforms)) {
            total++
            session.cooldown.check(p) // 冷却期一个请求都不发（这条在拦截器里也会拦，这里是提前说清楚）
            val me = try {
                env.source(p).whoami()
            } catch (e: Exception) {
                failed++
                worst = session.reportPlatformFailure(e)
                continue
            }
            if (session.json) {
                session.emit(
                    "account",
                    buildJsonObject {
                        put("platform", JsonPrimitive(p.key))
                        put("label", JsonPrimitive(p.label))
                        put("loggedIn", JsonPrimitive(me != null))
                        me?.let { user ->
                            put("user", FeedJson.encodeToJsonElement(feedhub.core.UserRef.serializer(), user))
                        }
                    },
                )
            } else {
                session.say("${p.label.padEnd(4, '　')} " + (me?.let { "已登录  ${it.name}（${it.id}）" } ?: "未登录"))
            }
        }
        if (session.json) {
            session.emit(
                "done",
                extra = buildMap {
                    put("count", JsonPrimitive(total))
                    put("failed", JsonPrimitive(failed))
                    put("requests", JsonPrimitive(session.requests.get()))
                },
            )
        }
        // 一家都没成功：用那个错误码退出；部分失败算命令跑通了（错误已经在 stderr 上一行行给了）
        if (failed > 0 && failed == total) throw CliFailure(worst ?: ErrorCode.INTERNAL, "所有平台都失败了")
    }
}

class RawCmd : CliktCommand(name = "raw") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "平台")
    private val url by argument(help = "完整 URL，或以 / 开头的路径（相对于该平台的 apiBase）。URL 里带 = 时会被当成选项：参数改用 -q k=v，或在 URL 前加 --")
    private val method by option("-X", "--method").default("GET")
    private val query by option("-q", help = "追加 query 参数 k=v，可重复").multiple()
    private val form by option("-d", help = "表单参数 k=v，可重复（会改用 POST）").multiple()
    private val headers by option("-H", help = "请求头 'Name: value'，可重复").multiple()
    private val noSign by option("--no-sign", help = "不加签名").flag()
    private val jsonBody by option("--json", help = "以 application/json 发送这段 body（会改用 POST）")
    private val include by option("-i", help = "同时打印状态码和响应头").flag()
    private val noPretty by option("--no-pretty", help = "不格式化 JSON").flag()

    override fun help(context: Context) = "按平台规则（Cookie、请求头、签名）调任意接口，打印原始响应。agent 模式下拒绝（能绕过所有约束）"

    private fun kv(s: String, sep: Char): Pair<String, String> {
        val i = s.indexOf(sep)
        if (i <= 0) throw CliFailure(ErrorCode.BAD_INPUT, "参数格式不对：$s")
        return s.substring(0, i).trim() to s.substring(i + 1).trim()
    }

    override fun run() = runBlocking {
        // raw 能调任意接口、绕过限速 / 冷却 / 预算的所有规则：agent 模式下直接拒绝
        session.rejectInAgentMode("raw")
        val source = env.source(platformOf(platform))
        val req = RawRequest(
            url = url,
            method = if ((form.isNotEmpty() || jsonBody != null) && method == "GET") "POST" else method,
            query = query.map { kv(it, '=') },
            form = form.map { kv(it, '=') },
            json = jsonBody,
            headers = headers.map { kv(it, ':') },
            sign = !noSign,
        )
        val res = try {
            source.raw(req)
        } catch (e: Exception) {
            throw session.classify(e)
        }
        if (session.json) {
            session.emit(
                "raw",
                extra = buildMap {
                    put("status", JsonPrimitive(res.code))
                    put("url", JsonPrimitive(res.url))
                    put("body", JsonPrimitive(res.body))
                    if (include) {
                        put(
                            "headers",
                            buildJsonObject { res.headers.forEach { (k, v) -> put(k, JsonPrimitive(v)) } },
                        )
                    }
                },
            )
            return@runBlocking
        }
        if (include) {
            println("HTTP ${res.code}  ${res.url}")
            res.headers.forEach { (k, v) -> println("$k: $v") }
            println()
        }
        val body = if (noPretty) res.body else runCatching {
            PrettyJson.encodeToString(JsonElement.serializer(), Json.parseToJsonElement(res.body))
        }.getOrDefault(res.body)
        println(body)
    }
}

class CookieCmd : CliktCommand(name = "cookie") {
    override fun help(context: Context) = "管理本地 Cookie（只显示名字，不显示值）"
    override fun run() = Unit
}

class CookieImportCmd : CliktCommand(name = "import") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "平台")
    private val replace by option("--replace", help = "先清掉该平台原有的 Cookie").flag()

    override fun help(context: Context) = """
        从标准输入读入 Cookie 并保存（agent 模式下拒绝：这是写操作）。支持两种格式：
        1) 浏览器开发者工具 → Network → 任一请求 → Request Headers 里 cookie 的值（a=1; b=2）
        2) 浏览器扩展导出的 JSON 数组 [{"name":..,"value":..,"domain":..}]
    """.trimIndent()

    override fun run() = runBlocking {
        session.rejectInAgentMode("导入 Cookie")
        val p = platformOf(platform)
        val source = env.source(p)
        val domain = cookieDomain(p)
        val text = generateSequence(::readLine).joinToString("\n").trim()
        if (text.isEmpty()) throw CliFailure(ErrorCode.BAD_INPUT, "标准输入是空的。用法：feedhub cookie import $platform < cookie.txt")
        val cookies = if (text.startsWith("[")) parseJsonCookies(text, domain) else parseCookieHeader(text, domain)
        if (cookies.isEmpty()) throw CliFailure(ErrorCode.BAD_INPUT, "没解析出任何 Cookie")
        if (replace) env.ctx.cookies.clearDomain(domain)
        env.ctx.cookies.put("https://www.${domain.trimStart('.')}/".toHttpUrl(), cookies)
        println("已导入 ${cookies.size} 个：${cookies.joinToString(" ") { it.name }}")
        val missing = source.login.requiredCookies - cookies.map { it.name }.toSet()
        if (missing.isNotEmpty()) System.err.println("⚠ 缺少 ${missing.joinToString()}，可能登录不上")
        val me = try {
            source.whoami()?.let { "已登录：${it.name}（${it.id}）" } ?: "仍是未登录状态"
        } catch (e: Exception) {
            "检查登录态出错：${describe(e)}"
        }
        println(me)
    }

    private fun parseJsonCookies(text: String, defaultDomain: String): List<Cookie> {
        val arr = Json.parseToJsonElement(text) as? JsonArray
            ?: throw CliFailure(ErrorCode.BAD_INPUT, "不是 JSON 数组")
        val expires = System.currentTimeMillis() + 365L * 24 * 3600 * 1000
        return arr.mapNotNull { el ->
            val o = el as? JsonObject ?: return@mapNotNull null
            val name = o["name"]?.jsonPrimitive?.content ?: return@mapNotNull null
            val value = o["value"]?.jsonPrimitive?.content ?: return@mapNotNull null
            val domain = (o["domain"]?.jsonPrimitive?.content ?: defaultDomain).trimStart('.')
            Cookie.Builder().name(name).value(value).domain(domain)
                .path(o["path"]?.jsonPrimitive?.content ?: "/").expiresAt(expires).build()
        }
    }
}

class CookieListCmd : CliktCommand(name = "list") {
    private val env by requireObject<Env>()
    override fun help(context: Context) = "按平台列出已保存的 Cookie 名"
    override fun run() {
        val all = env.ctx.cookies.all()
        val shown = mutableSetOf<Cookie>()
        for (p in installed.platforms) {
            val login = env.source(p).login
            val d = login.cookieDomain.trimStart('.')
            val mine = all.filter { it.domain == d || it.domain.endsWith(".$d") }
            shown += mine
            val missing = login.requiredCookies - mine.map { it.name }.toSet()
            val status = when {
                mine.isEmpty() -> "（没有）"
                missing.isEmpty() -> "${mine.size} 个，关键 Cookie 齐全"
                else -> "${mine.size} 个，缺 ${missing.joinToString()}"
            }
            println("${p.label}  $status")
            if (mine.isNotEmpty()) println("    " + mine.joinToString(" ") { it.name })
        }
        val rest = all - shown
        if (rest.isNotEmpty()) println("其他域：" + rest.groupBy { it.domain }.entries.joinToString { "${it.key}(${it.value.size})" })
    }
}

class CookieClearCmd : CliktCommand(name = "clear") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "平台")
    override fun help(context: Context) = "删除某个平台的全部 Cookie（agent 模式下拒绝）"
    override fun run() {
        session.rejectInAgentMode("清除 Cookie")
        val p = platformOf(platform)
        env.ctx.cookies.clearDomain(cookieDomain(p))
        println("已清除 ${p.label} 的 Cookie")
    }
}

/**
 * 只删指定名字的 Cookie（登录态保留）。调试用：删掉酷安的 `validate` 可以让 App / cli
 * 重新走一次人机验证，不影响 uid / username / token。
 */
class CookieRemoveCmd : CliktCommand(name = "remove") {
    private val env by requireObject<Env>()
    private val session by requireObject<Session>(SESSION_KEY)
    private val platform by argument(help = "平台")
    private val names by argument(help = "要删的 Cookie 名，例如 validate").multiple()
    override fun help(context: Context) = "只删指定名字的 Cookie（登录态保留；agent 模式下拒绝）"
    override fun run() {
        session.rejectInAgentMode("删 Cookie")
        if (names.isEmpty()) throw CliFailure(ErrorCode.BAD_INPUT, "要删哪些 Cookie？例如 validate")
        val p = platformOf(platform)
        val domain = cookieDomain(p)
        env.ctx.cookies.remove(domain, names.toSet())
        println("已从 $domain 删除：${names.joinToString(" ")}")
    }
}

private fun buildRoot(out: PrintStream, err: PrintStream): Feedhub = Feedhub(out, err)
    .subcommands(
        FeedCmd(), MixCmd(), SearchCmd(), WhoamiCmd(), LoginCmd(), RawCmd(), PlatformsCmd(),
        DetailCmd(), CommentsCmd(), RepliesCmd(), PlayCmd(), DanmakuCmd(),
        FollowingsCmd(), FollowCmd(), UnfollowCmd(),
        CookieCmd().subcommands(CookieImportCmd(), CookieListCmd(), CookieClearCmd(), CookieRemoveCmd()),
        PluginCmd().subcommands(PluginListCmd(), PluginAddCmd(), PluginRemoveCmd(), PluginNewCmd()),
        LibraryCmd().subcommands(LibraryFavoritesCmd(), LibraryHistoryCmd()),
        CooldownCmd().subcommands(CooldownListCmd(), CooldownClearCmd()),
        CapabilitiesCmd(),
    )

/**
 * cli 的入口：**解析 + 跑命令 + 把任何异常变成错误码 / 一行 JSON**。
 *
 * 返回进程退出码（[ErrorCode.exitCode] 那张表）；`main` 只负责 `exitProcess`。
 * 拆成这个函数是为了测试能在同一个进程里跑命令、拿到退出码和输出。
 */
fun runCli(args: List<String>, out: PrintStream = System.out, err: PrintStream = System.err): Int {
    val root = buildRoot(out, err)
    CliRun.session = null
    return try {
        root.parse(args)
        0
    } catch (e: PrintHelpMessage) {
        // --help：照 clikt 的规矩打出来，退出码 0（`--json --help` 也打帮助，不当成错误）
        root.echoFormattedHelp(e)
        e.statusCode
    } catch (e: PrintCompletionMessage) {
        root.echoFormattedHelp(e)
        e.statusCode
    } catch (e: PrintMessage) {
        // --version 之类：照 clikt 的规矩打出来，退出码 0
        root.echoFormattedHelp(e)
        e.statusCode
    } catch (e: CliktError) {
        val json = "--json" in args || CliRun.session?.json == true
        val session = CliRun.session
        if (json) {
            val failure = CliFailure(
                ErrorCode.BAD_INPUT,
                e.message?.takeIf { it.isNotBlank() }
                    ?: "参数不对（用 feedhub capabilities --json 看命令和参数；不带 --json 跑一次看完整用法）",
            )
            if (session != null) {
                session.reportError(failure)
            } else {
                err.println(jsonErrorLine(failure))
            }
            ErrorCode.BAD_INPUT.exitCode
        } else {
            root.echoFormattedHelp(e)
            e.statusCode
        }
    } catch (e: Throwable) {
        val session = CliRun.session
        val failure = session?.classify(e) ?: e.toCliFailure()
        // 风控 / 验证码：写冷却（单个平台命令走到了这里，说明没有命令层的 reportPlatformFailure）
        val platform = failure.platform
        if (platform != null && (failure.error == ErrorCode.RISK_CONTROL || failure.error == ErrorCode.CAPTCHA_REQUIRED)) {
            session?.cooldown?.mark(platform, failure.error.key)
        }
        if (session != null) session.reportError(failure) else err.println(jsonErrorLine(failure))
        // 调试用：FEEDHUB_CLI_DEBUG=1 时把堆栈打到 stderr（默认只给一行错误）
        if (System.getenv("FEEDHUB_CLI_DEBUG") != null) e.printStackTrace(err)
        failure.error.exitCode
    }
}

/** 还没建 Session（参数解析就失败了）时打一行 JSON 错误用的。 */
private fun jsonErrorLine(f: CliFailure): String = buildJsonObject {
    put("schema", JsonPrimitive(SCHEMA_VERSION))
    put("type", JsonPrimitive("error"))
    put("error", JsonPrimitive(f.error.key))
    f.platform?.let { put("platform", JsonPrimitive(it.key)) }
    put("message", JsonPrimitive(f.message ?: ""))
    put("retryable", JsonPrimitive(f.retryable))
}.toString()

fun main(args: Array<String>) {
    val exit = runCli(args.toList())
    // OkHttp 的线程池不是守护线程，不显式退出 JVM 会再挂一分钟
    exitProcess(exit)
}
