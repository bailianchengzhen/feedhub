package feedhub.player

import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiDanmakuTimeline
import uniffi.feedhub_ffi.FfiDecodeLedger
import uniffi.feedhub_ffi.FfiLedgerSummaryRow
import uniffi.feedhub_ffi.FfiPlayFailure
import uniffi.feedhub_ffi.FfiPlaybackSession
import uniffi.feedhub_ffi.FfiPlaybackState
import uniffi.feedhub_ffi.FfiRelay
import uniffi.feedhub_ffi.FfiRelayEntry

/**
 * 播放器决策层里**有跨调用状态**的那几件的 Kotlin 门面：
 * 解码账本（[RustDecodeLedger]）、播放状态机（[RustPlaybackSession]）、中转映射（[RustRelay]）。
 *
 * 三件都在 Rust 那边带 `Mutex`（播放引擎 / 弹幕画线程 / 界面线程会同时碰），Kotlin 这边不再加锁。
 */
object RustPlayerObjects {
    internal inline fun <T> native(block: () -> T): T = try {
        RustNative.ensureLoaded()
        block()
    } catch (e: UnsatisfiedLinkError) {
        throw IllegalStateException("播放器决策：Rust 核心加载失败（找不到 libfeedhub_ffi.so）：${e.message}", e)
    }
}

/** 账本界面那一行的一格。 */
data class LedgerSummaryRow(
    val codec: feedhub.core.VideoCodec,
    val cap: Long,
    val label: String,
    val measuredAtMs: Long?,
)

/**
 * **解码能力账本**（Rust `FfiDecodeLedger` 的 Kotlin 面，替代 `object DecodeLedger`）。
 *
 * 一个进程一个实例（`AppGraph` 里建、`GstPlayerEngine` 和设置页共用）。所有方法直接落到 Rust 的锁里。
 * **不读时钟**：`nowMs` 由调用方给（测试注入假钟）。
 */
class RustDecodeLedger {
    private val inner = RustPlayerObjects.native { FfiDecodeLedger() }

    /**
     * 交给别处共享的那份句柄（**桌面播放引擎的帧节奏记账**要往同一个账本里写，不是复制一份）。
     *
     * 只给 `RustGstPlayerEngine.attachLedger` 用——`FfiDecodeLedger` 现在是 `Arc<Mutex<…>>`，
     * 两边看到的是同一份数据。
     */
    internal fun ffi(): FfiDecodeLedger = inner

    /** 接上落盘：`stored` 是 `KeyValueStore` 读出来的原文（没有就 null），`fingerprint` 是当前环境指纹。 */
    fun attach(stored: String?, fingerprint: String?) = RustPlayerObjects.native {
        inner.attach(stored, fingerprint)
    }

    /** 把接进来的存档立刻解析掉（读完马上清理过期 / 指纹对不上的记录）。 */
    fun loadNow(nowMs: Long) = RustPlayerObjects.native { inner.loadNow(nowMs) }

    /** 记一次实测。 */
    fun record(codec: feedhub.core.VideoCodec?, pixelsPerSecond: Long, smooth: Boolean, nowMs: Long) =
        RustPlayerObjects.native {
            inner.record(codec?.let { RustPlayer.run { it.toFfi() } }, pixelsPerSecond, smooth, nowMs)
        }

    /** 这种编码现在记下的上限（null = 不限制）。 */
    fun rate(codec: feedhub.core.VideoCodec, nowMs: Long): Long? = RustPlayerObjects.native {
        inner.rate(RustPlayer.run { codec.toFfi() }, nowMs)?.toLong()
    }

    /** 现在有效的全部上限。 */
    fun snapshot(nowMs: Long): Map<feedhub.core.VideoCodec, Long> = RustPlayerObjects.native {
        inner.snapshot(nowMs).associate { entry ->
            RustPlayer.run { entry.codec.toCore() } to entry.pixelsPerSecond.toLong()
        }
    }

    /** 现在有效的全部上限 + 调用方给的 `hardware`。 */
    fun capability(hardware: Set<feedhub.core.VideoCodec>, nowMs: Long): DecodeCapability =
        RustPlayerObjects.native {
            DecodeCapability(
                hardware = hardware,
                smoothPixelRate = inner
                    .capability(hardware.map { RustPlayer.run { it.toFfi() } }, nowMs)
                    .smoothPixelRate
                    .associate { entry ->
                        RustPlayer.run { entry.codec.toCore() } to entry.pixelsPerSecond.toLong()
                    },
            )
        }

    /**
     * 界面那一行（`AV1 ≤ 1080P60`）。**日期串由宿主拼**（系统时区 / 地区是操作系统的
     * 事）：[dayOf] 把时刻格式化成 `yyyy-MM-dd`。
     */
    fun summaryText(nowMs: Long, dayOf: (Long) -> String): String? = RustPlayerObjects.native {
        val rows = inner.summary(nowMs)
        if (rows.isEmpty()) {
            null
        } else {
            rows.sortedBy { it.codec.ordinal }
                .joinToString("；") { row ->
                    val codec = RustPlayer.run { row.codec.toCore() }
                    val head = "${RustPlayer.codecLabel(codec)} ≤ ${row.label}"
                    val at = row.measuredAtMs
                    if (at == null) head else "$head（${dayOf(at)} 实测）"
                }
        }
    }

    /** 有没有测出过结果。 */
    fun isEmpty(nowMs: Long): Boolean = RustPlayerObjects.native { inner.isEmpty(nowMs) }

    /** 「清除，重新测」。 */
    fun clear() = RustPlayerObjects.native { inner.clear() }

    /** 测试 / 检查工具：只清内存。 */
    fun reset() = RustPlayerObjects.native { inner.reset() }

    /** 检查工具 / 截图：灌一份只在内存里的假记录。 */
    fun seedForDemo(codec: feedhub.core.VideoCodec, cap: Long, at: Long) = RustPlayerObjects.native {
        inner.seedForDemo(RustPlayer.run { codec.toFfi() }, cap.toULong(), at)
    }

    /** 取走待落盘的动作（宿主每次调用完问一遍）。 */
    fun takePersist(): LedgerPersist? = RustPlayerObjects.native {
        inner.takePersist()?.let { RustPlayer.run { persistFromFfi(it) } }
    }

    /** 落盘内容（不看"有没有待写"）：测试用。 */
    fun encoded(): String = RustPlayerObjects.native { inner.encoded() }
}

/**
 * **播放状态机**（Rust `FfiPlaybackSession` 的 Kotlin 面）：`PlaybackSession` 里"不是界面也不是引擎"
 * 的那一半。每个方法返回宿主要做的事（[PlayEffect]）。
 */
class RustPlaybackSession {
    private val inner = RustPlayerObjects.native { FfiPlaybackSession() }

    fun togglePlay(state: PlaybackSnapshot): PlayEffect = RustPlayerObjects.native {
        inner.togglePlay(state.toFfi()).toCore()
    }

    fun pauseForBackground(state: PlaybackSnapshot): PlayEffect = RustPlayerObjects.native {
        inner.pauseForBackground(state.toFfi()).toCore()
    }

    fun resumeFromBackground(state: PlaybackSnapshot): PlayEffect? = RustPlayerObjects.native {
        inner.resumeFromBackground(state.toFfi())?.toCore()
    }

    fun seekBy(state: PlaybackSnapshot, deltaMs: Long): PlayEffect = RustPlayerObjects.native {
        inner.seekBy(state.toFfi(), deltaMs).toCore()
    }

    fun setSpeed(rate: Float): PlayEffect = RustPlayerObjects.native { inner.setSpeed(rate).toCore() }

    fun holdSpeed(state: PlaybackSnapshot, rate: Float): PlayEffect = RustPlayerObjects.native {
        inner.holdSpeed(state.toFfi(), rate).toCore()
    }

    fun releaseSpeed(): PlayEffect? = RustPlayerObjects.native { inner.releaseSpeed()?.toCore() }

    fun longPressStart(state: PlaybackSnapshot): PlayEffect = RustPlayerObjects.native {
        inner.longPressStart(state.toFfi()).toCore()
    }

    fun longPressSwipeUp(): Boolean = RustPlayerObjects.native { inner.longPressSwipeUp() }

    fun longPressEnd(): PlayEffect? = RustPlayerObjects.native { inner.longPressEnd()?.toCore() }

    fun stepSpeed(state: PlaybackSnapshot, delta: Int): PlayEffect = RustPlayerObjects.native {
        inner.stepSpeed(state.toFfi(), delta).toCore()
    }

    fun saveProgress(
        partIndex: Int,
        positionMs: Long,
        durationMs: Long,
        partId: String?,
    ): List<PlayEffect> = RustPlayerObjects.native {
        inner.saveProgress(partIndex, positionMs, durationMs, partId).map { it.toCore() }
    }

    fun speedLocked(): Boolean = RustPlayerObjects.native { inner.speedLocked() }

    fun speedPressing(): Boolean = RustPlayerObjects.native { inner.speedPressing() }

    /**
     * 底座倍速（提示里"恢复成几倍"用）：临时加速中是**加速前**那个，否则就是 [current]。
     *
     * 基线里是 `heldFrom ?: state.speed`；WIP 的 `PlaybackSession.baseSpeed` 一度退化成直接取
     * `state.speed`——长按 3 倍加速时提示会显示"恢复成 3 倍"，而正确值是长按之前的 1 倍。
     */
    fun baseSpeed(current: Float): Float = RustPlayerObjects.native { inner.baseSpeed(current) }

    /** 取流失败的文案。 */
    fun playFailure(failure: PlayFailure, platformLabel: String, proxied: Boolean): FailureText =
        RustPlayerObjects.native {
            inner.playFailure(RustPlayer.run { failure.toFfi() }, platformLabel, proxied).let {
                FailureText(it.needsLogin, it.error, it.hint)
            }
        }
}

/**
 * **中转映射**（Rust `FfiRelay` 的 Kotlin 面）：登记远端地址 → 本机地址，路由判定在 Rust。
 *
 * `id` 由**宿主**生成（UUID）：URL 里的 id 要能对回远端地址，而"生成随机串"留在宿主。
 */
class RustRelay(base: String) {
    internal val inner = RustPlayerObjects.native { FfiRelay(base) }

    fun resource(remoteUrl: String, id: String): String = RustPlayerObjects.native {
        inner.resource(remoteUrl, id)
    }

    fun playlist(remoteUrl: String, id: String): String = RustPlayerObjects.native {
        inner.playlist(remoteUrl, id)
    }

    fun template(remoteTemplate: String, id: String): String = RustPlayerObjects.native {
        inner.template(remoteTemplate, id)
    }

    fun local(body: String, contentType: String?, id: String): String = RustPlayerObjects.native {
        inner.local(body, contentType, id)
    }

    fun route(path: String, rawQuery: String?): RelayRoute = RustPlayerObjects.native {
        inner.route(path, rawQuery).toCore()
    }

    fun len(): Int = RustPlayerObjects.native { inner.len().toInt() }

    fun isEmpty(): Boolean = len() == 0

    /** 按 id 取条目（检查工具用）。 */
    fun has(id: String): Boolean = RustPlayerObjects.native { inner.entry(id) != null }

    /** 登记过的 id（**登记顺序**）。 */
    fun registeredIds(): List<String> = RustPlayerObjects.native { inner.registeredIds() }

    /** 按 id 取条目（检查工具 / 测试用）。 */
    fun entry(id: String): RelayEntryInfo? = RustPlayerObjects.native {
        when (val e = inner.entry(id)) {
            null -> null
            is FfiRelayEntry.Local -> RelayEntryInfo.Local(e.contentType)
            is FfiRelayEntry.Remote -> RelayEntryInfo.Remote(e.url, e.dash)
            is FfiRelayEntry.Resource -> RelayEntryInfo.Resource(e.url)
            is FfiRelayEntry.Template -> RelayEntryInfo.Template(e.remote, e.tokens)
        }
    }
}

/** 中转登记项的 Kotlin 面。 */
sealed interface RelayEntryInfo {
    data class Local(val contentType: String?) : RelayEntryInfo
    data class Remote(val url: String, val dash: Boolean) : RelayEntryInfo
    data class Resource(val url: String) : RelayEntryInfo
    data class Template(val remote: String, val tokens: List<String>) : RelayEntryInfo
}

/** 中转条目的形状（检查工具 / 测试用）。 */
internal fun FfiRelayEntry.describe(): String = when (this) {
    is FfiRelayEntry.Local -> "local"
    is FfiRelayEntry.Remote -> "remote:${if (dash) "mpd" else "m3u8"}"
    is FfiRelayEntry.Resource -> "resource"
    is FfiRelayEntry.Template -> "template:${tokens.size}"
}

/** 播放状态机给回来的一件事 → Kotlin 的 [PlayEffect]。 */
internal fun uniffi.feedhub_ffi.FfiPlayEffect.toCore(): PlayEffect = when (this) {
    uniffi.feedhub_ffi.FfiPlayEffect.Play -> PlayEffect.Play
    uniffi.feedhub_ffi.FfiPlayEffect.Pause -> PlayEffect.Pause
    uniffi.feedhub_ffi.FfiPlayEffect.Replay -> PlayEffect.Replay
    is uniffi.feedhub_ffi.FfiPlayEffect.Seek -> PlayEffect.Seek(positionMs)
    is uniffi.feedhub_ffi.FfiPlayEffect.SetRate -> PlayEffect.SetRate(rate)
    is uniffi.feedhub_ffi.FfiPlayEffect.SaveProgress -> PlayEffect.SaveProgress(positionMs, durationMs)
    is uniffi.feedhub_ffi.FfiPlayEffect.SavePart -> PlayEffect.SavePart(partId)
}

/** 取流失败 → FFI enum。 */
internal fun PlayFailure.toFfi(): FfiPlayFailure = when (this) {
    PlayFailure.NotLoggedIn -> FfiPlayFailure.NotLoggedIn
    PlayFailure.RiskControl -> FfiPlayFailure.RiskControl
    is PlayFailure.Other -> FfiPlayFailure.Other(message)
}

/** 播放快照 → FFI record。 */
internal fun PlaybackSnapshot.toFfi(): FfiPlaybackState = FfiPlaybackState(
    playing = playing,
    buffering = buffering,
    positionMs = positionMs,
    durationMs = durationMs,
    ended = ended,
    speed = speed,
    muted = muted,
    volume = volume,
)

/** 时间线的底层对象（给 `RustDanmakuScheduler` 用）。 */
internal val RustDanmakuTimeline.ffiTimeline: FfiDanmakuTimeline get() = inner
