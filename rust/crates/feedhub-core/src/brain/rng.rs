//! `kotlin.random.Random(seed: Long)` 的逐位复刻。
//!
//! 为什么要有这个：首页穿插排序在"不同平台位置分数相等"时，用**这一批的种子**打乱平台先后
//! 。那条规则是用户能看见的顺序，
//! 当前是纯重构，**行为一条都不能变**，所以 Rust 必须和 Kotlin 排出**逐个位置相同**的结果。
//!
//! Kotlin 的 `Random(seed)` 是 `XorWowRandom`（xorshift 族的一个变体），Kotlin 2.3.21 的实现照
//! `kotlin-stdlib` 的字节码逐行复刻：
//!
//! - 构造：`XorWowRandom(seed.toInt(), seed.shr(32).toInt())`，再**空转 64 次** `nextInt()`；
//! - `nextInt()`：`t = x ^ (x ushr 2); x = y; y = z; z = w; w = v; v = (t ^ (t shl 1)) ^ v ^ (v shl 4); addend += 362437; return v + addend`
//!   （全部按 32 位补码回绕；`ushr` 是**无符号**右移）；
//! - `nextBits(k)`：取 `nextInt()` 的**高** k 位（`takeUpperBits`，k = 0 时恒 0）；
//! - `nextInt(until)` = `nextInt(0, until)`：`n = until - from`，`n` 是 2 的幂时走 `nextBits(log2(n))`，
//!   否则用 `nextInt().ushr(1) % n` + 拒绝采样（`rnd - value + (n - 1) < 0` 就重来）；
//! - `List.shuffled(random)`：`for (i in lastIndex downTo 1) { j = random.nextInt(i + 1); swap(i, j) }`。
//!
//! 定值向量是从真的 Kotlin（`kotlin-stdlib` 2.3.21，用 javac 直接调 `RandomKt.Random(long)` 和
//! `CollectionsKt.shuffled`）跑出来的，见下面的测试表；和 Kotlin 的随机对拍在
//! `rust-bridge` 的 `BrainParityTest` 里（200+ 组固定种子）。
//!
//! 只实现到"打乱一个下标数组"够用为止，**不暴露给 Kotlin**：Rust 里不取随机数，种子由调用方注入
//! （和这一层其它平台对象的规矩一致：时间 / 随机量都从外面来）。

/// Kotlin `Random(seed: Long)` 的那台机器。
pub struct KotlinRandom {
    x: i32,
    y: i32,
    z: i32,
    w: i32,
    v: i32,
    addend: i32,
}

impl KotlinRandom {
    /// `Random(seed: Long)`：低位当 seed、高位当 seed2，然后空转 64 次。
    pub fn new(seed: i64) -> Self {
        let lo = seed as i32;
        let hi = (seed >> 32) as i32;
        let mut r = Self {
            x: lo,
            y: hi,
            z: 0,
            w: 0,
            v: !lo,
            // (seed shl 10) xor (seed2 ushr 4)：seed2 是**无符号**右移
            addend: (lo << 10) ^ (((hi as u32) >> 4) as i32),
        };
        for _ in 0..64 {
            r.next_int();
        }
        r
    }

    /// `XorWowRandom.nextInt()`。
    pub fn next_int(&mut self) -> i32 {
        let mut t = self.x;
        t ^= ((t as u32) >> 2) as i32;
        self.x = self.y;
        self.y = self.z;
        self.z = self.w;
        let v = self.v;
        self.w = v;
        t = (t ^ (t << 1)) ^ v ^ (v << 4);
        self.v = t;
        self.addend = self.addend.wrapping_add(362_437);
        self.v.wrapping_add(self.addend)
    }

    /// `XorWowRandom.nextBits(bitCount)`：取高 `bitCount` 位。
    pub fn next_bits(&mut self, bit_count: i32) -> i32 {
        let value = self.next_int();
        // JVM 的 (value ushr (32 - bitCount)) and ((-bitCount) shr 31)：移位量按 5 位取模，
        // bitCount = 0 时掩码是 0（结果 0），不要写成 >> 32（那是未定义行为）。
        let mask = (-bit_count) >> 31;
        let shifted = ((value as u32) >> ((32 - bit_count) & 31)) as i32;
        shifted & mask
    }

    /// `Random.nextInt(until)`。
    pub fn next_int_until(&mut self, until: i32) -> i32 {
        self.next_int_from(0, until)
    }

    /// `Random.nextInt(from, until)`（Kotlin 2.3.21 的实现，逐行照字节码）。
    pub fn next_int_from(&mut self, from: i32, until: i32) -> i32 {
        let n = until.wrapping_sub(from);
        if n > 0 || n == i32::MIN {
            if n & n.wrapping_neg() == n {
                // n 是 2 的幂（含 n == 0 / i32::MIN 的退化情形，照 Kotlin 原样走）
                let bit_count = 31 - (n as u32).leading_zeros() as i32;
                return from.wrapping_add(self.next_bits(bit_count));
            }
            loop {
                let rnd = ((self.next_int() as u32) >> 1) as i32;
                let value = rnd % n;
                if rnd.wrapping_sub(value).wrapping_add(n - 1) >= 0 {
                    return from.wrapping_add(value);
                }
            }
        }
        loop {
            let rnd = self.next_int();
            if rnd >= from && rnd < until {
                return rnd;
            }
        }
    }
}

/// `list.shuffled(Random(seed))` 对 `0..n` 做的排列（返回下标数组）。
pub fn shuffled_indices(n: usize, seed: i64) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..n).collect();
    if n <= 1 {
        return idx;
    }
    let mut random = KotlinRandom::new(seed);
    let mut i = (n - 1) as i32;
    while i >= 1 {
        let j = random.next_int_until(i + 1) as usize;
        idx.swap(i as usize, j);
        i -= 1;
    }
    idx
}

