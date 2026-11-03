//! X 的 txid 加载链规则与三跳解析器（页面 → entry 脚本 → importer → sign 模块）。
//!
//! 对齐 Kotlin `feedhub.source.x.XSignChain`：
//! - 依次尝试 `pagePaths`（默认 `["/home", "/"]`）；
//! - 功能开关检查：页面 HTML 中 `"rweb_client_transaction_id_enabled":{"value":false}` 则立即终止；
//! - 候选 entry 脚本收集与兜底；
//! - `main.<hash>.js` 先检查 txid 关键字 `obfiowerehiring`；
//! - 相对路径解析对齐 `java.net.URI(base).resolve(path)`；
//! - 诊断观察者回调。

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::host::{HttpHost, HttpRequest, StoreHost};
use crate::x::XError;

pub const DEFAULT_SIGN_CHAIN_JSON: &str = include_str!(
    "../../../../../source-x/src/main/resources/feedhub/source/x/x_sign_chain.json"
);

pub const KEY: &str = "x.sign_chain";
pub const X_ORIGIN: &str = "https://x.com";

/// 三跳解析结果：页面 HTML + sign 模块正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XtxSources {
    pub page_url: String,
    pub page_html: String,
    pub entry_url: String,
    pub sign_url: String,
    pub sign_file_text: String,
}

/// 解析的可能结果：成功拿到文本，或者页面功能开关明确关闭。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XSignChainOutcome {
    Success(XtxSources),
    DisabledByPage,
}

/// 加载链每一步的旁路观察者（可选）。异常吞掉，绝不影响解析。
pub trait XChainDiagnostics: Send + Sync {
    fn on_page(&self, _page_url: &str, _page_html: &str) {}
    fn on_entry_candidates(&self, _page_url: &str, _entries: &[String]) {}
    fn on_script(&self, _url: &str, _hop: &str, _names: &[String], _hints: &[String]) {}
}

pub struct NoChainDiagnostics;
impl XChainDiagnostics for NoChainDiagnostics {}

/// 脚本正文结构线索。
pub struct XScriptHints;

impl XScriptHints {
    pub fn of(text: &str, names: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        let keywords = [("obfiowerehiring", "txid 关键字")];
        let importer_shapes = [
            ("client-transaction-id-plugin", "导入器名 client-transaction-id-plugin"),
            ("startup-dynamic", "导入器名 startup-dynamic"),
            ("sentry-filter", "导入器名 sentry-filter"),
        ];
        let sign_shapes = [("sign.o-", "sign 模块名 sign.o-")];

        for (needle, label) in keywords.iter().chain(&importer_shapes).chain(&sign_shapes) {
            if text.contains(needle) {
                out.push(label.to_string());
            }
        }
        static HASH_RE: OnceLock<Regex> = OnceLock::new();
        let hash_regex =
            HASH_RE.get_or_init(|| Regex::new(r"^[\w-]+\.[0-9a-f]{8,}\.js$").expect("HASH_RE"));
        if names.iter().any(|n| hash_regex.is_match(n)) {
            out.push("打包器哈希文件名".to_string());
        }
        out
    }
}

/// 从一份正文中提取脚本名（保序、去重、最多 24 个）。
pub fn script_names_in(text: &str) -> Vec<String> {
    static SCRIPT_REF_RE: OnceLock<Regex> = OnceLock::new();
    static FULL_URL_RE: OnceLock<Regex> = OnceLock::new();
    let script_ref_regex = SCRIPT_REF_RE.get_or_init(|| {
        Regex::new(r#"["'`](\.{0,2}/[\w./-]+\.js(?:\?[\w=&.\-]*)?)["'`]"#).expect("SCRIPT_REF_RE")
    });
    let full_url_regex = FULL_URL_RE.get_or_init(|| {
        Regex::new(r#"https?://[^"'\s]+/([\w.-]+\.js)"#).expect("FULL_URL_RE")
    });

    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for cap in script_ref_regex.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            let cleaned = m.as_str().trim_start_matches(|c| c == '.' || c == '/');
            if !cleaned.is_empty() && cleaned.len() <= 200 && seen.insert(cleaned.to_string()) {
                out.push(cleaned.to_string());
                if out.len() >= 24 {
                    return out;
                }
            }
        }
    }
    for cap in full_url_regex.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            let cleaned = m.as_str().trim_start_matches(|c| c == '.' || c == '/');
            if !cleaned.is_empty() && cleaned.len() <= 200 && seen.insert(cleaned.to_string()) {
                out.push(cleaned.to_string());
                if out.len() >= 24 {
                    return out;
                }
            }
        }
    }
    out
}

/// 检查页面 HTML 中的 txid 开关。
pub fn txid_switch(html: &str) -> Option<bool> {
    static TXID_SWITCH_RE: OnceLock<Regex> = OnceLock::new();
    let re = TXID_SWITCH_RE.get_or_init(|| {
        Regex::new(r#""rweb_client_transaction_id_enabled"\s*:\s*\{\s*"value"\s*:\s*(true|false)"#)
            .expect("TXID_SWITCH_RE")
    });
    re.captures(html)
        .and_then(|cap| cap.get(1))
        .and_then(|m| m.as_str().parse::<bool>().ok())
}

/// 错误文案中只截取文件名（去掉路径和查询参数）。
pub fn file_name(url: &str) -> String {
    let without_query = url.split('?').next().unwrap_or(url);
    without_query.rsplit('/').next().unwrap_or(without_query).to_string()
}

/// 相对路径解析，严格对齐 `java.net.URI(base).resolve(path)`。
pub fn resolve_import(base: &str, path: &str) -> Result<String, String> {
    if path.starts_with("http://") || path.starts_with("https://") {
        return Ok(path.to_string());
    }

    let scheme_end = base.find("://").ok_or_else(|| format!("base 缺少 scheme: {base}"))?;
    let scheme = &base[..scheme_end];
    let after_scheme = &base[scheme_end + 3..];

    if path.starts_with("//") {
        return Ok(format!("{scheme}:{path}"));
    }

    let authority_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    let origin = format!("{scheme}://{authority}");
    let base_path = &after_scheme[authority_end..];
    let base_path = if base_path.is_empty() { "/" } else { base_path };

    let (path_only, query_frag) = match path.find(|c| c == '?' || c == '#') {
        Some(idx) => (&path[..idx], &path[idx..]),
        None => (path, ""),
    };

    let raw_combined = if path_only.starts_with('/') {
        path_only.to_string()
    } else {
        let base_dir = match base_path.rfind('/') {
            Some(idx) => &base_path[..=idx],
            None => "/",
        };
        format!("{base_dir}{path_only}")
    };

    // 规范化路径段
    let mut segments = Vec::new();
    let is_absolute = raw_combined.starts_with('/');
    for seg in raw_combined.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        } else if seg == ".." {
            segments.pop();
        } else {
            segments.push(seg);
        }
    }

    let mut normalized = String::new();
    if is_absolute {
        normalized.push('/');
    }
    normalized.push_str(&segments.join("/"));
    if raw_combined.ends_with('/') && !normalized.ends_with('/') {
        normalized.push('/');
    }
    normalized.push_str(query_frag);

    Ok(format!("{origin}{normalized}"))
}

/// txid 加载链规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XSignChain {
    pub page_paths: Vec<String>,
    pub entry_pattern: String,
    pub importer_pattern: String,
    pub sign_pattern: String,
}

impl Default for XSignChain {
    fn default() -> Self {
        Self::parse(DEFAULT_SIGN_CHAIN_JSON).unwrap_or_else(|_| Self {
            page_paths: vec!["/home".to_string(), "/".to_string()],
            entry_pattern: r#"src=["'](https?://[^"']+/(?:entry-client[\w-]*|main\.[\w-]{6,})\.js)["']"#.to_string(),
            importer_pattern: r#"["']([./\w-]*(?:client-transaction-id-plugin|startup-dynamic|sentry-filter)-[\w-]+\.js)["']"#.to_string(),
            sign_pattern: r#"["'`]([./\w-]*sign\.o-[\w-]+\.js)["'`]"#.to_string(),
        })
    }
}

impl XSignChain {
    pub fn parse(json: &str) -> Result<Self, String> {
        let val: serde_json::Value =
            serde_json::from_str(json).map_err(|e| format!("解析 sign_chain JSON 失败: {e}"))?;
        let page_paths = val
            .get("pagePaths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec!["/home".to_string(), "/".to_string()]);

        let entry_pattern = val
            .get("entryPattern")
            .and_then(|v| v.as_str())
            .unwrap_or(r#"src=["'](https?://[^"']+/(?:entry-client[\w-]*|main\.[\w-]{6,})\.js)["']"#)
            .to_string();

        let importer_pattern = val
            .get("importerPattern")
            .and_then(|v| v.as_str())
            .unwrap_or(r#"["']([./\w-]*(?:client-transaction-id-plugin|startup-dynamic|sentry-filter)-[\w-]+\.js)["']"#)
            .to_string();

        let sign_pattern = val
            .get("signPattern")
            .and_then(|v| v.as_str())
            .unwrap_or(r#"["'`]([./\w-]*sign\.o-[\w-]+\.js)["'`]"#)
            .to_string();

        Ok(Self {
            page_paths,
            entry_pattern,
            importer_pattern,
            sign_pattern,
        })
    }

    pub fn load(store: Option<&dyn StoreHost>) -> Self {
        if let Some(store) = store {
            if let Some(text) = store.get(KEY) {
                if let Ok(parsed) = Self::parse(&text) {
                    return parsed;
                }
            }
        }
        Self::default()
    }

    /// 收集 entry 候选：优先 entry_pattern 直接命中；若为空，才用兜底 ENTRY_FILE_REGEX。
    pub fn collect_candidates(&self, html: &str, origin: &str) -> Vec<String> {
        let entry_re = match Regex::new(&self.entry_pattern) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        let direct: Vec<String> = entry_re
            .captures_iter(html)
            .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
            .collect();
        if !direct.is_empty() {
            return direct;
        }

        static FALLBACK_RE: OnceLock<Regex> = OnceLock::new();
        let fallback_re = FALLBACK_RE.get_or_init(|| {
            Regex::new(
                r#"(https://[\w.\-]+/[^"'\s]*/entry-client[\w.-]*\.js|/[\w.\-/]*/entry-client[\w.-]*\.js)"#,
            )
            .expect("FALLBACK_RE")
        });

        let base = format!("{origin}/");
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        for cap in fallback_re.captures_iter(html) {
            if let Some(m) = cap.get(1) {
                if let Ok(resolved) = resolve_import(&base, m.as_str()) {
                    if seen.insert(resolved.clone()) {
                        candidates.push(resolved);
                    }
                }
            }
        }
        candidates
    }

    /// 顺着三跳解析 txid 所需的两份文本。
    pub fn resolve(
        &self,
        origin: &str,
        page_http: &dyn HttpHost,
        diagnostics: Option<&dyn XChainDiagnostics>,
    ) -> Result<XSignChainOutcome, XError> {
        let mut fetched = HashMap::<String, String>::new();
        let mut fetch = |url: &str| -> Result<String, XError> {
            if let Some(existing) = fetched.get(url) {
                return Ok(existing.clone());
            }
            let res = page_http
                .execute(HttpRequest {
                    method: "GET".to_string(),
                    url: url.to_string(),
                    headers: super::source::page_headers(),
                    body: Vec::new(),
                })
                .map_err(XError::from)?;
            if !(200..=299).contains(&res.status) {
                return Err(XError::Api {
                    code: format!("HTTP {}", res.status),
                    detail: format!("取 {url} 失败"),
                });
            }
            let body_str = String::from_utf8_lossy(&res.body).to_string();
            fetched.insert(url.to_string(), body_str.clone());
            Ok(body_str)
        };

        let mut page_url_found: Option<String> = None;
        let mut page_html_found: Option<String> = None;
        let mut entries = Vec::new();

        for path in &self.page_paths {
            let url = format!("{origin}{path}");
            let html = fetch(&url)?;

            // 页面开关关着：官方网页也不带 txid，照做，后续脚本一个都不抓
            if txid_switch(&html) == Some(false) {
                return Ok(XSignChainOutcome::DisabledByPage);
            }

            let candidates = self.collect_candidates(&html, origin);
            if let Some(diag) = diagnostics {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    diag.on_page(&url, &html);
                    diag.on_entry_candidates(&url, &candidates);
                }));
            }

            page_url_found = Some(url.clone());
            page_html_found = Some(html);

            if !candidates.is_empty() {
                entries = candidates;
                break;
            }
        }

        if entries.is_empty() {
            return Err(XError::Signature {
                detail: format!(
                    "第 1 跳：x.com 页面里没找到 entry 脚本（试过 {}）——X 又改了加载链，改 XSignChain.entryPattern / pagePaths",
                    self.page_paths.join("、")
                ),
            });
        }

        let (entry_url, sign_url) = self.attempt(&entries, &mut fetch, diagnostics)?;
        let sign_text = fetch(&sign_url)?;

        if let Some(diag) = diagnostics {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let names = script_names_in(&sign_text);
                let hints = XScriptHints::of(&sign_text, &names);
                diag.on_script(&sign_url, "sign 模块", &names, &hints);
            }));
        }

        Ok(XSignChainOutcome::Success(XtxSources {
            page_url: page_url_found.unwrap_or_default(),
            page_html: page_html_found.unwrap_or_default(),
            entry_url,
            sign_url,
            sign_file_text: sign_text,
        }))
    }

    fn attempt<F>(
        &self,
        entries: &[String],
        fetch: &mut F,
        diagnostics: Option<&dyn XChainDiagnostics>,
    ) -> Result<(String, String), XError>
    where
        F: FnMut(&str) -> Result<String, XError>,
    {
        static MAIN_BUNDLE_RE: OnceLock<Regex> = OnceLock::new();
        let main_bundle_re = MAIN_BUNDLE_RE
            .get_or_init(|| Regex::new(r"^main\.[\w-]{6,}\.js$").expect("MAIN_BUNDLE_RE"));
        let importer_re = Regex::new(&self.importer_pattern).map_err(|e| XError::Signature {
            detail: format!("importerPattern 正则非法: {e}"),
        })?;
        let sign_re = Regex::new(&self.sign_pattern).map_err(|e| XError::Signature {
            detail: format!("signPattern 正则非法: {e}"),
        })?;

        let mut last_failure: Option<String> = None;

        for entry_url in entries {
            let fname = file_name(entry_url);
            let require_keyword = main_bundle_re.is_match(&fname);
            let entry_text = fetch(entry_url)?;

            if let Some(diag) = diagnostics {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let names = script_names_in(&entry_text);
                    let hints = XScriptHints::of(&entry_text, &names);
                    diag.on_script(entry_url, "entry 脚本", &names, &hints);
                }));
            }

            if require_keyword && !entry_text.contains("obfiowerehiring") {
                last_failure = Some(format!(
                    "第 2 跳：新打包器的 entry 脚本（{}）里没有 txid 关键字（obfiowerehiring）——sign 逻辑可能已经不在这份 bundle 里了，别拿它当 entry",
                    fname
                ));
                continue;
            }

            let importer_rel = match importer_re.captures(&entry_text).and_then(|cap| cap.get(1)) {
                Some(m) => m.as_str(),
                None => {
                    last_failure = Some(format!(
                        "第 2 跳：entry 脚本（{}）里没找到 sign 模块导入器（client-transaction-id-plugin / startup-dynamic / sentry-filter）——改 XSignChain.importerPattern",
                        fname
                    ));
                    continue;
                }
            };

            let importer_url = resolve_import(entry_url, importer_rel)
                .map_err(|detail| XError::Signature { detail })?;

            let importer_text = fetch(&importer_url)?;

            if let Some(diag) = diagnostics {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let names = script_names_in(&importer_text);
                    let hints = XScriptHints::of(&importer_text, &names);
                    diag.on_script(&importer_url, "导入器", &names, &hints);
                }));
            }

            let sign_rel = match sign_re.captures(&importer_text).and_then(|cap| cap.get(1)) {
                Some(m) => m.as_str(),
                None => {
                    last_failure = Some(format!(
                        "第 3 跳：导入器（{}）里没找到 sign.o 模块——改 XSignChain.signPattern",
                        file_name(&importer_url)
                    ));
                    continue;
                }
            };

            let sign_url = resolve_import(&importer_url, sign_rel)
                .map_err(|detail| XError::Signature { detail })?;

            return Ok((entry_url.clone(), sign_url));
        }

        let failure_msg = last_failure.unwrap_or_else(|| "第 1 跳：没有任何 entry 候选脚本".to_string());
        Err(XError::Signature {
            detail: format!(
                "页面里找到 {} 个 entry 候选（{}），但都没走到 sign 模块：{}",
                entries.len(),
                entries.iter().map(|e| file_name(e)).collect::<Vec<_>>().join("、"),
                failure_msg
            ),
        })
    }
}
