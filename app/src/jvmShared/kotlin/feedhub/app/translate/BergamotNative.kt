package feedhub.app.translate

/**
 * Bergamot 本地翻译引擎的 JNI 入口（桌面 Linux 与 Android 共用，位于 `jvmShared`）。
 *
 * 对应 C++ 实现：`tools/translate/bergamot_jni/feedhub_bergamot.cpp`（编译为 `libfeedhub_bergamot.so`）。
 *
 * 注意：保持 `public object` + 普通实例 `external fun`（不要改 `internal`、不要加 `@JvmStatic`），
 * 以免 Kotlin 编译器改写方法名或额外生成静态桥导致 JNI 符号签名对不上。
 */
object BergamotNative {
    /** 引擎版本串（如 `bergamot-0.6.0`）；未加载 `.so` 前不可调用。 */
    external fun nativeVersion(): String

    /** 最近一次失败的可读原因（无错误时为空串）。 */
    external fun nativeLastError(): String

    /**
     * 加载模型并完成一次预热自检，返回原生会话句柄；`0L` 表示失败（原因见 [nativeLastError]）。
     *
     * @param modelPath     量化模型路径（`model.*.intgemm*.bin`，内部按 256 字节对齐读入内存）
     * @param srcVocabPath  源语言 SentencePiece 词表路径（`.spm`）
     * @param trgVocabPath  目标语言 SentencePiece 词表路径（`.spm`，共用词表时可与 [srcVocabPath] 相同）
     * @param shortlistPath 词汇剪枝表路径（`lex.*.s2t.bin`）
     * @param ssplitPrefixPath 分句缩写前缀表路径（`nonbreaking_prefix.en`，可选，无则传 `null`）
     */
    external fun nativeCreate(
        modelPath: String,
        srcVocabPath: String,
        trgVocabPath: String,
        shortlistPath: String,
        ssplitPrefixPath: String?,
    ): Long

    /**
     * 翻译一段标准 UTF-8 字节序列，返回译文的标准 UTF-8 字节序列；失败返回 `null`（原因见 [nativeLastError]）。
     *
     * 使用 `ByteArray` 而非 `String` 过 JNI，彻底避开 JNI Modified UTF-8 在 4 字节 Emoji（如 🎉🔥）
     * 上触发 PCRE2 UTF-8 校验错误或 Android ART CheckJNI 崩溃。
     */
    external fun nativeTranslate(handle: Long, utf8Input: ByteArray): ByteArray?

    /** 释放原生会话及其占用的模型内存（重复传入 `0L` 安全）。 */
    external fun nativeClose(handle: Long)
}
