package feedhub.app.embed

import feedhub.rust.RustEmbed

/**
 * 一整个向量 → f16 字节（小端，2 字节一个），`ItemEmbedding.vec` 存的就是它。
 *
 * 就近舍入到偶数、和 numpy 的 `astype(np.float16)` 一致）。
 * 这里只剩薄门面：**一次一整条**过边界（不是逐分量），所以调用方不会因为换实现而变慢。
 */
fun floatVecToHalfBytes(vec: FloatArray): ByteArray = RustEmbed.halfBytes(vec)

/** f16 字节里的第 [i] 个分量（小端）→ float。整条读用 [embeddingVecHalfToFloats]。 */
fun embeddingVecHalfToFloat(bytes: ByteArray, i: Int): Float =
    RustEmbed.halfAt(bytes, i) ?: throw IllegalArgumentException("第 $i 个分量越界（${bytes.size} 字节）")

/**
 * 一整条 f16 向量 → `FloatArray`（长度不对返回 null）。
 *
 * 这条是**批量**入口：`ItemEmbedding.toFloats()` 一次过边界拿整条，别逐分量调
 * [embeddingVecHalfToFloat]（256 次 JNA 调用够呛）。
 */
fun embeddingVecHalfToFloats(bytes: ByteArray, dim: Int): FloatArray? = RustEmbed.vecFromHalf(bytes, dim)
