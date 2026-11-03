//! Clash / mihomo 订阅（`proxies:` 那一段）的解析（`core/proxy/ClashYaml.kt` 的 Rust 版）。
//!
//! **仍然不引 YAML 库**（Kotlin 那边也是自己写的一个子集，理由同样是"为一个字段子集塞一个完整 YAML
//! 实现不值得"）：只认 Clash 配置里够用的那部分——缩进表示层级、`- ` 表示列表项、`key: value` 标量、
//! `[a, b]` / `{k: v}` 两种行内写法、`#` 注释、单双引号。多行标量（`|` / `>`）不支持（当成空值），
//! 认不出的层级直接跳过；真正不支持的写法会让那条节点被跳过并在界面上说明原因，不会静默出错。
//!
//! 值模型 [`Yaml`] 只有四种：`Null` / `Bool` / `Int`（`Long`）/ `Str` / `List` / `Map`。
//! **映射保持插入顺序**（Kotlin 是 `LinkedHashMap`；`plugin-opts` 拼 `plugin_opts` 时顺序会影响字节）。

use crate::proxy_core::json_text::V;
use crate::proxy_core::share_links::{outbound_to_node, ShareLinkResult};

/// 解析出来的一小片 YAML 值。
#[derive(Debug, Clone, PartialEq)]
pub enum Yaml {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<Yaml>),
    Map(Vec<(String, Yaml)>),
}

impl Yaml {
    fn as_list(&self) -> Vec<Yaml> {
        match self {
            Yaml::List(l) => l.clone(),
            _ => Vec::new(),
        }
    }

    /// `Any?.asMap()`：不是映射就是 `None`。
    fn to_map(self) -> Option<Vec<(String, Yaml)>> {
        match self {
            Yaml::Map(m) => Some(m),
            _ => None,
        }
    }
}

/// 取一段 `proxies:` 列表。没找到 `proxies:` 段返回 `None`（= Kotlin 的 `parseClashProxies` 返回 null）。
pub fn parse_clash_proxies(text: &str) -> Option<Vec<Vec<(String, Yaml)>>> {
    let lines: Vec<String> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .map(strip_comment)
        .collect();
    let head = lines
        .iter()
        .position(|l| l.trim_start_matches([' ', '\t']).starts_with("proxies:"))?;
    let base_indent = indent_of(&lines[head]);
    let inline_value = lines[head]
        .trim_start_matches([' ', '\t'])
        .strip_prefix("proxies:")
        .unwrap_or("")
        .trim();
    if inline_value.starts_with('[') {
        return Some(scalar_of(inline_value).as_list().into_iter().filter_map(Yaml::to_map).collect());
    }

    // `proxies:` 后面缩进更大的行都是它这一段，直到下一个缩进 <= baseIndent 的非空行
    let mut block: Vec<String> = Vec::new();
    for line in lines.iter().skip(head + 1) {
        if !line.trim().is_empty() && indent_of(line) <= base_indent {
            break;
        }
        block.push(line.clone());
    }
    let first_index = block.iter().position(|l| !l.trim().is_empty())?;
    let first_indent = indent_of(&block[first_index]);
    let (value, _) = parse_block(&block, first_index, first_indent)?;
    Some(value.as_list().into_iter().filter_map(Yaml::to_map).collect())
}

/// 去掉行尾注释（引号里的 `#` 不算）。
fn strip_comment(line: &str) -> String {
    let mut in_single = false;
    let mut in_double = false;
    let chars: Vec<char> = line.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        match c {
            '\'' => {
                if !in_double {
                    in_single = !in_single;
                }
            }
            '"' => {
                if !in_single {
                    in_double = !in_double;
                }
            }
            '#' => {
                let prev_ws = i == 0 || chars[i - 1].is_whitespace();
                if !in_single && !in_double && prev_ws {
                    return chars[..i].iter().collect();
                }
            }
            _ => {}
        }
    }
    line.to_string()
}

fn indent_of(line: &str) -> usize {
    line.chars().count() - line.trim_start().chars().count()
}

/// 解析一段缩进块：以 `- ` 开头的是列表，否则是映射。返回 (值, 下一行的下标)。
fn parse_block(lines: &[String], start: usize, _indent: usize) -> Option<(Yaml, usize)> {
    let first = (start..lines.len()).find(|i| !lines[*i].trim().is_empty())?;
    if lines[first].trim_start().starts_with("- ") {
        parse_list(lines, first, indent_of(&lines[first]))
    } else {
        parse_map(lines, first, indent_of(&lines[first]))
    }
}

fn parse_list(lines: &[String], start: usize, indent: usize) -> Option<(Yaml, usize)> {
    let mut items: Vec<Yaml> = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let line = &lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        let ind = indent_of(line);
        if ind != indent || !line.trim_start().starts_with("- ") {
            break;
        }
        let rest = line.trim_start().strip_prefix("- ").unwrap_or("").trim().to_string();
        if rest.is_empty() {
            let child = (i + 1..lines.len()).find(|k| !lines[*k].trim().is_empty());
            match child {
                None => {
                    items.push(Yaml::Null);
                    i += 1;
                }
                Some(child) if indent_of(&lines[child]) <= ind => {
                    items.push(Yaml::Null);
                    i += 1;
                }
                Some(child) => match parse_block(lines, child, indent_of(&lines[child])) {
                    Some((v, next)) => {
                        items.push(v);
                        i = next;
                    }
                    None => {
                        items.push(Yaml::Null);
                        i += 1;
                    }
                },
            }
        } else if rest.starts_with('{') || rest.starts_with('[') {
            items.push(scalar_of(&rest));
            i += 1;
        } else if rest.find(':').map(|c| c > 0).unwrap_or(false) {
            // `- key: value`：这一项是映射，第一对键值在本行，后面的键缩进比 `- ` 深
            let mut map: Vec<(String, Yaml)> = Vec::new();
            i = parse_map_with_first(&mut map, lines, i, ind + 2, &rest);
            items.push(Yaml::Map(map));
        } else {
            items.push(scalar_of(&rest));
            i += 1;
        }
    }
    Some((Yaml::List(items), i))
}

fn parse_map(lines: &[String], start: usize, indent: usize) -> Option<(Yaml, usize)> {
    let mut map: Vec<(String, Yaml)> = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let line = &lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        let ind = indent_of(line);
        if ind < indent {
            break;
        }
        if ind > indent {
            i += 1;
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("- ") {
            break;
        }
        i = parse_map_entry(&mut map, lines, i, indent, trimmed);
    }
    Some((Yaml::Map(map), i))
}

/// 处理"第一个键在这一行上"的映射（列表项 `- key: value`）：先吃掉这一对键值，
/// 再接着吃同一缩进下的后续键（这才是列表项的内容）。
fn parse_map_with_first(
    map: &mut Vec<(String, Yaml)>,
    lines: &[String],
    start: usize,
    key_indent: usize,
    first_text: &str,
) -> usize {
    let mut i = parse_map_entry(map, lines, start, key_indent, first_text);
    while i < lines.len() {
        let line = &lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        let ind = indent_of(line);
        if ind < key_indent {
            break;
        }
        if ind > key_indent {
            i += 1;
            continue;
        }
        let t = line.trim_start();
        if t.starts_with("- ") {
            break;
        }
        i = parse_map_entry(map, lines, i, key_indent, t);
    }
    i
}

/// 处理一条 `key: value`（`text` 是这一行去掉缩进后的内容，`key_indent` 是键所在的缩进）。返回下一行的下标。
fn parse_map_entry(
    map: &mut Vec<(String, Yaml)>,
    lines: &[String],
    index: usize,
    key_indent: usize,
    text: &str,
) -> usize {
    let Some(colon) = text.find(':') else {
        return index + 1;
    };
    let key = text[..colon].trim().to_string();
    let value = text[colon + 1..].trim().to_string();
    let put = |map: &mut Vec<(String, Yaml)>, key: String, value: Yaml| {
        match map.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => map.push((key, value)),
        }
    };
    if !value.is_empty() {
        put(map, key, scalar_of(&value));
        return index + 1;
    }
    let child = (index + 1..lines.len()).find(|k| !lines[*k].trim().is_empty());
    if let Some(child) = child {
        if indent_of(&lines[child]) > key_indent {
            match parse_block(lines, child, indent_of(&lines[child])) {
                Some((v, next)) => {
                    put(map, key, v);
                    return next;
                }
                None => {
                    put(map, key, Yaml::Null);
                    return index + 1;
                }
            }
        }
    }
    put(map, key, Yaml::Null);
    index + 1
}

/// `[a, b]` / `{k: v, k2: v2}` / 标量。
pub fn scalar_of(raw: &str) -> Yaml {
    let v = raw.trim();
    if v.is_empty() || v == "~" || v == "null" {
        return Yaml::Null;
    }
    if v.starts_with('[') && v.ends_with(']') {
        return Yaml::List(
            split_inline(&v[1..v.len() - 1])
                .iter()
                .map(|s| scalar_of(s))
                .collect(),
        );
    }
    if v.starts_with('{') && v.ends_with('}') {
        let mut map: Vec<(String, Yaml)> = Vec::new();
        for part in split_inline(&v[1..v.len() - 1]) {
            let Some(c) = part.find(':') else { continue };
            let key = unquote(part[..c].trim());
            let value = scalar_of(&part[c + 1..]);
            match map.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => map.push((key, value)),
            }
        }
        return Yaml::Map(map);
    }
    let chars: Vec<char> = v.chars().collect();
    if chars.len() >= 2 && chars[0] == '"' && chars[chars.len() - 1] == '"' {
        return Yaml::Str(v[1..v.len() - 1].replace("\\\"", "\""));
    }
    if chars.len() >= 2 && chars[0] == '\'' && chars[chars.len() - 1] == '\'' {
        return Yaml::Str(v[1..v.len() - 1].replace("''", "'"));
    }
    if v.eq_ignore_ascii_case("true") {
        return Yaml::Bool(true);
    }
    if v.eq_ignore_ascii_case("false") {
        return Yaml::Bool(false);
    }
    if let Ok(n) = v.parse::<i64>() {
        return Yaml::Int(n);
    }
    Yaml::Str(v.to_string())
}

fn split_inline(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut sb = String::new();
    let mut depth = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    for c in body.chars() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                sb.push(c);
            }
            '"' if !in_single => {
                in_double = !in_double;
                sb.push(c);
            }
            '[' | '{' => {
                depth += 1;
                sb.push(c);
            }
            ']' | '}' => {
                depth -= 1;
                sb.push(c);
            }
            ',' if depth == 0 && !in_single && !in_double => {
                out.push(sb.trim().to_string());
                sb.clear();
            }
            _ => sb.push(c),
        }
    }
    if !sb.trim().is_empty() {
        out.push(sb.trim().to_string());
    }
    out
}

fn unquote(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 && chars[0] == '"' && chars[chars.len() - 1] == '"' {
        return s[1..s.len() - 1].to_string();
    }
    if chars.len() >= 2 && chars[0] == '\'' && chars[chars.len() - 1] == '\'' {
        return s[1..s.len() - 1].to_string();
    }
    s.to_string()
}

// ---------------------------------------------------------------------------
// 取值小工具（对应 Kotlin 的 str / num / flag / map / list）
// ---------------------------------------------------------------------------

fn get<'a>(node: &'a [(String, Yaml)], key: &str) -> Option<&'a Yaml> {
    node.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// `Map<String, Any?>.str(vararg keys)`：字符串（非空白）或数字的文本。
fn str_of(node: &[(String, Yaml)], keys: &[&str]) -> Option<String> {
    for key in keys {
        match get(node, key) {
            Some(Yaml::Str(s)) if !crate::hosted::is_kotlin_blank(s) => return Some(s.clone()),
            Some(Yaml::Int(n)) => return Some(n.to_string()),
            _ => {}
        }
    }
    None
}

/// `Map<String, Any?>.num(vararg keys)`。
fn num_of(node: &[(String, Yaml)], keys: &[&str]) -> Option<i64> {
    for key in keys {
        match get(node, key) {
            Some(Yaml::Int(n)) => return Some(*n),
            Some(Yaml::Str(s)) => {
                if let Ok(n) = s.trim().parse::<i64>() {
                    return Some(n);
                }
            }
            _ => {}
        }
    }
    None
}

/// `Map<String, Any?>.flag(vararg keys)`。
fn flag_of(node: &[(String, Yaml)], keys: &[&str]) -> bool {
    for key in keys {
        if let Some(Yaml::Bool(b)) = get(node, key) {
            return *b;
        }
    }
    false
}

/// `Map<String, Any?>.map(vararg keys)`。
fn map_of<'a>(node: &'a [(String, Yaml)], keys: &[&str]) -> Option<&'a Vec<(String, Yaml)>> {
    for key in keys {
        if let Some(Yaml::Map(m)) = get(node, key) {
            return Some(m);
        }
    }
    None
}

/// `Map<String, Any?>.list(vararg keys)`：列表里的每个元素转文本（`null` 丢掉）。
fn list_of(node: &[(String, Yaml)], keys: &[&str]) -> Vec<String> {
    for key in keys {
        if let Some(Yaml::List(items)) = get(node, key) {
            return items
                .iter()
                .filter_map(|item| match item {
                    Yaml::Null => None,
                    Yaml::Str(s) => Some(s.clone()),
                    Yaml::Int(n) => Some(n.to_string()),
                    Yaml::Bool(b) => Some(b.to_string()),
                    other => Some(format!("{other:?}")),
                })
                .collect();
        }
    }
    Vec::new()
}

fn json_string_array(values: &[String]) -> V {
    V::Arr(values.iter().map(|s| V::Str(s.clone())).collect())
}

/// 一条 Clash 节点 → sing-box 出站。不认识的类型 / 缺字段返回 [`ShareLinkResult::Skip`]（带原因）。
///
/// 字段名按 mihomo 的文档：`servername` / `sni` / `skip-cert-verify` / `ws-opts` / `grpc-opts` /
/// `reality-opts` / `client-fingerprint` / `congestion-controller` / `udp-relay-mode`。
pub fn clash_node_to_outbound(
    node: &[(String, Yaml)],
    subscription_id: Option<&str>,
) -> ShareLinkResult {
    let Some(type_name) = str_of(node, &["type"]).map(|s| s.to_lowercase()) else {
        return ShareLinkResult::Skip("Clash 节点没有 type".to_string());
    };
    let Some(server) = str_of(node, &["server"]) else {
        return ShareLinkResult::Skip("Clash 节点没有 server".to_string());
    };
    let Some(port) = num_of(node, &["port"]) else {
        return ShareLinkResult::Skip("Clash 节点没有 port".to_string());
    };
    let name = str_of(node, &["name"]).unwrap_or_else(|| format!("{server}:{port}"));
    let alpn = {
        let list = list_of(node, &["alpn"]);
        if list.is_empty() {
            None
        } else {
            Some(list)
        }
    };

    let transport: Option<V> = match str_of(node, &["network"]).map(|s| s.to_lowercase()).as_deref() {
        Some("ws") => {
            let ws = map_of(node, &["ws-opts"]);
            let mut fields = vec![
                ("type".to_string(), V::Str("ws".into())),
                (
                    "path".to_string(),
                    V::Str(
                        ws.and_then(|m| str_of(m, &["path"]))
                            .unwrap_or_else(|| "/".to_string()),
                    ),
                ),
            ];
            if let Some(host) = ws
                .and_then(|m| map_of(m, &["headers"]))
                .and_then(|m| str_of(m, &["Host", "host"]))
            {
                fields.push((
                    "headers".into(),
                    V::Obj(vec![("Host".to_string(), V::Str(host))]),
                ));
            }
            Some(V::Obj(fields))
        }
        Some("grpc") => Some(V::Obj(vec![
            ("type".to_string(), V::Str("grpc".into())),
            (
                "service_name".to_string(),
                V::Str(
                    map_of(node, &["grpc-opts"])
                        .and_then(|m| str_of(m, &["grpc-service-name"]))
                        .unwrap_or_default(),
                ),
            ),
        ])),
        Some("h2") | Some("http") => {
            let h2 = map_of(node, &["h2-opts"]);
            let mut fields = vec![
                ("type".to_string(), V::Str("http".into())),
                (
                    "path".to_string(),
                    V::Str(
                        h2.and_then(|m| str_of(m, &["path"]))
                            .unwrap_or_else(|| "/".to_string()),
                    ),
                ),
            ];
            let hosts = h2.map(|m| list_of(m, &["host"])).unwrap_or_default();
            if !hosts.is_empty() {
                fields.push(("host".into(), json_string_array(&hosts)));
            }
            Some(V::Obj(fields))
        }
        _ => None,
    };

    // 各协议的"服务器名"字段名不一样：vless / vmess 用 servername，hy2 / tuic 用 sni。
    let tls = |enabled: bool, reality: Option<&Vec<(String, Yaml)>>| -> Option<V> {
        if !enabled && reality.is_none() {
            return None;
        }
        let mut fields = vec![("enabled".to_string(), V::Bool(true))];
        if let Some(name) = str_of(node, &["servername", "sni", "peer"]) {
            fields.push(("server_name".into(), V::Str(name)));
        }
        if flag_of(node, &["skip-cert-verify"]) {
            fields.push(("insecure".into(), V::Bool(true)));
        }
        if let Some(alpn) = &alpn {
            fields.push(("alpn".into(), json_string_array(alpn)));
        }
        if let Some(reality) = reality {
            let mut reality_fields = vec![("enabled".to_string(), V::Bool(true))];
            if let Some(key) = str_of(reality, &["public-key"]) {
                reality_fields.push(("public_key".into(), V::Str(key)));
            }
            if let Some(id) = str_of(reality, &["short-id"]) {
                reality_fields.push(("short_id".into(), V::Str(id)));
            }
            fields.push(("reality".into(), V::Obj(reality_fields)));
        }
        let fp = str_of(node, &["client-fingerprint"])
            .or_else(|| reality.map(|_| "chrome".to_string()));
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
    };

    let outbound = match type_name.as_str() {
        "ss" => {
            let Some(method) = str_of(node, &["cipher"]) else {
                return ShareLinkResult::Skip("ss 节点没有 cipher".to_string());
            };
            let mut fields = vec![
                ("type".to_string(), V::Str("shadowsocks".into())),
                ("server".to_string(), V::Str(server.clone())),
                ("server_port".to_string(), V::Int(port)),
                ("method".to_string(), V::Str(method)),
                (
                    "password".to_string(),
                    V::Str(str_of(node, &["password"]).unwrap_or_default()),
                ),
            ];
            if let Some(plugin) = str_of(node, &["plugin"]) {
                fields.push(("plugin".into(), V::Str(plugin)));
            }
            if let Some(opts) = map_of(node, &["plugin-opts"]) {
                let parts: Vec<String> = opts
                    .iter()
                    .filter_map(|(k, v)| match v {
                        Yaml::Null | Yaml::Bool(true) => Some(k.clone()),
                        Yaml::Bool(false) => None,
                        Yaml::Str(s) => Some(format!("{k}={s}")),
                        Yaml::Int(n) => Some(format!("{k}={n}")),
                        Yaml::List(l) => Some(format!("{k}={}", l.len())),
                        Yaml::Map(_) => Some(format!("{k}={v:?}")),
                    })
                    .collect();
                if !parts.is_empty() {
                    fields.push(("plugin_opts".into(), V::Str(parts.join(";"))));
                }
            }
            V::Obj(fields)
        }
        "vmess" => {
            let Some(uuid) = str_of(node, &["uuid"]) else {
                return ShareLinkResult::Skip("vmess 节点没有 uuid".to_string());
            };
            let mut fields = vec![
                ("type".to_string(), V::Str("vmess".into())),
                ("server".to_string(), V::Str(server.clone())),
                ("server_port".to_string(), V::Int(port)),
                ("uuid".to_string(), V::Str(uuid)),
                (
                    "security".to_string(),
                    V::Str(
                        str_of(node, &["cipher"])
                            .filter(|s| !crate::hosted::is_kotlin_blank(s))
                            .unwrap_or_else(|| "auto".to_string()),
                    ),
                ),
                (
                    "alter_id".to_string(),
                    V::Int(num_of(node, &["alterId", "alterid"]).unwrap_or(0)),
                ),
            ];
            if let Some(tls) = tls(flag_of(node, &["tls"]), None) {
                fields.push(("tls".into(), tls));
            }
            if let Some(transport) = &transport {
                fields.push(("transport".into(), transport.clone()));
            }
            V::Obj(fields)
        }
        "vless" => {
            let Some(uuid) = str_of(node, &["uuid"]) else {
                return ShareLinkResult::Skip("vless 节点没有 uuid".to_string());
            };
            let reality = map_of(node, &["reality-opts"]);
            let mut fields = vec![
                ("type".to_string(), V::Str("vless".into())),
                ("server".to_string(), V::Str(server.clone())),
                ("server_port".to_string(), V::Int(port)),
                ("uuid".to_string(), V::Str(uuid)),
            ];
            if let Some(flow) = str_of(node, &["flow"]) {
                fields.push(("flow".into(), V::Str(flow)));
            }
            if let Some(tls) = tls(flag_of(node, &["tls"]) || reality.is_some(), reality) {
                fields.push(("tls".into(), tls));
            }
            if let Some(transport) = &transport {
                fields.push(("transport".into(), transport.clone()));
            }
            V::Obj(fields)
        }
        "trojan" => {
            let Some(password) = str_of(node, &["password"]) else {
                return ShareLinkResult::Skip("trojan 节点没有 password".to_string());
            };
            let mut fields = vec![
                ("type".to_string(), V::Str("trojan".into())),
                ("server".to_string(), V::Str(server.clone())),
                ("server_port".to_string(), V::Int(port)),
                ("password".to_string(), V::Str(password)),
                (
                    "tls".to_string(),
                    tls(true, None).unwrap_or_else(|| {
                        V::Obj(vec![("enabled".to_string(), V::Bool(true))])
                    }),
                ),
            ];
            if let Some(transport) = &transport {
                fields.push(("transport".into(), transport.clone()));
            }
            V::Obj(fields)
        }
        "hysteria2" | "hy2" => {
            let Some(password) = str_of(node, &["password"]) else {
                return ShareLinkResult::Skip("hysteria2 节点没有 password".to_string());
            };
            let mut fields = vec![
                ("type".to_string(), V::Str("hysteria2".into())),
                ("server".to_string(), V::Str(server.clone())),
                ("server_port".to_string(), V::Int(port)),
                ("password".to_string(), V::Str(password)),
                (
                    "tls".to_string(),
                    tls(true, None).unwrap_or_else(|| {
                        V::Obj(vec![("enabled".to_string(), V::Bool(true))])
                    }),
                ),
            ];
            if let Some(obfs) = str_of(node, &["obfs"]) {
                let mut obfs_fields = vec![("type".to_string(), V::Str(obfs))];
                if let Some(pw) = str_of(node, &["obfs-password"]) {
                    obfs_fields.push(("password".into(), V::Str(pw)));
                }
                fields.push(("obfs".into(), V::Obj(obfs_fields)));
            }
            V::Obj(fields)
        }
        "tuic" => {
            let Some(uuid) = str_of(node, &["uuid"]) else {
                return ShareLinkResult::Skip("tuic 节点没有 uuid".to_string());
            };
            let mut fields = vec![
                ("type".to_string(), V::Str("tuic".into())),
                ("server".to_string(), V::Str(server.clone())),
                ("server_port".to_string(), V::Int(port)),
                ("uuid".to_string(), V::Str(uuid)),
                (
                    "password".to_string(),
                    V::Str(str_of(node, &["password"]).unwrap_or_default()),
                ),
                (
                    "tls".to_string(),
                    tls(true, None).unwrap_or_else(|| {
                        V::Obj(vec![("enabled".to_string(), V::Bool(true))])
                    }),
                ),
            ];
            if let Some(cc) = str_of(node, &["congestion-controller", "congestion_control"]) {
                fields.push(("congestion_control".into(), V::Str(cc)));
            }
            if let Some(mode) = str_of(node, &["udp-relay-mode", "udp_relay_mode"]) {
                fields.push(("udp_relay_mode".into(), V::Str(mode)));
            }
            V::Obj(fields)
        }
        "http" | "socks5" | "socks" => {
            let mut fields = vec![
                (
                    "type".to_string(),
                    V::Str(if type_name == "http" { "http" } else { "socks" }.into()),
                ),
                ("server".to_string(), V::Str(server.clone())),
                ("server_port".to_string(), V::Int(port)),
            ];
            if let Some(username) = str_of(node, &["username"]) {
                fields.push(("username".into(), V::Str(username)));
            }
            if let Some(password) = str_of(node, &["password"]) {
                fields.push(("password".into(), V::Str(password)));
            }
            if let Some(tls) = tls(flag_of(node, &["tls"]), None) {
                fields.push(("tls".into(), tls));
            }
            V::Obj(fields)
        }
        other => {
            return ShareLinkResult::Skip(format!("不支持的 Clash 节点类型：{other}"));
        }
    };
    match outbound_to_node(&outbound, Some(&name), subscription_id) {
        Some(node) => ShareLinkResult::Ok(Box::new(node)),
        None => ShareLinkResult::Skip("缺少服务器地址".to_string()),
    }
}

