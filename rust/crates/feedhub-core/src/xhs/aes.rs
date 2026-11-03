//! `x-rap-param` 用的分组密码：标准 AES-128（ECB，一次一块），只是把 S 盒换成了小红书自己的 256 字节表，密钥固定。
//!
//! 逐条对照 `source-xiaohongshu/src/main/kotlin/feedhub/source/xiaohongshu/XhsAes.kt`。
//! xhshow 把它叫"SM4 变体"并预展开了轮密钥，Spider_XHS 的 `rap_crypto.js` 说明它其实就是换了 S 盒的 AES-128：
//! 两边的 S 盒逐字节一致，xhshow 的第 0 轮密钥字 0x6B714931 正是 ASCII "kqI1"。这里按 AES 的密钥扩展自己算轮密钥，
//! 并在单测里和 xhshow 预展开的 10 轮密钥逐字对照（见 `xhs/tests.rs`）。
//!
//! 这是"不许自己实现密码学原语"的唯一例外：S 盒不是标准的，RustCrypto 的 `aes`
//! 和其他现成库都不能换 S 盒。它也不承担安全用途，只是为了对上小红书的请求格式。

/// 小红书自己的 S 盒（xhshow 和 Spider_XHS 逐字节一致）。
pub const SBOX_HEX: &str = "7a0158e0504e02791d4b53da6b48d452ed7712211415ec1018e5b9f10c08fc7d\
f9cdb5c8e637268756bab82badf068f78b8dd35e364d2e923182f229703d2dd7\
b640b243448078d20d494a09636c073a9ed506c6e162f4342459a9572a003e17\
2c0a1a42fa93bedcf5b36a13e803c797bb737686e3467247d0054c387c1f81ab\
7551ebf33274118f84899c71227e9dcf3f9169653c6d96a2989933399acac39f\
a0bce4a3a4547fa7a8046f5dacb727afb02841aeb46e0b1bdf8e30b1fe906160\
c0cb5c0eef1683ea20e9c955c44585cc1eaa678a7b35d619d8d9c2db94dd1cde\
a6fff8bf5b5a0fe7c1bdd166c525ee8ce25f88a13ba5f6ce952f6423fbfd4f9b";

/// x-rap-param 的固定密钥。
pub const RAP_KEY: &[u8] = b"kqI1DTcwKX90ZtAy";

pub struct XhsAes {
    /// 11 个 16 字节的轮密钥，按字节顺序（第 c 个字的 4 个字节放在 `[4c, 4c+4)`）。
    round_keys: Vec<[u8; 16]>,
}

impl XhsAes {
    /// 用固定的 S 盒和密钥新建（生产上只有 [RAP_KEY] 这一种）。
    pub fn new(key: &[u8], sbox: &[u8; 256]) -> Result<Self, String> {
        if key.len() != 16 {
            return Err("AES-128 密钥要 16 字节".to_string());
        }
        Ok(Self {
            round_keys: expand(key, sbox),
        })
    }

    /// `x-rap-param` 用的小红书 AES（固定密钥 + 固定 S 盒）。
    pub fn rap() -> Self {
        Self::new(RAP_KEY, &sbox()).expect("RAP_KEY 是 16 字节")
    }

    /// 第 `round` 轮密钥的 4 个大端 32 位字，测试里和 xhshow 的表对照用。
    pub fn round_key_words(&self, round: usize) -> [u32; 4] {
        let rk = &self.round_keys[round];
        let mut out = [0u32; 4];
        for (c, slot) in out.iter_mut().enumerate() {
            *slot = ((rk[4 * c] as u32) << 24)
                | ((rk[4 * c + 1] as u32) << 16)
                | ((rk[4 * c + 2] as u32) << 8)
                | (rk[4 * c + 3] as u32);
        }
        out
    }

    /// 加密一个 16 字节的块；不足 16 字节的右边补 0。
    pub fn encrypt_block(&self, block: &[u8]) -> Result<[u8; 16], String> {
        if block.len() > 16 {
            return Err("块长不能超过 16".to_string());
        }
        let sbox = sbox();
        let mut s = [0u8; 16];
        s[..block.len()].copy_from_slice(block);
        add_round_key(&mut s, &self.round_keys[0]);
        for round in 1..=10 {
            for v in s.iter_mut() {
                *v = sbox[*v as usize];
            }
            shift_rows(&mut s);
            if round < 10 {
                mix_columns(&mut s);
            }
            add_round_key(&mut s, &self.round_keys[round]);
        }
        Ok(s)
    }

    /// ECB：逐块加密，最后一块不足 16 字节就用 `pad`（长度够补满）补，`None` 时补 0。
    pub fn encrypt_ecb(&self, data: &[u8], pad: Option<&[u8]>) -> Result<Vec<u8>, String> {
        let mut full = data.to_vec();
        if full.len() % 16 != 0 {
            let need = 16 - full.len() % 16;
            match pad {
                Some(p) => {
                    if p.len() < need {
                        return Err(format!("补位不够：要 {need} 字节，只有 {}", p.len()));
                    }
                    full.extend_from_slice(&p[..need]);
                }
                None => full.extend(std::iter::repeat(0u8).take(need)),
            }
        }
        let mut out = Vec::with_capacity(full.len());
        for chunk in full.chunks(16) {
            out.extend_from_slice(&self.encrypt_block(chunk)?);
        }
        Ok(out)
    }
}

/// S 盒（256 字节）。Kotlin 那边是从一串十六进制解出来的，这里同样按十六进制常量解一次。
pub fn sbox() -> [u8; 256] {
    static SBOX: std::sync::OnceLock<[u8; 256]> = std::sync::OnceLock::new();
    *SBOX.get_or_init(|| {
        let bytes = super::codec::unhex(SBOX_HEX).expect("SBOX_HEX 是偶数长度的十六进制");
        let mut out = [0u8; 256];
        out.copy_from_slice(&bytes);
        out
    })
}

fn expand(key: &[u8], sbox: &[u8; 256]) -> Vec<[u8; 16]> {
    let mut w = [[0u8; 4]; 44];
    for i in 0..4 {
        for j in 0..4 {
            w[i][j] = key[4 * i + j];
        }
    }
    let mut rcon: u8 = 1;
    for i in 4..44 {
        let mut t = w[i - 1];
        if i % 4 == 0 {
            let r = [t[1], t[2], t[3], t[0]];
            for j in 0..4 {
                t[j] = sbox[r[j] as usize];
            }
            t[0] ^= rcon;
            rcon = xtime(rcon);
        }
        for j in 0..4 {
            w[i][j] = w[i - 4][j] ^ t[j];
        }
    }
    (0..11)
        .map(|round| {
            let mut rk = [0u8; 16];
            for (i, slot) in rk.iter_mut().enumerate() {
                *slot = w[4 * round + i / 4][i % 4];
            }
            rk
        })
        .collect()
}

fn xtime(a: u8) -> u8 {
    ((a << 1) ^ if a & 0x80 != 0 { 0x1B } else { 0 }) & 0xFF
}

fn mul2(a: u8) -> u8 {
    xtime(a)
}

fn mul3(a: u8) -> u8 {
    xtime(a) ^ a
}

fn add_round_key(s: &mut [u8; 16], rk: &[u8; 16]) {
    for i in 0..16 {
        s[i] ^= rk[i];
    }
}

fn shift_rows(s: &mut [u8; 16]) {
    let t = *s;
    for c in 0..4 {
        for r in 0..4 {
            s[4 * c + r] = t[4 * ((c + r) % 4) + r];
        }
    }
}

fn mix_columns(s: &mut [u8; 16]) {
    for c in 0..4 {
        let a0 = s[4 * c];
        let a1 = s[4 * c + 1];
        let a2 = s[4 * c + 2];
        let a3 = s[4 * c + 3];
        s[4 * c] = mul2(a0) ^ mul3(a1) ^ a2 ^ a3;
        s[4 * c + 1] = a0 ^ mul2(a1) ^ mul3(a2) ^ a3;
        s[4 * c + 2] = a0 ^ a1 ^ mul2(a2) ^ mul3(a3);
        s[4 * c + 3] = mul3(a0) ^ a1 ^ a2 ^ mul2(a3);
    }
}
