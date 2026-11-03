package feedhub.cli

import feedhub.core.FeedJson
import feedhub.core.Platform
import feedhub.core.RiskControlException
import feedhub.core.local.FileLocks
import feedhub.ratelimit.CooldownCheck
import feedhub.ratelimit.RustCooldownStore
import feedhub.ratelimit.RustCrossProcessRateLimit
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonPrimitive
import okhttp3.Interceptor
import okhttp3.Response
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.StandardCopyOption

/**
 * cli 自己的缓存目录（**不和 App 的设置混**）：
 * - `~/.cache/feedhub/cli-rate/`：跨进程限速的状态（每个平台一个文件 + 一把锁）；
 * - `~/.cache/feedhub/cooldown/`：风控冷却标记（每个平台一个 json）。
 *
 * 遵守 `XDG_CACHE_HOME`；`FEEDHUB_HOME` 只影响 `~/.config/feedhub`（Cookie / 设置），缓存照旧按 XDG 走。
 */
object CliPaths {
    val cacheHome: Path by lazy {
        System.getenv("XDG_CACHE_HOME")?.takeIf { it.isNotBlank() }?.let { Paths.get(it) }
            ?: Paths.get(System.getProperty("user.home"), ".cache")
    }
    val feedhubCache: Path get() = cacheHome.resolve("feedhub")
    val rateDir: Path get() = feedhubCache.resolve("cli-rate")
    val cooldownDir: Path get() = feedhubCache.resolve("cooldown")
}

/**
 * 跨进程限速：**同一个平台**的最小请求间隔，状态落在
 * `~/.cache/feedhub/cli-rate/<平台>.rate`，用 `<平台>.rate.lock` 的排他文件锁排队
 * （[FileLocks]）。
 *
 * 这里只剩三件薄活——
 * **拿文件锁**、**睡**、**写文件**。两个 cli 进程同时跑时，后一个会一直等到前一个的间隔过去，
 * 所以不会出现"两个进程同时打同一个平台"。
 *
 * 为什么锁在"等之前"就要拿到：放锁之后再等，两个进程会同时在等、然后同时发。这里锁住的是
 * **读上次时间 → 等够间隔 → 写下这次时间**这一整段，天然是"排队"。
 *
 * 间隔按平台写在一张表里（值见 [RustCrossProcessRateLimit.DEFAULT_INTERVALS_MS]）；不在表里的平台用
 * [RustCrossProcessRateLimit.DEFAULT_INTERVAL_MS]。
 */
class CrossProcessRateLimit(
    private val dir: Path,
    private val clock: () -> Long = System::currentTimeMillis,
    private val sleep: (Long) -> Unit = { Thread.sleep(it) },
    /** 换过的间隔表（**测试 / 探针用**：真实间隔 1.5～3 秒等不起）。表里没写的平台照旧走默认。 */
    intervals: Map<String, Long> = emptyMap(),
    defaultIntervalMs: Long = 0,
) {
    private val rusty = if (intervals.isEmpty() && defaultIntervalMs <= 0) {
        RustCrossProcessRateLimit.create(dir)
    } else {
        RustCrossProcessRateLimit.create(dir, intervals, defaultIntervalMs)
    }

    fun intervalFor(platform: Platform): Long = rusty.intervalFor(platform.key)

    /** 等到这个平台"上一次请求 + 间隔"之后，记下这次请求的时间。返回实际等了多久（毫秒）。 */
    fun acquire(platform: Platform): Long {
        val lock = FileLocks.lockFileFor(dir.resolve("${platform.key}.rate"))
        return FileLocks.withLock(lock) {
            val now = clock()
            val wait = rusty.waitMs(platform.key, now)
            if (wait > 0) sleep(wait)
            rusty.commit(platform.key, clock())
            wait
        }
    }

    /** 这个平台上次请求记在什么时候（没有记录 / 读不出来返回 null）。 */
    fun lastRequestAt(platform: Platform): Long? = rusty.lastRequestAt(platform.key)

    companion object {
        /**
         * 国内平台 1.5 秒；搜索这种一次要打两三个请求的接口也够慢，风控雷达上不明显。
         * 境外几家（X / 小红书 / 抖音 / TikTok）更长：它们本来就走代理、也更敏感。
         * 单位毫秒，**只往长调**（变短要先问用户）。**值在 Rust**，这里只是转出来给 `capabilities` 用。
         */
        val DEFAULT_INTERVAL_MS: Long get() = RustCrossProcessRateLimit.DEFAULT_INTERVAL_MS

        val DEFAULT_INTERVALS_MS: Map<String, Long> get() = RustCrossProcessRateLimit.DEFAULT_INTERVALS_MS
    }
}

/**
 * 风控冷却：**任何命令**遇到风控信号 / 验证码，就给这个平台写一个 30 分钟的标记；
 * 冷却期内所有 cli 对这个平台的请求**一个都不发**，直接返回 `cooldown`。
 *
 * 读的时候顺手删掉）。
 * 这个类只把结论翻成 cli 的 `CliFailure`。
 *
 * **App 不受影响**：App 是用户本人在用，这里只是 cli 自己的闸。
 */
class CooldownStore(
    private val dir: Path,
    private val clock: () -> Long = System::currentTimeMillis,
) {
    private val rusty = RustCooldownStore.create(dir)

    /** 正在冷却就抛 `cooldown`（`retryAfterMs` 告诉 agent 还要等多久）；过期了就顺手删掉。 */
    fun check(platform: Platform) {
        val now = clock()
        val cooling = rusty.check(platform.key, now) as? CooldownCheck.Cooling ?: return
        throw CliFailure(
            ErrorCode.COOLDOWN,
            "${platform.label}（${platform.key}）正在风控冷却期（原因 ${cooling.reason}），" +
                "还剩 ${(cooling.retryAfterMs + 999) / 1000} 秒。冷却期内 cli 对它一个请求都不发；" +
                "等冷却过去，或者让用户确认后跑 feedhub cooldown clear ${platform.key}。",
            platform,
            retryable = true,
            extra = mapOf(
                "retryAfterMs" to JsonPrimitive(cooling.retryAfterMs),
                "until" to JsonPrimitive(cooling.until),
                "reason" to JsonPrimitive(cooling.reason),
            ),
        )
    }

    /** 写一笔冷却（覆盖这个平台原来的标记）。失败只打一行到 stderr，不能盖住原来的错误。 */
    fun mark(platform: Platform, reason: String) {
        rusty.mark(platform.key, reason, clock())
    }

    /** 现在在冷却的平台，按 `until` 从早到晚。 */
    fun list(): List<Entry> = rusty.list(clock()).map { Entry(it.platform, it.until, it.at, it.reason) }

    /** 清掉一个平台的冷却。清掉了返回 true。 */
    fun clear(platform: Platform): Boolean = rusty.clear(platform.key, clock())

    /** 一条冷却记录（字段和 Rust 的那份一样；cli 的 `cooldown list` 输出用它）。 */
    data class Entry(val platform: String, val until: Long, val at: Long, val reason: String)

    companion object {
        /** 冷却时长：30 分钟。 */
        const val DEFAULT_TTL_MS: Long = RustCooldownStore.DEFAULT_TTL_MS
    }
}

/**
 * 所有请求的最后一道闸（应用拦截器，**在连接之前**）：冷却 → 预算 → 跨进程限速 → 发请求；
 * 回来时把 403 / 412 / 429 当成风控信号（写冷却 + 抛 [RiskControlException]，**不重试**）。
 *
 * 只认"这个域名属于哪个平台"（[platformOfHost]，和代理守卫用同一份插件描述的 `hosts`）；
 * 认不出的域名照旧发，只算进预算。
 */
class CliGateInterceptor(
    private val session: Session,
    private val platformOfHost: (String) -> Platform?,
    /** agent 模式才给（[RequestBudget]）；null = 不限。 */
    private val budget: RequestBudget? = null,
) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val platform = platformOfHost(chain.request().url.host)
        if (platform != null) session.cooldown.check(platform)
        budget?.take()
        session.requests.incrementAndGet()
        if (platform != null) session.rateLimit.acquire(platform)
        val response = chain.proceed(chain.request())
        if (platform != null && response.code in RISK_STATUS) {
            session.cooldown.mark(platform, "http_${response.code}")
            response.close()
            throw RiskControlException(
                platform,
                response.code.toString(),
                "HTTP ${response.code}（按风控信号处理：不重试，已给 ${platform.key} 写 30 分钟冷却）",
            )
        }
        return response
    }

    companion object {
        /** 风控状态码。 */
        val RISK_STATUS: Set<Int> = setOf(403, 412, 429)
    }
}

/**
 * 小文件落盘：先写临时文件再原子换名（别的进程正读时不会读到半截）。
 *
 * 同一套"临时文件 + 原子替换"）。这个函数只剩一处调用：录制模式的夹具目录。留着它是为了不动那条
 * 与限速无关的路径。
 */
internal fun writeAtomically(path: Path, text: String) {
    path.parent?.let { Files.createDirectories(it) }
    val tmp = path.resolveSibling(path.fileName.toString() + ".tmp-" + ProcessHandle.current().pid())
    Files.writeString(tmp, text)
    try {
        Files.move(tmp, path, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
    } catch (_: java.nio.file.AtomicMoveNotSupportedException) {
        Files.move(tmp, path, StandardCopyOption.REPLACE_EXISTING)
    }
}
