//! 抖音 web 端的两个签名：
//!
//! 1. **a_bogus**（[`Profile`] / [`Env`] / [`Parts`] / [`sign`]）——骨架移植自
//!    `ylcangel/douyin_sign: a_bogus/utils.js` 与同目录的 `sm3.js`
//!    （`Copyright AngelToms`，Apache-2.0）。
//!    对应 bdms 版本 `1.0.1.19-fix.01`，和 Kotlin 的 `AbogusSigner` 逐行等价。
//! 2. **`x-secsdk-web-signature`**（[`secsdk_sign`] / [`canonical_query`]）——按
//!    cv-cat/DouYin_Spider `utils/secsdk_web_sign.py` 的实现翻译；同一作者后来把这段以 MIT 收进
//!    cv-cat/catbus（`src/platforms/douyin/web/sign.ts` 的 `canonicalQuery` / `signedUrl`），规则和盐与那里一致。
//!    和 Kotlin 的 `SecSdkSigner` 逐行等价。
//!
//! ## 边界
//!
//! **这里不取时间、不取随机数、不读文件**：
//! - `now_ms`（`clock()`）/ `enter_page_ts`（进入页面 / SDK 加载的时间）由 Kotlin 注入；
//! - a_bogus 要的随机数由 Kotlin 按**和 Kotlin 实现完全相同的顺序**从它那个 `AbogusRandom` 里取好，
//!   当 `&[f64]` 传进来（[`random_count`] 是"这次要取几个"的纯计算，对照 Kotlin `AbogusSigner.buildParts`）；
//! - `abogus_profile.json` 的常量（[`Profile`]）和浏览器环境（[`Env`]）也由 Kotlin 读好传进来。
//!
//! ## 与 Kotlin 实现对齐的几处细节
//!
//! - `String.length` / `toCharArray()` 都是 **UTF-16 code unit**：指纹串、`u81`、UA 都按 UTF-16 取码点，
//!   和 Kotlin 在非 BMP 字符上也一致。
//! - `String.trim()` 是 **Java 语义**（去掉 `<= ' '` 的字符），不是 Rust 的 Unicode 空白；
//!   [`java_trim`] 照那个写（UA 里其实只有 ASCII 空格）。
//! - `Double.toInt()`（`x | 0`）向零截断、溢出饱和：Rust 的 `as i32` 语义一致。
//! - `U[26]` 的"两周一计数"是 `(now - 1721836800000) / 1000 / 60 / 60 / 24 / 14` 的 **Double 除法**再截断。


use crate::douyin::sm3;
use crate::douyin::DouyinError;

// ===========================================================================
// a_bogus 的常量 / 环境
// ===========================================================================

/// a_bogus 的固定盐（query 末尾追加）。
pub const SALT: &str = "dhzx";

/// "两周一计数"的起点。
const TWO_WEEK_EPOCH: i64 = 1_721_836_800_000;

const E1: i32 = 145;
const E2: i32 = 110;
const E3: i32 = 66;
const E4: i32 = 189;
const E5: i32 = 44;
const E6: i32 = 211;

/// 最终那一步 RC4 的 key（单字节 211）。
const RC4_KEY: i32 = 211;

/// `U[87]` 里除 `U[86]` 的 8 个字节之外要异或的 U 下标（顺序无关，照移植来源抄的清单）。
const U87_INDICES: [usize; 50] = [
    24, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    51, 52, 53, 55, 56, 57, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 79, 80,
    84, 85,
];

/// tlist 的 50 个下标（顺序和移植来源逐项一致，**不要重排**）。
const TLIST_INDICES: [usize; 50] = [
    34, 44, 56, 61, 73, 29, 70, 45, 35, 49, 38, 66, 51, 68, 28, 48, 64, 47, 30, 71, 26, 55, 31, 69,
    59, 40, 62, 63, 27, 72, 41, 74, 57, 52, 42, 39, 33, 67, 53, 43, 65, 46, 36, 24, 60, 32, 79, 80,
    84, 85,
];

/// U 表的长度（最大下标 85）。
const U_LEN: usize = 96;

const TABLES: [(&str, &str); 5] = [
    ("s0", "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/="),
    ("s1", "Dkdpgh4ZKsQB80/Mfvw36XI1R25+WUAlEi7NLboqYTOPuzmFjJnryx9HVGcaStCe="),
    ("s2", "Dkdpgh4ZKsQB80/Mfvw36XI1R25-WUAlEi7NLboqYTOPuzmFjJnryx9HVGcaStCe="),
    ("s3", "ckdp1h4ZKsUB80/Mfvw36XIgR25+WQAlEi7NLboqYTOPuzmFjJnryx9HVGDaStCe"),
    ("s4", "Dkdpgh2ZmsQB80/MfvV36XI1R45-WUAlEixNLwoqYTOPuzKFjJnry79HbGcaStCe"),
];

/// a_bogus 要用的、**按子域 / bdms 版本 / 会话**变的常量（Kotlin `AbogusProfile`）。
///
/// 这些值不是永久常量：同一份 www 抓包，早先反解出 `u17=8` / `u37=[64,2,2,0,1]`，
/// 所以 Kotlin 那边可以由 `ctx.store` 整份覆盖，再由 FFI 传进来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// 站点 aid。www / live / login 都是 6383，creator 是 2906。
    pub aid: i32,
    /// 页面 ID，**每个子域不同**。
    pub page_id: i32,
    /// bdms 版本号（按 `.` 拆开参与计算）。
    pub sdk_version: String,
    /// UA 那条 RC4 的 key 第三个字节，**按 UA / bdms 版本**解。
    pub sdk_minor: i32,
    /// `U[44]`。
    pub u17: i32,
    /// `U[37]`：前两个对应 `l40` / `l41`，`[4]` 对应 `l42`；Kotlin 侧固定 5 项。
    pub u37: Vec<i32>,
    /// 调用计数器 `U[27]`。
    pub counter: i32,
}

/// 签名时用的浏览器环境（Kotlin `AbogusEnv`）。这些值参与 a_bogus 的字节，
/// **必须和信息流请求真正声称的一致**（UA / 窗口 / 屏幕 / 平台段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    pub user_agent: String,
    pub platform: String,
    pub inner_width: i32,
    pub inner_height: i32,
    pub outer_width: i32,
    pub outer_height: i32,
    pub avail_width: i32,
    pub avail_height: i32,
    pub screen_size_width: i32,
    pub screen_size_height: i32,
    /// 直接从**真实浏览器**里拿到的窗口指纹原串；给了就不按上面那些字段拼。
    pub fingerprint_override: Option<String>,
}

impl Env {
    /// `U[76]`：窗口指纹串，末尾不留 `|`。
    pub fn fingerprint(&self) -> String {
        if let Some(f) = &self.fingerprint_override {
            return f.clone();
        }
        [
            self.inner_width,
            self.inner_height,
            self.outer_width,
            self.outer_height,
            self.avail_width,
            self.avail_height,
            self.screen_size_width,
            self.screen_size_height,
        ]
        .iter()
        .map(|n| n.to_string())
        .chain(std::iter::once(self.platform.clone()))
        .collect::<Vec<_>>()
        .join("|")
    }
}

/// 算法中间的几段字节（Kotlin `AbogusSigner.Parts`）。
///
/// 抽出来是为了**能拿真实抓包逐字节对拍**（真实 Chrome 抓到的就是 `u86 ++ u90`），
/// 见 `douyin::tests` 和 `source-douyin` 的 `AbogusSignerTest`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    /// 8 字节随机（`U[86]`）。
    pub u86: Vec<i32>,
    /// `tlist ++ 窗口指纹 ++ 时间串 ++ [U87]`（`U[88]`），是 [`Parts::u90`] 的输入。
    pub u88: Vec<i32>,
    /// 4 字节随机头（`U[89]`）。
    pub u89: Vec<i32>,
    /// `u88_transform(u88)` 的结果（`U[90]`）。
    pub u90: Vec<i32>,
}

/// 这一签要从随机数流里取几个值。
///
/// 顺序：`U[86]` 两次 `random()`（1 + 3 次）→ `u88_transform` 每个完整三元组 1 次 →
/// `U[89]` 一次 `random()`（1 + 1 次）。Kotlin 那边就是按这个顺序 `nextDouble()` 的。
pub fn random_count(env: &Env, now_ms: i64) -> usize {
    let fingerprint_len = env.fingerprint().encode_utf16().count();
    let u81_len = format!("{},", (now_ms + 3) & 255).encode_utf16().count();
    // tlist(50) + 指纹 + u81 + u87(1)
    let u88_len = 50 + fingerprint_len + u81_len + 1;
    4 + u88_len / 3 + 2
}

/// 生成 a_bogus。
///
/// `uri` 是 query 串（已百分号编码，不含 `a_bogus` 自己）；`rand` 是 Kotlin 取好的随机数流
/// （长度至少 [`random_count`]）。返回的字符串长度**不固定**（184~200 都出现过），调用方不要按长度校验。
pub fn sign(
    profile: &Profile,
    env: &Env,
    uri: &str,
    now_ms: i64,
    enter_page_ts: i64,
    rand: &[f64],
) -> Result<String, DouyinError> {
    let parts = build_parts(profile, env, uri, now_ms, enter_page_ts, rand)?;
    let mut plain = parts.u86.clone();
    plain.extend_from_slice(&parts.u90);
    let crypt = dy_rc4(&[RC4_KEY], &plain);
    let mut bytes = parts.u89.clone();
    bytes.extend_from_slice(&crypt);
    dy_base64(&bytes, "s4")
}

/// 算到 RC4 之前为止的所有中间字节。
pub fn build_parts(
    profile: &Profile,
    env: &Env,
    uri: &str,
    now_ms: i64,
    enter_page_ts: i64,
    rand: &[f64],
) -> Result<Parts, DouyinError> {
    let expected = random_count(env, now_ms);
    if rand.len() < expected {
        return Err(internal(format!(
            "随机数不够：这次要 {expected} 个，只给了 {}",
            rand.len()
        )));
    }
    let mut rng = RandStream::new(rand);
    let mut u: [Option<i32>; U_LEN] = [None; U_LEN];

    let now = now_ms;
    let ink = now - 1;

    // U[18] / U[19] / U[21]：三处 SM3
    let url_sm3 = sm3::digest_twice(
        if uri.ends_with(SALT) {
            uri.to_string()
        } else {
            format!("{uri}{SALT}")
        }
        .as_bytes(),
    );
    let halt_sm3 = sm3::digest_twice(SALT.as_bytes());
    let ua = java_trim(&env.user_agent);
    let ua_codes: Vec<i32> = ua.encode_utf16().map(|c| c as i32).collect();
    let ua_encode = dy_base64(&dy_rc4(&create_key(profile), &ua_codes), "s3")?;
    let uae_sm3 = sm3::digest(ua_encode.as_bytes());

    // U[2..10]
    set(&mut u, 2, 1);
    set(&mut u, 3, 0);
    set(&mut u, 4, 8);
    set(&mut u, 6, 0);
    set(&mut u, 8, profile.page_id);
    set(&mut u, 9, profile.aid);

    // U[12] / U[16] / U[17]
    set(&mut u, 12, 3);
    set(&mut u, 16, 1);
    set(&mut u, 17, profile.u17);

    // 两周一计数（Double 除法再截断）
    set(
        &mut u,
        26,
        (((now - TWO_WEEK_EPOCH) as f64) / 1000.0 / 60.0 / 60.0 / 24.0 / 14.0) as i32,
    );
    set(&mut u, 27, profile.counter);
    set(&mut u, 24, 41);

    // U[25]：SDK 版本按 "." 拆开（Kotlin 里只有 [0] / [1] 被用到，但整段都要按 JS 的 Number 规则转）
    let version_array: Vec<i32> = profile.sdk_version.split('.').map(js_to_int32).collect();
    let version_head = if version_array.len() >= 2 {
        [version_array[0], version_array[1]]
    } else {
        return Err(internal(format!(
            "sdkVersion 拆不出两段：{:?}",
            profile.sdk_version
        )));
    };

    // U[28..36]
    set(&mut u, 28, ((now - enter_page_ts + 3) & 255) as i32);
    set(&mut u, 29, (now & 255) as i32);
    set(&mut u, 30, ((now >> 8) & 255) as i32);
    set(&mut u, 31, ((now >> 16) & 255) as i32);
    set(&mut u, 32, ((now >> 24) & 255) as i32);
    set(&mut u, 33, ((now / 4_294_967_296) & 255) as i32);
    set(&mut u, 34, ((now / 1_099_511_627_776) & 255) as i32);
    let n16 = get(&u, 16)?;
    set(&mut u, 35, (n16 % 256) & 255);
    set(&mut u, 36, (n16 / 256) & 255);

    // U[37..47]
    if profile.u37.len() < 5 {
        return Err(internal(format!(
            "u37 要有 5 项，实际 {} 项",
            profile.u37.len()
        )));
    }
    let u37 = &profile.u37;
    set(&mut u, 38, u37[4] & 255);
    set(&mut u, 39, (u37[4] >> 8) & 255);
    set(&mut u, 40, u37[0]);
    set(&mut u, 41, u37[1]);
    set(&mut u, 42, u37[2]);
    set(&mut u, 43, u37[3]);
    let n17 = get(&u, 17)?;
    set(&mut u, 44, n17 & 255);
    set(&mut u, 45, (n17 >> 8) & 255);
    set(&mut u, 46, (n17 >> 16) & 255);
    set(&mut u, 47, (n17 >> 24) & 255);

    // U[48..59]：三份 SM3 摘要里挑出来的字节
    set(&mut u, 48, url_sm3[9] as i32 & 255);
    set(&mut u, 49, url_sm3[18] as i32 & 255);
    set(&mut u, 51, url_sm3[3] as i32 & 255);
    set(&mut u, 52, halt_sm3[10] as i32 & 255);
    set(&mut u, 53, halt_sm3[19] as i32 & 255);
    set(&mut u, 55, halt_sm3[4] as i32 & 255);
    set(&mut u, 56, uae_sm3[11] as i32 & 255);
    set(&mut u, 57, uae_sm3[21] as i32 & 255);
    set(&mut u, 59, uae_sm3[5] as i32 & 255);

    // U[60..65] = ink 的各个字节
    set(&mut u, 60, (ink & 255) as i32);
    set(&mut u, 61, ((ink >> 8) & 255) as i32);
    set(&mut u, 62, ((ink >> 16) & 255) as i32);
    set(&mut u, 63, ((ink >> 24) & 255) as i32);
    set(&mut u, 64, ((ink / 4_294_967_296) & 255) as i32);
    set(&mut u, 65, ((ink / 1_099_511_627_776) & 255) as i32);
    let n12 = get(&u, 12)?;
    set(&mut u, 66, n12);

    // U[67..74] = pageId / aid 的字节
    let n8 = get(&u, 8)?;
    let n9 = get(&u, 9)?;
    set(&mut u, 67, n8 & 255);
    set(&mut u, 68, (n8 >> 8) & 255);
    set(&mut u, 69, (n8 >> 16) & 255);
    set(&mut u, 70, (n8 >> 24) & 255);
    set(&mut u, 71, n9 & 255);
    set(&mut u, 72, (n9 >> 8) & 255);
    set(&mut u, 73, (n9 >> 16) & 255);
    set(&mut u, 74, (n9 >> 24) & 255);

    // U[76..85]：窗口指纹串 + "(时间+3)," 的字符码（UTF-16 code unit，和 Kotlin 的 length 一致）
    let fingerprint = env.fingerprint();
    let fingerprint_len = fingerprint.encode_utf16().count() as i32;
    set(&mut u, 78, fingerprint_len);
    set(&mut u, 79, fingerprint_len & 255);
    set(&mut u, 80, (fingerprint_len >> 8) & 255);
    let u81 = format!("{},", (now + 3) & 255);
    let u81_len = u81.encode_utf16().count() as i32;
    set(&mut u, 83, u81_len);
    set(&mut u, 84, u81_len & 255);
    set(&mut u, 85, (u81_len >> 8) & 255);

    // U[86]：8 字节随机（两个 random 调用）
    let mut u86 = random(&mut rng, version_head, None, 1)?;
    u86.extend(random(&mut rng, version_head, Some(2), 2)?);

    // U[87]：一串字段异或
    let mut u87 = 0;
    for b in &u86 {
        u87 ^= *b;
    }
    for index in U87_INDICES {
        u87 ^= get(&u, index)?;
    }

    // tlist（顺序和移植来源逐项一致，**不要重排**）
    let mut u88: Vec<i32> = Vec::with_capacity(51 + fingerprint_len as usize);
    for index in TLIST_INDICES {
        u88.push(get(&u, index)?);
    }
    u88.extend(fingerprint.encode_utf16().map(|c| c as i32));
    u88.extend(u81.encode_utf16().map(|c| c as i32));
    u88.push(u87);

    // U[89]：4 字节随机头
    let u89 = random(&mut rng, [3, 82], Some(1), 2)?;

    // U[90]
    let u90 = u88_transform(&mut rng, &u88)?;

    if rng.consumed() != expected {
        return Err(internal(format!(
            "随机数消耗数不对：按公式 {expected}，实际 {}",
            rng.consumed()
        )));
    }

    Ok(Parts {
        u86,
        u88,
        u89,
        u90,
    })
}

/// 抽出来的随机数流。Kotlin 的 `AbogusRandom.nextDouble()` 就是这个顺序。
pub struct RandStream<'a> {
    values: &'a [f64],
    pos: usize,
}

impl<'a> RandStream<'a> {
    pub fn new(values: &'a [f64]) -> Self {
        Self { values, pos: 0 }
    }

    pub fn next(&mut self) -> Result<f64, DouyinError> {
        let v = self
            .values
            .get(self.pos)
            .copied()
            .ok_or_else(|| internal("随机数用完了（Kotlin 那边取少了）".to_string()))?;
        self.pos += 1;
        Ok(v)
    }

    pub fn consumed(&self) -> usize {
        self.pos
    }
}

// ===========================================================================
// 加密原语（都按 JS 的位运算语义实现）
// ===========================================================================

/// 最终那一步 RC4 的**密钥流**（长度 `length`）。
///
/// 给对拍工具用：RC4 是流密码，密钥流只取决于密钥（这里固定是单字节 211），
/// 所以拿全 0 明文走一遍 `dy_rc4` 得到的就是密钥流。
pub fn keystream(length: usize) -> Vec<i32> {
    dy_rc4(&[RC4_KEY], &vec![0; length])
}

/// 魔改 RC4：S 盒初始化为 `255 - i`，密钥调度是 `j = (j * S[i] + j + K[i]) % 256`，PRGA 是标准的。
pub fn dy_rc4(key: &[i32], text: &[i32]) -> Vec<i32> {
    let mut s = [0i32; 256];
    for (i, slot) in s.iter_mut().enumerate() {
        *slot = 255 - i as i32;
    }
    let mut k = [0i32; 256];
    for (i, slot) in k.iter_mut().enumerate() {
        *slot = key[i % key.len()] & 0xFF;
    }
    let mut j: i32 = 0;
    for idx in 0..256 {
        j = (j * s[idx] + j + k[idx]) % 256;
        s.swap(idx, j as usize);
    }
    let mut out = vec![0i32; text.len()];
    let mut a: usize = 0;
    let mut b: usize = 0;
    for (n, t) in text.iter().enumerate() {
        a = (a + 1) % 256;
        b = (b + s[a] as usize) % 256;
        s.swap(a, b);
        let rnd = s[(s[a] + s[b]) as usize % 256];
        out[n] = ((t & 0xFFFF) ^ rnd) & 0xFF;
    }
    out
}

/// 5 张自定义 Base64 表里的 `table`（`s0`~`s4`）：逐 3 字节取，不足补 `=`。
pub fn dy_base64(bytes: &[i32], table: &str) -> Result<String, DouyinError> {
    let t = TABLES
        .iter()
        .find(|(name, _)| *name == table)
        .map(|(_, chars)| *chars)
        .ok_or_else(|| internal(format!("未知的 Base64 表：{table}")))?;
    let t: Vec<char> = t.chars().collect();
    let mut sb = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let c1 = bytes[i] & 255;
        let c2 = bytes.get(i + 1).map(|v| v & 255);
        let c3 = bytes.get(i + 2).map(|v| v & 255);
        let c = (c1 << 16) | ((c2.unwrap_or(0)) << 8) | c3.unwrap_or(0);
        sb.push(t[((c & 16_515_072) >> 18) as usize]);
        sb.push(t[((c & 258_048) >> 12) as usize]);
        if c2.is_some() {
            sb.push(t[((c & 4032) >> 6) as usize]);
        }
        if c3.is_some() {
            sb.push(t[(c & 63) as usize]);
        }
        if c2.is_none() {
            sb.push('=');
        }
        if c3.is_none() {
            sb.push('=');
        }
        i += 3;
    }
    Ok(sb)
}

/// `createKey()`：`String.fromCharCode(1/256, 1%256, sdkMinor)` → 前两个是 0 和 1。
fn create_key(profile: &Profile) -> [i32; 3] {
    [0, 1, profile.sdk_minor & 0xFF]
}

/// `u88Transform`：每 3 字节混进一个随机数变 4 字节；不满 3 字节的尾巴照原样带上。
pub fn u88_transform(rng: &mut RandStream<'_>, array: &[i32]) -> Result<Vec<i32>, DouyinError> {
    let mut out = Vec::with_capacity(array.len() * 4 / 3 + 2);
    let mut i = 0;
    while i < array.len() {
        if i + 2 < array.len() {
            let x = (rng.next()? * 1000.0) as i32 & 255;
            let a = (x & E1) | (array[i] & E2);
            let b = (x & E3) | (array[i + 1] & E4);
            let c = (x & E5) | (array[i + 2] & E6);
            let d = (array[i] & E1) | (array[i + 1] & E3) | (array[i + 2] & E5);
            out.push(a);
            out.push(b);
            out.push(c);
            out.push(d);
        } else {
            out.push(array[i]);
            if let Some(v) = array.get(i + 1) {
                out.push(*v);
            }
        }
        i += 3;
    }
    Ok(out)
}

/// `random(inObj)`：把一个 2 字节输入和随机数按位混成 4 字节。
///
/// `flag` 决定 r1 / r2 怎么来：`length > 1` 且 `one_value` 存在时 `flag = one_value`。
/// 移植来源里 `flag == 1` / `flag == 2` 两条分支少了 `Math.random` 的调用括号，
/// 这里按**真实浏览器行为**（真的调用了）实现，Kotlin 那边有抓包证据（见 `AbogusSigner` 的类说明）。
fn random(
    rng: &mut RandStream<'_>,
    zero: [i32; 2],
    one_value: Option<i32>,
    length: i32,
) -> Result<Vec<i32>, DouyinError> {
    let in0 = zero[0];
    let in1 = zero[1];
    let ri = (rng.next()? * 65535.0) as i32;
    let mut r1 = ri & 255;
    let mut r2 = (ri >> 8) & 255;

    let flag = if length > 1 { one_value.unwrap_or(0) } else { 0 };
    if flag == 1 {
        r2 = (rng.next()? * 40.0) as i32;
    }
    if flag == 2 {
        r1 = (rng.next()? * 240.0) as i32;
        if r1 > 109 {
            r1 += r1 % 2;
            r1 += 1;
        }
        r2 = (rng.next()? * 255.0) as i32 & 77;
        r2 |= (1 << 1) | (1 << 4) | (1 << 5) | (1 << 7);
    }

    let u1 = (r1 & 170) | (in0 & 85);
    let u2 = (r1 & 85) | (in0 & 170);
    let u3 = (r2 & 170) | (in1 & 85);
    let u4 = (r2 & 85) | (in1 & 170);
    Ok(vec![u1, u2, u3, u4])
}

/// JS 的 `Number("...")` 只认整段是数字（否则 NaN → 0）。Kotlin 那边的正则：
/// `^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$`。
fn js_to_int32(text: &str) -> i32 {
    let t = java_trim(text);
    if !is_js_number(t) {
        return 0;
    }
    t.parse::<f64>().map(|d| d as i32).unwrap_or(0)
}

fn is_js_number(t: &str) -> bool {
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let mut digits_before = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        digits_before += 1;
        i += 1;
    }
    let mut digits_after = 0;
    let mut has_dot = false;
    if i < b.len() && b[i] == b'.' {
        has_dot = true;
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            digits_after += 1;
            i += 1;
        }
    }
    // `\d+\.?\d*` 或 `\.\d+`
    let mantissa_ok = if has_dot {
        digits_before > 0 || digits_after > 0
    } else {
        digits_before > 0
    };
    if !mantissa_ok {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let mut exp_digits = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            exp_digits += 1;
            i += 1;
        }
        if exp_digits == 0 {
            return false;
        }
    }
    i == b.len()
}

/// Java `String.trim()`：去掉两端 `<= ' '` 的字符（不是 Unicode 空白）。
pub fn java_trim(s: &str) -> &str {
    let mut start = 0;
    let mut end = 0;
    let mut seen = false;
    for (i, c) in s.char_indices() {
        if c > ' ' {
            if !seen {
                start = i;
                seen = true;
            }
            end = i + c.len_utf8();
        }
    }
    if !seen {
        return "";
    }
    &s[start..end]
}

// ===========================================================================
// x-secsdk-web-signature
// ===========================================================================

/// VM 常量池里的固定盐（与账号 / 会话无关）。
pub const SECSDK_SALT: &str = "A96D855A08C0A9707F8BEF0D9A527E4E";
/// 签名头 / query 参数名。
pub const SECSDK_HEADER: &str = "x-secsdk-web-signature";
/// 时间戳参数名。
pub const TIMESTAMP_PARAM: &str = "timestamp";

/// `encodeURIComponent` 不编码的字符（除 `A-Za-z0-9` 与 `- _ . ~` 之外）。
const JS_SAFE: &str = "-_.!~*'()";

/// SDK 的 `webSign` 策略保护的表（GET）。`module/feed` 和评论**不在**表里，所以推荐流只要 a_bogus。
pub const PROTECTED_GET: [&str; 14] = [
    "/aweme/v1/web/aweme/detail/",
    "/aweme/v1/web/aweme/post/",
    "/aweme/v1/web/aweme/favorite/",
    "/aweme/v1/web/aweme/listcollection/",
    "/aweme/v1/web/mix/aweme/",
    "/aweme/v1/web/tab/feed/",
    "/aweme/v1/web/mix/list/",
    "/aweme/v1/web/music/aweme/",
    "/aweme/v1/web/music/list/",
    "/aweme/v1/web/mix/detail/",
    "/aweme/v1/web/mix/listcollection/",
    "/aweme/v1/web/music/detail/",
    "/aweme/v1/web/collects/list/",
    "/aweme/v1/web/collects/video/list/",
];

/// POST 的前 6 条。
pub const PROTECTED_POST: [&str; 6] = [
    "/aweme/v1/web/aweme/detail/",
    "/aweme/v1/web/aweme/post/",
    "/aweme/v1/web/aweme/favorite/",
    "/aweme/v1/web/aweme/listcollection/",
    "/aweme/v1/web/mix/aweme/",
    "/aweme/v1/web/tab/feed/",
];

/// 这个路径要不要加 [`SECSDK_HEADER`]（`method` 不看大小写）。
pub fn secsdk_is_protected(path: &str, method: &str) -> bool {
    if method.eq_ignore_ascii_case("POST") {
        PROTECTED_POST.contains(&path)
    } else {
        PROTECTED_GET.contains(&path)
    }
}

/// 一次签名的结果：签名值 + **实际应该发出去的** query。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    pub signature: String,
    pub query: String,
    pub timestamp: i64,
}

/// 规范化 query（不含 `timestamp`，也不含签名本身）。
///
/// 已经有了 `timestamp` / 签名字段时会先剥掉，所以对同一个 URL 反复调用是幂等的。
pub fn canonical_query(query: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (raw_key, raw_value) = match pair.find('=') {
            Some(eq) => (&pair[..eq], &pair[eq + 1..]),
            None => (pair, ""),
        };
        let key = decode_component(raw_key);
        if key == TIMESTAMP_PARAM || key == SECSDK_HEADER {
            continue;
        }
        parts.push(format!("{key}={}", encode_component(&decode_component(raw_value))));
    }
    parts.join("&")
}

/// 算签名。`uifid_fallback` 只在 query 里没有 `uifid` 时补在末尾。
pub fn secsdk_sign(query: &str, uifid_fallback: Option<&str>, timestamp: i64) -> Signed {
    let mut canon = canonical_query(query);
    let has_uifid = canon
        .split('&')
        .filter(|p| !p.is_empty())
        .any(|p| p.split('=').next().unwrap_or("") == "uifid");
    let fallback = uifid_fallback.filter(|f| !f.is_empty());
    if !has_uifid {
        if let Some(f) = fallback {
            let extra = format!("uifid={}", encode_component(f));
            canon = if canon.is_empty() {
                extra
            } else {
                format!("{canon}&{extra}")
            };
        }
    }
    let signed = if canon.is_empty() {
        format!("{TIMESTAMP_PARAM}={timestamp}")
    } else {
        format!("{canon}&{TIMESTAMP_PARAM}={timestamp}")
    };

    // 参与拼明文的 uifid 是「解码后的原值」
    let uifid = signed
        .split('&')
        .find(|p| p.starts_with("uifid="))
        .map(|p| decode_component(&p["uifid=".len()..]))
        .or_else(|| fallback.map(|f| f.to_string()))
        .unwrap_or_default();

    let plain = format!("{uifid}_{timestamp}_{SECSDK_SALT}_{signed}");
    Signed {
        signature: md5_hex(&plain),
        query: signed,
        timestamp,
    }
}

/// 等价于 JS 的 `decodeURIComponent`（外加 `+` → 空格）。
///
/// **逐 UTF-16 code unit 处理**，这一点是照 Kotlin 抄的、不是为了"正确"：Kotlin 那边
/// `c.toString().toByteArray(UTF_8)` 对落单的代理项（emoji 的半个）会编码成 `?`，
/// 所以含非 BMP 字符的原文经 Kotlin 走一遍会变成 `??`。生产上 query 传到这儿时已经是
/// `%XX` 编码过的 ASCII（`signedUrl` 读的是 OkHttp 的 `encodedQuery`），碰不到这种情况；
/// 但两边必须**任何输入都给同一个结果**，所以这里照抄那个行为。
pub fn decode_component(text: &str) -> String {
    if !text.contains('%') && !text.contains('+') {
        return text.to_string();
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out: Vec<u8> = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < units.len() {
        let unit = units[i];
        if unit == b'+' as u16 {
            out.push(b' ');
            i += 1;
        } else if unit == b'%' as u16 && i + 2 < units.len() {
            if let Some(b) = hex_pair(units[i + 1], units[i + 2]) {
                out.push(b);
                i += 3;
            } else {
                push_utf16_unit(&mut out, unit);
                i += 1;
            }
        } else {
            push_utf16_unit(&mut out, unit);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Kotlin `String.toByteArray(UTF_8)` 对一个 UTF-16 code unit 的行为：
/// BMP 字符按 UTF-8 写；落单的代理项被替换成 `?`（和 JDK 的 REPLACE 动作一样）。
fn push_utf16_unit(out: &mut Vec<u8>, unit: u16) {
    match char::from_u32(unit as u32) {
        Some(c) => {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        None => out.push(b'?'),
    }
}

/// 一个 2-UTF-16-单元的片段能不能按 Kotlin `substring(i+1, i+3).toIntOrNull(16)` 解析成字节：
/// 允许一个前导 `+` / `-`，其余每位都得是十六进制数字；空白 / 别的字符一律不认
/// （Java 的 parseInt 不跳过空白）。
fn hex_pair(hi: u16, lo: u16) -> Option<u8> {
    let hi = char::from_u32(hi as u32)?;
    let lo = char::from_u32(lo as u32)?;
    let (negative, digits): (bool, &[char]) = match hi {
        '+' => (false, &[lo]),
        '-' => (true, &[lo]),
        _ => (false, &[hi, lo]),
    };
    if digits.is_empty() || !digits.iter().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = digits
        .iter()
        .fold(0i32, |acc, c| acc * 16 + c.to_digit(16).unwrap() as i32);
    let value = if negative { -value } else { value };
    Some((value & 0xFF) as u8)
}

/// 等价于 JS 的 `encodeURIComponent`。
pub fn encode_component(text: &str) -> String {
    let mut sb = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        let ch = byte as char;
        if ch.is_ascii_alphanumeric() || JS_SAFE.contains(ch) {
            sb.push(ch);
        } else {
            sb.push('%');
            sb.push(HEX[(byte >> 4) as usize] as char);
            sb.push(HEX[(byte & 0x0F) as usize] as char);
        }
    }
    sb
}

const HEX: &[u8] = b"0123456789ABCDEF";

/// 小写十六进制 MD5。
// `md5_hex` 从 `crate::hosted` 来（小写十六进制）；`pub use` 保留老路径。
pub use crate::hosted::md5_hex;

// ===========================================================================
// 内部小工具
// ===========================================================================

fn internal(detail: String) -> DouyinError {
    DouyinError::Internal { detail }
}

fn set(u: &mut [Option<i32>; U_LEN], index: usize, value: i32) {
    u[index] = Some(value);
}

fn get(u: &[Option<i32>; U_LEN], index: usize) -> Result<i32, DouyinError> {
    u[index].ok_or_else(|| internal(format!("U[{index}] 还没赋值（算法移植有误）")))
}
