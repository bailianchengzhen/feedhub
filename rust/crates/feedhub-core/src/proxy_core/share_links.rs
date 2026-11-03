//! 分享链接 → sing-box 出站（`core/proxy/ShareLinks.kt` 的 Rust 版）。
//!
//! 认：`vless://`（含 reality / xtls-rprx-vision / ws / grpc / h2 / httpupgrade）、`vmess://`、
//! `trojan://`、`ss://`（SIP002 和老格式）、`hysteria2://` / `hy2://`、`tuic://`。
//!
//! 出站字段一律按 **sing-box 1.14.3** 的 schema 写。
//! 解析不了的条目**跳过并说明原因**，原因里不回显原文（原文可能有密码 / uuid）。
//!
//! **顺序也是口径**：`JsonObject` 在 Kotlin 是 `LinkedHashMap`，出站的键顺序会原样进
//! `sing-box-config.json`；这里用 `Vec<(String, V)>` 逐个 `push`，键的先后和 Kotlin 的 `put` 顺序一样。

use crate::proxy_core::json_text::V;
use crate::proxy_core::link_text::{self, LinkParts};
use crate::proxy_core::model::{node_id_of, without_tag, ProxyNode};
use crate::proxy_core::subscription::{j_int, j_text};

/// 一条分享链接的解析结果。
#[derive(Debug, Clone, PartialEq)]
pub enum ShareLinkResult {
    Ok(Box<ProxyNode>),
    Skip(String),
}

/// 认识哪几种分享链接（别的交给调用方报"不支持的协议"）。
pub const SHARE_LINK_SCHEMES: [&str; 7] = ["vless", "vmess", "trojan", "ss", "hysteria2", "hy2", "tuic"];

/// 解析一条分享链接。
pub fn parse_share_link(raw: &str, subscription_id: Option<&str>) -> ShareLinkResult {
    let text = crate::hosted::kotlin_trim(raw);
    let scheme = text
        .find("://")
        .map(|i| text[..i].to_lowercase())
        .unwrap_or_default();
    if !text.contains("://") {
        return ShareLinkResult::Skip("不是链接（没有「://」）".to_string());
    }
    if !SHARE_LINK_SCHEMES.contains(&scheme.as_str()) {
        return ShareLinkResult::Skip(format!("不支持的协议：{scheme}"));
    }
    let result = match scheme.as_str() {
        "vmess" => parse_vmess(text, subscription_id),
        "ss" => parse_shadowsocks(text, subscription_id),
        _ => parse_url_style(text, &scheme, subscription_id),
    };
    result
}

fn parse_url_style(text: &str, scheme: &str, subscription_id: Option<&str>) -> ShareLinkResult {
    let Some(link) = link_text::parse_link(text) else {
        return ShareLinkResult::Skip("链接格式不完整（缺主机或端口）".to_string());
    };
    let name = if crate::hosted::is_kotlin_blank(&link.fragment) {
        format!("{}:{}", link.host, link.port)
    } else {
        link.fragment.clone()
    };
    let tls = tls_of_query(&link);
    let transport = transport_of_query(&link);
    let outbound = match scheme {
        "vless" => {
            if crate::hosted::is_kotlin_blank(&link.user_info) {
                return ShareLinkResult::Skip("vless 链接里没有 uuid".to_string());
            }
            let mut fields = vec![
                ("type".to_string(), V::Str("vless".into())),
                ("server".to_string(), V::Str(link.host.clone())),
                ("server_port".to_string(), V::Int(link.port as i64)),
                ("uuid".to_string(), V::Str(link.user_info.clone())),
            ];
            if let Some(flow) = link.str_param(&["flow"]) {
                if !crate::hosted::is_kotlin_blank(&flow) {
                    fields.push(("flow".into(), V::Str(flow)));
                }
            }
            if let Some(tls) = tls.clone() {
                fields.push(("tls".into(), tls));
            }
            if let Some(transport) = transport.clone() {
                fields.push(("transport".into(), transport));
            }
            if let Some(enc) = link.str_param(&["packetEncoding", "packet-encoding"]) {
                fields.push(("packet_encoding".into(), V::Str(enc)));
            }
            V::Obj(fields)
        }
        "trojan" => {
            if crate::hosted::is_kotlin_blank(&link.user_info) {
                return ShareLinkResult::Skip("trojan 链接里没有密码".to_string());
            }
            let mut fields = vec![
                ("type".to_string(), V::Str("trojan".into())),
                ("server".to_string(), V::Str(link.host.clone())),
                ("server_port".to_string(), V::Int(link.port as i64)),
                ("password".to_string(), V::Str(link.user_info.clone())),
                // trojan 本身就是 TLS，链接里即使没写 security 也要开
                (
                    "tls".to_string(),
                    tls.clone().unwrap_or_else(tls_enabled_only),
                ),
            ];
            if let Some(transport) = transport.clone() {
                fields.push(("transport".into(), transport));
            }
            V::Obj(fields)
        }
        "hysteria2" | "hy2" => {
            if crate::hosted::is_kotlin_blank(&link.user_info) {
                return ShareLinkResult::Skip("hysteria2 链接里没有密码".to_string());
            }
            let mut fields = vec![
                ("type".to_string(), V::Str("hysteria2".into())),
                ("server".to_string(), V::Str(link.host.clone())),
                ("server_port".to_string(), V::Int(link.port as i64)),
                ("password".to_string(), V::Str(link.user_info.clone())),
                (
                    "tls".to_string(),
                    tls.clone().unwrap_or_else(tls_enabled_only),
                ),
            ];
            if let Some(obfs) = link.str_param(&["obfs"]) {
                let mut obfs_fields =
                    vec![("type".to_string(), V::Str(obfs.clone()))];
                if let Some(pw) = link.str_param(&["obfs-password", "obfs_password"]) {
                    obfs_fields.push(("password".into(), V::Str(pw)));
                }
                fields.push(("obfs".into(), V::Obj(obfs_fields)));
            }
            V::Obj(fields)
        }
        "tuic" => {
            let Some((uuid, password)) = split_user_info(&link.user_info) else {
                return ShareLinkResult::Skip("tuic 链接里没有 uuid:密码".to_string());
            };
            let mut fields = vec![
                ("type".to_string(), V::Str("tuic".into())),
                ("server".to_string(), V::Str(link.host.clone())),
                ("server_port".to_string(), V::Int(link.port as i64)),
                ("uuid".to_string(), V::Str(uuid)),
                ("password".to_string(), V::Str(password)),
                (
                    "tls".to_string(),
                    tls.clone().unwrap_or_else(tls_enabled_only),
                ),
            ];
            if let Some(cc) = link.str_param(&["congestion_control", "congestion-controller"]) {
                fields.push(("congestion_control".into(), V::Str(cc)));
            }
            if let Some(mode) = link.str_param(&["udp_relay_mode", "udp-relay-mode"]) {
                fields.push(("udp_relay_mode".into(), V::Str(mode)));
            }
            V::Obj(fields)
        }
        _ => return ShareLinkResult::Skip(format!("不支持的协议：{scheme}")),
    };
    match outbound_to_node(&outbound, Some(&name), subscription_id) {
        Some(node) => ShareLinkResult::Ok(Box::new(node)),
        None => ShareLinkResult::Skip("缺少服务器地址".to_string()),
    }
}

/// vless / trojan / hy2 的 userInfo 就是 uuid / 密码；tuic 是 `uuid:password`。
fn split_user_info(user_info: &str) -> Option<(String, String)> {
    let colon = user_info.find(':')?;
    if colon == 0 || colon == user_info.len() - 1 {
        return None;
    }
    Some((
        user_info[..colon].to_string(),
        user_info[colon + 1..].to_string(),
    ))
}

/// `vmess://` 的内容是一段 base64 的 JSON（v2ray 的格式），字段名很短：
/// `v` 版本、`ps` 备注、`add` 地址、`port` 端口、`id` uuid、`aid` alterId、`scy` 加密、
/// `net` 传输、`type` 伪装类型、`host` / `path`、`tls`、`sni`、`alpn`、`fp`。
fn parse_vmess(text: &str, subscription_id: Option<&str>) -> ShareLinkResult {
    let after = text.split_once("://").map(|(_, r)| r).unwrap_or("");
    let payload = after.split('#').next().unwrap_or(after);
    let Some(decoded) = link_text::base64_decode(payload) else {
        return ShareLinkResult::Skip("vmess 的内容不是合法 base64".to_string());
    };
    let Ok(root) = crate::xhs::jsonord::parse(&decoded) else {
        return ShareLinkResult::Skip("vmess 解出来不是 JSON 对象".to_string());
    };
    let json = crate::proxy_core::model::v_from_j(&root);
    let Some(obj) = super::subscription::as_obj(&json) else {
        return ShareLinkResult::Skip("vmess 解出来不是 JSON 对象".to_string());
    };
    let get = |k: &str| obj.iter().find(|(name, _)| name == k).map(|(_, v)| v);
    let Some(server) = j_text(get("add")) else {
        return ShareLinkResult::Skip("vmess 里没有服务器地址".to_string());
    };
    let Some(port) = j_int(get("port")) else {
        return ShareLinkResult::Skip("vmess 里没有端口".to_string());
    };
    let Some(uuid) = j_text(get("id")) else {
        return ShareLinkResult::Skip("vmess 里没有 uuid".to_string());
    };
    let name = j_text(get("ps")).unwrap_or_else(|| format!("{server}:{port}"));
    let net = j_text(get("net"))
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "tcp".to_string());
    let header_type = j_text(get("type"));
    let host = j_text(get("host"));
    let path = j_text(get("path"));
    let transport = transport_of(
        Some(&net),
        header_type.as_deref(),
        host.as_deref(),
        path.as_deref(),
        path.as_deref(),
    );
    let tls = if j_text(get("tls")).map(|s| s.to_lowercase()).as_deref() == Some("tls") {
        let sni = j_text(get("sni")).filter(|s| s != "sni").or_else(|| host.clone());
        tls_of(
            true,
            sni.as_deref(),
            j_text(get("alpn")).as_deref(),
            j_text(get("fp")).as_deref(),
            false,
            None,
            None,
            None,
        )
    } else {
        None
    };
    let mut fields = vec![
        ("type".to_string(), V::Str("vmess".into())),
        ("server".to_string(), V::Str(server)),
        ("server_port".to_string(), V::Int(port as i64)),
        ("uuid".to_string(), V::Str(uuid)),
        (
            "security".to_string(),
            V::Str(
                j_text(get("scy"))
                    .filter(|s| !crate::hosted::is_kotlin_blank(s))
                    .unwrap_or_else(|| "auto".to_string()),
            ),
        ),
        (
            "alter_id".to_string(),
            V::Int(j_int(get("aid")).unwrap_or(0) as i64),
        ),
    ];
    if let Some(tls) = tls {
        fields.push(("tls".into(), tls));
    }
    if let Some(transport) = transport {
        fields.push(("transport".into(), transport));
    }
    let outbound = V::Obj(fields);
    match outbound_to_node(&outbound, Some(&name), subscription_id) {
        Some(node) => ShareLinkResult::Ok(Box::new(node)),
        None => ShareLinkResult::Skip("缺少服务器地址".to_string()),
    }
}

/// `ss://` 两种格式：
/// - SIP002：`ss://base64(method:password)@host:port?plugin=…#备注`（userinfo 也可以是明文的 `method:password`）；
/// - 老格式：`ss://base64(method:password@host:port)#备注`（整个都是 base64，没有 `@`）。
fn parse_shadowsocks(text: &str, subscription_id: Option<&str>) -> ShareLinkResult {
    let body = text.split_once("://").map(|(_, r)| r).unwrap_or("");
    let without_fragment = body.split('#').next().unwrap_or(body);
    let query_text = without_fragment
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or("");
    let authority = without_fragment
        .split_once('?')
        .map(|(a, _)| a)
        .unwrap_or(without_fragment);

    let (user_info, host_port);
    if let Some(at) = authority.rfind('@') {
        user_info = authority[..at].to_string();
        host_port = authority[at + 1..].to_string();
    } else {
        // 老格式：整段是 base64（method:password@host:port）
        let Some(decoded) = link_text::base64_decode(authority) else {
            return ShareLinkResult::Skip("ss 链接不是 SIP002，也不是合法 base64".to_string());
        };
        let Some(at) = decoded.rfind('@') else {
            return ShareLinkResult::Skip("ss 老格式里没有「@」".to_string());
        };
        user_info = decoded[..at].to_string();
        host_port = decoded[at + 1..].to_string();
    }
    let (host, port_text) = match host_port.rfind(':') {
        Some(colon) => (host_port[..colon].to_string(), host_port[colon + 1..].to_string()),
        None => (String::new(), String::new()),
    };
    if crate::hosted::is_kotlin_blank(&host) {
        return ShareLinkResult::Skip("ss 链接里没有服务器地址".to_string());
    }
    let Some(port) = port_text.parse::<i32>().ok() else {
        return ShareLinkResult::Skip("ss 链接里没有端口".to_string());
    };

    // 明文写法里密码可能是 percent-encoded（`pass%40word`）；base64 写法先解码再拆
    let (method, password);
    if user_info.contains(':') {
        let (m, p) = user_info.split_once(':').unwrap_or((&user_info, ""));
        method = link_text::percent_decode(m);
        password = link_text::percent_decode(p);
    } else {
        let Some(decoded) = link_text::base64_decode(&user_info) else {
            return ShareLinkResult::Skip("ss 的用户名密码不是合法 base64".to_string());
        };
        let (m, p) = decoded.split_once(':').unwrap_or((&decoded, ""));
        method = m.to_string();
        password = p.to_string();
    }
    if crate::hosted::is_kotlin_blank(&method) {
        return ShareLinkResult::Skip("ss 链接里没有加密方式".to_string());
    }

    let fragment = text.split_once('#').map(|(_, f)| f).unwrap_or("");
    let name = if crate::hosted::is_kotlin_blank(fragment) {
        format!("{host}:{port}")
    } else {
        link_text::percent_decode(fragment)
    };
    let mut plugin: Option<String> = None;
    for part in query_text.split('&') {
        if part.trim().is_empty() {
            continue;
        }
        let (k, v) = match part.find('=') {
            Some(eq) => (&part[..eq], link_text::percent_decode(&part[eq + 1..])),
            None => (part, String::new()),
        };
        if link_text::percent_decode(k) == "plugin" {
            plugin = Some(v);
        }
    }

    let mut fields = vec![
        ("type".to_string(), V::Str("shadowsocks".into())),
        ("server".to_string(), V::Str(host)),
        ("server_port".to_string(), V::Int(port as i64)),
        ("method".to_string(), V::Str(method)),
        ("password".to_string(), V::Str(password)),
    ];
    if let Some(plugin) = plugin.filter(|p| !crate::hosted::is_kotlin_blank(p)) {
        // plugin 的写法是 `名字;选项=值;选项=值`，sing-box 把选项原样交给插件
        let plugin_name = plugin.split(';').next().unwrap_or("").to_string();
        let opts = plugin.split_once(';').map(|(_, o)| o).unwrap_or("").to_string();
        fields.push(("plugin".into(), V::Str(plugin_name)));
        if !crate::hosted::is_kotlin_blank(&opts) {
            fields.push(("plugin_opts".into(), V::Str(opts)));
        }
    }
    let outbound = V::Obj(fields);
    match outbound_to_node(&outbound, Some(&name), subscription_id) {
        Some(node) => ShareLinkResult::Ok(Box::new(node)),
        None => ShareLinkResult::Skip("缺少服务器地址".to_string()),
    }
}

// ---------------------------------------------------------------------------
// 出站公共部件：TLS / 传输
// ---------------------------------------------------------------------------

fn tls_enabled_only() -> V {
    V::Obj(vec![("enabled".to_string(), V::Bool(true))])
}

/// 链接查询参数 → sing-box 的 `tls`。没有任何 TLS 迹象时返回 `None`。
fn tls_of_query(link: &LinkParts) -> Option<V> {
    let security = link.str_param(&["security"]).map(|s| s.to_lowercase());
    let pbk = link.str_param(&["pbk", "public-key", "public_key"]);
    let server_name = link.str_param(&["sni", "peer", "serverName"]);
    let alpn = link.str_param(&["alpn"]);
    let fingerprint = link.str_param(&["fp", "fingerprint"]);
    let insecure = link.bool_param(&["allowInsecure", "insecure", "skip-cert-verify"]);
    // 只要有 TLS 的迹象就把 tls 段写出来（trojan / hy2 / tuic 本来就走 TLS，链接里常常只写 sni）
    let enabled = matches!(security.as_deref(), Some("tls") | Some("reality") | Some("xtls"))
        || pbk.is_some()
        || server_name.is_some()
        || alpn.is_some()
        || insecure
        || fingerprint.is_some();
    tls_of(
        enabled,
        server_name.as_deref(),
        alpn.as_deref(),
        fingerprint.as_deref(),
        insecure,
        pbk.as_deref(),
        link.str_param(&["sid", "shortId", "short_id"]).as_deref(),
        link.str_param(&["pinSHA256", "pinnedPeerCertificateChainSha256"])
            .as_deref(),
    )
}

#[allow(clippy::too_many_arguments)]
fn tls_of(
    enabled: bool,
    server_name: Option<&str>,
    alpn: Option<&str>,
    fingerprint: Option<&str>,
    insecure: bool,
    public_key: Option<&str>,
    short_id: Option<&str>,
    pin_sha256: Option<&str>,
) -> Option<V> {
    if !enabled {
        return None;
    }
    let mut fields = vec![("enabled".to_string(), V::Bool(true))];
    if let Some(name) = server_name.filter(|s| !crate::hosted::is_kotlin_blank(s)) {
        fields.push(("server_name".into(), V::Str(name.to_string())));
    }
    if insecure {
        fields.push(("insecure".into(), V::Bool(true)));
    }
    if let Some(alpn) = alpn {
        let items: Vec<V> = alpn
            .split(',')
            .map(|s| crate::hosted::kotlin_trim(s).to_string())
            .filter(|s| !s.is_empty())
            .map(V::Str)
            .collect();
        if !items.is_empty() {
            fields.push(("alpn".into(), V::Arr(items)));
        }
    }
    if let Some(pin) = pin_sha256.filter(|s| !crate::hosted::is_kotlin_blank(s)) {
        fields.push((
            "certificate_public_key_sha256".into(),
            V::Arr(vec![V::Str(pin.to_string())]),
        ));
    }
    if let Some(public_key) = public_key {
        let mut reality = vec![
            ("enabled".to_string(), V::Bool(true)),
            ("public_key".to_string(), V::Str(public_key.to_string())),
        ];
        if let Some(short_id) = short_id.filter(|s| !crate::hosted::is_kotlin_blank(s)) {
            reality.push(("short_id".into(), V::Str(short_id.to_string())));
        }
        fields.push(("reality".into(), V::Obj(reality)));
    }
    // reality 一定要带 utls 指纹；给了指纹也带上（伪装成浏览器）
    let fp = fingerprint
        .filter(|s| !crate::hosted::is_kotlin_blank(s))
        .map(String::from)
        .or_else(|| public_key.map(|_| "chrome".to_string()));
    if let Some(fp) = fp {
        fields.push((
            "utls".into(),
            V::Obj(vec![
                ("enabled".to_string(), V::Bool(true)),
                ("fingerprint".to_string(), V::Str(fp)),
            ]),
        ));
    }
    Some(V::Obj(fields))
}

/// 链接查询参数 → sing-box 的 `transport`（tcp 直连时返回 `None`）。
fn transport_of_query(link: &LinkParts) -> Option<V> {
    transport_of(
        link.str_param(&["type", "headerType", "network"]).as_deref(),
        link.str_param(&["headerType"]).as_deref(),
        link.str_param(&["host"]).as_deref(),
        link.str_param(&["path"]).as_deref(),
        link.str_param(&["serviceName"]).as_deref(),
    )
}

/// 传输层：`tcp`（可带 http 伪装头）/ `ws` / `grpc` / `h2` / `httpupgrade` / `quic`。
/// 认不出来就按 tcp（不返回错的传输，免得整个节点起不来）。
fn transport_of(
    net: Option<&str>,
    header_type: Option<&str>,
    host: Option<&str>,
    path: Option<&str>,
    service_name: Option<&str>,
) -> Option<V> {
    let net = net.map(|s| s.to_lowercase());
    match net.as_deref() {
        None | Some("") | Some("tcp") | Some("raw") => {
            if header_type.map(|s| s.to_lowercase()).as_deref() == Some("http") {
                let mut fields = vec![("type".to_string(), V::Str("http".into()))];
                if let Some(host) = host.filter(|s| !crate::hosted::is_kotlin_blank(s)) {
                    fields.push(("host".into(), V::Arr(vec![V::Str(host.to_string())])));
                }
                Some(V::Obj(fields))
            } else {
                None
            }
        }
        Some("ws") | Some("websocket") => {
            let mut fields = vec![
                ("type".to_string(), V::Str("ws".into())),
                (
                    "path".to_string(),
                    V::Str(
                        path.filter(|s| !crate::hosted::is_kotlin_blank(s))
                            .unwrap_or("/")
                            .to_string(),
                    ),
                ),
            ];
            if let Some(host) = host.filter(|s| !crate::hosted::is_kotlin_blank(s)) {
                fields.push((
                    "headers".into(),
                    V::Obj(vec![("Host".to_string(), V::Str(host.to_string()))]),
                ));
            }
            Some(V::Obj(fields))
        }
        Some("grpc") => {
            let service = service_name
                .filter(|s| !crate::hosted::is_kotlin_blank(s))
                .map(String::from)
                .unwrap_or_else(|| path.map(|p| p.trim_start_matches('/').to_string()).unwrap_or_default());
            Some(V::Obj(vec![
                ("type".to_string(), V::Str("grpc".into())),
                ("service_name".to_string(), V::Str(service)),
            ]))
        }
        Some("h2") | Some("http") => {
            let mut fields = vec![
                ("type".to_string(), V::Str("http".into())),
                (
                    "path".to_string(),
                    V::Str(
                        path.filter(|s| !crate::hosted::is_kotlin_blank(s))
                            .unwrap_or("/")
                            .to_string(),
                    ),
                ),
            ];
            if let Some(host) = host.filter(|s| !crate::hosted::is_kotlin_blank(s)) {
                fields.push(("host".into(), V::Arr(vec![V::Str(host.to_string())])));
            }
            Some(V::Obj(fields))
        }
        Some("httpupgrade") => {
            let mut fields = vec![
                ("type".to_string(), V::Str("httpupgrade".into())),
                (
                    "path".to_string(),
                    V::Str(
                        path.filter(|s| !crate::hosted::is_kotlin_blank(s))
                            .unwrap_or("/")
                            .to_string(),
                    ),
                ),
            ];
            if let Some(host) = host.filter(|s| !crate::hosted::is_kotlin_blank(s)) {
                fields.push(("host".into(), V::Str(host.to_string())));
            }
            Some(V::Obj(fields))
        }
        Some("quic") => Some(V::Obj(vec![("type".to_string(), V::Str("quic".into()))])),
        _ => None,
    }
}

/// sing-box 出站 → [`ProxyNode`]（补齐 id、名字、服务器 / 端口）。缺 `server` / `server_port` / `type` 时返回 `None`。
///
/// 订阅里给的可能是 `direct` / `block` / `dns` / `selector` / `urltest` 这类不是真实节点的出站，
/// 调用方按 `type` 先筛掉（[`is_real_node_type`]）。
pub fn outbound_to_node(
    outbound: &V,
    name: Option<&str>,
    subscription_id: Option<&str>,
) -> Option<ProxyNode> {
    let obj = match outbound {
        V::Obj(fields) => fields,
        _ => return None,
    };
    let type_name = crate::proxy_core::subscription::obj_text(obj, "type")?;
    let server = crate::proxy_core::subscription::obj_text(obj, "server")?;
    let port = crate::proxy_core::subscription::obj_int(obj, "server_port")?;
    let clean = without_tag(outbound);
    let display = name
        .filter(|s| !crate::hosted::is_kotlin_blank(s))
        .map(String::from)
        .unwrap_or_else(|| format!("{server}:{port}"));
    Some(ProxyNode {
        id: node_id_of(&type_name, &clean),
        name: display,
        kind: type_name,
        server,
        port,
        outbound: clean,
        subscription_id: subscription_id.map(String::from),
        last_latency_ms: None,
        last_latency_at: None,
    })
}

/// 订阅 `outbounds` 里哪些是真实节点：只把**结构性**的出站排除掉（`direct` / `block` / `dns` /
/// `selector` / `urltest`——它们不是"一台服务器"）。别的类型（socks / http / wireguard / ssh…）原样保留。
pub fn is_real_node_type(type_name: &str) -> bool {
    !matches!(type_name, "direct" | "block" | "dns" | "selector" | "urltest")
}

