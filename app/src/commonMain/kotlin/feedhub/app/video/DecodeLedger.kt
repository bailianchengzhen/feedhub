package feedhub.app.video

import feedhub.core.KeyValueStore
import feedhub.core.VideoCodec
import feedhub.player.LedgerPersist
import feedhub.player.RustDecodeLedger
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * 播放过程中量出来的"这台机器各编码能稳定实时解到多少像素/秒"。**会落盘**（[attach]）。
 *
 * 逐字节和原来一样）全在那边。
 *
 * 这一层只剩三件宿主的事：
 * 1. **读盘 / 写盘**：`KeyValueStore` 的 `app.decodeLedger` 键（Rust 不读存储，它只给 [LedgerPersist]）；
 * 2. **环境指纹**：`decodeEnvironmentFingerprint()`（问 GStreamer / libva，是操作系统的事）；
 * 3. **日期串**：界面那一行的 `yyyy-MM-dd`（系统时区 / 地区）。
 *
 * 为什么还是 `object`：原来 25 处调用点写的是 `DecodeLedger.record(...)` / `DecodeLedger.attach(...)`，
 * 而且它天然是"进程一份"（引擎在播放线程写、设置页在界面线程读）。Rust 那侧带 `Mutex`，所以这里不加锁。
 */
object DecodeLedger {
    /** 落盘用的键（放在 App 的 [KeyValueStore]，桌面是 `~/.config/feedhub/state.json`）。 */
    const val KEY = "app.decodeLedger"

    /** 记下的上限多久没有新的实测就作废（毫秒，真值在 Rust）。 */
    const val TTL_MS: Long = 30L * 24 * 60 * 60 * 1000

    private val rust = RustDecodeLedger()

    /**
     * 交给桌面播放引擎的那份句柄。
     * 只有 `RustGstPlayerEngine.attachLedger` 用得到。
     */
    internal fun rustLedger(): RustDecodeLedger = rust

    /** 接上的存储（Rust 只给要写什么，写由这里做）。 */
    @Volatile private var store: KeyValueStore? = null

    /** 环境指纹（每次现算；拿不到就是 null → 既不读也不写旧记录）。 */
    @Volatile private var fingerprint: (() -> String?)? = null

    /**
     * 接上落盘（桌面组装时调一次）。[fingerprint] 每次现算（它要问 GStreamer / libva，第一次会比较慢）；
     * 拿不到当前指纹（没装 GStreamer、VA-API 起不来）就**不读也不写**旧记录——宁可不落盘，也不拿一份
     * 不知道对谁有效的记录去选流。
     */
    fun attach(store: KeyValueStore?, fingerprint: () -> String?) {
        this.store = store
        this.fingerprint = fingerprint
        rust.attach(store?.get(KEY), fingerprint())
        // 读进来之后 Rust 会按"指纹对不对 / 有没有过期"决定要不要重写存储：这里**立刻**照做一次，
        // 不留一条读不出来的死记录（Kotlin 原版是在第一次 `ensureLoaded` 里顺手清掉的）
        flushAfterLoad()
    }

    /**
     * 记一次实测：[codec] 在 [pixelsPerSecond] 这个负载下稳不稳。
     * 不稳就把上限压到它的三分之一；稳的样本不动上限。
     */
    fun record(codec: VideoCodec?, pixelsPerSecond: Long, smooth: Boolean, nowMs: Long = System.currentTimeMillis()) {
        rust.record(codec, pixelsPerSecond, smooth, nowMs)
        flush()
    }

    /** 这种编码现在记下的上限；没记过（或已过期作废）返回 null = 不限制。 */
    fun rate(codec: VideoCodec, nowMs: Long = System.currentTimeMillis()): Long? =
        rust.rate(codec, nowMs).also { flush() }

    /** 现在有效的全部上限（[currentDecodeCapability] 用）。 */
    fun snapshot(nowMs: Long = System.currentTimeMillis()): Map<VideoCodec, Long> =
        rust.snapshot(nowMs).also { flush() }

    /** 界面那一行要显示的内容（每种有记录的编码一句），如 `AV1 ≤ 1080P60`；没有就 null。 */
    fun summaryText(nowMs: Long = System.currentTimeMillis()): String? =
        rust.summaryText(nowMs) { dayOf(it) }.also { flush() }

    /** 「清除，重新测」：内存和落盘一起清掉，下一次播放重新学。 */
    fun clear() {
        rust.clear()
        flush()
    }

    /** 测试用：只清内存，不动落盘；也把接上的 store 断掉。 */
    fun reset() {
        rust.reset()
        store = null
        fingerprint = null
    }

    /** 检查工具 / 截图用：换成一份**只在内存里**的假记录（不落盘，绝不改用户的数据）。 */
    internal fun seedForDemo(codec: VideoCodec, cap: Long, at: Long) = rust.seedForDemo(codec, cap, at)

    /** 有没有测出过结果（检查工具用）。 */
    val isEmpty: Boolean
        get() = rust.isEmpty(System.currentTimeMillis()).also { flush() }

    /**
     * `attach` 之后立刻问一次"要不要重写存储"：Rust 那边是惰性装载的（第一次用到才解析），所以这里
     * 用一个**什么都不改**的查询把它叫醒（`isEmpty` → `ensure_loaded`），然后照它的动作做。
     */
    private fun flushAfterLoad() {
        runCatching { rust.loadNow(System.currentTimeMillis()) }
        flush()
    }

    /** 把 Rust 攒下的落盘动作照做（`attach` 之后第一次调用时也会把"读进来"这件事落回去）。 */
    internal fun flush() {
        when (val action = rust.takePersist()) {
            null -> Unit
            LedgerPersist.Delete -> store?.put(KEY, null)
            is LedgerPersist.Write -> store?.put(KEY, action.text)
        }
    }

    /**
     * 落盘内容（调试 / 测试用）：直接问 Rust 要。
     *
     * 注意：`attach` 之后**第一次**调这个方法之前要先做点事（`record` / `rate` / `snapshot`），否则 Rust
     * 那边还没解析存档——和原来的口径一样（惰性装载）。
     */
    internal fun encoded(): String = rust.encoded()

    private fun dayOf(ms: Long): String = SimpleDateFormat("yyyy-MM-dd", Locale.getDefault()).format(Date(ms))
}
