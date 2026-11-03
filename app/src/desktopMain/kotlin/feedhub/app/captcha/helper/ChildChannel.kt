package feedhub.app.captcha.helper

import com.sun.jna.Library
import com.sun.jna.Native
import feedhub.app.captcha.CaptchaLog
import feedhub.app.captcha.HelperCommand
import feedhub.app.captcha.HelperWire
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.FileDescriptor
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.OutputStream
import java.io.PrintStream
import java.nio.ByteBuffer

/**
 * 子进程这头的通道（格式见 [feedhub.app.captcha.HelperProtocol]）。**不碰 CEF**，假子进程（测试）也用它。
 *
 * 一建好就做两件事：
 * 1. 把真正的 stdout（接 App 的管道）复制到一个私有 fd（`F_DUPFD_CLOEXEC`，CEF 起的渲染进程拿不到），
 *    再把 fd 1 指到 stderr：CEF / JCEF / JVM 往 stdout 打的字都进 stderr，不会混进帧流；
 * 2. `System.out` 也换成 stderr。
 *
 * 写是加锁的（CEF 的 UI 线程写帧、读线程回状态）；每条消息写完就 flush。
 * 写失败（App 那头没了）只记一次、之后的写直接丢——读线程会读到 EOF 并让进程退出。
 */
internal class ChildChannel private constructor(rawOut: OutputStream) {
    private val out = DataOutputStream(BufferedOutputStream(rawOut, 1 shl 20))
    private val input = DataInputStream(BufferedInputStream(FileInputStream(FileDescriptor.`in`), 1 shl 16))
    private val lock = Any()

    @Volatile var broken = false
        private set

    private var row = ByteArray(0)

    fun readCommand(): HelperCommand? = HelperWire.readCommand(input)

    fun hello(pid: Long) = send { HelperWire.writeHello(it, pid) }
    fun status(text: String) = send { HelperWire.writeStatus(it, text) }
    fun cursor(type: Int) = send { HelperWire.writeCursor(it, type) }
    fun loaded(httpStatus: Int) = send { HelperWire.writeLoaded(it, httpStatus) }
    fun failed(message: String) = send { HelperWire.writeFailed(it, message) }
    fun bye() = send { HelperWire.writeBye(it) }

    /**
     * 发一帧：[buffer] 是 CEF 给的整幅 BGRA（[width]×[height]），[rects] 是脏区（`x, y, w, h`，已经裁到画面里）。
     * 在 onPaint 里同步写完（缓冲区只在回调期间有效）；管道满了就等，CEF 的出帧会跟着慢下来。
     */
    fun frame(buffer: ByteBuffer, width: Int, height: Int, rects: List<IntArray>, paintStamp: Long, inputStamp: Long) = send { o ->
        HelperWire.writeFrameHeader(o, width, height, rects.size, paintStamp, inputStamp)
        for (r in rects) {
            if (row.size < r[2] * 4) row = ByteArray(r[2] * 4)
            HelperWire.writeRect(o, buffer, width, r[0], r[1], r[2], r[3], row)
        }
    }

    private inline fun send(block: (DataOutputStream) -> Unit) {
        if (broken) return
        synchronized(lock) {
            if (broken) return
            try {
                block(out)
                out.flush()
            } catch (e: Exception) {
                broken = true
                CaptchaLog.log("channel write failed: ${e.javaClass.simpleName}")
            }
        }
    }

    private interface LibC : Library {
        fun fcntl(fd: Int, cmd: Int, arg: Int): Int
        fun dup2(oldFd: Int, newFd: Int): Int
        fun close(fd: Int): Int
    }

    companion object {
        private const val F_DUPFD_CLOEXEC = 1030

        /** 在子进程 main 的第一行调用（在 CEF / JCEF 打出任何东西之前）。 */
        fun open(): ChildChannel {
            val rawOut: OutputStream = runCatching {
                // 直接给文件名：给 "c" 的话 JNA 会先起一个 `ldconfig -p` 子进程去查（几十毫秒）
                val libc = Native.load("libc.so.6", LibC::class.java)
                val fd = libc.fcntl(1, F_DUPFD_CLOEXEC, 10)
                check(fd >= 0) { "fcntl 失败" }
                // 重新打开这个 fd 对应的管道（Linux 的 /proc/self/fd 对管道可以这样开），之后私有 fd 关掉
                val stream = FileOutputStream("/proc/self/fd/$fd")
                libc.close(fd)
                check(libc.dup2(2, 1) == 1) { "dup2 失败" }
                stream
            }.getOrElse { e ->
                // 挪不动就直接用 stdout（只在非 Linux / JNA 不可用时；CEF 往 stdout 写字会弄坏帧流，App 那头会按"子进程出错"退回系统浏览器）
                CaptchaLog.log("stdout swap failed: ${e.message}")
                FileOutputStream(FileDescriptor.out)
            }
            System.setOut(PrintStream(FileOutputStream(FileDescriptor.err), true))
            return ChildChannel(rawOut)
        }
    }
}
