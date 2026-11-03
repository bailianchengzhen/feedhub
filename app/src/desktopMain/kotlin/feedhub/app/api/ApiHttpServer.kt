package feedhub.app.api

import java.io.ByteArrayOutputStream
import java.net.StandardProtocolFamily
import java.net.UnixDomainSocketAddress
import java.nio.ByteBuffer
import java.nio.channels.SelectionKey
import java.nio.channels.Selector
import java.nio.channels.ServerSocketChannel
import java.nio.channels.SocketChannel
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.attribute.BasicFileAttributes
import java.nio.file.attribute.PosixFilePermissions
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import java.net.URLDecoder

/**
 * **HTTP over unix socket** 服务端。
 *
 * 为什么自己写（而不是引一个库）：JDK 自带的 `com.sun.net.httpserver` **只会 bind TCP 端口**，
 * 不支持 AF_UNIX；而这份接口的要求是"**不开 TCP 端口**"（本机任何进程、包括浏览器页面都能碰 TCP，
 * unix socket 只有本用户能连）。所以用 JDK 自带的 `ServerSocketChannel(StandardProtocolFamily.UNIX)`
 * + 一个很小的 HTTP/1.1 子集（只 GET、`Connection: close`、定长 body）。**不引任何依赖**。
 *
 * 线程模型：一条 accept 线程 + 一个**定长**处理池（[MAX_CONCURRENCY]，多出来的进有界队列，
 * 队列也满了就直接回 `503 busy`）。请求在池线程上用 `runBlocking` 调 Source——**绝不在 Compose 主线程上**
 * 干活，所以几个 agent 同时调也不会把界面卡住。
 *
 * 安全：socket 文件 0600、目录 0700；不监听 TCP；只接受 GET。
 */
internal class UnixHttpServer(
    private val socketPath: Path,
    private val handler: (ApiRequest) -> ApiReply,
) {
    private val acceptThread = AtomicReference<Thread?>(null)
    private val running = AtomicBoolean(false)
    private val server = AtomicReference<ServerSocketChannel?>(null)
    private val pool = AtomicReference<ThreadPoolExecutor?>(null)

    /**
     * 这个 socket 文件是**我们自己绑出来的**吗。只有它才归我们删：起不来的时候（例如另一个 feedhub
     * 进程已经在听）**绝不能删别人的 socket**，那是把人家弄哑。
     */
    private val bound = AtomicBoolean(false)

    /**
     * 绑出来的那个 socket 文件的身份（Linux 上是 dev + inode）。删之前比一下：路径上已经是**别人**
     * （例如重启时新进程）重新绑的文件了就不删。
     */
    private val boundKey = AtomicReference<Any?>(null)

    /** 起不来 / 跑着跑着出事的最后一个原因（[AppApiStatus.error] 用它）；null = 好的。 */
    @Volatile
    var lastError: String? = null
        private set

    /** socket 真的在听吗（[start] 成功之后为 true，[stop] 之后 false）。 */
    val listening: Boolean get() = running.get()

    /**
     * 上一次 [start] 没起来，是不是因为**路径上有别人在听**（[SocketBusyException]）。这种多半是刚关掉、
     * 还在收尾的上一个 feedhub 进程，过几秒就会让出来，调用方据此在后台重试。
     */
    @Volatile
    var busy: Boolean = false
        private set

    @Synchronized
    fun start() {
        if (running.get()) return
        lastError = null
        busy = false
        try {
            prepareDirectory(socketPath.parent)
            removeStaleSocket(socketPath)
            val ch = ServerSocketChannel.open(StandardProtocolFamily.UNIX)
            ch.bind(UnixDomainSocketAddress.of(socketPath))
            bound.set(true)
            boundKey.set(fileKeyOf(socketPath))
            // 只有本用户能连：socket 文件 0600（目录 0700 在 prepareDirectory 里）
            runCatching { Files.setPosixFilePermissions(socketPath, PosixFilePermissions.fromString("rw-------")) }
            server.set(ch)
            val executor = ThreadPoolExecutor(
                MAX_CONCURRENCY,
                MAX_CONCURRENCY,
                30,
                TimeUnit.SECONDS,
                ArrayBlockingQueue(QUEUE_DEPTH),
                { r -> Thread(r, "feedhub-api-worker").apply { isDaemon = true } },
            ).apply { allowCoreThreadTimeOut(true) }
            pool.set(executor)
            running.set(true)
            val t = Thread({ acceptLoop(ch, executor) }, "feedhub-api-accept").apply { isDaemon = true }
            acceptThread.set(t)
            t.start()
        } catch (e: Throwable) {
            lastError = e.message ?: e::class.simpleName ?: "起不来"
            busy = e is SocketBusyException
            cleanup()
        }
    }

    @Synchronized
    fun stop() {
        if (!running.getAndSet(false)) {
            // 没在听：只清掉**我们自己绑出来**但没删掉的那个文件（别人的 socket 不碰）
            deleteOwnSocket()
            return
        }
        cleanup()
    }

    /**
     * 收尾。**先删文件、再关 channel**：反过来的话，关掉之后、删文件之前
     * （等处理池最多 2 秒）新进程会把这个连不上的文件当残留删掉、绑上自己的，接着又被我们删掉——
     * 新进程显示"在听"，路径上却没有文件。先删的话，新进程要么连上我们（→ [SocketBusyException]、
     * 后台重试），要么看到路径是空的、直接绑。删也只删**自己绑的那个 inode**。
     * 注意 accept 线程阻塞在可中断 channel 上，interrupt 会直接关掉 channel，所以删文件要排在它前面。
     */
    private fun cleanup() {
        deleteOwnSocket()
        acceptThread.getAndSet(null)?.let { runCatching { it.interrupt() } }
        runCatching { server.getAndSet(null)?.close() }
        pool.getAndSet(null)?.let { p ->
            p.shutdown()
            runCatching { p.awaitTermination(2, TimeUnit.SECONDS) }
        }
    }

    /** 只删**我们自己绑出来**、而且路径上还是同一个 inode 的那个文件（别人的 socket 不碰）。 */
    private fun deleteOwnSocket() {
        if (!bound.getAndSet(false)) return
        val mine = boundKey.getAndSet(null)
        val current = fileKeyOf(socketPath)
        // 拿不到身份（文件系统不给 fileKey）时退回老行为：只看 bound
        if (mine != null && current != mine) return
        runCatching { Files.deleteIfExists(socketPath) }
    }

    private fun acceptLoop(ch: ServerSocketChannel, executor: ThreadPoolExecutor) {
        while (running.get()) {
            val client = try {
                ch.accept()
            } catch (e: Throwable) {
                if (running.get()) lastError = "accept 失败：${e.message ?: e::class.simpleName}"
                break
            } ?: continue
            try {
                executor.execute { serve(client) }
            } catch (_: java.util.concurrent.RejectedExecutionException) {
                // 队列也满了：不排队，直接说"忙"（调用方按 503 + retryable 等一会儿再来）
                runCatching {
                    val busy = ApiFailure(ApiError.BUSY, "同时在跑的调用太多，等会儿再来")
                    // 连处理都没轮到：这次调用一个平台请求都没发（requests = 0，和 done 行同一个字段）
                    writeReply(client, ApiReply(busy.error.httpStatus, busy.toJsonLine(0) + "\n", busy.error.code))
                    client.close()
                }
            }
        }
    }

    private fun serve(client: SocketChannel) {
        try {
            val request = readRequest(client) ?: return
            val result = if (request.method != "GET") {
                val wrongMethod = ApiFailure(ApiError.BAD_INPUT, "这个接口是只读的，只接受 GET（收到 ${request.method}）")
                // 没进路由：这次调用一个平台请求都没发（requests = 0，和 done 行同一个字段）
                ApiReply(405, wrongMethod.toJsonLine(0) + "\n", wrongMethod.error.code)
            } else {
                try {
                    handler(request)
                } catch (e: Throwable) {
                    val failure = e.toApiFailure()
                    ApiReply(failure.error.httpStatus, failure.toJsonLine() + "\n", failure.error.code)
                }
            }
            writeReply(client, result)
        } catch (_: Throwable) {
            // 客户端断开了 / 半截请求：什么都不用做
        } finally {
            runCatching { client.close() }
        }
    }

    private fun readRequest(ch: SocketChannel): ApiRequest? {
        val buffer = ByteArrayOutputStream()
        val tmp = ByteBuffer.allocate(4096)
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(READ_TIMEOUT_S)
        ch.configureBlocking(false)
        Selector.open().use { selector ->
            ch.register(selector, SelectionKey.OP_READ)
            while (true) {
                val head = parseHeaderEnd(buffer.toByteArray())
                if (head > 0) break
                if (buffer.size() > MAX_HEADER_BYTES) return null
                val left = deadline - System.nanoTime()
                if (left <= 0) return null
                selector.select(left.coerceAtLeast(1))
                selector.selectedKeys().clear()
                tmp.clear()
                val n = ch.read(tmp)
                // 对端在给出完整请求头之前就关了：当坏请求，不回应
                if (n < 0) return null
                if (n == 0) continue
                buffer.write(tmp.array(), 0, n)
            }
        }
        ch.configureBlocking(true)
        return parseRequest(buffer.toByteArray())
    }

    private fun writeReply(ch: SocketChannel, reply: ApiReply) {
        val body = reply.body
        val head = buildString {
            append("HTTP/1.1 ").append(reply.status).append(' ').append(reason(reply.status)).append("\r\n")
            append("Content-Type: ").append(reply.contentType).append("\r\n")
            append("Content-Length: ").append(body.size).append("\r\n")
            append("Cache-Control: no-store\r\n")
            append("Connection: close\r\n")
            append("X-Feedhub-Schema: ").append(SCHEMA_VERSION).append("\r\n")
            // 额外响应头
            for ((name, value) in reply.extraHeaders) {
                append(name).append(": ").append(value).append("\r\n")
            }
            append("\r\n")
        }.toByteArray(Charsets.US_ASCII)
        val all = ByteBuffer.allocate(head.size + body.size).put(head).put(body)
        all.flip()
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(WRITE_TIMEOUT_S)
        ch.configureBlocking(false)
        Selector.open().use { selector ->
            ch.register(selector, SelectionKey.OP_WRITE)
            while (all.hasRemaining()) {
                val left = deadline - System.nanoTime()
                if (left <= 0) return
                selector.select(left.coerceAtLeast(1))
                selector.selectedKeys().clear()
                ch.write(all)
            }
        }
        // 半关：对端读到 EOF 就知道到头了（HTTP/1.1 的 Connection: close）
        runCatching { ch.shutdownOutput() }
    }

    companion object {
        /** 同时在跑的调用上限（和 App 自己的取流共用网络，不抢太多）。 */
        const val MAX_CONCURRENCY: Int = 4

        /** 排队上限：满了直接回 `503 busy`，不无限攒着。 */
        const val QUEUE_DEPTH: Int = 16

        private const val READ_TIMEOUT_S = 15L
        private const val WRITE_TIMEOUT_S = 30L
        private const val MAX_HEADER_BYTES = 16 * 1024

        private fun reason(status: Int): String = when (status) {
            200 -> "OK"
            400 -> "Bad Request"
            401 -> "Unauthorized"
            403 -> "Forbidden"
            404 -> "Not Found"
            405 -> "Method Not Allowed"
            409 -> "Conflict"
            500 -> "Internal Server Error"
            502 -> "Bad Gateway"
            503 -> "Service Unavailable"
            504 -> "Gateway Timeout"
            else -> "Status"
        }

        /** 目录：不存在就建（0700）；已存在（桌面 `~/.cache/feedhub`）不动它的权限——socket 本身是 0600。 */
        internal fun prepareDirectory(dir: Path?) {
            if (dir == null) return
            if (Files.isDirectory(dir)) return
            Files.createDirectories(dir)
            runCatching { Files.setPosixFilePermissions(dir, PosixFilePermissions.fromString("rwx------")) }
        }

        /**
         * 上次没收干净的 socket 文件：先试着连一下——连得上说明**另一个 feedhub 进程**正在提供接口，
         * 这时不能删（删掉就把人家弄哑了），直接报错；连不上就是残留，删掉重新 bind。
         */
        internal fun removeStaleSocket(path: Path) {
            if (!Files.exists(path, LinkOption.NOFOLLOW_LINKS)) return
            val live = runCatching {
                SocketChannel.open(UnixDomainSocketAddress.of(path)).use { true }
            }.getOrDefault(false)
            if (live) throw SocketBusyException(path)
            Files.deleteIfExists(path)
        }

        /** 文件身份（dev + inode）；不存在 / 读不了就是 null。 */
        private fun fileKeyOf(path: Path): Any? = runCatching {
            Files.readAttributes(path, BasicFileAttributes::class.java, LinkOption.NOFOLLOW_LINKS).fileKey()
        }.getOrNull()

        private fun parseHeaderEnd(bytes: ByteArray): Int {
            for (i in 0 until bytes.size - 3) {
                if (bytes[i] == 13.toByte() && bytes[i + 1] == 10.toByte() &&
                    bytes[i + 2] == 13.toByte() && bytes[i + 3] == 10.toByte()
                ) {
                    return i
                }
            }
            return -1
        }

        /** 请求行 + 头 → [ApiRequest]；看不懂就是 null（当坏请求，客户端那边看到连接被关）。 */
        internal fun parseRequest(bytes: ByteArray): ApiRequest? {
            val text = String(bytes, Charsets.UTF_8)
            val lines = text.split("\r\n")
            val requestLine = lines.firstOrNull()?.trim().orEmpty()
            val parts = requestLine.split(' ')
            if (parts.size < 3) return null
            val method = parts[0]
            val target = parts[1]
            val headers = LinkedHashMap<String, String>()
            for (line in lines.drop(1)) {
                if (line.isEmpty()) break
                val i = line.indexOf(':')
                if (i <= 0) continue
                headers[line.substring(0, i).trim().lowercase()] = line.substring(i + 1).trim()
            }
            val q = target.indexOf('?')
            val path = if (q < 0) target else target.substring(0, q)
            val query = if (q < 0) emptyMap() else parseQuery(target.substring(q + 1))
            return ApiRequest(method.trim(), path, query, headers)
        }

        /** `a=1&b=x%20y` → 解码后的表（同名参数取第一个；`+` 当空格）。 */
        internal fun parseQuery(raw: String): Map<String, String> {
            val out = LinkedHashMap<String, String>()
            if (raw.isEmpty()) return out
            for (pair in raw.split('&')) {
                if (pair.isEmpty()) continue
                val i = pair.indexOf('=')
                val key = if (i < 0) pair else pair.substring(0, i)
                val value = if (i < 0) "" else pair.substring(i + 1)
                val k = decode(key)
                if (!out.containsKey(k)) out[k] = decode(value)
            }
            return out
        }

        private fun decode(s: String): String = runCatching { URLDecoder.decode(s, Charsets.UTF_8) }.getOrDefault(s)
    }
}

/** 路径上已经有人在听（[UnixHttpServer.removeStaleSocket] 连上了）：不抢、不删，把原因报出来。 */
internal class SocketBusyException(path: Path) : IllegalStateException(
    "另一个 feedhub 进程（可能是刚关掉、还没退干净的那个）已经在提供这个接口（$path），这个窗口里的开关先不生效",
)

/** 一个请求：方法、路径、解码后的查询参数、头（键小写）。 */
internal class ApiRequest(
    val method: String,
    val path: String,
    val query: Map<String, String>,
    val headers: Map<String, String>,
) {
    /** 取一个参数；空串当没给（`?platform=` 和没写一样）。 */
    fun param(name: String): String? = query[name]?.trim()?.takeIf { it.isNotEmpty() }

    fun required(name: String): String =
        param(name) ?: throw ApiFailure(ApiError.BAD_INPUT, "缺少参数 $name")
}

/**
 * 一个响应：状态码 + 正文（**字节**：[ApiReply] 既能给 JSON Lines，也能给一张 JPEG）+ 错误码
 * （成功是 null，只用来给设置页记"最近一次失败"）。
 *
 * 尺寸 / 段数那些元信息放响应头（`X-Feedhub-Width` / `-Height` / `-Segments` / `-Requests`）。
 */
internal class ApiReply(
    val status: Int,
    val body: ByteArray,
    val errorCode: String? = null,
    val contentType: String = NDJSON,
    val extraHeaders: Map<String, String> = emptyMap(),
) {
    constructor(
        status: Int,
        body: String,
        errorCode: String? = null,
        extraHeaders: Map<String, String> = emptyMap(),
    ) : this(status, body.toByteArray(Charsets.UTF_8), errorCode, NDJSON, extraHeaders)

    companion object {
        const val NDJSON: String = "application/x-ndjson; charset=utf-8"
    }
}
