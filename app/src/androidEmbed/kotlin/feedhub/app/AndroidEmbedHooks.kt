package feedhub.app

import android.content.Context
import android.os.BatteryManager
import kotlinx.coroutines.CoroutineScope

/**
 * 安卓本地向量的真实组装（仅在 `-Pfeedhub.embed=true` 或 `-Pfeedhub.embedNpu=true` 时编进 APK）。
 *
 * [includeCover] = false 时只装纯文字向量（`androidDemoGraph` 演示环境用，不装封面视觉图）。
 */
internal fun createAndroidEmbedBundle(
    appContext: Context,
    scope: CoroutineScope,
    log: (String) -> Unit,
    includeCover: Boolean = true,
): AndroidEmbedBundle {
    val npuEngines = androidNpuEngines(appContext, log)
    val embedder = androidEmbedder(appContext, scope, log, npuEngines)
    if (!includeCover) return AndroidEmbedBundle(embedder = embedder)
    val coverEmbedder = feedhub.app.embed.NpuOnnxCoverEmbedder(
        context = appContext,
        textSessions = embedder as? feedhub.app.embed.TextSessionGate,
        textRunner = npuEngines?.coverTextRunner(feedhub.app.embed.CoverLayout.TEXT_LEN),
        log = log,
    )
    val coverImages = feedhub.app.embed.CoilCoverImages(
        context = appContext,
        // `AppGraph.current` 还没设上时（构造期）退回一份"跟随全局 + 按主机名"的独立取图：
        // 只在第一帧之前被调到才会走到，行为和水那条一样（浏览器 UA 也补）。
        fetch = { AppGraph.current?.imageFetch ?: androidImageFetch() },
        log = log,
    )
    return AndroidEmbedBundle(
        embedder = embedder,
        coverEmbedder = coverEmbedder,
        coverImages = coverImages,
    )
}

/**
 * 本地向量的组装：CPU 那份 + NPU 那几张（每张一个固定长度的图）+ 后端路由。
 *
 * **两份模型不再同时进包**：
 * `-Pfeedhub.embedNpu=true` 的构建里 `assets/embed/` 是空的（构建时就不拷那 246 MB），所以这里
 * `OnnxTextEmbedder.bundled == false` → **CPU 会话根本不会建**（`cpu = null`）。NPU 真用不了时本地向量
 * 就是"不可用"——安静地不算、不存，不回退。普通包（不带 NPU）仍然只有 CPU，
 *
 * 带几张图由构建产物决定，
 * 运行时只**发现** assets 里有什么（[feedhub.app.embed.NpuOnnxTextEmbedder.discover]），不猜：长度就是图的名字。
 *
 * [charging] 只用来判断"现在能不能开始编图"。读的是
 * `BatteryManager.isCharging`，**不读电流 / 电量**。
 */
internal fun androidEmbedder(
    context: Context,
    scope: CoroutineScope,
    log: (String) -> Unit,
    /** 包里的 NPU 图（封面向量要借同一个实例，所以由组装方建好传进来；null = 没带）。 */
    npu: feedhub.app.embed.NpuOnnxEngines? = androidNpuEngines(context, log),
): feedhub.app.embed.Embedder {
    val npuModels = npu?.variants.orEmpty()
    // 包里有 CPU 模型才建（NPU 包没有；构造本身只查一次 assets.list，不读内容、不建会话）
    val cpu = feedhub.app.embed.OnnxTextEmbedder(context, log = log).takeIf { it.bundled }
    log(
        when {
            npuModels.isEmpty() && cpu == null ->
                "[向量] 包里没带任何本地向量模型（CPU / NPU 都没有），本地向量不可用"
            npuModels.isEmpty() ->
                "[向量] 包里没带 NPU 图（这个构建没有 -Pfeedhub.embedNpu=true），照样用 CPU"
            else ->
                "[向量] 包里带了 " + npuModels.joinToString(" / ") { "每条 ${it.seqLen} token" } +
                    " 的 NPU 图（共用一份权重；按批挑最短够用的那张，model = " +
                    feedhub.app.library.npuModelId(feedhub.app.embed.NpuAssets.spaceLen(npuModels.map { it.seqLen })) + "）" +
                    if (cpu == null) "；这个包不带 CPU 模型，NPU 用不了就是不可用（不回退）" else ""
        },
    )
    return feedhub.app.embed.RoutingEmbedder(
        cpu = cpu,
        npu = npu,
        scope = scope,
        charging = { chargingNow(context) },
        log = log,
    )
}

/** 包里带的 NPU 图（没带就是 null）。纯文字的路由和封面向量拿的是**同一个**工厂（引擎按长度缓存）。 */
internal fun androidNpuEngines(
    context: Context,
    log: (String) -> Unit,
): feedhub.app.embed.NpuOnnxEngines? =
    feedhub.app.embed.NpuOnnxEngines(context, feedhub.app.embed.NpuOnnxTextEmbedder.discover(context), log = log)
        .takeIf { it.lengths.isNotEmpty() }

/** 在充电吗（只用于"能不能开始编图"；拿不到就当没充，靠"空闲"那一半放行）。 */
private fun chargingNow(context: Context): Boolean = runCatching {
    val manager = context.getSystemService(Context.BATTERY_SERVICE) as? BatteryManager
    manager?.isCharging == true
}.getOrDefault(false)
