package feedhub.app.embed

/**
 * 桌面推理的 JNI 入口。
 *
 * 实现是 `app/src/desktopMain/cpp/feedhub_onnx_embed.c`：一层很薄的转发，直接调 ONNX Runtime 的 **C API**
 * 。这里只声明方法，
 * 加载 / 找文件在 [DesktopEmbedRuntime] 里。
 *
 * **这些方法的 JVM 名字就是 C 里的符号名**（`Java_feedhub_app_embed_OrtNative_nativeCreate` 之类），
 * Kotlin 的 `internal` 会给函数名加后缀、`@JvmStatic` 会多出一个静态桥，所以这个对象和它的方法都必须是
 * **public + 普通实例方法**（`object` 的实例方法，JNI 第二个参数是 `jobject`）。别改可见性 / 别加 `@JvmStatic`。
 *
 * 约定：返回 `0` / `null` 表示失败，原因用 [nativeLastError] 取（只有一条，调用方串行使用）。
 * 传进来的 `ids` / `mask` 是**已经补齐**的 `batch × len` 的 int64 平铺数组；返回 `batch × dim` 的 float，
 * **没有归一化**（归一化 + MRL 截断在 Kotlin 里做，和安卓那条路完全一样）。
 */
object OrtNative {
    /** 运行库自己的版本串（例如 `1.31.0`）；`.so` 没加载时不要调。 */
    external fun nativeVersion(): String

    /** 最近一次失败的说明（可能为空串）。 */
    external fun nativeLastError(): String

    /**
     * 建会话。返回不透明的句柄，0 = 失败。
     *
     * @param modelPath `.onnx` 的绝对路径；外部权重（`.onnx_data`）必须在**同一个目录**
     * @param threads intra-op 线程数（桌面默认 3：给界面和用户别的程序留核）
     * @param interThreads inter-op 线程数（默认 1：一条一条算，不并发）
     */
    external fun nativeCreate(modelPath: String, threads: Int, interThreads: Int): Long

    /** 算一批（`ids` / `mask` 各 `batch × len` 个 int64）。 */
    external fun nativeEmbed(handle: Long, ids: LongArray, mask: LongArray, batch: Int, len: Int): FloatArray?

    /** 输出维数（还没算过是 0）。 */
    external fun nativeDim(handle: Long): Int

    /** 释放会话（约 155 MB）。重复调 / 传 0 都安全。 */
    external fun nativeClose(handle: Long)
}
