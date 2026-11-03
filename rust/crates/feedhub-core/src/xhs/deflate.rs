//! `x-rap-param` 里的 gzip 那一步：**raw deflate 直接调系统的 zlib**（`libz`），不引第三方 crate。
//!
//! 为什么不是自己写、也不用纯 Rust 的 deflate 实现：**签好的包必须和 Kotlin 版逐字节一样**
//! （`XhsRequestEquivalenceTest` 比整个 `x-rap-param` 头），而 Kotlin 那边是
//! `java.util.zip.Deflater(6, /* nowrap = */ true)`。桌面 JDK 的 Deflater、安卓的 Deflater、我们这里的 FFI
//! 最终都是同一份系统 zlib（桌面在 `libz.so.1`、安卓在 `/system/lib64/libz.so`），所以输出天然一致；
//! 换成 miniz_oxide / zlib-rs 这类"另一个实现"跑同一档压缩，字节会差。
//!
//! 代价：`.so` 多一个 `DT_NEEDED libz.so`（Linux / 安卓都是系统自带；NDK 的 sysroot 里有 `libz.so` 桩，
//! 链接参数不用改）。它仍然是**纯函数**：不碰网络、不读文件、不存状态。
//!
//! Rust 要读老数据、写回 Kotlin 能读的串。用系统 zlib 的
//! `inflateInit2_(15 + 16)`，**没有引 `flate2`**。

use std::os::raw::{c_char, c_int, c_uint, c_ulong};

/// zlib 的 `z_stream`（LP64 上 `uLong` 是 64 位；桌面 x86_64 / 安卓 arm64、x86_64 都是 LP64）。
#[repr(C)]
struct ZStream {
    next_in: *const u8,
    avail_in: c_uint,
    total_in: c_ulong,
    next_out: *mut u8,
    avail_out: c_uint,
    total_out: c_ulong,
    msg: *const c_char,
    state: *mut u8,
    zalloc: *const u8,
    zfree: *const u8,
    opaque: *mut u8,
    data_type: c_int,
    adler: c_ulong,
    reserved: c_ulong,
}

const Z_OK: c_int = 0;
const Z_STREAM_END: c_int = 1;
const Z_BUF_ERROR: c_int = -5;
const Z_FINISH: c_int = 4;
const Z_DEFLATED: c_int = 8;

#[link(name = "z")]
extern "C" {
    fn zlibVersion() -> *const c_char;
    fn deflateInit2_(
        strm: *mut ZStream,
        level: c_int,
        method: c_int,
        window_bits: c_int,
        mem_level: c_int,
        strategy: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    fn deflate(strm: *mut ZStream, flush: c_int) -> c_int;
    fn deflateEnd(strm: *mut ZStream) -> c_int;
    fn inflateInit2_(
        strm: *mut ZStream,
        window_bits: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    fn inflate(strm: *mut ZStream, flush: c_int) -> c_int;
    fn inflateEnd(strm: *mut ZStream) -> c_int;
}

/// raw deflate（没有 zlib 头 / 尾，等价于 `Deflater(level, true)`）。
pub fn deflate_raw(data: &[u8], level: i32) -> Result<Vec<u8>, String> {
    // 输出从"输入长度 + 128"开始，不够就翻倍重来（deflate 在 Z_FINISH 下会停在缓冲区满）。
    let mut out = vec![0u8; data.len() + 128];
    unsafe {
        let mut s: ZStream = std::mem::zeroed();
        let rc = deflateInit2_(
            &mut s,
            level as c_int,
            Z_DEFLATED,
            // -15 = raw（不要 zlib 外壳），和 Java 的 `nowrap = true` 同一个意思
            -15,
            8,
            0,
            zlibVersion(),
            std::mem::size_of::<ZStream>() as c_int,
        );
        if rc != Z_OK {
            return Err(format!("deflateInit2_ 失败：{rc}"));
        }
        s.next_in = data.as_ptr();
        s.avail_in = data.len() as c_uint;
        loop {
            let used = s.total_out as usize;
            s.next_out = out.as_mut_ptr().add(used);
            s.avail_out = (out.len() - used) as c_uint;
            let rc = deflate(&mut s, Z_FINISH);
            if rc == Z_STREAM_END {
                break;
            }
            if rc != Z_OK && rc != Z_BUF_ERROR {
                deflateEnd(&mut s);
                return Err(format!("deflate 失败：{rc}"));
            }
            out.resize(out.len() * 2, 0u8);
        }
        let n = s.total_out as usize;
        deflateEnd(&mut s);
        out.truncate(n);
    }
    Ok(out)
}

/// gzip：头 `1f 8b 08 00 <mtime 小端> 00 03`（系统字节 3，和网页一致），deflate 用 zlib 的 `level`，
/// 尾巴是 CRC32 和原始长度（都小端）。
pub fn gzip(raw: &[u8], mtime: u32, level: i32) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(raw.len() + 32);
    out.extend_from_slice(&[0x1F, 0x8B, 8, 0]);
    for i in 0..4 {
        out.push((mtime >> (8 * i)) as u8);
    }
    out.push(0); // XFL（和 Kotlin 一样写 0）
    out.push(3); // OS：3 = Unix
    out.extend_from_slice(&deflate_raw(raw, level)?);
    let crc = super::codec::crc32(raw);
    for i in 0..4 {
        out.push((crc >> (8 * i)) as u8);
    }
    for i in 0..4 {
        out.push(((raw.len() as u32) >> (8 * i)) as u8);
    }
    Ok(out)
}

/// **gunzip**：解 gzip（`windowBits = 15 + 16`，和 Java `GZIPInputStream` 读的是同一个格式）。
///
/// 谁在用：小红书关注流的作者缓冲存在 `ctx.store` 里是 `gz1:` + base64(gzip(JSON))
/// （Kotlin `XhsFollowStore.saveAuthors`），Rust 要读老数据、也要写回能被 Kotlin 读的串。
///
/// 只认 gzip 头（`1f 8b`）；**空的 / 不是 gzip 的**直接报错——调用方（关注流 store）照 Kotlin
/// `runCatching{}.getOrDefault` 的口径当成"没有缓存"，不往外抛。
pub fn gunzip(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 2 || data[0] != 0x1F || data[1] != 0x8B {
        return Err("不是 gzip 数据（头两个字节不是 1f 8b）".to_string());
    }
    // 输出从"输入长度 × 4 + 256"开始，不够就翻倍再来（gzip 的压缩比通常 3~10 倍）。
    let mut out = vec![0u8; data.len().saturating_mul(4).saturating_add(256)];
    unsafe {
        let mut s: ZStream = std::mem::zeroed();
        let rc = inflateInit2_(
            &mut s,
            // 15 + 16 = gzip 外壳（zlib 的 `MAX_WBITS + 16`）
            15 + 16,
            zlibVersion(),
            std::mem::size_of::<ZStream>() as c_int,
        );
        if rc != Z_OK {
            return Err(format!("inflateInit2_ 失败：{rc}"));
        }
        s.next_in = data.as_ptr();
        s.avail_in = data.len() as c_uint;
        loop {
            let used = s.total_out as usize;
            s.next_out = out.as_mut_ptr().add(used);
            s.avail_out = (out.len() - used) as c_uint;
            let rc = inflate(&mut s, 0 /* Z_NO_FLUSH */);
            if rc == Z_STREAM_END {
                break;
            }
            if rc != Z_OK && rc != Z_BUF_ERROR {
                let msg = if s.msg.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(s.msg).to_string_lossy().into_owned()
                };
                inflateEnd(&mut s);
                return Err(format!("inflate 失败：{rc} {msg}"));
            }
            if s.avail_out != 0 {
                // 还没到流尾、输出却没用完：输入被截断了
                inflateEnd(&mut s);
                return Err("gzip 数据被截断".to_string());
            }
            out.resize(out.len().saturating_mul(2).max(1024), 0u8);
        }
        let n = s.total_out as usize;
        inflateEnd(&mut s);
        out.truncate(n);
    }
    Ok(out)
}
