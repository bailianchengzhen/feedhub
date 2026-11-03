package feedhub.core.sync

import feedhub.rust.RustSync
import kotlinx.serialization.Serializable
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import uniffi.feedhub_ffi.FfiSyncException
import uniffi.feedhub_ffi.FfiSyncPairingMessage
import uniffi.feedhub_ffi.FfiSyncPairingOffer
import uniffi.feedhub_ffi.ffiSyncCleanDeviceName
import uniffi.feedhub_ffi.ffiSyncPairResult
import uniffi.feedhub_ffi.ffiSyncPairingAad
import uniffi.feedhub_ffi.ffiSyncPairingMessageDecode
import uniffi.feedhub_ffi.ffiSyncPairingMessageEncode
import uniffi.feedhub_ffi.ffiSyncPairingMessageOwn
import uniffi.feedhub_ffi.ffiSyncPairingOfferAad
import uniffi.feedhub_ffi.ffiSyncPairingOfferContent
import uniffi.feedhub_ffi.ffiSyncPairingOfferLooksLike
import uniffi.feedhub_ffi.ffiSyncPairingOfferParse
import uniffi.feedhub_ffi.ffiSyncPairingOfferPath
import uniffi.feedhub_ffi.ffiSyncPairingOfferUrl
import uniffi.feedhub_ffi.ffiSyncPairingProtocol
import java.io.BufferedInputStream
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketException
import java.net.SocketTimeoutException

/**
 * 设备配对的线上协议。
 *
 * **复用「登录同步」那套通道**（`SyncCrypto` / `SyncServers` 的形状：局域网 IPv4、随机端口、
 * 一次性的 AES-256-GCM 会话密钥、二维码、两分钟量级的 TTL、成功一次就关），不另起一套传输：
 * 只是把"服务端 GET 一份密文"换成"**手机 POST 自己的公钥，电脑回自己的公钥**"这一来一回。
 *
 * 二维码内容（`feedhub-pair:` 前缀，和登录同步的 `feedhub-sync:` 分开，扫错了不会串）：
 * ```
 * feedhub-pair:1?h=192.168.0.5&p=43127&s=<22 字符 base64url>&k=<43 字符 base64url>
 * ```
 * **二维码里没有公钥、没有设备名、没有 Cookie**；只有"这个会话的一次性密钥"。
 *
 * 交换完各自算一个 **6 位短校验码**（[PairingCode]：两边公钥 + 设备 id 的 SHA-256），用户在两台设备上
 * 目测一致后点「确认」才把对方存进 `sync_peer`——防扫错码 / 局域网中间人。
 *
 * **解析 / 报文编解码 / 设备名清洗在 Rust**（`feedhub-core/src/sync/pairing.rs`）；
 * [PairingServer] / [PairingServers] / [PairingClient]（局域网收发）留在 Kotlin。
 */
object PairProtocol {
    private val values by lazy { RustSync.guard { ffiSyncPairingProtocol() } }

    val VERSION: Int get() = values.version

    /** 配对二维码的开头（和登录同步的 `feedhub-sync` 区分开）。 */
    val QR_PREFIX: String get() = values.qrPrefix

    /** 服务端只认这一个路径前缀，后面接会话 id。 */
    val PATH_PREFIX: String get() = values.pathPrefix

    /** 配对二维码有效期：三分钟（比登录同步的两分钟宽裕一点，要等用户核对 6 位码）。 */
    val TTL_MS: Long get() = values.ttlMs

    val KEY_BYTES: Int get() = values.keyBytes.toInt()
    val SESSION_BYTES: Int get() = values.sessionBytes.toInt()
    const val NONCE_BYTES = 16

    /** 一条配对报文（明文 JSON）的上限；里面只有设备 id / 名字 / 公钥 / 随机数。 */
    val MAX_MESSAGE_BYTES: Int get() = values.maxMessageBytes.toInt()

    /** 请求体（密文）上限。 */
    val MAX_BODY_BYTES: Int get() = values.maxBodyBytes.toInt()

    /** 设备名最长多少个字符（界面显示 / 存库也截到这么长）。 */
    val NAME_MAX: Int get() = values.nameMax.toInt()

    /** 和登录同步同一套 AAD 形状，只换域名前缀；换会话 / 换版本都解不开。 */
    fun aad(sessionId: String): ByteArray = RustSync.guard { ffiSyncPairingAad(sessionId) }
}

/** 配对过程中的失败（消息可以直接给用户看，**不含密钥**）。 */
class PairingException(message: String, cause: Throwable? = null) : Exception(message, cause)

/**
 * 配对二维码：电脑（或另一台手机）的局域网地址 + 端口 + 一次性会话 id + 一次性密钥。
 * 和 [SyncOffer] 一样，[toString] 不含密钥。
 */
class PairingOffer(
    val host: String,
    val port: Int,
    val sessionId: String,
    private val keyBytes: ByteArray,
    val version: Int = PairProtocol.VERSION,
) {
    init {
        // 判定在 Rust（`PairingOffer::new`，和 Kotlin 原实现的 `init` 一样）；错误翻回 IllegalArgumentException。
        // 和 [SyncOffer] 一样**构造前先 `ensureLoaded`**：构造器可能比任何一次 `RustSync.guard` 都早
        // （`PairingOffer.create` 直接 new）。`.so` 加载失败抛 `IllegalStateException`，不折成参数错误。
        try {
            RustSync.guard { ffiSyncPairingOfferPath(raw()) }
        } catch (e: FfiSyncException.Format) {
            throw IllegalArgumentException(e.detail)
        } catch (e: FfiSyncException.Internal) {
            throw IllegalArgumentException(e.detail)
        }
    }

    /** 一次性密钥的副本（和 Kotlin 原实现的 `val key` 一个用法，但拿到的是拷贝）。 */
    val key: ByteArray get() = keyBytes.copyOf()

    internal fun keyInternal(): ByteArray = keyBytes

    val content: String get() = RustSync.guard { ffiSyncPairingOfferContent(raw()) }

    val path: String get() = RustSync.guard { ffiSyncPairingOfferPath(raw()) }

    val url: String get() = RustSync.guard { ffiSyncPairingOfferUrl(raw()) }

    private fun raw(): FfiSyncPairingOffer =
        FfiSyncPairingOffer(host = host, port = port.toUInt(), sessionId = sessionId, key = keyBytes, version = version)

    fun aad(): ByteArray = RustSync.guard { ffiSyncPairingOfferAad(raw()) }

    override fun toString(): String = "PairingOffer($host:$port, session=$sessionId, v=$version)"

    override fun equals(other: Any?): Boolean =
        other is PairingOffer && other.host == host && other.port == port &&
            other.sessionId == sessionId && other.keyBytes.contentEquals(keyBytes) && other.version == version

    override fun hashCode(): Int = listOf(host, port, sessionId, version).hashCode() * 31 + keyBytes.contentHashCode()

    companion object {
        fun create(host: String, port: Int): PairingOffer =
            PairingOffer(host, port, SyncCrypto.newSessionId(), SyncCrypto.newKey())

        fun looksLikePairing(content: String): Boolean = RustSync.guard { ffiSyncPairingOfferLooksLike(content) }

        /** 严格解析；任何一处不对都返回 null（不猜、不修正，也不把密钥写进错误里）。 */
        fun parse(content: String): PairingOffer? = RustSync.guard {
            ffiSyncPairingOfferParse(content)?.let {
                PairingOffer(it.host, it.port.toInt(), it.sessionId, it.key, it.version)
            }
        }
    }
}

/** 配对时交换的那一份信息：设备 id + 设备名 + 公钥（+ 请求随机数，响应里原样带回）。 */
@Serializable
data class PairingMessage(
    val v: Int = PairProtocol.VERSION,
    val device: String,
    val name: String,
    val publicKey: String,
    val nonce: String,
)

/** 配对成功后的对端（还没落库；用户点「确认」才写 `sync_peer`）。 */
class PairingPeer(
    val device: String,
    val name: String,
    val publicKey: ByteArray,
) {
    override fun toString(): String = "PairingPeer($device, $name)"
}

/** 配对结果：对端 + 两边应该一致的 6 位校验码。 */
data class PairingResult(
    val peer: PairingPeer,
    val code: String,
)

/**
 * 配对报文的编解码 + 严格校验。**实现在 Rust**（`sync/pairing.rs` 的 `codec`）。
 *
 * 和快照编解码不同，这边是 `Json { encodeDefaults = true; ignoreUnknownKeys = false }`：
 * 字段全部写出来（含 `v`），多一个不认识的字段就整条拒。
 */
object PairingCodec {
    fun encode(message: PairingMessage): ByteArray = RustSync.guard {
        ffiSyncPairingMessageEncode(message.toFfi())
    }

    /** 解不开 / 字段不对 / 版本不对 / 公钥不合法 → null。 */
    fun decode(bytes: ByteArray): PairingMessage? = RustSync.guard {
        ffiSyncPairingMessageDecode(bytes)?.toModel()
    }

    /** 本机的一条报文（新随机数）。 */
    fun own(
        deviceId: String,
        name: String,
        publicKey: ByteArray,
        random: java.security.SecureRandom = java.security.SecureRandom(),
    ): PairingMessage {
        val nonce = ByteArray(PairProtocol.NONCE_BYTES).also(random::nextBytes)
        return RustSync.guard { ffiSyncPairingMessageOwn(deviceId, name, publicKey, nonce).toModel() }
    }

    /** 界面 / 库里显示的设备名：去掉控制字符、压掉多余空白、截到 [PairProtocol.NAME_MAX]。 */
    fun cleanName(name: String): String = RustSync.guard { ffiSyncCleanDeviceName(name) }

    /**
     * 两台设备的配对结果（对端 + 6 位码）。设备 id 撞上 / 公钥长度不对 → 抛 [PairingException]。
     */
    internal fun result(own: PairingMessage, peer: PairingMessage): PairingResult =
        RustSync.guard { ffiSyncPairResult(own.toFfi(), peer.toFfi()) }?.let {
            PairingResult(PairingPeer(it.device, it.name, it.publicKey), it.code)
        } ?: throw PairingException("对方报上来的设备 id 和本机一样，不能自己和自己配对")
}

internal fun PairingMessage.toFfi(): FfiSyncPairingMessage =
    FfiSyncPairingMessage(v = v, device = device, name = name, publicKey = publicKey, nonce = nonce)

internal fun FfiSyncPairingMessage.toModel(): PairingMessage =
    PairingMessage(v = v, device = device, name = name, publicKey = publicKey, nonce = nonce)

/**
 * 显示配对二维码的那一端（桌面 / 安卓都能当）：一条一次性的局域网小服务。
 *
 * 只认一个请求：`POST <PATH_PREFIX><会话 id>`，body 是对方用一次性密钥封好的 [PairingMessage]。
 * 收到后解开、校验、把本机的报文（带对方的 nonce）封回去，服务立刻关。**成功只有一次**。
 * 安全形状和 [SyncServer] 一样：绑具体局域网地址（不绑 0.0.0.0）、最多 8 个连接、错 3 次提前收摊、
 * 请求头 / body 都有上限、到点自己关。
 */
class PairingServer(
    private val bindAddress: InetAddress,
    private val offer: PairingOffer,
    private val own: PairingMessage,
    private val port: Int = 0,
    private val ttlMs: Long = PairProtocol.TTL_MS,
    private val clock: () -> Long = System::currentTimeMillis,
) {
    enum class State {
        IDLE,
        LISTENING,

        /** 收到并解开了对方的公钥（响应已经写回去了）。 */
        PAIRED,
        EXPIRED,
        STOPPED,
        FAILED,
    }

    @Volatile
    var state: State = State.IDLE
        private set

    @Volatile
    var boundPort: Int = 0
        private set

    @Volatile
    var expiresAtMs: Long = 0
        private set

    @Volatile
    var failure: String? = null
        private set

    /** 收到的对端信息（[State.PAIRED] 之后才有）。 */
    @Volatile
    var peer: PairingPeer? = null
        private set

    /** 6 位校验码（[State.PAIRED] 之后才有；两边应该一样）。 */
    val code: String?
        get() {
            val p = peer ?: return null
            val ownKey = SyncCrypto.base64UrlDecode(own.publicKey) ?: return null
            val message = PairingMessage(
                v = own.v, device = own.device, name = own.name, publicKey = own.publicKey, nonce = own.nonce,
            )
            val peerMessage = PairingMessage(
                v = own.v, device = p.device, name = p.name, publicKey = SyncCrypto.base64Url(p.publicKey),
                nonce = message.nonce,
            )
            return runCatching {
                if (ownKey.isEmpty()) return null
                PairingCodec.result(message, peerMessage).code
            }.getOrNull()
        }

    private val expectedPath = offer.path
    @Volatile private var stopped = false
    private var serverSocket: ServerSocket? = null
    private var thread: Thread? = null

    @Synchronized
    fun start(): Int {
        check(state == State.IDLE) { "PairingServer 只能 start 一次" }
        val socket = ServerSocket()
        try {
            socket.reuseAddress = false
            socket.bind(InetSocketAddress(bindAddress, port), 1)
        } catch (e: IOException) {
            runCatching { socket.close() }
            state = State.FAILED
            failure = "没法在 ${bindAddress.hostAddress}:$port 上监听：${e.message ?: e.javaClass.simpleName}"
            throw PairingException(failure!!, e)
        }
        socket.soTimeout = ACCEPT_POLL_MS
        serverSocket = socket
        boundPort = socket.localPort
        expiresAtMs = clock() + ttlMs
        state = State.LISTENING
        thread = Thread({ loop(socket) }, "feedhub-pair-server").apply {
            isDaemon = true
            start()
        }
        return boundPort
    }

    @Synchronized
    fun stop() {
        stopped = true
        if (state == State.LISTENING) state = State.STOPPED
        runCatching { serverSocket?.close() }
        serverSocket = null
    }

    private fun loop(socket: ServerSocket) {
        var bad = 0
        var accepted = 0
        var resultingState: State = State.STOPPED
        try {
            while (!stopped) {
                if (clock() >= expiresAtMs) {
                    resultingState = State.EXPIRED
                    break
                }
                val client = try {
                    socket.accept()
                } catch (e: SocketTimeoutException) {
                    continue
                } catch (e: SocketException) {
                    resultingState = State.STOPPED
                    break
                }
                accepted++
                val badRequest = try {
                    handle(client)
                } finally {
                    runCatching { client.close() }
                }
                if (state == State.PAIRED) {
                    resultingState = State.PAIRED
                    break
                }
                if (badRequest) bad++
                if (bad >= MAX_BAD_REQUESTS || accepted >= MAX_CONNECTIONS) {
                    resultingState = State.STOPPED
                    break
                }
            }
        } catch (e: IOException) {
            resultingState = if (stopped) State.STOPPED else State.FAILED
            if (resultingState == State.FAILED) failure = e.message ?: e.javaClass.simpleName
        } finally {
            runCatching { socket.close() }
            synchronized(this) {
                if (state == State.LISTENING) state = resultingState
                serverSocket = null
            }
        }
    }

    /** @return true = 这个请求不对（要记一次坏请求）。 */
    private fun handle(client: Socket): Boolean {
        client.soTimeout = READ_TIMEOUT_MS
        val input = BufferedInputStream(client.getInputStream())
        val header = readHeader(input) ?: return true
        val lines = header.split("\r\n").filter { it.isNotEmpty() }
        val requestLine = lines.firstOrNull()?.trim().orEmpty()
        val parts = requestLine.split(' ')
        if (parts.size != 3 || !parts[2].startsWith("HTTP/1.")) {
            respond(client, 404, "Not Found", ByteArray(0))
            return true
        }
        if (parts[0] != "POST") {
            respond(client, 405, "Method Not Allowed", ByteArray(0), extra = "Allow: POST\r\n")
            return true
        }
        if (parts[1] != expectedPath || clock() >= expiresAtMs) {
            respond(client, 404, "Not Found", ByteArray(0))
            return true
        }
        val declared = lines.drop(1)
            .firstOrNull { it.substringBefore(':').trim().equals("Content-Length", ignoreCase = true) }
            ?.substringAfter(':')?.trim()?.toIntOrNull()
        if (declared == null || declared !in 1..PairProtocol.MAX_BODY_BYTES) {
            respond(client, 400, "Bad Request", ByteArray(0))
            return true
        }
        val sealed = readExactly(input, declared) ?: run {
            respond(client, 400, "Bad Request", ByteArray(0))
            return true
        }
        val plain = SyncCrypto.open(offer.keyInternal(), offer.aad(), sealed)
            ?: run {
                respond(client, 400, "Bad Request", ByteArray(0))
                return true
            }
        val message = PairingCodec.decode(plain)
            ?: run {
                respond(client, 400, "Bad Request", ByteArray(0))
                return true
            }
        // 不能和自己配对；设备 id 也不能和本机撞上
        if (message.device == own.device || message.publicKey == own.publicKey) {
            respond(client, 400, "Bad Request", ByteArray(0))
            return true
        }
        val pub = SyncCrypto.base64UrlDecode(message.publicKey) ?: run {
            respond(client, 400, "Bad Request", ByteArray(0))
            return true
        }
        val responseMessage = own.copy(nonce = message.nonce)
        val response = SyncCrypto.seal(offer.keyInternal(), offer.aad(), PairingCodec.encode(responseMessage))
        peer = PairingPeer(message.device, PairingCodec.cleanName(message.name), pub)
        respond(client, 200, "OK", response)
        state = State.PAIRED
        stopped = true
        return false
    }

    private fun readHeader(input: InputStream): String? {
        val out = ByteArrayOutputStream()
        var tail = 0
        while (out.size() < MAX_HEADER_BYTES) {
            val b = try {
                input.read()
            } catch (e: SocketTimeoutException) {
                return null
            }
            if (b < 0) return null
            out.write(b)
            tail = (tail shl 8) or (b and 0xFF)
            if (tail == 0x0D0A0D0A || (tail and 0xFFFF) == 0x0A0A) return out.toString("ISO-8859-1")
        }
        return null
    }

    private fun readExactly(input: InputStream, size: Int): ByteArray? {
        val out = ByteArray(size)
        var off = 0
        while (off < size) {
            val n = try {
                input.read(out, off, size - off)
            } catch (e: SocketTimeoutException) {
                return null
            }
            if (n <= 0) return null
            off += n
        }
        return out
    }

    private fun respond(client: Socket, code: Int, reason: String, payload: ByteArray, extra: String = "") {
        val head = buildString {
            append("HTTP/1.1 ").append(code).append(' ').append(reason).append("\r\n")
            append("Content-Type: application/octet-stream\r\n")
            append("Content-Length: ").append(payload.size).append("\r\n")
            append("Cache-Control: no-store\r\n")
            append("Connection: close\r\n")
            append(extra)
            append("\r\n")
        }.toByteArray(Charsets.ISO_8859_1)
        try {
            val out: OutputStream = client.getOutputStream()
            out.write(head)
            if (payload.isNotEmpty()) out.write(payload)
            out.flush()
        } catch (e: IOException) {
            // 对面断了：这条会话不重试（一次性），用户重新生成二维码
        }
    }

    private companion object {
        const val MAX_BAD_REQUESTS = 3
        const val MAX_CONNECTIONS = 8
        const val MAX_HEADER_BYTES = 8 * 1024
        const val READ_TIMEOUT_MS = 4_000
        const val ACCEPT_POLL_MS = 250
    }
}

/** 显示配对二维码的一端：正在跑的一次性服务 + 二维码内容（端口是实际绑定的那个）。 */
class PairingSession(
    val server: PairingServer,
    val offer: PairingOffer,
)

/**
 * 用局域网地址起配对服务（界面只拿得到字符串，不让 commonMain 直接碰 `InetAddress`）。
 * 地址不合法 / 监听失败都抛 [PairingException]。
 */
object PairingServers {
    fun start(
        host: String,
        own: PairingMessage,
        ttlMs: Long = PairProtocol.TTL_MS,
    ): PairingSession {
        val sessionId = SyncCrypto.newSessionId()
        val key = SyncCrypto.newKey()
        val address = try {
            InetAddress.getByName(host)
        } catch (e: Exception) {
            throw PairingException("局域网地址不合法：$host", e)
        }
        // 端口 1 只是占位：服务端只认会话 id 那一段路径，真正端口 start 之后才有
        val server = PairingServer(address, PairingOffer(host, 1, sessionId, key), own, ttlMs = ttlMs)
        val port = server.start()
        return PairingSession(server, PairingOffer(host, port, sessionId, key))
    }
}

/** 扫 / 粘二维码的那一端：POST 自己的报文，取回对方的报文，算校验码。 */
class PairingClient(
    private val http: OkHttpClient = SyncLanHttp.defaultClient(),
) {
    /**
     * @throws PairingException 连不上 / 二维码过期或已用过 / 密钥不对 / 内容被改 / 报文不对。
     *   消息里不含密钥和设备公钥值。
     */
    fun exchange(offer: PairingOffer, own: PairingMessage): PairingResult {
        val sealed = SyncCrypto.seal(offer.keyInternal(), offer.aad(), PairingCodec.encode(own))
        val request = Request.Builder()
            .url(offer.url)
            .header("Accept", "application/octet-stream")
            .post(sealed.toRequestBody("application/octet-stream".toMediaType()))
            .build()
        val response = try {
            http.newCall(request).execute()
        } catch (e: IOException) {
            throw PairingException(
                "连不上另一台设备（${offer.host}:${offer.port}）。确认两台在同一个局域网；也可能二维码过期了，重新生成一个",
                e,
            )
        }
        response.use { resp ->
            if (resp.code == 404 || resp.code == 410) {
                throw PairingException("配对二维码已过期或已经用过了。在另一台设备上重新生成一个")
            }
            if (!resp.isSuccessful) throw PairingException("对方拒绝了这次配对（HTTP ${resp.code}）")
            val body = resp.body ?: throw PairingException("对方没有返回配对信息")
            if (body.contentLength() > PairProtocol.MAX_BODY_BYTES) throw PairingException("对方返回的配对信息太大，已拒绝")
            val bytes = try {
                body.bytes()
            } catch (e: IOException) {
                throw PairingException("接收配对信息时断开了，重新生成二维码再试", e)
            }
            if (bytes.isEmpty() || bytes.size > PairProtocol.MAX_BODY_BYTES) throw PairingException("对方返回的配对信息不对")
            val plain = SyncCrypto.open(offer.keyInternal(), offer.aad(), bytes)
                ?: throw PairingException("配对信息校验失败（密钥不对或内容被改过）。重新生成二维码再扫")
            val message = PairingCodec.decode(plain)
                ?: throw PairingException("对方返回的配对信息格式不对，请升级两端 App 后重试")
            if (message.device == own.device) throw PairingException("对方报上来的设备 id 和本机一样，不能自己和自己配对")
            // 响应必须回带我们这次请求的随机数：防重放 / 防拿别的会话的响应来冒充
            if (message.nonce != own.nonce) throw PairingException("配对响应和这次请求对不上，可能有人插在中间，重新生成二维码再试")
            val pub = SyncCrypto.base64UrlDecode(message.publicKey)
                ?: throw PairingException("对方返回的设备公钥不合法")
            SyncCrypto.base64UrlDecode(own.publicKey)
                ?: throw PairingException("本机设备公钥不合法")
            return try {
                PairingCodec.result(own, message)
            } catch (e: PairingException) {
                throw e
            } catch (e: Exception) {
                throw PairingException("算不出校验码，重新生成二维码再试", e)
            }
        }
    }
}
