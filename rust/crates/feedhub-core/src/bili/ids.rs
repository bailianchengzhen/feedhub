//! BV 号 ⇄ AV 号（aid）互转，以及 `DetailTarget.Native` 的 id 格式。和 `BiliIds.kt` / `BvAv.kt` 一致。
//!
//! 评论、弹幕接口要的是 aid，而我们的 id 是 BV 号，所以不查接口、直接算。
//! 换算规则是公开协议：
//! - aid → BV：`(2^51 | aid) ^ 23442827791579`，按 58 进制写成 9 位（高位在前，字母表固定），前面接 `BV1`，
//!   再交换第 3 / 9 位和第 4 / 7 位（下标从整个 12 位字符串算起）；
//! - BV → aid：倒过来做，最后用 `2^51 - 1` 去掉最高位再异或回去。
//!
//! 期望值见 `BvAvTest`（用的是详情接口真实返回的 bvid / aid 对）。

const XOR: i64 = 23_442_827_791_579;
const TOP_BIT: i64 = 1 << 51;
const LOW_BITS: i64 = TOP_BIT - 1;
const DIGITS: &[u8; 58] = b"FcwAPNKTMug3GV5Lj7EJnHpWsx4tb8haYeviqBz6rkCy12mUSDQX9RdoZf";
const PREFIX: &[u8; 3] = b"BV1";
const LEN: usize = 12;

fn digit_value(c: u8) -> Option<i64> {
    DIGITS.iter().position(|&d| d == c).map(|v| v as i64)
}

/// 交换第 3 / 9 位和第 4 / 7 位；做两次等于没做，所以两个方向共用。
fn shuffle(chars: &mut [u8; LEN]) {
    chars.swap(3, 9);
    chars.swap(4, 7);
}

/// `BV1` + 9 个 base58 字符（只检查格式，不保证存在）。
pub fn is_bvid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == LEN && b.starts_with(PREFIX) && b[PREFIX.len()..].iter().all(|&c| DIGITS.contains(&c))
}

pub fn to_bvid(aid: i64) -> Option<String> {
    if !(1..TOP_BIT).contains(&aid) {
        return None;
    }
    let mut out = [0u8; LEN];
    out[..PREFIX.len()].copy_from_slice(PREFIX);
    let mut n = (TOP_BIT | aid) ^ XOR;
    for slot in out[PREFIX.len()..].iter_mut().rev() {
        *slot = DIGITS[(n % 58) as usize];
        n /= 58;
    }
    shuffle(&mut out);
    String::from_utf8(out.to_vec()).ok()
}

/// 不是合法 BV 号返回 `None`。
pub fn to_aid(bvid: &str) -> Option<i64> {
    if !is_bvid(bvid) {
        return None;
    }
    let mut chars: [u8; LEN] = bvid.as_bytes().try_into().ok()?;
    shuffle(&mut chars);
    let mut n: i64 = 0;
    for &c in &chars[PREFIX.len()..] {
        n = n * 58 + digit_value(c)?;
    }
    Some((n & LOW_BITS) ^ XOR)
}

/// 动态 / 图文 / 专栏的原生详情 id：`dyn:<id_str>`。
pub fn dynamic_native_id(id_str: &str) -> String {
    format!("dyn:{id_str}")
}

/// 解析 `DetailTarget.Native.id`（视频就是 bvid，动态是 `dyn:<id_str>`；纯数字也当动态 id 收）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BiliId {
    Video(String),
    Dynamic(String),
}

pub fn parse_id(id: &str) -> Option<BiliId> {
    if is_bvid(id) {
        return Some(BiliId::Video(id.to_string()));
    }
    if let Some(rest) = id.strip_prefix("dyn:") {
        return if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
            Some(BiliId::Dynamic(rest.to_string()))
        } else {
            None
        };
    }
    if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
        return Some(BiliId::Dynamic(id.to_string()));
    }
    None
}

