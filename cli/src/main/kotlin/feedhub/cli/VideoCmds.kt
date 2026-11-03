package feedhub.cli

import com.github.ajalt.clikt.core.CliktCommand
import com.github.ajalt.clikt.core.CliktError
import com.github.ajalt.clikt.core.Context
import com.github.ajalt.clikt.core.requireObject
import com.github.ajalt.clikt.parameters.arguments.argument
import com.github.ajalt.clikt.parameters.options.default
import com.github.ajalt.clikt.parameters.options.flag
import com.github.ajalt.clikt.parameters.options.option
import com.github.ajalt.clikt.parameters.types.int
import feedhub.core.Danmaku
import feedhub.core.DanmakuMode
import feedhub.core.FeedJson
import feedhub.core.PlayInfo
import feedhub.core.PrettyJson
import feedhub.core.VideoSource
import feedhub.core.fetch
import kotlinx.coroutines.runBlocking
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.Request

// 视频播放地址和弹幕：feedhub play / danmaku。

private fun Env.videoSource(platformKey: String): VideoSource {
    val p = platformOf(platformKey)
    return source(p) as? VideoSource ?: throw CliktError("${p.label}没有原生视频播放")
}

/** 没给 --part 时取详情里的第一 P。 */
private suspend fun VideoSource.firstPart(id: String): String =
    detail(id).video?.parts?.firstOrNull()?.id ?: throw CliktError("详情里没有分 P 信息，用 --part 指定")

private fun host(url: String) = url.toHttpUrlOrNull()?.host ?: "?"

class PlayCmd : CliktCommand(name = "play") {
    private val env by requireObject<Env>()
    private val platform by argument(help = "平台，如 bili")
    private val id by argument(help = "视频 id（如 BV 号）")
    private val part by option("--part", help = "分 P 的 id（B站 cid、知乎 video_id……；正文里的视频见 detail 输出的 [视频] 行），默认第一 P")
    private val quality by option("-q", "--quality", help = "清晰度 id（Quality.id）").int()
    private val probe by option("--probe", help = "每种编码各取一路视频、再取一路音频，只下载前 1KB 验证地址能用").flag()
    private val json by option("--json", help = "输出 JSON（PlayInfo）").flag()

    override fun help(context: Context) = "打印视频的播放地址（各清晰度、编码）"

    override fun run() = runBlocking {
        val source = env.videoSource(platform)
        val info: PlayInfo = try {
            source.play(id, part ?: source.firstPart(id), quality)
        } catch (e: CliktError) {
            throw e
        } catch (e: Exception) {
            throw CliktError(describe(e))
        }
        if (json) {
            echo(PrettyJson.encodeToString(PlayInfo.serializer(), info))
            return@runBlocking
        }
        val labels = info.qualities.associate { it.id to it.label }
        echo("时长 ${info.durationMs?.let { "%d:%02d".format(it / 60000, it / 1000 % 60) } ?: "?"} · 当前清晰度 ${info.quality?.let { labels[it] ?: it } ?: "?"}")
        echo("清晰度：" + info.qualities.joinToString("  ") { "${it.id}=${it.label}" + if (it.available) "" else "(无)" })
        echo("请求头：" + info.headers.keys.joinToString(", ").ifEmpty { "（无）" })
        for (v in info.video) {
            echo(
                "  视频 ${labels[v.quality] ?: v.quality} ${v.codec} ${v.codecs ?: ""} ${v.width ?: "?"}x${v.height ?: "?"} " +
                    "${v.frameRate ?: "?"}fps ${v.bandwidth?.let { "${it / 1000}kbps" } ?: ""} ${host(v.url)} 备用×${v.backupUrls.size}",
            )
        }
        for (a in info.audio) {
            echo("  音频 ${a.id} ${a.codecs ?: ""} ${a.bandwidth?.let { "${it / 1000}kbps" } ?: ""} ${host(a.url)} 备用×${a.backupUrls.size}")
        }
        for (p in info.progressive) {
            echo("  整段 ${p.quality?.let { labels[it] ?: it } ?: ""} ${p.codec ?: ""} ${p.width ?: "?"}x${p.height ?: "?"} ${p.bandwidth?.let { "${it / 1000}kbps" } ?: ""} ${p.sizeBytes?.let { "${it / 1024 / 1024}MB" } ?: ""} ${host(p.url)}${if (p.url.substringBefore('?').endsWith(".m3u8")) " (HLS)" else ""}")
        }
        if (probe) {
            // 探测也要走这个平台自己的客户端：ctx.client(platform) 上有按平台的代理选择器和
            // 「指定」没填全时的 fail-closed 检查。别用 env.ctx.http：那是没套任何平台代理、
            // 也没挂 fail-closed 检查的基础客户端，等于绕过按平台代理。
            val client = env.ctx.client(platformOf(platform)).build()
            val urls = info.video.groupBy { it.codec }.map { (codec, list) -> "视频 $codec" to list.first().url } +
                listOfNotNull(info.audio.firstOrNull()?.let { "音频" to it.url }, info.progressive.firstOrNull()?.let { "整段" to it.url })
            for ((label, url) in urls) {
                val req = Request.Builder().url(url).header("Range", "bytes=0-1023")
                    .apply { info.headers.forEach { (k, v) -> header(k, v) } }.build()
                val res = try {
                    client.fetch(req)
                } catch (e: Exception) {
                    echo("  探测 $label：${describe(e)}")
                    continue
                }
                echo("  探测 $label：HTTP ${res.code} ${res.headers.firstOrNull { it.first.equals("Content-Type", true) }?.second ?: ""}")
            }
        }
    }
}

class DanmakuCmd : CliktCommand(name = "danmaku") {
    private val env by requireObject<Env>()
    private val platform by argument(help = "平台，如 bili")
    private val id by argument(help = "视频 id（如 BV 号）")
    private val part by option("--part", help = "分 P 的 id（B站 cid），默认第一 P")
    private val segment by option("-s", "--segment", help = "第几段（从 1 开始）").int().default(1)
    private val limit by option("-n", "--limit", help = "打印前几条（按时间）").int().default(30)
    private val json by option("--json", help = "每行输出一条 JSON（Danmaku）").flag()

    override fun help(context: Context) = "打印视频某一段的弹幕"

    override fun run() = runBlocking {
        val source = env.videoSource(platform)
        val list: List<Danmaku> = try {
            source.danmaku(id, part ?: source.firstPart(id), segment)
        } catch (e: CliktError) {
            throw e
        } catch (e: Exception) {
            throw CliktError(describe(e))
        }
        val sorted = list.sortedBy { it.timeMs }
        if (json) {
            sorted.forEach { echo(FeedJson.encodeToString(Danmaku.serializer(), it)) }
            return@runBlocking
        }
        echo("共 ${list.size} 条；" + list.groupingBy { it.mode }.eachCount().entries.joinToString(" ") { "${it.key}=${it.value}" })
        for (d in sorted.take(limit)) {
            val mode = when (d.mode) {
                DanmakuMode.SCROLL -> "滚"
                DanmakuMode.TOP -> "顶"
                DanmakuMode.BOTTOM -> "底"
                DanmakuMode.OTHER -> "特"
            }
            echo("  %d:%02d.%d %s #%06X %s".format(d.timeMs / 60000, d.timeMs / 1000 % 60, d.timeMs / 100 % 10, mode, d.color, d.text))
        }
    }
}
