package feedhub.rust

/**
 * 资料库（`app` 的 `RustLibrary`）要的那一个入口：把 `.so` 装进进程。
 *
 * 为什么单独一个文件而不是直接用 [RustNative]：`RustNative` 是 `internal`（只给这个模块里的门面用），
 * 而资料库的门面在 `:app`（`src/jvmShared/`）里。这里只转发一件事——**加载**；桌面的 `.so` 要先解压到
 * `~/.cache/feedhub/native/rust/<哈希>/` 再把路径告诉 JNA（见 [RustNative]），不加载就直接调绑定会
 * `UnsatisfiedLinkError`。
 */
object RustLibraryBridge {
    /** 确保 `.so` 能用；不能时抛 `IllegalStateException`（调用方据此退回 `MemoryLibrary`）。 */
    fun ensureLoaded() = RustNative.ensureLoaded()
}
