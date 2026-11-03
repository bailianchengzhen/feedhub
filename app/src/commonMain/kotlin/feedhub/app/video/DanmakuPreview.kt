package feedhub.app.video

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import feedhub.core.Danmaku
import feedhub.core.DanmakuMode
import kotlinx.coroutines.flow.first
import kotlin.random.Random

/** 示意弹幕循环的长度（毫秒）。 */
const val SAMPLE_LOOP_MS = 24_000L

/**
 * 一段示意用的假弹幕，循环 [SAMPLE_LOOP_MS]：滚动 / 顶部 / 底部、白的和彩色的、大小两种字号、权重高低不一、
 * 夹着几处同一句话连发的（试合并重复）和几条能拿来试屏蔽词的（"剧透预警"、"第12集"）。固定种子，每次一样。
 * 设置页的预览（[DanmakuPreview]）和截图工具的演示场景用。
 */
fun sampleDanmaku(): List<Danmaku> {
    val random = Random(20261015)
    val texts = listOf(
        "前方高能", "哈哈哈哈哈", "awsl", "名场面", "这个配乐绝了", "太真实了", "笑死我了", "泪目", "下次一定", "我爱了",
        "好家伙", "弹幕护体", "来了来了", "打卡", "神仙打架", "素质三连", "吃瓜", "你们看到了吗", "这波操作我给满分", "已三连",
        "剧透预警", "第12集 开始了", "up主辛苦了", "二刷", "我的天", "封面党？", "高清重制版", "手机党表示看不清",
    )
    val colors = listOf(0xFF6699, 0x00BFFF, 0xFFD700, 0x7CFC00, 0xFF4500, 0xBA55D3)
    var next = 0
    fun make(timeMs: Long, text: String, mode: DanmakuMode = DanmakuMode.SCROLL, weight: Int? = random.nextInt(1, 10)) = Danmaku(
        id = "sample-${next++}",
        timeMs = timeMs,
        mode = mode,
        text = text,
        color = if (random.nextInt(100) < 70) 0xFFFFFF else colors[random.nextInt(colors.size)],
        fontSize = if (random.nextInt(100) < 85) 25 else 18,
        weight = weight,
    )
    val out = ArrayList<Danmaku>()
    // 滚动弹幕：平均 0.17 秒一条，前三秒略密，好让头一屏就有东西
    var t = 200L
    while (t < SAMPLE_LOOP_MS - 500) {
        out += make(t, texts[random.nextInt(texts.size)])
        t += random.nextLong(60, 300)
    }
    // 同一句话连发：合并重复会把它们并成一条 "666666 x5"
    for (at in listOf(3_000L, 9_500L, 16_000L)) repeat(5) { out += make(at + it * 90L, "666666", weight = 5) }
    for (at in listOf(6_000L, 13_000L, 20_000L)) repeat(4) { out += make(at + it * 120L, "哈哈哈哈", weight = 3) }
    // 顶部、底部弹幕
    for ((i, text) in listOf("高能预警", "前方核能", "本期完", "感谢观看", "求三连", "第12集 预告").withIndex()) {
        out += make(1_000L + i * 3_800L, text, DanmakuMode.TOP)
        out += make(2_500L + i * 3_800L, texts[(i * 7) % texts.size], DanmakuMode.BOTTOM)
    }
    return out.sortedBy { it.timeMs }
}

/**
 * 设置页里弹幕设置面板上方的示意画面：一块暗色背景，循环放 [sampleDanmaku]，按 [settings] 的样子画。
 * 用的就是播放器上的 [DanmakuLayer]，改什么选项这里立刻看得到；没有真的视频，只有个渐变色的底。
 */
@Composable
fun DanmakuPreview(settings: DanmakuSettings, modifier: Modifier = Modifier) {
    val state = remember { PlayerState().apply { durationMs = SAMPLE_LOOP_MS; playing = true; setRunning(true) } }
    val timeline = remember { DanmakuTimeline(SAMPLE_LOOP_MS).also { it.put(1, sampleDanmaku()) } }
    // 窗口有焦点才放：切后台 / 锁屏时窗口失焦，这里就挂起——不然这个"每帧都要画面"的循环
    // 会在后台继续要帧（`withFrameNanos` 一轮接一轮），是"后台还在出帧"里最典型的一条。
    val focused = rememberUpdatedState(LocalWindowInfo.current.isWindowFocused)
    // 位置跟着时钟走，到头回到开头（DanmakuScheduler 看到进度往回跳会重新对齐）
    LaunchedEffect(Unit) {
        while (true) {
            if (!focused.value) snapshotFlow { focused.value }.first { it }
            val begin = withFrameNanos { it }
            while (focused.value) {
                val position = (withFrameNanos { it } - begin) / 1_000_000 % SAMPLE_LOOP_MS
                if (position < state.positionMs) state.jump(position) else state.report(position)
            }
        }
    }
    Box(
        modifier.clip(RoundedCornerShape(10.dp)).background(Brush.linearGradient(listOf(Color(0xFF1B2A41), Color(0xFF3B5B73), Color(0xFF0C1821)))),
    ) {
        // 预览底下是这里画的渐变底、不是视频，所以**不要**用安卓那块独立的 SurfaceView（它会把渐变挖掉、
        // 透出桌面），照旧走 Compose 那份（见 [DanmakuLayer] 的 onSurface）。
        DanmakuLayer(timeline, state, settings, Modifier.fillMaxSize(), onSurface = false)
    }
}
