package feedhub.app.video

import kotlinx.serialization.Serializable
import kotlin.math.abs
import kotlin.math.roundToInt

/**
 * 弹幕设置。纯数据 + 换算，不碰 Compose，好写单元测试。
 *
 * 播放器里的"弹幕设置"面板和设置页打开的是同一个面板，改了立刻生效、立刻存成默认值——不分"本次"和"默认"，BiliPai 也是这么做的。
 * 弹幕开关例外：它在 [feedhub.app.AppSettings.danmakuOn]（"弹幕默认开启"），播放器里的开关按钮只管这一次。
 *
 * 各选项的含义、取值范围和默认值照 BiliPai（DanmakuConfig / SettingsManager）；默认值里和 BiliPai 不一样的几个（滚动时长、行高、字重）
 * 是为了和这个项目原来的弹幕看起来一样。选项按"改了会影响什么"分三类，见 [filter]、[layout] 和只管绘制的 [opacity]、[strokeWidth]：
 * - 过滤（[filter]）：弹幕进时间线时就处理好，改了要对已取回的弹幕重新处理一遍；
 * - 排布（[layout]）：影响轨道和弹幕宽度，改了要从当前时间点起重新排；
 * - 只管绘制：透明度、描边——不动已经排好的轨道。
 */
/**
 * 这里只留一个 typealias，业务代码 import 一行不用改。
 *
 * 各选项的含义、取值范围和默认值照 BiliPai（DanmakuConfig / SettingsManager）；默认值里和 BiliPai 不一样的
 * 几个（滚动时长、行高、字重）是为了和这个项目原来的弹幕看起来一样。选项按"改了会影响什么"分三类，
 * 见 `filter`、`layout` 和只管绘制的 `opacity`、`strokeWidth`。
 */
typealias DanmakuSettings = feedhub.player.DanmakuSettings
