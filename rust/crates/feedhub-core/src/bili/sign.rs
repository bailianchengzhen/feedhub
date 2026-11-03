//! B站网页接口的 WBI 签名（补上 `wts`、`w_rid` 两个参数），和 `source-bilibili/WbiSigner.kt` 逐字节一致。
//!
//! 协议是公开的：
//! 1. `nav` 接口的 `wbi_img` 给两张图的地址，文件名（去扩展名）就是 img_key、sub_key；
//! 2. 两者首尾相接成 64 个字符，按固定的 32 项下标表挑字符，得到 mixin key；
//! 3. 参数补上 `wts`（秒），按键名排序；值里删掉 `!'()*`，键和值都按 `encodeURIComponent` 编码，拼成 `k=v&…`；
//! 4. `w_rid` = 小写十六进制的 md5(拼好的 query + mixin key)。
//!
//! 签错时服务端只回 -403，本地看不出来，所以靠定值测试兜底（`WbiSignerTest` 和下面的测试）。

use crate::hosted::md5_hex;

/// mixin key 的下标表：第 n 个字符取 `img_key + sub_key` 的第 `PICK[n]` 位。
const PICK: [usize; 32] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13,
];

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// 由 img_key / sub_key 算 32 个字符的 mixin key（key 不够长时跳过越界的下标）。
pub fn mixin_key(img_key: &str, sub_key: &str) -> String {
    let joined: Vec<char> = img_key.chars().chain(sub_key.chars()).collect();
    let mut key = String::with_capacity(PICK.len());
    for &pos in &PICK {
        if let Some(&c) = joined.get(pos) {
            key.push(c);
        }
    }
    key
}

/// 给一份参数补上 `wts` / `w_rid`，返回**保持插入顺序**的新参数表（调用方按顺序拼 query，请求字节才不变）：
/// 已有同名键只换值、位置不变，没有就接在最后——和 Kotlin `LinkedHashMap` 的 `put` 一样。
///
/// 纯函数：key 和 `wts` 都由调用方给，所以能用定值做回归。
pub fn sign(params: &[(String, String)], mixin_key: &str, wts: i64) -> Vec<(String, String)> {
    let mut out = params.to_vec();
    put(&mut out, "wts", wts.to_string());

    // 按键名排序（稳定排序；B站的参数名都是 ASCII，字节序和 Kotlin 的 UTF-16 序一致）。
    let mut order: Vec<usize> = (0..out.len()).collect();
    order.sort_by(|&a, &b| out[a].0.cmp(&out[b].0));

    let mut text = String::new();
    for (n, &idx) in order.iter().enumerate() {
        if n > 0 {
            text.push('&');
        }
        let (key, value) = &out[idx];
        push_component(&mut text, key);
        text.push('=');
        let kept: String = value.chars().filter(|c| !"!'()*".contains(*c)).collect();
        push_component(&mut text, &kept);
    }
    text.push_str(mixin_key);

    let w_rid = md5_hex(&text);
    put(&mut out, "w_rid", w_rid);
    out
}

fn put(params: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(slot) = params.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value;
    } else {
        params.push((key.to_string(), value));
    }
}

/// 按 UTF-8 逐字节编码：RFC 3986 的 unreserved（字母、数字、`-_.~`）原样，其余写成 `%XX`（大写）。
/// 去掉 `!'()*` 之后就和 JS `encodeURIComponent` 一致。**不能用**表单编码：它把空格编成 `+`。
fn push_component(out: &mut String, s: &str) {
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX_UPPER[usize::from(b >> 4)] as char);
            out.push(HEX_UPPER[usize::from(b & 0x0F)] as char);
        }
    }
}

/// `https://i0.hdslb.com/bfs/wbi/7cd0...077c.png` → `7cd0...077c`
pub fn key_from_url(url: &str) -> String {
    let name = url.rsplit('/').next().unwrap_or(url);
    name.split('.').next().unwrap_or(name).to_string()
}

