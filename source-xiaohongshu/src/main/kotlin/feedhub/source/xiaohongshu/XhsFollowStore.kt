package feedhub.source.xiaohongshu

import feedhub.core.KeyValueStore
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.util.Base64
import java.util.zip.GZIPInputStream
import java.util.zip.GZIPOutputStream

/**
 * 小红书关注流的持久化（键都以 `xhs.following.` 开头），照贴吧的 ForumStore：
 *
 * - 关注列表（几十个人，几 KB）：明文 JSON，最多一天刷新一次——里面的 xsec_token 过期了拉主页会失败，所以也不能存太久。
 * - 各作者的缓冲：每人最多 [XhsFollowFeed.KEEP_PER_AUTHOR] 条，JSON 之后 gzip + base64，整体一个键，一次 `put`
 *   （`FileKeyValueStore` 每次 `put` 都重写整个文件）。
 * 缓存读不出来（版本不对、损坏）就当没有，重新取；不会因此抛异常。
 */
internal object XhsFollowStore {
    const val KEY_USERS = "xhs.following.users"
    const val KEY_AUTHORS = "xhs.following.authors"
    const val VERSION = 1
    private const val GZIP_PREFIX = "gz1:"

    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true; explicitNulls = false }

    @Serializable
    internal class StoredUsers(val v: Int, val at: Long, val users: List<FollowedAuthor>)

    @Serializable
    internal class StoredAuthors(val v: Int, val extraAt: Long = 0, val states: List<AuthorState>)

    /** 关注列表和它的取得时间（毫秒）。 */
    class UserList(val at: Long, val users: List<FollowedAuthor>)

    /** 各作者的状态（按 id）和上次问 `followings/extra` 的时间（毫秒）。 */
    class Authors(val extraAt: Long, val states: Map<String, AuthorState>)

    fun loadUsers(store: KeyValueStore): UserList? = runCatching {
        val s = json.decodeFromString(StoredUsers.serializer(), store.get(KEY_USERS) ?: return null)
        if (s.v != VERSION) null else UserList(s.at, s.users)
    }.getOrNull()

    fun saveUsers(store: KeyValueStore, list: UserList) {
        store.put(KEY_USERS, json.encodeToString(StoredUsers.serializer(), StoredUsers(VERSION, list.at, list.users)))
    }

    fun loadAuthors(store: KeyValueStore): Authors = runCatching {
        val raw = store.get(KEY_AUTHORS) ?: return Authors(0, emptyMap())
        val text = if (raw.startsWith(GZIP_PREFIX)) gunzip(raw.removePrefix(GZIP_PREFIX)) else raw
        val s = json.decodeFromString(StoredAuthors.serializer(), text)
        if (s.v != VERSION) Authors(0, emptyMap()) else Authors(s.extraAt, s.states.associateBy { it.id })
    }.getOrDefault(Authors(0, emptyMap()))

    fun saveAuthors(store: KeyValueStore, authors: Authors) {
        val text = json.encodeToString(StoredAuthors.serializer(), StoredAuthors(VERSION, authors.extraAt, authors.states.values.toList()))
        store.put(KEY_AUTHORS, GZIP_PREFIX + gzip(text))
    }

    private fun gzip(text: String): String {
        val out = ByteArrayOutputStream()
        GZIPOutputStream(out).use { it.write(text.toByteArray(Charsets.UTF_8)) }
        return Base64.getEncoder().encodeToString(out.toByteArray())
    }

    private fun gunzip(b64: String): String =
        GZIPInputStream(ByteArrayInputStream(Base64.getDecoder().decode(b64))).use { it.readBytes().toString(Charsets.UTF_8) }
}
