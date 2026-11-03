package feedhub.app.settings

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.width
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import feedhub.app.AppGraph
import feedhub.app.embed.BehaviorSignal
import feedhub.app.embed.BehaviorSnapshotLimits
import feedhub.app.embed.SignalKind
import feedhub.app.embed.backendLabel
import feedhub.app.embed.buildBehaviorSignals
import feedhub.app.embed.CoverRules
import feedhub.app.embed.collectBehaviorSnapshot
import feedhub.app.embed.explanation
import feedhub.app.embed.label
import feedhub.app.embed.signalCounts
import feedhub.app.export.exportBehaviorData
import feedhub.app.library.EMBEDDING_MODEL_ID
import feedhub.app.library.EMBEDDING_SOURCE_FAVORITE
import feedhub.app.library.EMBEDDING_SOURCE_FEED
import feedhub.app.library.EMBEDDING_SOURCE_HISTORY
import feedhub.app.library.EXPOSURE_RETENTION_DAYS
import feedhub.app.library.EmbeddingStat
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * "行为数据"卡片：记不记本地行为数据 + 现在记了多少 + 清除。
 *
 * 这里记的是**曝光**（卡片可见面积 ≥ 50% 且连续 ≥ 1 秒）和**停留时长**，给以后的本地个性化用
 * 。"点开"本来就在历史里记着，
 * "曝光了但没点"可以从两份数据 join 出来（见 [feedhub.app.library.ExposureStats]）。
 *
 * 放这个卡片而不是塞进"存储"卡片：整块功能（含文案）都在这里，`SettingsScreen.kt` 只加一行调用，
 * 另一个分支正在改那个文件里的"网络"分组，改动局部化会少很多合并冲突。
 */
@Composable
fun BehaviorSettings(graph: AppGraph) {
    val record = graph.settings.value.recordBehavior
    val scope = rememberCoroutineScope()
    var count by remember { mutableStateOf<Int?>(null) }
    var cleared by remember { mutableStateOf(0) }
    var refresh by remember { mutableIntStateOf(0) }
    // 「清除」之后要让下面「本地向量（实验）」里的信号统计也跟着重算（否则那一行还停在清除前的数）
    var signalsRevision by remember { mutableIntStateOf(0) }

    LaunchedEffect(refresh, record) {
        count = withContext(Dispatchers.IO) { graph.library.exposureCount() }
    }

    SettingsCard("行为数据") {
        SwitchRow(
            "记录浏览行为（曝光 / 停留）",
            if (record) {
                "只存在本机、不发任何请求：每条内容看到了多久、哪些看到了没点开。给以后的本地个性化用。"
            } else {
                "关着：不记录新的（已经在记的那次丢掉）。已经记下的可以点右边清除。"
            },
            record,
        ) { v -> graph.settings.update { it.copy(recordBehavior = v) } }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("已记录的曝光", style = MaterialTheme.typography.bodyMedium)
                Hint(
                    when (val n = count) {
                        null -> "正在统计…"
                        else -> "$n 条 · 只保留最近 $EXPOSURE_RETENTION_DAYS 天" + if (cleared > 0) " · 刚清除了 $cleared 条" else ""
                    },
                )
            }
            Spacer(Modifier.width(12.dp))
            OutlinedButton(
                onClick = {
                    val before = count ?: 0
                    scope.launch {
                        withContext(Dispatchers.IO) { graph.library.clearExposure() }
                        // 向量也跟着清了（Library.clearExposure 连 item_embedding 一起删），统计归零
                        graph.embedding.resetStats()
                        cleared = before
                        count = 0
                        signalsRevision++
                    }
                },
                enabled = (count ?: 0) > 0,
            ) { Text("清除") }
        }
    }
    LocalEmbeddingSettings(graph, signalsRevision)
}

/**
 * 「本地向量（实验）」。
 *
 * 算不算向量跟着上面的「记录浏览行为」走（关了就不算、不存）；**「用 NPU 算」是另一个开关**（默认关）。
 * 普通包（带 CPU 模型）里它只管"在哪儿算"：打开后尽量走手机的 NPU，第一次要编图（只在前台、充电或空闲时），
 * 准备期间要算的批先等着、失败才回退 CPU 并显示原因（`当前实际用的后端` 那一行）。
 * **只带 NPU 模型的包**里它**就是本地向量的
 * 总开关**：关着 = 一条都不算；NPU 用不了 = "不可用"、不回退（没有 CPU 可退）。
 *
 * 显示：模型（EG2 q4 / 另带 w8a16 的 HTP 版本）、「用 NPU 算」开关、当前实际用的后端、已算条数（含来源拆开）、
 * **行为信号**、最近平均每条耗时、「导出」按钮。
 * 导出写一个 JSON Lines 文件（`seen_item` / `exposure` / `history` / `favorite` / `signal` / `profile` /
 * `item_embedding`），
 * 位置由平台的 `BehaviorExportTarget` 决定（安卓 `Android/data/feedhub.app/files/export/`，桌面 `~/.config/feedhub/export/`），
 * 只在这个按钮被点时导出。当前**权重是 0、首页顺序一条都不变**，等离线回放（`tools/embed/replay.py`）比完模型再接。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LocalEmbeddingSettings(graph: AppGraph, signalsRevision: Int = 0) {
    val embedding = graph.embedding
    val target = graph.behaviorExport
    val scope = rememberCoroutineScope()
    var exporting by remember { mutableStateOf(false) }
    var result by remember { mutableStateOf<String?>(null) }
    val count = embedding.count
    val stats = embedding.stats
    val avgMs = embedding.avgMs
    val available = embedding.available
    val blockWords = graph.settings.value.blockWords
    // 行为信号：打开这一页时在 IO 上算一次（只读库；条数上限见 BehaviorSnapshotLimits.SETTINGS）
    var signals by remember { mutableStateOf<List<BehaviorSignal>?>(null) }
    LaunchedEffect(blockWords, signalsRevision) {
        signals = withContext(Dispatchers.IO) {
            buildBehaviorSignals(collectBehaviorSnapshot(graph.library, blockWords))
        }
    }
    // 本地向量走 NPU：开关 + "当前实际用的后端"。没有路由的组装（桌面 / 测试 /
    // 没带 NPU 的包）这里是 null，开关灰显。
    val backend = graph.embeddingBackend
    val npuBundled = backend?.npuBundled == true
    val npuOn = graph.settings.value.embedNpu
    val npuStatus = backend?.npuStatus
    // 包里带了几张图：只用来写说明，不能选——用哪张由调度器按批挑
    val npuLengths = backend?.npuLengths.orEmpty()
    // 现在就绪的那几张（"当前实际用的后端"那行还带长度，例如 `NPU · 32 / 128 token`）
    val activeLens = (npuStatus as? feedhub.app.embed.NpuStatus.Active)?.lens.orEmpty()
    // **NPU 包不带 CPU 模型**：这时「用 NPU 算」就是本地向量的总开关，
    // 文案里不能再说"回退到 CPU"（没有 CPU 可退），也不能把"关着"写成"未包含模型"。
    val noCpuModel = backend != null && !backend.cpuBundled
    // 桌面：没有 NPU 那条路，但可以自报"ONNX Runtime + 3 线程"
    val runtimeLabel = (graph.embedder as? feedhub.app.embed.EmbeddingRuntimeInfo)?.runtimeLabel
    // 桌面没有"退到后台"这回事，几句说明要分开写
    val desktop = runtimeLabel != null

    LaunchedEffect(Unit) { embedding.refreshCountAsync() }

    SettingsCard("本地向量（实验）") {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("模型", style = MaterialTheme.typography.bodyMedium)
                Hint(
                    when {
                        // 只带 NPU 模型的包：模型说明只讲 w8a16（不要说"未包含模型"，模型在，只是没有 CPU 那份）
                        noCpuModel -> "EmbeddingGemma 2 · w8a16（HTP，NPU 专用）· 存 256 维（MRL 截断 + L2 归一化）；" +
                            "包里带了 ${npuLengths.joinToString(" / ")} token 的图（共用一份 271 MB 的权重）。" +
                            "这个包不带 CPU 的 q4 模型（省 174 MB）：NPU 用不了时本地向量就是不可用，不回退。"
                        !available -> "未包含模型"
                        npuBundled -> "EmbeddingGemma 2 · q4（accuracy_level=4，每条最多 64 token）· 存 256 维" +
                            "（MRL 截断 + L2 归一化）；包里另带 w8a16 的 HTP 版本（" +
                            npuLengths.joinToString(" / ") { "每条 $it token" } + "，共用一份 271 MB 的权重）"
                        else -> "EmbeddingGemma 2 · q4（accuracy_level=4）· 存 256 维（MRL 截断 + L2 归一化）"
                    },
                )
            }
        }
        // 「用 NPU 算」：默认关；包里没带 NPU 模型时灰显并说明。
        // **只带 NPU 模型的包**里它就是本地向量的总开关（没有 CPU 可退回、没有"降级"）。
        SwitchRow(
            if (noCpuModel) "本地向量（用 NPU 算）" else "用 NPU 算（实验）",
            hint = when {
                backend == null -> "这个构建没有 NPU 实现（桌面版不接 NPU）"
                !npuBundled -> "这个包没带 NPU 模型（构建时没加 -Pfeedhub.embedNpu=true）"
                noCpuModel && !npuOn -> "关着：这个包只带 NPU 模型，所以这个开关就是本地向量的总开关——" +
                    "关着就一条都不算、不存（没有 CPU 版本可以退回）。打开只走手机 NPU"
                noCpuModel -> "打开：只走手机的 NPU。第一次要编图（只在前台、充电或空闲时，十几秒到一分钟），" +
                    "这段时间要算的批先等着、不先用 CPU 顶；NPU 真用不了时本地向量就是不可用（不回退、不降级）"
                npuOn -> "打开：下一批起尽量走手机的 NPU。第一次要编图（只在前台、充电或空闲时，十几秒到一分钟），" +
                    "这段时间要算的批先等着（不回退 CPU，免得编图一直等不到空闲）"
                else -> "关着：只用 CPU。打开后会在前台编一次图，之后每批走 NPU；省不省电要自己看"
            },
            checked = npuOn,
            enabled = npuBundled,
        ) { v -> graph.settings.update { it.copy(embedNpu = v) } }
        // 包里带了多张图：只说明"怎么选"，
        // 不给用户选——每批按"这批里最长那条"挑最短够用的那张。
        if (npuLengths.size > 1) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("NPU 的图", style = MaterialTheme.typography.bodyMedium)
                    Hint(
                        "包里带了 ${npuLengths.size} 张：${npuLengths.joinToString(" / ")} token" +
                            if (activeLens.isNotEmpty()) "；已就绪 ${activeLens.joinToString(" / ")}" else "",
                        detail = "长度是烘进图里的（HTP 不支持动态形状）。每一批按「这批里最长那条」自动挑能装下它的" +
                            "最短那张：短文本走 32（真机上量到 257 ms/批），长文本才走 128（1241 ms/批）。" +
                            "装得下的文本，两张图算出来是同一个向量（256 维余弦最小 0.9993），" +
                            "所以 item_embedding.model 不变、已经算好的不用重算。第一次要逐张编图（只在前台、" +
                            "充电或空闲时，短的先编）。",
                    )
                }
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("当前实际用的后端", style = MaterialTheme.typography.bodyMedium)
                Hint(
                    npuStatus?.explanation ?: runtimeLabel?.let { "ONNX Runtime · $it（桌面 CPU）" } ?: "CPU（这个构建没有 NPU）",
                    detail = when {
                        noCpuModel -> "这个包只带 NPU 模型（w8a16），没有 CPU 的 q4 可以退回：'NPU 用不了'时本地向量直接不可用" +
                            "（不算、不存，和包里没带模型同一种安静）。以前装普通包时算的 q4 向量按 " +
                            "item_embedding.model 分开存，不再有新的，30 天保留期到了自己过期。" +
                            "NPU 编好的图缓存在 App 私有目录，之后启动直接加载（不重编）。"
                        desktop -> "桌面只有 CPU 一条路：ONNX Runtime 1.31.0 + q4 图（accuracy_level=4）、3 个 intra-op 线程、" +
                            "关自旋，每批 8 条、批间让 50 ms；和安卓 CPU 路径是同一个 model 标识、同一份词表，所以两边算的向量" +
                            "在同一个空间（导出可以混着回放）。桌面的 XDNA NPU 在 Linux 上用不了，没有 NPU 那条路。" +
                            "会话建好后常驻约 155 MB，关窗口时释放。"
                        else -> "CPU 版（q4，每条最多 64 token）和 NPU 版（w8a16）算出来的向量不是同一个空间" +
                            "（实测中位余弦 0.978、最小 0.876），所以库里按后端分开存（item_embedding.model）。" +
                            "NPU 那份的 model 是最长那张图（例如 …-htp-l128）：装得下的文本用短图算，向量和 128 图" +
                            "是同一个空间（余弦最小 0.9993），所以换图不重算。切换后端之后，另一份模型的卡片会重新算一遍。" +
                            "NPU 编好的图缓存在 App 私有目录，之后启动直接加载；任何一步失败都回退 CPU，不会崩。"
                    },
                )
            }
            Spacer(Modifier.width(12.dp))
            Text(npuStatus?.backendLabel ?: "CPU", style = MaterialTheme.typography.bodyMedium)
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("已算条数", style = MaterialTheme.typography.bodyMedium)
                Hint(
                    embeddingCountLine(count, stats),
                    detail = "首页 / 关注的新卡片优先算，收藏 / 历史里的条目会补算（最近 30 天、最近的先算）；" +
                        "18+ 不算；保留 30 天，和「曝光过的卡片」一起清。括号里是这条向量当初怎么进的队列：" +
                        "首页（新卡片 / 点开详情）、收藏或历史（补算）——同一条只算一次，先被首页算过就记首页。",
                )
            }
        }
        CoverVectorRow(graph)
        Row(verticalAlignment = Alignment.Top) {
            Column(Modifier.weight(1f)) {
                Text("行为信号", style = MaterialTheme.typography.bodyMedium)
                Hint(
                    signalSummaryLine(signals),
                    detail = "把收藏（+1.0）、点开（+0.35）、看完（+0.65）、详情看得久（+0.45）、点开就退（−0.25）、" +
                        "划过没点（−0.08）、屏蔽作者（−1.0）、命中屏蔽词（−0.5）折成信号，带时间衰减" +
                        "（半衰期 7～120 天，屏蔽词不衰减）。只数最近 " +
                        "${BehaviorSnapshotLimits.SETTINGS.favorites} 条收藏 / ${BehaviorSnapshotLimits.SETTINGS.history} 条历史 / " +
                        "${BehaviorSnapshotLimits.SETTINGS.exposures} 次曝光 / ${BehaviorSnapshotLimits.SETTINGS.seen} 张卡片。" +
                        "这一期没接权重：信号只用于导出和离线回放，不改首页顺序。",
                )
                // 各信号各几条单独一行：窄屏上"共 N 条（正/负）"放得下，全列出来放不下，宁可折行也不省略
                val currentSignals = signals
                if (currentSignals != null && currentSignals.isNotEmpty()) Hint(signalBreakdownLine(currentSignals))
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("最近平均每条耗时", style = MaterialTheme.typography.bodyMedium)
                Hint(
                    avgMs?.let {
                        val tail = if (desktop) "每批 8 条，跑在后台，关窗口时释放" else "每批 8 条，跑在后台，退到后台立刻停"
                        "$it ms / 条（$tail）"
                    } ?: "还没算过",
                )
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("导出行为数据", style = MaterialTheme.typography.bodyMedium)
                Hint(
                    when {
                        target == null || !target.available -> "这个设备 / 这次组装不能导出（拿不到可写的目录）"
                        else -> "JSON Lines，写到 ${target.location}；含曝光 / 历史 / 收藏 / 信号 / 向量 / 画像，" +
                            "不含 Cookie 和 token"
                    },
                )
            }
            Spacer(Modifier.width(12.dp))
            OutlinedButton(
                enabled = !exporting && target != null && target.available,
                onClick = {
                    val t = target ?: return@OutlinedButton
                    exporting = true
                    scope.launch {
                        val r = exportBehaviorData(graph.library, t, blockWords = blockWords)
                        r.fold(
                            onSuccess = {
                                result = "已导出 ${it.lines} 行（${it.bytes / 1024} KB）→ ${it.path}"
                            },
                            onFailure = {
                                result = "导出失败：${it.message ?: it::class.simpleName}"
                            },
                        )
                        exporting = false
                    }
                },
            ) { Text(if (exporting) "正在导出…" else "导出") }
        }
        result?.let { Hint(it) }
        Hint(
            "这一期只搭通路：向量不算进排序（权重 0），首页顺序和以前完全一样。「用 NPU 算」只改「在哪儿算」，" +
                "不改排序；收藏 / 历史的补算和信号也一样只存不算。等攒够一两周的浏览数据、离线回放比完模型再接权重。",
        )
    }
}

/** 设置页「已算条数」那一行：总数 + 来源拆开（有多个模型时先按后端拆）。 */
internal fun embeddingCountLine(count: Int, stats: List<EmbeddingStat>): String {
    if (count <= 0) return "0 条 · 还没算过"
    if (stats.isEmpty()) return "$count 条（正在统计来源…）"
    val byModel = stats.groupBy { it.model }
    fun sources(rows: List<EmbeddingStat>): String {
        val bySource = rows.associate { it.source to it.count }
        val parts = listOf(
            EMBEDDING_SOURCE_FEED to "首页",
            EMBEDDING_SOURCE_FAVORITE to "收藏",
            EMBEDDING_SOURCE_HISTORY to "历史",
        ).mapNotNull { (k, label) -> bySource[k]?.takeIf { it > 0 }?.let { "$label $it" } }
        // 不认识的新来源（以后加的）也要显示出来，不能默默吞掉
        val known = setOf(EMBEDDING_SOURCE_FEED, EMBEDDING_SOURCE_FAVORITE, EMBEDDING_SOURCE_HISTORY)
        val extra = bySource.filterKeys { it !in known }.map { (k, n) -> "$k $n" }
        return (parts + extra).joinToString(" · ")
    }
    return if (byModel.size <= 1) {
        "$count 条（${sources(stats)}）"
    } else {
        "$count 条 · " + byModel.entries.joinToString("；") { (model, rows) ->
            "${shortModelLabel(model)} ${rows.sumOf { it.count }} 条（${sources(rows)}）"
        }
    }
}

/**
 * CPU / NPU / NPU 封面三个向量空间用短名，别把整串 model id 摆到界面上
 * （NPU 的 id 带序列长度、封面那条还带 `-img8x8`，都用前缀认）。
 */
internal fun shortModelLabel(model: String): String = when {
    model.startsWith(EMBEDDING_MODEL_ID) -> "CPU"
    // 封面向量：`…-htp-l128-img8x8`，和纯文字那个是**不同的空间**，标签要分开
    model.startsWith(NPU_MODEL_ID_PREFIX) && model.endsWith(COVER_MODEL_ID_SUFFIX) -> "NPU·封面"
    model.startsWith(NPU_MODEL_ID_PREFIX) -> "NPU"
    else -> model
}

/** 封面向量 model id 的后缀（`feedhub.app.library.npuCoverModelId`）。 */
const val COVER_MODEL_ID_SUFFIX: String = "-img8x8"

/** NPU 模型 id 的前缀。 */
private const val NPU_MODEL_ID_PREFIX: String = "embeddinggemma-2-w8a16-htp"

/**
 * 设置页「行为信号」第一行：总数 + 正负。
 *
 * 权重是 Rust 那份表里的）：这里只做文案。总数是条数本身，不涉及口径。
 */
internal fun signalSummaryLine(signals: List<BehaviorSignal>?): String {
    if (signals == null) return "正在统计…"
    if (signals.isEmpty()) return "还没有行为信号（收藏 / 点开 / 曝光都会算进来）"
    val positives = signals.count { it.kind.isPositive }
    return "共 ${signals.size} 条（正 $positives · 负 ${signals.size - positives}）"
}

/** 设置页「行为信号」第二行：只列非零的信号，零的不占地方；窄屏会自己折行（计数在 Rust）。 */
internal fun signalBreakdownLine(signals: List<BehaviorSignal>): String {
    val counts = signalCounts(signals)
    return SignalKind.entries.mapNotNull { k -> counts[k]?.takeIf { it > 0 }?.let { "${k.label} $it" } }
        .joinToString(" · ")
}

/**
 * 设置页「封面向量（实验）」那一行。
 *
 * 文案口径：
 * - **已算 / 待算**：已算是库里 `item_embedding.model = …-img8x8` 的行数，待算是这次运行排进队列还没算的条数；
 * - **什么时候跑**：前台、纯文字闲着就跑，一次一个窗口（32 条）、排在纯文字补算后面；
 * - **开关**（默认开）：关掉 = 一条都不算（库里已有的照旧保留，30 天保留期到了自己过期）。
 */
@Composable
private fun CoverVectorRow(graph: AppGraph) {
    val covers = graph.coverVectors
    val status = covers.status
    val on = graph.settings.value.coverVectors
    val bundled = covers.available
    val npuOn = graph.settings.value.embedNpu
    val last = covers.lastWindow
    LaunchedEffect(Unit) { covers.refreshAsync() }
    SwitchRow(
        "封面向量（实验）",
        hint = when {
            !bundled -> "这个包没带封面的两张图（构建时没加 -Pfeedhub.embedNpu=true，或者不是 NPU 包）"
            !on -> "关着：一条都不算（库里已有的照旧保留，30 天保留期到了自己过期）"
            // 封面向量只有 NPU 这条路，而且和纯文字**共用**那份 271 MB 权重（NPU 那条解包出来的）：
            // 「用 NPU 算」关着时这条旁路安静地不动，别让人以为坏了
            !npuOn -> "要先打开上面的「用 NPU 算（实验）」：封面向量只有 NPU 这条路，而且和纯文字共用那份权重"
            else ->
                "开着：" + CoverRules.coverPlatforms().sorted().joinToString(" / ") +
                    " 的封面会额外算一份「文字 + 封面」向量。在前台就算（一次一个窗口 32 条，" +
                    "排在纯文字后面），封面图直接用首页已经缓存的那张缩略图。和纯文字向量并存、不替换。"
        },
        checked = on,
        enabled = bundled,
    ) { v ->
        graph.settings.update { it.copy(coverVectors = v) }
        // 打开之后立刻叫醒一次（队列里可能已经攒着东西，不用等下一次首页刷新 / 回前台）。
        // `SettingsStore.update` 是同步改 value 的，所以这里读到的已经是新值；
        if (v) covers.poke() else graph.coverEmbedder?.close()
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text("封面", style = MaterialTheme.typography.bodyMedium)
            Hint(
                "已算 ${covers.stored} 条 · 待算 ${covers.pending} 条（${status.label}）" +
                    last?.let { " · 上一轮 ${it.count} 条 ${it.msPerItem} ms/条" }.orEmpty(),
                detail = "「已算」是库里 model = ${feedhub.app.library.npuCoverModelId(128)} 的行数；" +
                    "「待算」是这次运行排进队列、还没算的条数（在前台、纯文字闲着时消化，退到后台就停）。" +
                    "一个窗口：先把这一批的封面用视觉图算成特征（每张约 0.8 s），再拼上文字、用纯文字向量那张图算向量" +
                    "（一批 8 条，和纯文字共用同一个会话）。第一次要编视觉图（十几秒），编的时候 NPU 上不能有别的会话，" +
                    "所以那一轮是分时的；编好的 context 缓存在 App 私有目录，之后视觉会话常驻（退后台才关）。" +
                    "常驻撞了 QNN 错就这次运行改回分时；分时也失败才停这条旁路，不影响纯文字向量，下次启动再试。",
            )
        }
    }
}
