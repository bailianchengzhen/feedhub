package feedhub.app

import androidx.compose.runtime.Composable
import androidx.compose.runtime.produceState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import java.io.File

/**
 * 插着电吗（预加载策略用，见 [feedhub.app.feed.PreloadPolicy]）：看 `/sys/class/power_supply` 下 `type=Mains` 的 `online`；
 * 没有 Mains 的机器看电池的 `status`（`Discharging` = 用电池）。都读不到给 null（调用方当插电）。两个 sysfs 小文件，代价可忽略。
 */
internal object PowerSupply {
    fun pluggedIn(root: File = File("/sys/class/power_supply")): Boolean? {
        val dirs = root.listFiles()?.filter { it.isDirectory || it.isFile } ?: return null
        fun read(dir: File, name: String) = runCatching { dir.resolve(name).readText().trim() }.getOrNull()
        val mains = dirs.filter { read(it, "type") == "Mains" }
        if (mains.isNotEmpty()) return mains.any { read(it, "online") == "1" }
        val batteries = dirs.filter { read(it, "type") == "Battery" }
        if (batteries.isEmpty()) return null
        return batteries.none { read(it, "status") == "Discharging" }
    }

    const val POLL_MS = 30_000L
}

/** 插着电吗，每 [PowerSupply.POLL_MS] 看一次；读不到当插电。 */
@Composable
internal fun rememberPluggedIn(): Boolean {
    val state = produceState(PowerSupply.pluggedIn() ?: true) {
        while (true) {
            delay(PowerSupply.POLL_MS)
            value = withContext(Dispatchers.IO) { PowerSupply.pluggedIn() } ?: true
        }
    }
    return state.value
}
