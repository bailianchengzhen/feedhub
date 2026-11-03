//! 生成 sing-box 的配置（`core/proxy/SingBoxConfig.kt` 的 Rust 版）。
//!
//! 核心只开**一个**本机入站：`mixed`（HTTP + SOCKS5 都在同一个端口上），只监听 `127.0.0.1`，
//! **不开 TUN、不当系统代理**（`set_system_proxy: false`）。出站就是节点列表；"自动（延迟最低）"
//! 用 sing-box 自己的 `urltest` 出站（每隔 [`URLTEST_INTERVAL`] 自己挑一次）。
//!
//! 字段名按 sing-box **1.14.3** 的 schema（`sing-box schema` / `sing-box check` 核对过；
//! `ProxyCoreE2eTest` 用真二进制把六种协议的出站都 `check` 了一遍）。
//!
//! **键顺序和 Kotlin 一样**（`buildJsonObject` 的插入顺序），因为生成的文件会原样交给 sing-box 和用户看。

use crate::proxy_core::json_text::{self, V};
use crate::proxy_core::model::{ProxyCoreState, ProxyNode, CORE_LOG_LEVELS, NO_NODES_REASON};

/// 入站的 tag。
pub const CORE_INBOUND_TAG: &str = "in";

/// "自动（延迟最低）"那个 urltest 出站的 tag。
pub const CORE_AUTO_TAG: &str = "auto";

/// urltest 每隔多久自己重挑一次节点。
pub const URLTEST_INTERVAL: &str = "3m";

/// 造 [`ProxyCoreState`] 对应的核心配置。
///
/// `port` 是 `mixed` 入站的端口（调用方先找一个空闲端口）。一个节点都没有时返回 [`NO_NODES_REASON`]。
pub fn build_core_config(state: &ProxyCoreState, port: i32) -> Result<V, String> {
    if !(1..=65535).contains(&port) {
        return Err(format!("端口不对：{port}"));
    }
    if state.nodes.is_empty() {
        return Err(NO_NODES_REASON.to_string());
    }
    let tags: Vec<String> = state.nodes.iter().map(|n| n.id.clone()).collect();
    let final_tag = match &state.selected_node_id {
        Some(sel) if tags.contains(sel) => sel.clone(),
        _ if tags.len() > 1 => CORE_AUTO_TAG.to_string(),
        _ => tags[0].clone(),
    };
    let mut outbounds: Vec<V> = state
        .nodes
        .iter()
        .map(|node| with_tag(&node.outbound, &node.id))
        .collect();
    if tags.len() > 1 {
        outbounds.push(V::Obj(vec![
            ("type".to_string(), V::Str("urltest".into())),
            ("tag".to_string(), V::Str(CORE_AUTO_TAG.into())),
            (
                "outbounds".to_string(),
                V::Arr(tags.iter().map(|t| V::Str(t.clone())).collect()),
            ),
            ("url".to_string(), V::Str(state.test_url.clone())),
            ("interval".to_string(), V::Str(URLTEST_INTERVAL.into())),
            ("tolerance".to_string(), V::Int(50)),
        ]));
    }
    Ok(V::Obj(vec![
        (
            "log".to_string(),
            V::Obj(vec![
                ("level".to_string(), V::Str(state.log_level.clone())),
                ("timestamp".to_string(), V::Bool(true)),
            ]),
        ),
        (
            "inbounds".to_string(),
            V::Arr(vec![V::Obj(vec![
                ("type".to_string(), V::Str("mixed".into())),
                ("tag".to_string(), V::Str(CORE_INBOUND_TAG.into())),
                ("listen".to_string(), V::Str("127.0.0.1".into())),
                ("listen_port".to_string(), V::Int(port as i64)),
                // 绝不当系统代理：这个核心只给 App 自己用
                ("set_system_proxy".to_string(), V::Bool(false)),
            ])]),
        ),
        ("outbounds".to_string(), V::Arr(outbounds)),
        (
            "route".to_string(),
            V::Obj(vec![
                ("rules".to_string(), V::Arr(vec![])),
                ("final".to_string(), V::Str(final_tag)),
            ]),
        ),
    ]))
}

/// 造一个"只测这一个节点"的配置，给延迟测试用（`sing-box tools fetch` 直接走出站，不需要入站）。
pub fn build_latency_probe_config(node: &ProxyNode) -> V {
    V::Obj(vec![
        (
            "log".to_string(),
            V::Obj(vec![("level".to_string(), V::Str("error".into()))]),
        ),
        (
            "outbounds".to_string(),
            V::Arr(vec![with_tag(&node.outbound, &node.id)]),
        ),
        (
            "route".to_string(),
            V::Obj(vec![("final".to_string(), V::Str(node.id.clone()))]),
        ),
    ])
}

/// 给一份出站补上 `tag`（原本的 `tag` 已经在校验时被 [`crate::proxy_core::model::without_tag`] 去掉了，
/// 但订阅里手写的 `tag` 可能还在别的路径上进得来，所以这里也**先删再加**）。
fn with_tag(outbound: &V, tag: &str) -> V {
    let mut fields: Vec<(String, V)> = match outbound {
        V::Obj(f) => f.iter().filter(|(k, _)| k != "tag").cloned().collect(),
        other => {
            return V::Obj(vec![
                ("tag".to_string(), V::Str(tag.to_string())),
                ("value".to_string(), other.clone()),
            ])
        }
    };
    fields.push(("tag".to_string(), V::Str(tag.to_string())));
    V::Obj(fields)
}

/// 配置写成文本（写盘用）。和 Kotlin 的 `toConfigText()` 逐字节一样（`prettyPrint`，4 空格）。
pub fn to_config_text(config: &V) -> String {
    json_text::write_pretty(config)
}

/// 配置有没有问题（起核心之前检查）。返回空表 = 没问题。
pub fn core_config_issues(state: &ProxyCoreState) -> Vec<String> {
    let mut issues = Vec::new();
    if state.nodes.is_empty() {
        issues.push(NO_NODES_REASON.to_string());
    }
    if let Some(sel) = &state.selected_node_id {
        if !state.nodes.iter().any(|n| &n.id == sel) {
            issues.push("选中的节点已经不在列表里了（会自动改用「自动（延迟最低）」）".to_string());
        }
    }
    if crate::hosted::is_kotlin_blank(&state.test_url) || !state.test_url.starts_with("http") {
        issues.push("测试地址要以 http 开头".to_string());
    }
    if !CORE_LOG_LEVELS.contains(&state.log_level.as_str()) {
        issues.push(format!("日志级别不认识：{}", state.log_level));
    }
    issues
}

