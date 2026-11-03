//! X 的 client profile（queryId / features / fieldToggles）与运行时扫脚本解析器（XQueryIdResolver）。
//!
//! 对齐 Kotlin `feedhub.source.x.XClientProfile` / `XQueryIdResolver` / `XFeatures`：
//! - 保留 `features` 与 `fieldToggles` 的原始键顺序（极简保序解析，不影响全局 serde_json）；
//! - 内嵌 `x_client_profile.json` 兜底，支持 `StoreHost` 读取 `"x.profile"` 覆盖；
//! - 扫网页脚本获取 queryId（支持 `x-web/x-web` 与 relative `./assets/`）；
//! - `XFeatures::update_from_html` 只更新已有 key，保持顺序。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, OnceLock};

use regex::Regex;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::host::{HttpHost, HttpRequest, StoreHost};
use crate::x::sign_chain::resolve_import;
use crate::x::{XError, XOperation};

pub const DEFAULT_PROFILE_JSON: &str = include_str!(
    "../../../../../source-x/src/main/resources/feedhub/source/x/x_client_profile.json"
);

pub const KEY: &str = "x.profile";

/// 随 X 版本变的 GraphQL 参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XClientProfile {
    pub schema: i32,
    pub query_ids: HashMap<String, String>,
    /// 时间线的 features（有序）。
    pub features: Vec<(String, bool)>,
    /// 时间线的 fieldToggles（有序；缺省时默认 `[("withPayments", false)]`）。
    pub field_toggles: Vec<(String, bool)>,
    /// 推文详情的 fieldToggles（有序；缺省时默认空列表）。
    pub tweet_detail_field_toggles: Vec<(String, bool)>,
}

impl Default for XClientProfile {
    fn default() -> Self {
        Self::parse(DEFAULT_PROFILE_JSON).unwrap_or_else(|_| Self {
            schema: 1,
            query_ids: HashMap::new(),
            features: Vec::new(),
            field_toggles: vec![("withPayments".to_string(), false)],
            tweet_detail_field_toggles: Vec::new(),
        })
    }
}

impl XClientProfile {
    pub fn query_id(&self, operation: &str) -> Option<&str> {
        self.query_ids
            .get(operation)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
    }

    /// 从 JSON 解析，严格保留 `features` / `fieldToggles` / `tweetDetailFieldToggles` 的出现顺序。
    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: RawClientProfile =
            serde_json::from_str(text).map_err(|e| format!("解析 profile JSON 失败: {e}"))?;
        Ok(Self {
            schema: raw.schema.unwrap_or(1),
            query_ids: raw.query_ids.unwrap_or_default(),
            features: raw.features.map(|m| m.0).unwrap_or_default(),
            field_toggles: raw
                .field_toggles
                .map(|m| m.0)
                .unwrap_or_else(|| vec![("withPayments".to_string(), false)]),
            tweet_detail_field_toggles: raw
                .tweet_detail_field_toggles
                .map(|m| m.0)
                .unwrap_or_default(),
        })
    }

    /// 优先从 `StoreHost` 读取 `"x.profile"`，解析失败或不存在时回退默认兜底。
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
}

#[derive(Deserialize)]
struct RawClientProfile {
    schema: Option<i32>,
    #[serde(rename = "queryIds")]
    query_ids: Option<HashMap<String, String>>,
    features: Option<OrderedBoolMap>,
    #[serde(rename = "fieldToggles")]
    field_toggles: Option<OrderedBoolMap>,
    #[serde(rename = "tweetDetailFieldToggles")]
    tweet_detail_field_toggles: Option<OrderedBoolMap>,
}

/// 按 JSON 词法流原样保序反序列化布尔对象（对齐 Kotlin `LinkedHashMap`：重复键原地覆写值，新键追加末尾）。
struct OrderedBoolMap(Vec<(String, bool)>);

impl<'de> Deserialize<'de> for OrderedBoolMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OrderedVisitor;

        impl<'de> Visitor<'de> for OrderedVisitor {
            type Value = OrderedBoolMap;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a JSON object mapping strings to booleans")
            }

            fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut entries: Vec<(String, bool)> =
                    Vec::with_capacity(access.size_hint().unwrap_or(0));
                while let Some((key, value)) = access.next_entry::<String, bool>()? {
                    if let Some(existing) = entries.iter_mut().find(|(k, _)| k == &key) {
                        existing.1 = value;
                    } else {
                        entries.push((key, value));
                    }
                }
                Ok(OrderedBoolMap(entries))
            }
        }

        deserializer.deserialize_map(OrderedVisitor)
    }
}

/// queryId 解析器：内存缓存 → profile 写死兜底 → 运行时扫网页脚本。
pub struct XQueryIdResolver {
    profile: XClientProfile,
    origin: String,
    max_bundles: usize,
    cache: Mutex<HashMap<String, String>>,
}

impl XQueryIdResolver {
    pub fn new(profile: XClientProfile, origin: impl Into<String>) -> Self {
        Self {
            profile,
            origin: origin.into(),
            max_bundles: 6,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn profile(&self) -> &XClientProfile {
        &self.profile
    }

    pub fn get(&self, operation: &str) -> Option<String> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(id) = cache.get(operation) {
            if !id.trim().is_empty() {
                return Some(id.clone());
            }
        }
        self.profile.query_id(operation).map(|s| s.to_string())
    }

    pub fn set(&self, operation: &str, query_id: &str) {
        if !query_id.trim().is_empty() {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.insert(operation.to_string(), query_id.to_string());
        }
    }

    pub fn set_all(&self, ids: &[(String, String)]) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        for (op, id) in ids {
            if !id.trim().is_empty() {
                cache.insert(op.clone(), id.clone());
            }
        }
    }

    pub fn invalidate(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// 当前该用的 queryId 全表快照（内存缓存优先，其次是 profile 写死值）。
    pub fn snapshot(&self) -> Vec<(String, String)> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (k, v) in cache.iter() {
            if !v.trim().is_empty() && seen.insert(k.clone()) {
                out.push((k.clone(), v.clone()));
            }
        }
        for (k, v) in &self.profile.query_ids {
            if !v.trim().is_empty() && seen.insert(k.clone()) {
                out.push((k.clone(), v.clone()));
            }
        }
        out
    }

    /// 当前该用哪个 queryId：内存缓存 → 写死兜底 → 联网扫。
    pub fn resolve(&self, operation: XOperation, page_http: &dyn HttpHost) -> Result<String, XError> {
        let op_name = operation.op();
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(id) = cache.get(op_name) {
                if !id.trim().is_empty() {
                    return Ok(id.clone());
                }
            }
        }
        if let Some(id) = self.profile.query_id(op_name) {
            return Ok(id.to_string());
        }
        match self.refresh(operation, page_http)? {
            Some(id) => Ok(id),
            None => Err(XError::Signature {
                detail: format!(
                    "拿不到 {} 的 queryId（写死的兜底里没有，扫网页脚本也没扫到）",
                    op_name
                ),
            }),
        }
    }

    /// 强制扫一遍网页脚本并存入内存缓存。
    pub fn refresh(&self, operation: XOperation, page_http: &dyn HttpHost) -> Result<Option<String>, XError> {
        let found = match self.scan_bundles(page_http) {
            Ok(map) => map,
            Err(_) => return Ok(None),
        };
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        for (k, v) in &found {
            cache.insert(k.clone(), v.clone());
        }
        Ok(found.get(operation.op()).cloned())
    }

    pub fn scan_bundles(&self, page_http: &dyn HttpHost) -> Result<HashMap<String, String>, XError> {
        static SCRIPT_SRC_RE: OnceLock<Regex> = OnceLock::new();
        static ENTRY_NAME_RE: OnceLock<Regex> = OnceLock::new();
        static RELATIVE_ASSET_RE: OnceLock<Regex> = OnceLock::new();
        static QUERY_ID_RE: OnceLock<Regex> = OnceLock::new();

        let page_url = format!("{}/", self.origin.trim_end_matches('/'));
        let res = page_http
            .execute(HttpRequest {
                method: "GET".to_string(),
                url: page_url.clone(),
                headers: super::source::page_headers(),
                body: Vec::new(),
            })
            .map_err(|e| XError::Network {
                detail: format!("取首页失败: {e}"),
            })?;
        if !(200..=299).contains(&res.status) {
            return Err(XError::Api {
                code: format!("HTTP {}", res.status),
                detail: format!("取 {page_url} 失败"),
            });
        }
        let page_html = String::from_utf8_lossy(&res.body).to_string();

        let script_src_regex = SCRIPT_SRC_RE.get_or_init(|| {
            Regex::new(
                r#"(?:src|href)=["'](https://abs\.twimg\.com/(?:responsive-web/client-web|x-web/x-web)[^"']+\.js)["']"#,
            )
            .expect("SCRIPT_SRC_RE")
        });

        let mut bundles = Vec::new();
        for cap in script_src_regex.captures_iter(&page_html) {
            if let Some(m) = cap.get(1) {
                let url = m.as_str().to_string();
                if !bundles.contains(&url) {
                    bundles.push(url);
                    if bundles.len() >= self.max_bundles {
                        break;
                    }
                }
            }
        }

        let mut texts = HashMap::<String, String>::new();
        let mut fetch_text = |url: &str| -> Option<String> {
            if let Some(t) = texts.get(url) {
                return Some(t.clone());
            }
            let res = page_http
                .execute(HttpRequest {
                    method: "GET".to_string(),
                    url: url.to_string(),
                    headers: super::source::page_headers(),
                    body: Vec::new(),
                })
                .ok()?;
            if !(200..=299).contains(&res.status) {
                return None;
            }
            let s = String::from_utf8_lossy(&res.body).to_string();
            texts.insert(url.to_string(), s.clone());
            Some(s)
        };

        let entry_name_regex =
            ENTRY_NAME_RE.get_or_init(|| Regex::new(r"entry-client|startup-dynamic").expect("ENTRY_NAME_RE"));
        let relative_asset_regex = RELATIVE_ASSET_RE.get_or_init(|| {
            Regex::new(r#"["']([./\w-]*assets/[\w.-]+\.js)["']"#).expect("RELATIVE_ASSET_RE")
        });

        for bundle in bundles.clone() {
            if !entry_name_regex.is_match(&bundle) {
                continue;
            }
            let text = match fetch_text(&bundle) {
                Some(t) => t,
                None => continue,
            };
            for cap in relative_asset_regex.captures_iter(&text) {
                if bundles.len() >= self.max_bundles {
                    break;
                }
                if let Some(m) = cap.get(1) {
                    if let Ok(resolved) = resolve_import(&bundle, m.as_str()) {
                        if !bundles.contains(&resolved) {
                            bundles.push(resolved);
                        }
                    }
                }
            }
        }

        let query_id_regex = QUERY_ID_RE.get_or_init(|| {
            Regex::new(r#"queryId:\s*"([A-Za-z0-9_-]+)"[^}]{0,200}operationName:\s*"([^"]+)""#)
                .expect("QUERY_ID_RE")
        });

        let mut found = HashMap::new();
        for bundle in bundles {
            let text = match fetch_text(&bundle) {
                Some(t) => t,
                None => continue,
            };
            for cap in query_id_regex.captures_iter(&text) {
                if let (Some(qid), Some(op)) = (cap.get(1), cap.get(2)) {
                    found
                        .entry(op.as_str().to_string())
                        .or_insert_with(|| qid.as_str().to_string());
                }
            }
        }

        Ok(found)
    }
}

/// features 的运行时更新（对齐 Kotlin `XFeatures`）。
pub struct XFeatures;

impl XFeatures {
    fn value_regex() -> &'static Regex {
        static VALUE_RE: OnceLock<Regex> = OnceLock::new();
        VALUE_RE.get_or_init(|| {
            Regex::new(r#""([a-z][a-z0-9_]+)":\s*\{\s*"value"\s*:\s*(true|false)"#)
                .expect("VALUE_RE")
        })
    }

    pub fn update_from_html(existing: &[(String, bool)], page_html: &str) -> Vec<(String, bool)> {
        let mut updates = HashMap::new();
        for cap in Self::value_regex().captures_iter(page_html) {
            if let (Some(k), Some(v)) = (cap.get(1), cap.get(2)) {
                let key = k.as_str();
                if existing.iter().any(|(ek, _)| ek == key) {
                    updates.insert(key, v.as_str() == "true");
                }
            }
        }

        existing
            .iter()
            .map(|(k, v)| {
                let val = updates.get(k.as_str()).copied().unwrap_or(*v);
                (k.clone(), val)
            })
            .collect()
    }

    pub fn matched_flags(page_html: &str) -> Vec<String> {
        Self::value_regex()
            .captures_iter(page_html)
            .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
            .collect()
    }
}
