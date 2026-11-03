//! 出帧的**共享直接内存**：Rust 写、Kotlin 读，一帧的像素不过 UniFFI。
//!
//! ## 为什么不用"每帧过 FFI 拷一个 ByteArray"
//!
//! 1080p 的 BGRA 一帧是 1920×1080×4 ≈ 8.3 MB，60 fps 就是每秒 500 MB。走 UniFFI 每次都要
//! 在 JNA 那边编一个 `RustBuffer`、Kotlin 再拷一份进 `ByteArray`、再拷进 Skia 的 `Image`
//! —— 三次全帧拷贝 + 一次 JNA 编解码，量级是"每帧 5～15 ms"，直接把播放拖垮（60 fps 只有 16.7 ms）。
//!
//! 所以改成：引擎这边 `mmap` 一块匿名共享内存（`MAP_SHARED|MAP_ANONYMOUS`，父子进程共享，
//! 但这里只用在同一进程里，靠的是"映射出来的地址可以给两边用"），在里面按
//! [`SURFACE_SLOTS`] 个槽轮转，每个槽前面有一个小头部（宽度 / 高度 / stride / 序号）。
//! Kotlin 拿到这块内存的地址 + 长度（各一次，`open` 时），之后每帧只问一个
//! `AtomicU64` 的序号（标量，JNA 直接读，**不是** record）。
//!
//! ## 三槽轮转的同步
//!
//! 写者（appsink 回调线程）与读者（Compose 渲染线程）不共享锁（跨语言的锁是最容易死锁的东西）。
//! 用**序号 + 三槽**：
//!
//! - 写者每写一帧：先挑一个"读者当前不在读"的槽（`read_slot` 与 `write_slot` 不同、也不是上一槽），
//!   写数据，**最后**用 `Release` 存 `frame_seq`；
//! - 读者每帧：`Acquire` 读 `frame_seq`，和上次读到的比，变了就把 `read_slot` 设成写者公布的那个槽，
//!   从那个槽读，读完再 `read_slot = NONE`。
//!
//! 三槽情况下写者永远能挑到一个既不等于 `write_slot`、也不等于读者手上那个的槽
//! （3 个槽里排除 2 个还剩 1 个），所以**读者读的时候那一槽一定不会被覆写**。
//! 这是单写者单读者的经典无锁环，只是把"环"换成了固定三槽——省掉了算模、也省掉了 ABA 问题。

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use super::{MAX_FRAME_BYTES, SURFACE_SLOTS};

/// 每个槽头部的字节数（8 字节对齐，方便 Kotlin 直接按 `long` 读）。
///
/// 布局（小端）：
/// ```text
/// +0  u32 width
/// +4  u32 height
/// +8  u32 stride
/// +12 u32 format   （1 = BGRA8888）
/// +16 u64 pts_ns   （有符号；高位全 1 = 没有 PTS）
/// +24 u64 pad      （保留，凑 32 字节对齐）
/// ```
pub const SLOT_HEADER: usize = 32;

/// 槽头里 `format` 字段的取值（Kotlin 侧照这个认；现在只有 BGRA）。
pub const FORMAT_BGRA: u32 = 1;

/// `frame_seq` / `pts_ns` 里表示"没有值"的哨兵（`u64::MAX`，Kotlin 侧按 `-1L` 读）。
pub const NO_VALUE: u64 = u64::MAX;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 一块给 Kotlin 读的帧内存。地址 / 长度在 `open` 时交出去一次，之后只递增序号。
///
/// **生命周期**：引擎自己持有（`Pipeline` 里的 appsink 回调也持有 `Arc`）。引擎 `release` 时
/// `munmap`——那时 Kotlin 那边的 `FrameSurface` 也已经 `close` 了（`RustGstPlayerEngine.release`
/// 先关 surface 再放引擎）。测试里直接 `drop` 也行（`Drop` 里 munmap）。
pub struct FrameShm {
    id: u64,
    /// mmap 的基地址（整个块：`SURFACE_SLOTS × (SLOT_HEADER + capacity)`）。
    base: *mut u8,
    /// 整块的字节数。
    len: usize,
    /// 每个槽的像素区容量（字节）。
    cap: usize,
    /// 写者已经写完的最新一帧的槽号 + 1（0 = 还没有帧）。**Kotlin 读这个决定要不要重画**。
    frame_seq: AtomicU64,
    /// 写者公布"最新帧在哪个槽"（`u32::MAX` = 没有）。和 `frame_seq` 一起用，但分开存是为了
    /// 让 Kotlin 只读一个标量就能判断"是不是新帧"（不必解包）。
    latest_slot: AtomicU32,
    /// 读者正在读的槽（`u32::MAX` = 没在读）。写者据此避开它。
    reader_slot: AtomicU32,
    /// 写者上次写的槽（挑下一个槽时避开）。
    writer_slot: AtomicU32,
    /// 已经写过的总帧数（验证 / 统计用；和 `frame_seq` 同一个含义但**不清零**）。
    frames_written: AtomicU64,
    /// 因为找不到空闲槽而丢掉（跳过）的帧数。三槽下正常应该恒为 0，非 0 就是出了设计之外的事，
    /// 结果里要看见它。
    frames_dropped: AtomicU64,
}

// 裸指针不是 Send/Sync，但这块内存是 mmap 出来的、没有 Rust 侧的别名规则参与（写者 / 读者通过原子
// 序号协调），可以跨线程。`unsafe impl` 就是这个意思。
unsafe impl Send for FrameShm {}
unsafe impl Sync for FrameShm {}

impl FrameShm {
    /// `mmap` 一块能装 `SURFACE_SLOTS` 个 `width×height×4` 帧的内存。
    ///
    /// `capacity` 按**源分辨率**给（不是缩放之后的画布尺寸）：同一份素材在画布变大时 target 尺寸
    /// 会长到源尺寸，槽必须装得下最大的那一帧。
    pub fn new(width: u32, height: u32) -> std::io::Result<Arc<Self>> {
        let stride = (width as usize).saturating_mul(4);
        let need = stride.saturating_mul(height as usize);
        if need == 0 || need > MAX_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("帧尺寸不合理：{width}x{height}（{need} 字节，超过上限 {MAX_FRAME_BYTES}）"),
            ));
        }
        // 向上取到页大小（mmap 的 len 不必对齐，但按页给能让 cap 好用一点）
        let page = 4096usize;
        let cap = (need + page - 1) / page * page;
        let len = SURFACE_SLOTS * (SLOT_HEADER + cap);
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                // MAP_SHARED|MAP_ANONYMOUS：不落盘、可读可写、对着同一个进程里的所有线程可见。
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        let shm = Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            base: ptr as *mut u8,
            len,
            cap,
            frame_seq: AtomicU64::new(0),
            latest_slot: AtomicU32::new(u32::MAX),
            reader_slot: AtomicU32::new(u32::MAX),
            writer_slot: AtomicU32::new(u32::MAX),
            frames_written: AtomicU64::new(0),
            frames_dropped: AtomicU64::new(0),
        };
        Ok(Arc::new(shm))
    }

    /// 给 Kotlin 的块地址（`open` 时交一次）。
    pub fn addr(&self) -> u64 {
        self.base as u64
    }

    /// 整块的字节数。
    pub fn byte_len(&self) -> u64 {
        self.len as u64
    }

    /// 每个槽的像素区容量。
    pub fn slot_capacity(&self) -> u64 {
        self.cap as u64
    }

    /// 每个槽的总跨度（头 + 像素区）。
    pub fn slot_stride(&self) -> u64 {
        (SLOT_HEADER + self.cap) as u64
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// 已经写完多少帧（不从零回过）。
    pub fn frames_written(&self) -> u64 {
        self.frames_written.load(Ordering::Acquire)
    }

    /// 因为三槽都占着而跳过的帧数（正常恒 0）。
    pub fn frames_dropped(&self) -> u64 {
        self.frames_dropped.load(Ordering::Acquire)
    }

    /// 最新一帧的序号（Kotlin 读它决定要不要重画）。`0` = 还没有帧。
    pub fn frame_seq(&self) -> u64 {
        self.frame_seq.load(Ordering::Acquire)
    }

    unsafe fn slot_ptr(&self, slot: u32) -> *mut u8 {
        self.base
            .add(slot as usize * (SLOT_HEADER + self.cap))
    }

    /// 写一帧：`src` 是 BGRA 的紧密排列像素（`stride × height` 字节，`src.len()` 至少这么多）。
    ///
    /// 返回 true = 写进去了；false = 没有空槽（记进 `frames_dropped`，调用方照常继续）。
    ///
    /// **写序**：先挑槽 → 写头部 + 像素 → `Release` 存 `frame_seq`。读者 `Acquire` 读到新序号时，
    /// 槽里的数据一定已经写完了。
    pub fn write_frame(&self, src: &[u8], width: u32, height: u32, stride: u32, pts_ns: i64) -> bool {
        let need = stride as usize * height as usize;
        if need == 0 || need > self.cap {
            self.frames_dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        if src.len() < need {
            self.frames_dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let reader = self.reader_slot.load(Ordering::Acquire);
        let last = self.writer_slot.load(Ordering::Relaxed);
        // 三槽里避开"读者正在读的"和"上一帧写的"，剩下的就是空闲槽。
        let mut chosen = u32::MAX;
        for slot in 0..SURFACE_SLOTS as u32 {
            if slot != reader && slot != last {
                chosen = slot;
                break;
            }
        }
        if chosen == u32::MAX {
            self.frames_dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        unsafe {
            let p = self.slot_ptr(chosen);
            // 头部
            (p as *mut u32).write_unaligned(width);
            (p.add(4) as *mut u32).write_unaligned(height);
            (p.add(8) as *mut u32).write_unaligned(stride);
            (p.add(12) as *mut u32).write_unaligned(FORMAT_BGRA);
            (p.add(16) as *mut u64).write_unaligned(pts_ns as u64);
            (p.add(24) as *mut u64).write_unaligned(0);
            // 像素
            std::ptr::copy_nonoverlapping(src.as_ptr(), p.add(SLOT_HEADER), need);
        }
        self.writer_slot.store(chosen, Ordering::Relaxed);
        self.latest_slot.store(chosen, Ordering::Release);
        self.frame_seq.fetch_add(1, Ordering::Release);
        self.frames_written.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Kotlin 那边读一帧之前先"占住"它：返回槽号（`None` = 还没有帧 / 拿不到）。
    ///
    /// Kotlin 侧的用法：`seq = frame_seq` → 变了才调 `acquire` → 读 → `release_slot`。
    /// 这里**不阻止写者**（写者只会避开这一个槽），所以读的时候数据是稳定的。
    pub fn acquire(&self) -> Option<u32> {
        let slot = self.latest_slot.load(Ordering::Acquire);
        if slot == u32::MAX || slot as usize >= SURFACE_SLOTS {
            return None;
        }
        self.reader_slot.store(slot, Ordering::Release);
        Some(slot)
    }

    /// 读完了，放开这个槽（写者可以覆写它了）。
    pub fn release_slot(&self) {
        self.reader_slot.store(u32::MAX, Ordering::Release);
    }

    /// 读某槽头里的 `(width, height, stride, pts_ns)`。**只在 `acquire` 之后对这个槽调**。
    pub fn slot_header(&self, slot: u32) -> Option<(u32, u32, u32, i64)> {
        if slot as usize >= SURFACE_SLOTS {
            return None;
        }
        unsafe {
            let p = self.slot_ptr(slot);
            let width = (p as *const u32).read_unaligned();
            let height = (p.add(4) as *const u32).read_unaligned();
            let stride = (p.add(8) as *const u32).read_unaligned();
            let pts = (p.add(16) as *const u64).read_unaligned() as i64;
            if width == 0 || height == 0 || stride == 0 {
                return None;
            }
            Some((width, height, stride, pts))
        }
    }
}

impl Drop for FrameShm {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.base as *mut libc::c_void, self.len);
        }
    }
}

/// 拷一份某槽的像素出来（调试 / 存 PNG 用）。
///
/// **只在 `acquire` 之后对这个槽调**。这条路每帧要拷一次全帧，只给检查工具和验证用；
/// 生产路径是 Kotlin 直接读那块内存，不走这里。
pub fn copy_slot_pixels(shm: &FrameShm, slot: u32) -> Vec<u8> {
    let Some((w, h, stride, _pts)) = shm.slot_header(slot) else {
        return Vec::new();
    };
    let need = stride as usize * h as usize;
    if need == 0 || need > shm.cap {
        return Vec::new();
    }
    let _ = w;
    unsafe {
        let p = shm.slot_ptr(slot).add(SLOT_HEADER);
        std::slice::from_raw_parts(p, need).to_vec()
    }
}

