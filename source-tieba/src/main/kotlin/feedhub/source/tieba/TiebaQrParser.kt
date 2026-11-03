package feedhub.source.tieba

import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.ReaderException
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import feedhub.core.FeedJson
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.text
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import java.io.ByteArrayOutputStream
import java.util.zip.DataFormatException
import java.util.zip.Inflater

/**
 * 百度通行证网页扫码登录（`passport.baidu.com`，`tpl=tb` 贴吧）的响应解析，纯函数。
 *
 * 接口来自公开的网页登录流程：
 * - `GET /v2/api/getqrcode?lp=pc&qrloginfrom=pc&gid=…&apiver=v3&tt=…&tpl=tb&_=…`：`{errno:0, sign, imgurl, prompt}`，
 *   不带 `callback` 时回普通 JSON，带的话是 JSONP；
 * - `GET /v2/api/qrcode?sign=…&lp=pc&qrloginfrom=pc`：二维码 PNG，扫出来是
 *   `https://wappass.baidu.com/wp/?qrlogin&t=<秒>&error=0&sign=<sign>&cmd=login&lp=pc&tpl=tb&adapter=3&qrloginfrom=pc&local=<地点>`。
 *   `t` 是生成时间、`local` 是服务端按 IP 查的地点，没法自己拼，所以**把 PNG 解出来**当二维码内容（[QrImage]）。
 *   sign 过期后这个地址返回的是另一张图，扫出来是 `error=1`；
 * - `GET /channel/unicast?channel_id=<sign>&tpl=tb&gid=…&callback=…`（长轮询，服务端约 30 秒才返回）：没有事件回 `{errno:1}`；有事件时 `errno:0` 加 `channel_v`
 *   （JSON 字符串，`{"status":1}` 已扫码、`{"status":0,"v":"<临时 bduss>"}` 已确认）——未实测。
 */
internal object TiebaQrParser {
    private val P = TIEBA

    /** 去掉 JSONP 的 `callback(...)` 外壳（没有就原样），解析成对象。出错信息里不带响应内容。 */
    fun json(body: String): JsonObject {
        val t = body.trim()
        val inner = if (t.startsWith("{")) t else {
            val open = t.indexOf('(')
            val close = t.lastIndexOf(')')
            if (open < 0 || close < open) throw ParseException(P, "百度通行证的响应不是 JSON / JSONP")
            t.substring(open + 1, close)
        }
        return try {
            FeedJson.parseToJsonElement(inner) as? JsonObject ?: throw ParseException(P, "百度通行证的响应不是 JSON 对象")
        } catch (e: ParseException) {
            throw e
        } catch (e: Exception) {
            throw ParseException(P, "百度通行证的响应不是合法 JSON")
        }
    }

    data class Generated(val sign: String, val imageUrl: String)

    fun generated(json: JsonObject): Generated {
        val errno = json.long("errno") ?: throw ParseException(P, "二维码响应缺少 errno")
        if (errno != 0L) throw feedhub.core.ApiException(P, errno.toString(), "百度没给二维码")
        val sign = json.text("sign") ?: throw ParseException(P, "二维码响应缺少 sign")
        val img = json.text("imgurl") ?: throw ParseException(P, "二维码响应缺少 imgurl")
        // 实测 imgurl 没有协议头："passport.baidu.com/v2/api/qrcode?sign=…"
        return Generated(sign, if (img.startsWith("http")) img else "https://$img")
    }

    /** unicast 的一次响应。 */
    sealed interface Event {
        /** 服务端等到超时都没事件（errno 1），或者客户端自己读超时：继续等。 */
        data object Nothing : Event
        data object Scanned : Event

        /** 手机上确认了；[tempBduss] 是换正式 BDUSS 用的临时凭据，**不能打印**。 */
        class Confirmed(val tempBduss: String) : Event

        /** 手机上取消 / 别的状态。 */
        data object Cancelled : Event
        data class Error(val errno: Long) : Event
    }

    fun unicast(json: JsonObject): Event {
        val errno = json.long("errno") ?: throw ParseException(P, "轮询响应缺少 errno")
        if (errno == 1L) return Event.Nothing
        if (errno != 0L) return Event.Error(errno)
        // channel_v 在顶层还是 data 里、是 JSON 字符串还是对象，都兼容
        val raw = json["channel_v"] ?: json.obj("data")?.get("channel_v") ?: return Event.Nothing
        val v = when (raw) {
            is JsonObject -> raw
            is JsonPrimitive -> if (raw.content.isBlank()) return Event.Nothing else json(raw.content)
            else -> return Event.Nothing
        }
        return when (v.int("status")) {
            1 -> Event.Scanned
            0 -> v.text("v")?.let { Event.Confirmed(it) } ?: Event.Cancelled
            else -> Event.Cancelled
        }
    }

    /** `qrbdusslogin` 的结果：`errInfo.no` 为 "0" 才算成功。 */
    data class LoginResult(val no: String?, val msg: String?) {
        val ok get() = no == null || no == "0"
    }

    fun loginResult(json: JsonObject): LoginResult {
        val info = json.obj("errInfo")
        return LoginResult(info?.text("no"), info?.text("msg"))
    }
}

/**
 * 从二维码 PNG 里解出内容。**不用 `javax.imageio`**（安卓上没有），PNG 自己解（只要 `java.util.zip.Inflater`），
 * 再交给 zxing（`com.google.zxing:core`，安卓也能用）。支持非隔行的 PNG，1 / 2 / 4 / 8 / 16 位的灰度、调色板、RGB（A）。
 */
internal object QrImage {
    private val P = TIEBA
    private val SIGNATURE = byteArrayOf(0x89.toByte(), 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A)

    fun decode(png: ByteArray): String {
        val (w, h, pixels) = toArgb(png)
        // 先原样试，不行再给四周补白边（有的二维码图片没留静区）
        return runCatching { read(w, h, pixels) }.getOrNull()
            ?: padded(w, h, pixels, 16).let { (pw, ph, pp) -> runCatching { read(pw, ph, pp) }.getOrNull() }
            ?: throw ParseException(P, "二维码图片里没有解出内容")
    }

    private fun read(w: Int, h: Int, pixels: IntArray): String? = try {
        val bitmap = BinaryBitmap(HybridBinarizer(RGBLuminanceSource(w, h, pixels)))
        QRCodeReader().decode(bitmap, mapOf(DecodeHintType.TRY_HARDER to true)).text
    } catch (_: ReaderException) {
        null
    }

    private fun padded(w: Int, h: Int, src: IntArray, pad: Int): Triple<Int, Int, IntArray> {
        val nw = w + 2 * pad
        val nh = h + 2 * pad
        val out = IntArray(nw * nh) { -1 } // 0xFFFFFFFF 白
        for (y in 0 until h) System.arraycopy(src, y * w, out, (y + pad) * nw + pad, w)
        return Triple(nw, nh, out)
    }

    /** PNG → 宽、高、ARGB 像素（只关心明暗，所以都转成灰度；透明的当白色）。 */
    fun toArgb(png: ByteArray): Triple<Int, Int, IntArray> {
        if (png.size < 8 || !png.copyOfRange(0, 8).contentEquals(SIGNATURE)) throw ParseException(P, "二维码图片不是 PNG")
        var pos = 8
        var width = 0
        var height = 0
        var depth = 0
        var colorType = 0
        var interlace = 0
        var palette: ByteArray? = null
        var trns: ByteArray? = null
        val idat = ByteArrayOutputStream()
        while (pos + 8 <= png.size) {
            val len = be32(png, pos)
            val type = String(png, pos + 4, 4, Charsets.ISO_8859_1)
            val data = pos + 8
            if (len < 0 || data + len > png.size) throw ParseException(P, "PNG 数据不完整")
            when (type) {
                "IHDR" -> {
                    width = be32(png, data)
                    height = be32(png, data + 4)
                    depth = png[data + 8].toInt() and 0xFF
                    colorType = png[data + 9].toInt() and 0xFF
                    interlace = png[data + 12].toInt() and 0xFF
                }
                "PLTE" -> palette = png.copyOfRange(data, data + len)
                "tRNS" -> trns = png.copyOfRange(data, data + len)
                "IDAT" -> idat.write(png, data, len)
            }
            if (type == "IEND") break
            pos = data + len + 4 // 跳过 CRC
        }
        if (width <= 0 || height <= 0 || width > 4096 || height > 4096) throw ParseException(P, "PNG 尺寸不对")
        if (interlace != 0) throw ParseException(P, "不支持隔行扫描的 PNG")
        val channels = when (colorType) {
            0, 3 -> 1
            2 -> 3
            4 -> 2
            6 -> 4
            else -> throw ParseException(P, "不支持的 PNG 颜色类型 $colorType")
        }
        if (depth !in setOf(1, 2, 4, 8, 16)) throw ParseException(P, "不支持的 PNG 位深 $depth")
        if (colorType == 3 && palette == null) throw ParseException(P, "PNG 缺少调色板")

        val bitsPerPixel = channels * depth
        val bpp = maxOf(1, bitsPerPixel / 8)
        val rowBytes = (width * bitsPerPixel + 7) / 8
        val raw = inflate(idat.toByteArray(), (rowBytes + 1) * height)
        if (raw.size < (rowBytes + 1) * height) throw ParseException(P, "PNG 像素数据不完整")

        // 还原每行的滤波
        val rows = Array(height) { ByteArray(rowBytes) }
        for (y in 0 until height) {
            val filter = raw[y * (rowBytes + 1)].toInt()
            val line = rows[y]
            val prev = if (y > 0) rows[y - 1] else ByteArray(rowBytes)
            val base = y * (rowBytes + 1) + 1
            for (x in 0 until rowBytes) {
                val cur = raw[base + x].toInt() and 0xFF
                val a = if (x >= bpp) line[x - bpp].toInt() and 0xFF else 0
                val b = prev[x].toInt() and 0xFF
                val c = if (x >= bpp) prev[x - bpp].toInt() and 0xFF else 0
                val v = when (filter) {
                    0 -> cur
                    1 -> cur + a
                    2 -> cur + b
                    3 -> cur + ((a + b) ushr 1)
                    4 -> cur + paeth(a, b, c)
                    else -> throw ParseException(P, "PNG 滤波类型 $filter 不对")
                }
                line[x] = v.toByte()
            }
        }

        val out = IntArray(width * height)
        val maxVal = (1 shl minOf(depth, 8)) - 1
        for (y in 0 until height) {
            val line = rows[y]
            for (x in 0 until width) {
                val luma = when (colorType) {
                    0 -> sample(line, x, depth) * 255 / maxVal.coerceAtLeast(1)
                    3 -> {
                        val i = sample(line, x, depth)
                        val pal = palette!!
                        if (i * 3 + 2 >= pal.size) 255
                        else if (trns != null && i < trns.size && (trns[i].toInt() and 0xFF) < 128) 255
                        else lumaOf(pal[i * 3].toInt() and 0xFF, pal[i * 3 + 1].toInt() and 0xFF, pal[i * 3 + 2].toInt() and 0xFF)
                    }
                    else -> {
                        val step = depth / 8
                        val o = x * channels * step
                        fun ch(k: Int) = line[o + k * step].toInt() and 0xFF // 16 位取高字节
                        when (colorType) {
                            2 -> lumaOf(ch(0), ch(1), ch(2))
                            4 -> if (ch(1) < 128) 255 else ch(0)
                            else -> if (ch(3) < 128) 255 else lumaOf(ch(0), ch(1), ch(2))
                        }
                    }
                }
                out[y * width + x] = (0xFF shl 24) or (luma shl 16) or (luma shl 8) or luma
            }
        }
        return Triple(width, height, out)
    }

    private fun lumaOf(r: Int, g: Int, b: Int) = (r * 299 + g * 587 + b * 114) / 1000

    /** 一行里第 [x] 个像素的样本值（灰度 / 调色板下标；16 位取高字节）。 */
    private fun sample(line: ByteArray, x: Int, depth: Int): Int = when (depth) {
        16 -> line[x * 2].toInt() and 0xFF
        8 -> line[x].toInt() and 0xFF
        else -> {
            val perByte = 8 / depth
            val b = line[x / perByte].toInt() and 0xFF
            (b shr (8 - depth * (x % perByte + 1))) and ((1 shl depth) - 1)
        }
    }

    private fun paeth(a: Int, b: Int, c: Int): Int {
        val p = a + b - c
        val pa = kotlin.math.abs(p - a)
        val pb = kotlin.math.abs(p - b)
        val pc = kotlin.math.abs(p - c)
        return if (pa <= pb && pa <= pc) a else if (pb <= pc) b else c
    }

    private fun be32(b: ByteArray, o: Int) =
        ((b[o].toInt() and 0xFF) shl 24) or ((b[o + 1].toInt() and 0xFF) shl 16) or ((b[o + 2].toInt() and 0xFF) shl 8) or (b[o + 3].toInt() and 0xFF)

    private fun inflate(data: ByteArray, expected: Int): ByteArray {
        val inflater = Inflater()
        try {
            inflater.setInput(data)
            val out = ByteArrayOutputStream(expected)
            val buf = ByteArray(8192)
            while (!inflater.finished()) {
                val n = inflater.inflate(buf)
                if (n == 0 && (inflater.needsInput() || inflater.needsDictionary())) break
                out.write(buf, 0, n)
            }
            return out.toByteArray()
        } catch (e: DataFormatException) {
            throw ParseException(P, "PNG 像素数据解压失败")
        } finally {
            inflater.end()
        }
    }
}
