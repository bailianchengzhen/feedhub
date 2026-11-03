package feedhub.app

import android.content.Context
import feedhub.core.ProxyMode
import feedhub.proxy.ProxyCore
import feedhub.proxy.ProxyCoreBridge
import feedhub.rust.ProxyTarget
import kotlinx.coroutines.CoroutineScope
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths

/**
 * 安卓上的内置代理核心（sing-box 子进程）组装。
 *
 * 和桌面**完全共用** `feedhub.proxy.ProxyCore`（配置生成 / 健康检查 / 退避重启 / 日志都在 Rust 那一份里），
 * 安卓这边不同的是三件事：
 *
 * 1. **二进制定位**：只认 `nativeLibraryDir/libsingbox.so`（[androidSingBoxBinary]）。Android 只允许执行
 *    `nativeLibraryDir` 下的文件，`filesDir` 从 Android 10 起是 noexec；所以构建时把
 *    `android-arm64/sing-box` 打进 `jniLibs/arm64-v8a/libsingbox.so`、并打开 `extractNativeLibs`
 *    （见 `androidApp/build.gradle.kts` 的 `stageSingBoxAndroid` 与 `packaging.jniLibs`）。
 *    找不到（比如 ABI 不匹配的设备、或者没带二进制的构建）就返回 null，界面干脆不显示「内置核心」那一档。
 * 2. **数据目录**：生成的配置与订阅 / 节点放在 [dir]（默认 `<filesDir>/singbox`）里，靠应用沙箱隔离，
 *    不申请任何新权限，也不开 TUN、不当系统代理。
 * 3. **只在前台跑**：判定表在 Rust（`proxy_core::foreground::ForegroundGate`，前台 + 退后台 30 秒宽限），
 *    由 `MainActivity` 的 `onStart` / `onStop` 经 [AppGraph.applyForeground] 喂进去。
 *
 * sha256 的期望值不用传：`ProxyCoreBridge.expectedSha256` 在 arm64 上就是 Android 那份
 * （和 `-Pfeedhub.singbox.dir` 取的那份对得上）。
 *
 * @param dir 放生成的配置和 `proxy-core.json` 的目录。**演示 / 截图组装传另一个目录**，绝不碰用户真实那一份
 *   （桌面截图工具也是这么做的）。
 * @param autoUpdateCheckMs 多久查一次"订阅该自动更新了"。演示 / 截图传 0：一次都不查，一个请求都不发。
 */
fun androidProxyCore(
    context: Context,
    scope: CoroutineScope,
    systemProxy: (host: String) -> ProxyTarget = { ProxyTarget.of(ProxyMode.System, listOf(it)) },
    dir: Path = Paths.get(context.filesDir.path, "singbox"),
    autoUpdateCheckMs: Long = 600_000,
): ProxyCore? {
    val binary = androidSingBoxBinary(context) ?: return null
    runCatching { Files.createDirectories(dir) }
    return ProxyCore(
        binary = binary,
        dataDir = dir,
        stateFile = dir.resolve("proxy-core.json"),
        scope = scope,
        systemProxy = systemProxy,
        expectedSha256Provider = { ProxyCoreBridge.expectedSha256For(binary) },
        autoUpdateCheckMs = autoUpdateCheckMs,
    )
}

/**
 * 安卓上的 sing-box 在哪：**只认 `nativeLibraryDir/libsingbox.so`**（+ 调试用的环境变量
 * `FEEDHUB_SINGBOX`，和桌面同一个变量名）。没有就返回 null——界面据此把「内置核心」那一档整个藏起来，
 * 免得用户选了一个一定起不来的档位。
 *
 * 为什么不做"从 assets 解出来"这类兜底：Android 只允许执行 `nativeLibraryDir` 里的文件，解到
 * `filesDir` 也执行不了。**不绕路。**
 *
 * 直接用本地文件系统探测，不在冷启动阶段为了判断有没有二进制去触发 FFI。
 */
internal fun androidSingBoxBinary(context: Context): Path? = listOfNotNull(
    System.getenv("FEEDHUB_SINGBOX")?.takeIf { it.isNotBlank() }?.let { Paths.get(it) },
    Paths.get(context.applicationInfo.nativeLibraryDir, "libsingbox.so"),
).firstOrNull { Files.isRegularFile(it) && Files.isExecutable(it) }
