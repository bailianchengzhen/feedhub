package feedhub.app.captcha

import feedhub.captcha.RustCaptcha
import uniffi.feedhub_ffi.FfiCaptchaCommand
import uniffi.feedhub_ffi.FfiCaptchaEvent
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.EOFException

/**
 * App 进程 ⇄ 验证码子进程（`feedhub.app.captcha.helper.CaptchaHelperMain`）之间的消息。
 *
 * 协议常量、编解码规则与拼帧规则由 Rust（`RustCaptcha`）唯一定义。
 */
internal object HelperProtocol {
    const val VERSION = 1

    // App → 子进程
    const val OPEN = 1
    const val RESIZE = 2
    const val MOUSE = 3
    const val WHEEL = 4
    const val KEY = 5
    const val FOCUS = 6
    const val CLOSE = 7

    // 子进程 → App
    const val HELLO = 101
    const val STATUS = 102
    const val FRAME = 103
    const val CURSOR = 104
    const val LOADED = 105
    const val FAILED = 106
    const val BYE = 107

    /** 单个画面边长上限（像素）：防止坏数据让 App 一次分配几个 G。 */
    const val MAX_SIDE = 8192
}

/** App → 子进程。坐标都是 CSS 像素（= 逻辑像素，DIP），[scale] 是设备像素比。 */
internal sealed interface HelperCommand {
    data class Open(val url: String, val width: Int, val height: Int, val scale: Float) : HelperCommand
    data class Resize(val width: Int, val height: Int, val scale: Float) : HelperCommand

    data class Mouse(
        val id: Int,
        val x: Int,
        val y: Int,
        val modifiers: Int,
        val button: Int,
        val clickCount: Int,
        val stamp: Long,
    ) : HelperCommand

    data class Wheel(val x: Int, val y: Int, val modifiers: Int, val rotation: Int, val precise: Double, val stamp: Long) : HelperCommand

    data class Key(val id: Int, val keyCode: Int, val keyChar: Char, val modifiers: Int, val location: Int, val stamp: Long) : HelperCommand

    data class Focus(val focused: Boolean) : HelperCommand
    data object Close : HelperCommand

    fun toFfi(): FfiCaptchaCommand = when (this) {
        is Open -> FfiCaptchaCommand.Open(url, width, height, scale)
        is Resize -> FfiCaptchaCommand.Resize(width, height, scale)
        is Mouse -> FfiCaptchaCommand.Mouse(id, x, y, modifiers, button, clickCount, stamp)
        is Wheel -> FfiCaptchaCommand.Wheel(x, y, modifiers, rotation, precise, stamp)
        is Key -> FfiCaptchaCommand.Key(id, keyCode, keyChar.code.toUShort(), modifiers, location, stamp)
        is Focus -> FfiCaptchaCommand.Focus(focused)
        Close -> FfiCaptchaCommand.Close
    }
}

/** 子进程 → App（[HelperEvent.Frame] 的像素由 [HelperWire.readEvent] 直接写进调用方给的 [FrameAssembler]）。 */
internal sealed interface HelperEvent {
    data class Hello(val version: Int, val pid: Long) : HelperEvent
    data class Status(val text: String) : HelperEvent

    data class Frame(val width: Int, val height: Int, val rects: Int, val paintStamp: Long, val inputStamp: Long) : HelperEvent
    data class Cursor(val type: Int) : HelperEvent
    data class Loaded(val httpStatus: Int) : HelperEvent
    data class Failed(val message: String) : HelperEvent
    data object Bye : HelperEvent
}

/** 线上格式。两头共用，离线测试用它来回编码（`HelperProtocolTest`）。 */
internal object HelperWire {

    fun writeCommand(out: DataOutputStream, c: HelperCommand) {
        val bytes = RustCaptcha.encodeCommand(c.toFfi())
        out.write(bytes)
    }

    /** 读一条命令；流正常结束（App 那头关了）返回 null。 */
    fun readCommand(input: DataInputStream): HelperCommand? {
        val type = input.read()
        if (type < 0) return null
        return when (type) {
            HelperProtocol.OPEN -> {
                val utflen = input.readUnsignedShort()
                val utfBytes = ByteArray(utflen)
                input.readFully(utfBytes)
                val rest = ByteArray(12)
                input.readFully(rest)
                val full = ByteArray(1 + 2 + utflen + 12)
                full[0] = HelperProtocol.OPEN.toByte()
                full[1] = (utflen shr 8).toByte()
                full[2] = utflen.toByte()
                System.arraycopy(utfBytes, 0, full, 3, utflen)
                System.arraycopy(rest, 0, full, 3 + utflen, 12)
                val cmd = RustCaptcha.decodeCommand(full) as? FfiCaptchaCommand.Open
                    ?: throw java.io.IOException("未知命令 $type")
                HelperCommand.Open(cmd.url, cmd.width, cmd.height, cmd.scale)
            }
            HelperProtocol.RESIZE -> {
                val buf = ByteArray(13)
                buf[0] = HelperProtocol.RESIZE.toByte()
                input.readFully(buf, 1, 12)
                val cmd = RustCaptcha.decodeCommand(buf) as? FfiCaptchaCommand.Resize
                    ?: throw java.io.IOException("未知命令 $type")
                HelperCommand.Resize(cmd.width, cmd.height, cmd.scale)
            }
            HelperProtocol.MOUSE -> {
                val buf = ByteArray(33)
                buf[0] = HelperProtocol.MOUSE.toByte()
                input.readFully(buf, 1, 32)
                val cmd = RustCaptcha.decodeCommand(buf) as? FfiCaptchaCommand.Mouse
                    ?: throw java.io.IOException("未知命令 $type")
                HelperCommand.Mouse(cmd.id, cmd.x, cmd.y, cmd.modifiers, cmd.button, cmd.clickCount, cmd.stamp)
            }
            HelperProtocol.WHEEL -> {
                val buf = ByteArray(33)
                buf[0] = HelperProtocol.WHEEL.toByte()
                input.readFully(buf, 1, 32)
                val cmd = RustCaptcha.decodeCommand(buf) as? FfiCaptchaCommand.Wheel
                    ?: throw java.io.IOException("未知命令 $type")
                HelperCommand.Wheel(cmd.x, cmd.y, cmd.modifiers, cmd.rotation, cmd.precise, cmd.stamp)
            }
            HelperProtocol.KEY -> {
                val buf = ByteArray(27)
                buf[0] = HelperProtocol.KEY.toByte()
                input.readFully(buf, 1, 26)
                val cmd = RustCaptcha.decodeCommand(buf) as? FfiCaptchaCommand.Key
                    ?: throw java.io.IOException("未知命令 $type")
                HelperCommand.Key(cmd.id, cmd.keyCode, cmd.keyChar.toInt().toChar(), cmd.modifiers, cmd.location, cmd.stamp)
            }
            HelperProtocol.FOCUS -> {
                val buf = ByteArray(2)
                buf[0] = HelperProtocol.FOCUS.toByte()
                buf[1] = input.readByte()
                val cmd = RustCaptcha.decodeCommand(buf) as? FfiCaptchaCommand.Focus
                    ?: throw java.io.IOException("未知命令 $type")
                HelperCommand.Focus(cmd.focused)
            }
            HelperProtocol.CLOSE -> HelperCommand.Close
            else -> throw java.io.IOException("未知命令 $type")
        }
    }

    fun writeHello(out: DataOutputStream, pid: Long) {
        out.write(RustCaptcha.encodeHello(HelperProtocol.VERSION, pid))
    }

    fun writeStatus(out: DataOutputStream, text: String) {
        out.write(RustCaptcha.encodeStatus(text))
    }

    fun writeCursor(out: DataOutputStream, type: Int) {
        out.write(RustCaptcha.encodeCursor(type))
    }

    fun writeLoaded(out: DataOutputStream, httpStatus: Int) {
        out.write(RustCaptcha.encodeLoaded(httpStatus))
    }

    fun writeFailed(out: DataOutputStream, message: String) {
        out.write(RustCaptcha.encodeFailed(message))
    }

    fun writeBye(out: DataOutputStream) {
        out.write(RustCaptcha.encodeBye())
    }

    fun writeFrameHeader(out: DataOutputStream, width: Int, height: Int, rects: Int, paintStamp: Long, inputStamp: Long) {
        out.write(RustCaptcha.encodeFrameHeader(width, height, rects, paintStamp, inputStamp))
    }

    fun writeRect(out: DataOutputStream, src: java.nio.ByteBuffer, srcWidth: Int, x: Int, y: Int, w: Int, h: Int, row: ByteArray) {
        val plan = RustCaptcha.planRect(srcWidth, Int.MAX_VALUE, x, y, w, h)
        if (!plan.valid) throw java.io.IOException("坏脏区 ${w}x$h")
        out.writeInt(x); out.writeInt(y); out.writeInt(w); out.writeInt(h)
        val bytes = plan.bytesPerRow
        val stride = plan.rowStrideBytes
        for (r in 0 until plan.rows) {
            src.get((y + r) * stride + x * 4, row, 0, bytes)
            out.write(row, 0, bytes)
        }
    }

    fun readEvent(input: DataInputStream, frames: FrameAssembler): HelperEvent? {
        val type = input.read()
        if (type < 0) return null
        return when (type) {
            HelperProtocol.HELLO -> {
                val buf = ByteArray(13)
                buf[0] = type.toByte()
                input.readFully(buf, 1, 12)
                val ev = RustCaptcha.decodeEventHeader(buf) as? FfiCaptchaEvent.Hello
                    ?: throw java.io.IOException("未知消息 $type")
                HelperEvent.Hello(ev.version, ev.pid)
            }
            HelperProtocol.STATUS -> {
                val utflen = input.readUnsignedShort()
                val utfBytes = ByteArray(utflen)
                input.readFully(utfBytes)
                val full = ByteArray(3 + utflen)
                full[0] = type.toByte()
                full[1] = (utflen shr 8).toByte()
                full[2] = utflen.toByte()
                System.arraycopy(utfBytes, 0, full, 3, utflen)
                val ev = RustCaptcha.decodeEventHeader(full) as? FfiCaptchaEvent.Status
                    ?: throw java.io.IOException("未知消息 $type")
                HelperEvent.Status(ev.text)
            }
            HelperProtocol.FRAME -> {
                val buf = ByteArray(29)
                buf[0] = type.toByte()
                input.readFully(buf, 1, 28)
                val ev = RustCaptcha.decodeEventHeader(buf) as? FfiCaptchaEvent.Frame
                    ?: throw java.io.IOException("未知消息 $type")
                frames.begin(ev.width, ev.height)
                repeat(ev.rects) {
                    frames.readRect(input, input.readInt(), input.readInt(), input.readInt(), input.readInt())
                }
                HelperEvent.Frame(ev.width, ev.height, ev.rects, ev.paintStamp, ev.inputStamp)
            }
            HelperProtocol.CURSOR -> {
                val buf = ByteArray(5)
                buf[0] = type.toByte()
                input.readFully(buf, 1, 4)
                val ev = RustCaptcha.decodeEventHeader(buf) as? FfiCaptchaEvent.Cursor
                    ?: throw java.io.IOException("未知消息 $type")
                HelperEvent.Cursor(ev.cursorType)
            }
            HelperProtocol.LOADED -> {
                val buf = ByteArray(5)
                buf[0] = type.toByte()
                input.readFully(buf, 1, 4)
                val ev = RustCaptcha.decodeEventHeader(buf) as? FfiCaptchaEvent.Loaded
                    ?: throw java.io.IOException("未知消息 $type")
                HelperEvent.Loaded(ev.httpStatus)
            }
            HelperProtocol.FAILED -> {
                val utflen = input.readUnsignedShort()
                val utfBytes = ByteArray(utflen)
                input.readFully(utfBytes)
                val full = ByteArray(3 + utflen)
                full[0] = type.toByte()
                full[1] = (utflen shr 8).toByte()
                full[2] = utflen.toByte()
                System.arraycopy(utfBytes, 0, full, 3, utflen)
                val ev = RustCaptcha.decodeEventHeader(full) as? FfiCaptchaEvent.Failed
                    ?: throw java.io.IOException("未知消息 $type")
                HelperEvent.Failed(ev.detail)
            }
            HelperProtocol.BYE -> HelperEvent.Bye
            else -> throw java.io.IOException("未知消息 $type")
        }
    }
}

/**
 * App 这边拼帧：保留一整幅 BGRA，脏区来了就覆盖那一块。
 */
internal class FrameAssembler {
    var width = 0
        private set
    var height = 0
        private set
    var pixels = ByteArray(0)
        private set

    fun begin(w: Int, h: Int) {
        if (w == width && h == height) return
        width = w
        height = h
        pixels = ByteArray(w * h * 4)
    }

    /** 读一个脏区的像素；越界的部分照样从流里读掉（保持流对齐），但不写。 */
    fun readRect(input: DataInputStream, x: Int, y: Int, w: Int, h: Int) {
        val plan = RustCaptcha.planRect(width, height, x, y, w, h)
        if (!plan.valid) throw java.io.IOException("坏脏区 ${w}x$h")
        if (plan.inside) {
            for (r in 0 until plan.rows) {
                input.readFully(pixels, plan.initialOffsetBytes + r * plan.rowStrideBytes, plan.bytesPerRow)
            }
        } else {
            skipFully(input, plan.totalBytes)
        }
    }

    private fun skipFully(input: DataInputStream, n: Long) {
        var left = n
        while (left > 0) {
            val s = input.skip(left)
            if (s <= 0) {
                if (input.read() < 0) throw EOFException()
                left--
            } else {
                left -= s
            }
        }
    }
}
