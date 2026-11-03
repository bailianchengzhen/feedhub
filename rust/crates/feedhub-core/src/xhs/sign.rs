//! 小红书网页接口的请求签名：`X-s`（`XYS_…`）、`X-t`、`X-S-Common`、两个 traceid，以及 homefeed / feed / user_posted
//! 要带的 `x-rap-param` 和 homefeed / feed 要带的 `xy-direction`。
//!
//! 逐条对照 `source-xiaohongshu/src/main/kotlin/feedhub/source/xiaohongshu/XhsSigner.kt`（含 `XhsMns` / `XhsB1` /
//! `XhsRap`）、`XhsCodec.kt`、`XhsAes.kt`。
//!
//! **当前把两样东西全部改成注入的**：
//! - 时间：`now_ms` 由 Kotlin 传（`X-t`、x3 里的时间戳、dsllt、xray traceid 都用它），Rust 不取时钟；
//! - 随机：Kotlin 的 `Random` 取到的值（x3 的 version、两个 traceid 的十六进制、`x-rap-param` 的掩码 / 会话密钥 /
//!   密钥 / 盐 / 补位 / "加密耗时"、没有 user_id 时的 `xy-direction`）都当参数传进来，Rust 的签名函数里没有随机源。
//!
//! 会话计数（`seq` / `signCount` / `dsllt` / `xraySeq`）跟着请求传进传出（`SignState`），Rust 不存状态。
//! 因为补位长度要等 gzip 之后才知道（Kotlin 版也是），所以调用方分两步：先 `rap_pad_len()` 拿到要取几个补位字符，
//! 再 `sign()` 一次签完。

use crate::xhs::aes::XhsAes;
use crate::xhs::codec::{self, base64, json_string, md5, X3_ALPHABET, XS_ALPHABET};
use crate::xhs::deflate;
use serde_json::Value;

/// `sign_profile.json` 里**随版本变的值**。Kotlin 那边读文件（或 `ctx.store` 里的覆盖版）解析成 `XhsProfile`，
/// 再序列化成 JSON 传进来——Rust 不读文件、不复制这份 schema 的所有权。
#[derive(Debug, Clone)]
pub struct Profile {
    pub sign_version: String,
    pub app_id: String,
    pub platform: String,
    pub web_build: String,
    /// 平台默认头里的 UA / `sec-ch-ua` / `Accept-Language`（平台层拼请求用；纯函数签名不用）。
    pub user_agent: String,
    pub sec_ch_ua: String,
    pub accept_language: String,
    pub dsl: String,
    pub mns_env_const: i32,
    pub mns_env_fp_tail: Vec<u8>,
    pub mns_keystream: Vec<u8>,
    pub b1_sdk_version: String,
    pub b1_frame_count: i32,
    pub b1_x37: String,
    pub b1_x38: String,
    pub b1_x39: String,
    pub b1_x43: String,
    pub b1_x45: String,
    pub b1_x46: String,
    pub b1_x48: String,
    pub b1_x49: String,
    pub b1_x50: String,
    pub b1_x51: String,
    pub b1_x52: String,
    pub b1_x82: String,
    pub b1_time_origin_offset_ms: f64,
    pub b1_telemetry_template: String,
    pub rap_version: i32,
    pub rap_template: Vec<u8>,
}

fn req_str(v: &Value, path: &[&str]) -> Result<String, String> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k).ok_or_else(|| format!("sign_profile 缺少 {}", path.join(".")))?;
    }
    match cur {
        Value::String(s) => Ok(s.clone()),
        other => Err(format!("sign_profile 的 {} 不是字符串：{other}", path.join("."))),
    }
}

fn req_i32(v: &Value, path: &[&str]) -> Result<i32, String> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k).ok_or_else(|| format!("sign_profile 缺少 {}", path.join(".")))?;
    }
    let n = cur
        .as_i64()
        .or_else(|| cur.as_f64().map(|f| f as i64))
        .ok_or_else(|| format!("sign_profile 的 {} 不是数字：{cur}", path.join(".")))?;
    Ok(n as i32)
}

fn req_f64(v: &Value, path: &[&str]) -> Result<f64, String> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k).ok_or_else(|| format!("sign_profile 缺少 {}", path.join(".")))?;
    }
    cur.as_f64().ok_or_else(|| format!("sign_profile 的 {} 不是数字：{cur}", path.join(".")))
}

fn req_hex(v: &Value, path: &[&str]) -> Result<Vec<u8>, String> {
    let text = req_str(v, path)?;
    codec::unhex(&text).map_err(|e| format!("sign_profile 的 {}：{e}", path.join(".")))
}

impl Profile {
    /// 解析 Kotlin 那边序列化过来的 `XhsProfile`（形状和 `sign_profile.json` 一样）。
    pub fn parse(text: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("sign_profile 不是合法 JSON：{e}"))?;
        let b1_keys = [
            "x37", "x38", "x39", "x43", "x45", "x46", "x48", "x49", "x50", "x51", "x52", "x82",
        ];
        let mut b1 = std::collections::HashMap::new();
        for k in b1_keys {
            b1.insert(k, req_str(&v, &["b1", k])?);
        }
        let profile = Profile {
            sign_version: req_str(&v, &["release", "signVersion"])?,
            app_id: req_str(&v, &["release", "appId"])?,
            platform: req_str(&v, &["release", "platform"])?,
            web_build: req_str(&v, &["release", "webBuild"])?,
            user_agent: req_str(&v, &["release", "userAgent"])?,
            sec_ch_ua: req_str(&v, &["release", "secChUa"])?,
            accept_language: req_str(&v, &["release", "acceptLanguage"])?,
            dsl: req_str(&v, &["dsl"])?,
            mns_env_const: req_i32(&v, &["mns", "envConst"])?,
            mns_env_fp_tail: req_hex(&v, &["mns", "envFpTailHex"])?,
            mns_keystream: req_hex(&v, &["mns", "keystreamHex"])?,
            b1_sdk_version: req_str(&v, &["b1", "sdkVersion"])?,
            b1_frame_count: req_i32(&v, &["b1", "frameCount"])?,
            b1_x37: b1.remove("x37").unwrap(),
            b1_x38: b1.remove("x38").unwrap(),
            b1_x39: b1.remove("x39").unwrap(),
            b1_x43: b1.remove("x43").unwrap(),
            b1_x45: b1.remove("x45").unwrap(),
            b1_x46: b1.remove("x46").unwrap(),
            b1_x48: b1.remove("x48").unwrap(),
            b1_x49: b1.remove("x49").unwrap(),
            b1_x50: b1.remove("x50").unwrap(),
            b1_x51: b1.remove("x51").unwrap(),
            b1_x52: b1.remove("x52").unwrap(),
            b1_x82: b1.remove("x82").unwrap(),
            b1_time_origin_offset_ms: req_f64(&v, &["b1", "timeOriginOffsetMs"])?,
            b1_telemetry_template: req_str(&v, &["b1", "telemetryTemplate"])?,
            rap_version: req_i32(&v, &["rap", "version"])?,
            rap_template: req_hex(&v, &["rap", "templateHex"])?,
        };
        if profile.mns_env_fp_tail.len() != 14 {
            return Err(format!("sign_profile 的 envFpTail 要 14 字节，实际 {}", profile.mns_env_fp_tail.len()));
        }
        Ok(profile)
    }
}

/// 一个待签名的请求（字段和 `XhsSigner.Request` 一一对应）。
#[derive(Debug, Clone)]
pub struct SignRequest {
    /// 签名用的 api：POST 是路径，GET 是路径 + `?` + 查询串。
    pub api: String,
    /// POST 的紧凑 JSON；GET 为 `None`。
    pub body: Option<String>,
    pub a1: String,
    /// Cookie 里的 `loadts`（页面加载时间，毫秒）。
    pub loadts_ms: i64,
    pub web_build: String,
    pub b1: String,
    /// 用户 id，算 `xy-direction`；不知道时 `None`（用注入的随机值）。
    pub user_id: Option<String>,
    pub rap: bool,
    pub xy: bool,
}

/// Kotlin `Random` 取到的值。**签名函数自己不取随机数**，所以这一组必须由调用方给。
#[derive(Debug, Clone)]
pub struct SignRandom {
    /// `random.nextInt()`：x3 里的 version（低字节参与异或，必须每次不同）。
    pub version: i32,
    /// `x-b3-traceid` 的 16 个十六进制字符。
    pub b3_trace_id: String,
    /// `x-xray-traceid` 尾巴的 16 个十六进制字符。
    pub xray_tail: String,
    /// 没有 user_id 时的 `xy-direction`（Kotlin 是 `10 + random.nextInt(91)`）；有 user_id 时忽略。
    pub xy_random: i32,
    /// `x-rap-param` 的掩码字节（网页从 `0-9a-z` 里挑一个字符的码点）。
    pub rap_mask: i32,
    /// `x-rap-param` 的 16 字节会话密钥（ASCII 字符）。
    pub rap_session_key: String,
    /// `x-rap-param` 的 16 字节随机密钥（ASCII 字符，循环异或用）。
    pub rap_key: String,
    /// 4~6 个 ASCII 字符的盐。
    pub rap_salt: String,
    /// 补到 16 倍数的补位字符（长度必须是 [`rap_pad_len`] 算出来的那个数）。
    pub rap_pad: String,
    /// 信封里的"加密耗时"字段（Kotlin 是 `60 + random.nextInt(181)`）。
    pub rap_enc_time_ms: i32,
}

/// 签名器的会话计数。Kotlin 那边算好初值传进来、每次签名后拿新的值存着（Rust 不存状态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignState {
    pub seq: i32,
    pub sign_count: i32,
    pub dsllt: i64,
    pub xray_seq: i32,
}

impl SignState {
    /// Kotlin `XhsSigner` 构造函数里那三笔：`seq = 12 + nextInt(29)`、`signCount = 5 + nextInt(60)`、
    /// `xraySeq = nextInt(0x7fffff)`。**由调用方按同样的顺序从自己的 `Random` 取好**。
    pub fn new(seq: i32, sign_count: i32, xray_seq: i32) -> Self {
        Self {
            seq,
            sign_count,
            dsllt: 0,
            xray_seq,
        }
    }
}

/// dsllt（`ds` 脚本加载时间）网页每 15 分钟刷新一次。
pub const DSL_REFRESH_MS: i64 = 15 * 60 * 1000;

/// 签一个请求。[`now_ms`] 是当前时间（毫秒），由调用方给；返回要加的请求头（**保持插入顺序**，名字小写）和新状态。
pub fn sign(
    profile: &Profile,
    req: &SignRequest,
    now_ms: i64,
    rand: &SignRandom,
    state: SignState,
) -> Result<(Vec<(String, String)>, SignState), String> {
    let content = format!("{}{}", req.api, req.body.as_deref().unwrap_or(""));
    let md5_full = md5(content.as_bytes());
    let seq = state.seq.wrapping_add(1);
    let plain = mns::pack(
        rand.version,
        now_ms,
        req.loadts_ms,
        seq,
        profile.mns_env_const,
        &content,
        &req.api,
        &req.a1,
        &profile.app_id,
        &profile.mns_env_fp_tail,
    )?;
    let x3 = mns::x3(&plain, &profile.mns_keystream)?;
    let xs = mns::xs(
        &profile.sign_version,
        &profile.app_id,
        &profile.platform,
        &x3,
        if req.body.is_none() { "" } else { "object" },
        &codec::hex(&md5_full),
    );
    let dsllt = if state.dsllt == 0 || now_ms - state.dsllt >= DSL_REFRESH_MS {
        now_ms
    } else {
        state.dsllt
    };
    let sign_count = state.sign_count.wrapping_add(1);
    let common = xs_common(profile, req, &format!("{dsllt};{}", profile.dsl), sign_count);

    let mut headers: Vec<(String, String)> = Vec::with_capacity(7);
    if req.xy {
        headers.push(("xy-direction".to_string(), xy_direction(req, rand).to_string()));
    }
    let xray_seq = (state.xray_seq.wrapping_add(1)) & 0x7f_ffff;
    headers.push(("x-xray-traceid".to_string(), xray_trace_id(now_ms, xray_seq, &rand.xray_tail)));
    headers.push(("x-t".to_string(), now_ms.to_string()));
    headers.push(("x-b3-traceid".to_string(), rand.b3_trace_id.clone()));
    headers.push(("x-s-common".to_string(), common));
    if req.rap {
        let rap = rap::generate(profile, req, now_ms, rand)?;
        headers.push(("x-rap-param".to_string(), rap));
    }
    headers.push(("x-s".to_string(), xs));

    Ok((
        headers,
        SignState {
            seq,
            sign_count,
            dsllt,
            xray_seq,
        },
    ))
}

/// 这一次 `x-rap-param` 要取几个补位字符（`(16 - gz.size % 16) % 16`）。
///
/// Kotlin 版里补位长度是 gzip 之后才知道的、补位字符又是从同一个 `Random` 里紧挨着取的，所以调用方要先问这一次的长度，
/// 再照那个数取字符、最后调 [`sign`]。这一路是纯计算（不占状态、不写任何东西），重复算一遍 payload+gzip。
pub fn rap_pad_len(
    profile: &Profile,
    req: &SignRequest,
    now_ms: i64,
    rap_mask: i32,
    rap_session_key: &str,
) -> Result<usize, String> {
    let gz = rap::gzip_part(profile, req, now_ms, rap_mask, rap_session_key)?;
    Ok((16 - gz.len() % 16) % 16)
}

/// 分片键：`murmur3(user_id, 151488) % 100 + 1`；不知道用户 id 时用注入的随机值。
fn xy_direction(req: &SignRequest, rand: &SignRandom) -> i32 {
    // Kotlin 里是 `xyDirection(req.userId ?: req.a1)`，而 `xyDirection` 只在"种子是空串"时才取随机数
    let seed: &str = match req.user_id.as_deref() {
        Some(id) => id,
        None => &req.a1,
    };
    if seed.is_empty() {
        return rand.xy_random;
    }
    (((codec::murmur3(seed, 151488) as u32) as u64) % 100) as i32 + 1
}

/// `hex16((毫秒时间戳 << 23) | 序号) + 16 位随机十六进制`，追踪用，不参与校验。
fn xray_trace_id(now_ms: i64, xray_seq: i32, tail: &str) -> String {
    let head = (now_ms << 23) | (xray_seq as i64);
    format!("{head:016x}{tail}")
}

/// X-S-Common：字段顺序和网页一致，JSON 紧凑。
pub fn xs_common(profile: &Profile, req: &SignRequest, x12: &str, x10: i32) -> String {
    let json = format!(
        "{{\"s0\":5,\"s1\":\"\",\"x0\":\"1\",\"x1\":{},\"x2\":{},\"x3\":{},\"x4\":{},\"x5\":{},\
\"x6\":\"\",\"x7\":\"\",\"x8\":{},\"x9\":{},\"x10\":{},\"x11\":\"normal\",\"x12\":{}}}",
        json_string(&profile.sign_version),
        json_string(&profile.platform),
        json_string(&profile.app_id),
        json_string(&req.web_build),
        json_string(&req.a1),
        json_string(&req.b1),
        codec::crc32_variant(&req.b1),
        x10,
        json_string(x12),
    );
    base64(json.as_bytes(), XS_ALPHABET)
}

/// 页码里的 `x-rap-param`：`XhsRap` 的移植。
pub mod rap {
    use super::*;

    /// 请求哈希的前缀：浏览器对 edith 接口用协议相对地址。
    pub const API_PREFIX: &str = "//edith.xiaohongshu.com";

    /// 明文：[`template`] 里写入会话密钥和请求哈希，前面接 16 字节明文头，头之后的部分整体异或 `mask`。
    pub fn payload(
        template: &[u8],
        ts_ms: i64,
        request_hash: u32,
        mask: u8,
        session_key: &[u8],
    ) -> Result<Vec<u8>, String> {
        if template.len() < 28 {
            return Err("模板太短".to_string());
        }
        if session_key.len() != 16 {
            return Err("会话密钥要 16 字节".to_string());
        }
        if !(template[0] == 0x03
            && template[1] == 0xEA
            && template[5] == 0x10
            && template[22] == 0x03
            && template[23] == 0xEB)
        {
            return Err("模板开头不是 03ea(16 字节) + 03eb".to_string());
        }
        let mut body = template.to_vec();
        body[6..22].copy_from_slice(session_key);
        put_u32_be(&mut body, 24, request_hash);
        let mut out = vec![0u8; 16 + body.len()];
        out[0] = 0x03;
        out[1] = 0xE8;
        for i in 0..8 {
            out[2 + i] = (ts_ms >> (56 - 8 * i)) as u8;
        }
        out[10] = 0x03;
        out[11] = 0xE9;
        put_u32_be(&mut out, 12, codec::xxh32(&[mask], 0));
        for (i, b) in body.iter().enumerate() {
            out[16 + i] = b ^ mask;
        }
        Ok(out)
    }

    /// 生产路径算出来的 gzip 之后的那一段（[`payload`] + [`deflate::gzip`]）。
    pub(crate) fn gzip_part(
        profile: &Profile,
        req: &SignRequest,
        now_ms: i64,
        rap_mask: i32,
        session_key: &str,
    ) -> Result<Vec<u8>, String> {
        let hash = codec::xxh32(
            format!("{}{}{}", API_PREFIX, req.api, req.body.as_deref().unwrap_or("")).as_bytes(),
            0,
        );
        let plain = payload(
            &profile.rap_template,
            now_ms,
            hash,
            (rap_mask & 0xFF) as u8,
            session_key.as_bytes(),
        )?;
        deflate::gzip(&plain, (now_ms / 1000) as u32, 6)
    }

    /// 一个请求的 `x-rap-param`。
    pub fn generate(
        profile: &Profile,
        req: &SignRequest,
        now_ms: i64,
        rand: &SignRandom,
    ) -> Result<String, String> {
        let gz = gzip_part(profile, req, now_ms, rand.rap_mask, &rand.rap_session_key)?;
        let pad_len = (16 - gz.len() % 16) % 16;
        if rand.rap_pad.len() != pad_len {
            return Err(format!(
                "补位字符数不对：要 {pad_len} 个，给了 {} 个",
                rand.rap_pad.len()
            ));
        }
        pack(
            &gz,
            rand.rap_key.as_bytes(),
            rand.rap_salt.as_bytes(),
            Some(rand.rap_pad.as_bytes()),
            rand.rap_enc_time_ms,
            profile.rap_version,
        )
    }

    /// 信封。[`gz`] 是 gzip 之后的明文，[`key`] 是 16 字节随机密钥（循环异或用，也加密后放进信封），
    /// [`salt`] 是 4~6 个 ASCII 字符，[`pad`] 是补到 16 倍数的字节（在异或之前补，网页补随机字符）。
    pub fn pack(
        gz: &[u8],
        key: &[u8],
        salt: &[u8],
        pad: Option<&[u8]>,
        enc_time_ms: i32,
        version: i32,
    ) -> Result<String, String> {
        if key.len() != 16 {
            return Err("密钥要 16 字节".to_string());
        }
        if salt.len() > 255 {
            return Err("盐太长".to_string());
        }
        let aes = XhsAes::rap();
        let pad_len = (16 - gz.len() % 16) % 16;
        let mut padded = gz.to_vec();
        if let Some(p) = pad {
            if p.len() < pad_len {
                return Err(format!("补位不够：要 {pad_len} 字节，只有 {}", p.len()));
            }
            padded.extend_from_slice(&p[..pad_len]);
        }
        let xored: Vec<u8> = padded
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ key[i % 16])
            .collect();
        let mut cipher_body = aes.encrypt_ecb(&xored, None)?;
        cipher_body.extend_from_slice(&u32_be(gz.len() as u32));
        let mut content = Vec::with_capacity(salt.len() + 16 + 4 + cipher_body.len());
        content.extend_from_slice(salt);
        content.extend_from_slice(&aes.encrypt_block(key)?);
        content.extend_from_slice(&u32_be(16));
        content.extend_from_slice(&cipher_body);

        let mut header = Vec::with_capacity(36 + content.len());
        header.extend_from_slice(&[0x07, 0x24, 0x01, salt.len() as u8]);
        header.extend_from_slice(&u32_be(1));
        header.extend_from_slice(&u32_be(20));
        header.extend_from_slice(&u32_be(cipher_body.len() as u32));
        header.extend_from_slice(&u32_be(codec::xxh32(&content, 0)));
        header.extend_from_slice(&u32_be(version as u32));
        header.extend_from_slice(&u32_be(enc_time_ms as u32));
        header.extend_from_slice(&[0u8; 8]);
        header.extend_from_slice(&content);
        Ok(b64_standard(&header))
    }

    fn u32_be(v: u32) -> [u8; 4] {
        [(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8]
    }

    fn put_u32_be(b: &mut [u8], o: usize, v: u32) {
        for i in 0..4 {
            b[o + i] = (v >> (24 - 8 * i)) as u8;
        }
    }

    /// 标准字母表的 Base64（和 `java.util.Base64.getEncoder()` 一样，带 `=` 补位）。
    pub fn b64_standard(bytes: &[u8]) -> String {
        base64(bytes, "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/")
    }

    /// 标准 Base64 解码（`x-rap-param` 的自检用）。
    pub fn b64_standard_decode(text: &str) -> Result<Vec<u8>, String> {
        const A: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        codec::base64_decode(text, A)
    }
}

/// x3：`mns0301_` 这一档，稳态业务请求都用它。
pub mod mns {
    use super::*;

    pub const PREFIX: &str = "mns0301_";

    /// 144 字节明文（a1 固定 52 个字符时）：
    /// ```text
    /// 4   魔数 79 68 60 29
    /// 4   version（每次随机的 u32，低字节 v 用来异或后面几段）
    /// 8   当前毫秒时间戳（小端）
    /// 8   页面加载时间 loadts（小端）
    /// 4   请求序号 seq（小端）
    /// 4   envConst（小端）
    /// 4   内容长度（UTF-8 字节数，小端）
    /// 8   MD5(内容) 的前 8 字节 ⊕ v
    /// 1+n a1 的长度和内容
    /// 1+n appId 的长度和内容
    /// 16  环境位：01, v⊕115, 然后 14 字节 envFpTail
    /// 3   设备标签 "a3"：长度 2, 'a', '3'
    /// 17  长度 16, 然后 16 字节 dsf(时间戳小端 8 字节 ++ MD5(api)) ⊕ v
    /// ```
    /// 内容 = api + 请求体；设备签名对 api（GET 带查询串）而不是内容取 MD5。
    #[allow(clippy::too_many_arguments)]
    pub fn pack(
        version: i32,
        ts_ms: i64,
        loadts_ms: i64,
        seq: i32,
        env_const: i32,
        content: &str,
        api: &str,
        a1: &str,
        app_id: &str,
        env_fp_tail: &[u8],
    ) -> Result<Vec<u8>, String> {
        if env_fp_tail.len() != 14 {
            return Err(format!("envFpTail 要 14 字节，实际 {}", env_fp_tail.len()));
        }
        let v = (version & 0xFF) as u8;
        let content_bytes = content.as_bytes();
        let md5_full = md5(content_bytes);
        let md5_api = md5(api.as_bytes());
        let ts_le = le(ts_ms, 8);
        let mut out = Vec::with_capacity(144);
        out.extend_from_slice(&[121, 104, 96, 41]);
        out.extend_from_slice(&le(version as u32 as i64, 4));
        out.extend_from_slice(&ts_le);
        out.extend_from_slice(&le(loadts_ms, 8));
        out.extend_from_slice(&le(seq as u32 as i64, 4));
        out.extend_from_slice(&le(env_const as u32 as i64, 4));
        out.extend_from_slice(&le(content_bytes.len() as i64, 4));
        for b in md5_full.iter().take(8) {
            out.push(b ^ v);
        }
        let a1_bytes = a1.as_bytes();
        if a1_bytes.len() > 255 {
            return Err(format!("a1 超过 255 字节：{}", a1_bytes.len()));
        }
        out.push(a1_bytes.len() as u8);
        out.extend_from_slice(a1_bytes);
        let app_bytes = app_id.as_bytes();
        if app_bytes.len() > 255 {
            return Err(format!("appId 超过 255 字节：{}", app_bytes.len()));
        }
        out.push(app_bytes.len() as u8);
        out.extend_from_slice(app_bytes);
        out.push(1);
        out.push(v ^ 115);
        out.extend_from_slice(env_fp_tail);
        out.extend_from_slice(&[2, b'a', b'3']);
        out.push(16);
        let mut dsf_in = ts_le.to_vec();
        dsf_in.extend_from_slice(&md5_api);
        let hash = codec::dsf(&dsf_in)?;
        for b in hash.iter() {
            out.push(b ^ v);
        }
        Ok(out)
    }

    pub fn x3(plain: &[u8], keystream: &[u8]) -> Result<String, String> {
        if keystream.len() < plain.len() {
            return Err(format!("密钥流比明文短：{} < {}", keystream.len(), plain.len()));
        }
        let cipher: Vec<u8> = plain.iter().zip(keystream).map(|(a, b)| a ^ b).collect();
        Ok(format!("{PREFIX}{}", base64(&cipher, X3_ALPHABET)))
    }

    /// X-s 外壳。x6 / x7 不带（只在页面拿到 localStorage 里的 `webSsk` 之后才有，我们没有）。
    pub fn xs(
        sign_version: &str,
        app_id: &str,
        platform: &str,
        x3: &str,
        x4: &str,
        x5: &str,
    ) -> String {
        let json = format!(
            "{{\"x0\":{},\"x1\":{},\"x2\":{},\"x3\":{},\"x4\":{},\"x5\":{}}}",
            json_string(sign_version),
            json_string(app_id),
            json_string(platform),
            json_string(x3),
            json_string(x4),
            json_string(x5),
        );
        format!("XYS_{}", base64(json.as_bytes(), XS_ALPHABET))
    }

    fn le(v: i64, n: usize) -> Vec<u8> {
        (0..n).map(|i| (v >> (8 * i)) as u8).collect()
    }
}

/// b1：X-S-Common 的 x8。19 个浏览器环境字段按固定顺序拼成紧凑 JSON，用 RC4（密钥 `xhswebmplfbt`）加密，
/// 密文当 Latin-1 字符串再按 UTF-8 编码成字节，最后自定义 Base64。
/// 字段值来自 sign_profile.json，只有 x36（帧数）、x44（生成时间）、x84（含 timeOrigin）是现算的。
pub mod b1 {
    use super::*;

    const RC4_KEY: &[u8] = b"xhswebmplfbt";

    /// 19 个字段，按网页的顺序。
    pub fn fields(profile: &Profile, now_ms: i64, loadts_ms: i64) -> Vec<(String, String)> {
        let time_origin = plain_decimal(loadts_ms as f64 + profile.b1_time_origin_offset_ms);
        let x82: String = profile.b1_x82.chars().take(300).collect();
        vec![
            ("x33".into(), "0".into()),
            ("x34".into(), "0".into()),
            ("x35".into(), "0".into()),
            ("x36".into(), profile.b1_frame_count.to_string()),
            ("x37".into(), profile.b1_x37.clone()),
            ("x38".into(), profile.b1_x38.clone()),
            ("x39".into(), profile.b1_x39.clone()),
            ("x42".into(), profile.b1_sdk_version.clone()),
            ("x43".into(), profile.b1_x43.clone()),
            ("x44".into(), now_ms.to_string()),
            ("x45".into(), profile.b1_x45.clone()),
            ("x46".into(), profile.b1_x46.clone()),
            ("x48".into(), profile.b1_x48.clone()),
            ("x49".into(), profile.b1_x49.clone()),
            ("x50".into(), profile.b1_x50.clone()),
            ("x51".into(), profile.b1_x51.clone()),
            ("x52".into(), profile.b1_x52.clone()),
            ("x82".into(), x82),
            (
                "x84".into(),
                profile.b1_telemetry_template.replace("__TIME_ORIGIN__", &time_origin),
            ),
        ]
    }

    pub fn encode(profile: &Profile, now_ms: i64, loadts_ms: i64) -> String {
        let json = fields(profile, now_ms, loadts_ms)
            .iter()
            .map(|(k, v)| format!("{}:{}", json_string(k), json_string(v)))
            .collect::<Vec<_>>()
            .join(",");
        let json = format!("{{{json}}}");
        let cipher = codec::rc4(json.as_bytes(), RC4_KEY);
        // 网页对字符串逐字符异或，得到码点 0..255 的字符串，再按 UTF-8 转成字节：>= 0x80 的字节变成两个字节
        let latin: String = cipher.iter().map(|&b| b as char).collect();
        base64(latin.as_bytes(), XS_ALPHABET)
    }

    /// 解开 b1 看字段（测试用）。
    pub fn decode(text: &str) -> Result<String, String> {
        let utf8 = codec::base64_decode(text, XS_ALPHABET)?;
        let s = String::from_utf8(utf8).map_err(|e| format!("b1 不是合法 UTF-8：{e}"))?;
        let mut latin = Vec::with_capacity(s.len());
        for c in s.chars() {
            let code = c as u32;
            if code > 0xFF {
                return Err(format!("b1 里出现超过 0xFF 的字符：{c}"));
            }
            latin.push(code as u8);
        }
        let plain = codec::rc4(&latin, RC4_KEY);
        String::from_utf8(plain).map_err(|e| format!("b1 解出来不是合法 UTF-8：{e}"))
    }

    /// `BigDecimal.valueOf(double).toPlainString()` 的效果：最短往返十进制、不用科学计数法。
    fn plain_decimal(d: f64) -> String {
        format!("{d}")
    }
}
