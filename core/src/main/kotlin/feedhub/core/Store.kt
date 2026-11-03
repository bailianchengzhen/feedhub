package feedhub.core

/**
 * 小型持久化键值存储，给 Source 存设备标识、签名密钥缓存之类的东西。
 * 键请加平台前缀，例如 "bili.wbi_keys"。App 里用 SharedPreferences / DataStore 实现。
 */
interface KeyValueStore {
    fun get(key: String): String?
    fun put(key: String, value: String?)
}

class MemoryKeyValueStore : KeyValueStore {
    private val map = mutableMapOf<String, String>()

    @Synchronized
    override fun get(key: String): String? = map[key]

    @Synchronized
    override fun put(key: String, value: String?) {
        if (value == null) map.remove(key) else map[key] = value
    }
}
