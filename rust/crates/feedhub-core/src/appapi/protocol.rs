//! 对外接口协议：JSON Lines 组装、自描述 (`/capabilities`)、平台清单 (`/platforms`)、错误响应。

use crate::appapi::{ERROR_CODES, SCHEMA_VERSION};
use crate::sync::json::{self, Value};

pub const DEFAULT_LIBRARY_LIMIT: i32 = 20;
pub const MAX_LIBRARY_LIMIT: i32 = 500;
pub const MAX_PAGE_LIMIT: i32 = 500;
pub const MAX_COMMENT_SEARCH_PAGES: i32 = 3;
pub const PAGE_BUDGET: i32 = 3;
pub const MAX_CONCURRENCY: i32 = 4;
pub const QUEUE_DEPTH: i32 = 16;
pub const JPEG_QUALITY: i32 = 85;

struct EndpointDesc {
    method: &'static str,
    path: &'static str,
    description: &'static str,
    network: bool,
    params: &'static [(&'static str, bool, &'static str)],
}

const ENDPOINTS: &[EndpointDesc] = &[
    EndpointDesc {
        method: "GET",
        path: "/capabilities",
        description: "接口清单、参数、错误码、各平台能力（离线，不建 Source、不发请求）",
        network: false,
        params: &[],
    },
    EndpointDesc {
        method: "GET",
        path: "/platforms",
        description: "装了哪些平台、各支持什么（离线）",
        network: false,
        params: &[],
    },
    EndpointDesc {
        method: "GET",
        path: "/search",
        description: "搜索。platform=all 时和 App 搜索页同一套规则（只搜首页点亮的、每家 3 页、混排、18+ / 内容过滤）",
        network: true,
        params: &[
            ("q", true, "搜索词（别名 query）"),
            ("platform", false, "平台 id，或 all（默认 all）"),
            ("only", false, "只搜这几家（逗号分隔；只有 all 能配，必须在首页点亮）"),
            ("limit", false, "最多给几条（默认 all 一整批 / 单平台一整页；最多 500，一次最多给一页）"),
            ("page", false, "第几页，1 起（每家最多 3 页）"),
            ("pages", false, "从第 1 页连续取几页（等价于 page=1..N）"),
            ("cursor", false, "上一轮 done.next 里的值（续页游标，不透明；也可能是平台游标）。只能和单个平台一起用；值里有 & % + = 等字符，要 URL 编码（curl 用 --data-urlencode）"),
            ("kind", false, "all / video / post / user / topic（user / topic 目前没有平台支持 → not_supported）"),
            ("sort", false, "relevance / latest / hot（同上）"),
            ("community", false, "贴吧吧名（给了就是吧内搜索）"),
        ],
    },
    EndpointDesc {
        method: "GET",
        path: "/detail",
        description: "一条内容的详情正文 / 视频分 P / 相关推荐（18+ 只回 type=hidden）。正文里的图片带 idx / isCover / segments，地址不输出",
        network: true,
        params: &[
            ("platform", true, "平台 id"),
            ("id", true, "内容 id（搜索结果的 data.detail.id；小红书必须用它，不能用 data.id）"),
        ],
    },
    EndpointDesc {
        method: "GET",
        path: "/image",
        description: "取一张图（**回的是图片本身**：image/jpeg；尺寸等元信息在 X-Feedhub-* 响应头里）。由 App 去取（和 App 刷流取图同一条路：浏览器 UA / 按域名的代理 / fail-closed；图片请求不带 Cookie），不需要也不允许传图片地址",
        network: true,
        params: &[
            ("platform", true, "平台 id"),
            ("id", true, "内容 id（= /detail 的 data.detail.id）"),
            ("idx", true, "第几张图（0 起，/detail 里 img 块的 idx）"),
            ("maxw", false, "缩到宽 ≤ 多少像素（默认 1024，最多 2048；不放大）"),
            ("seg", false, "长图取第几段（0 起；看图块的 segments / 响应的 X-Feedhub-Segments）。不传 = 第 0 段"),
            ("comment", false, "评论里的图：评论 id（idx 是这条评论里图的序号）"),
        ],
    },
    EndpointDesc {
        method: "GET",
        path: "/comments",
        description: "一级评论（含楼中楼预览；贴吧是楼层）",
        network: true,
        params: &[
            ("platform", true, "平台 id"),
            ("id", true, "内容 id"),
            ("limit", false, "最多给几条（默认一页；最多 500；截断时 done.truncated=true、done.next 可以续）"),
            ("pages", false, "翻几页（默认 1）"),
            ("cursor", false, "上一轮 done.next 里的值（续页游标；也可能是平台游标），要 URL 编码"),
            ("sort", false, "default / hot / newest"),
        ],
    },
    EndpointDesc {
        method: "GET",
        path: "/replies",
        description: "某条一级评论下的楼中楼",
        network: true,
        params: &[
            ("platform", true, "平台 id"),
            ("id", true, "内容 id"),
            ("comment", true, "一级评论 id（别名 commentId）"),
            ("limit", false, "最多给几条（最多 500）"),
            ("pages", false, "翻几页（默认 1）"),
            ("cursor", false, "上一轮 done.next 里的值（续页游标；也可能是平台游标），要 URL 编码"),
        ],
    },
    EndpointDesc {
        method: "GET",
        path: "/library/favorites",
        description: "本地收藏（只读 App 的资料库，一个网络请求都不发）",
        network: false,
        params: &[
            ("platform", false, "只看这个平台"),
            ("since", false, "从这个时间起（24h / 7d / 2026-10-01 / Unix 秒 / 毫秒）"),
            ("q", false, "在标题 / 作者 / 摘要里做本地子串匹配"),
            ("limit", false, "最多几条（默认 20，最多 500）"),
        ],
    },
    EndpointDesc {
        method: "GET",
        path: "/library/history",
        description: "本地历史（只读，同上）",
        network: false,
        params: &[
            ("platform", false, "只看这个平台"),
            ("since", false, "从这个时间起"),
            ("q", false, "本地子串匹配"),
            ("limit", false, "最多几条（默认 20，最多 500）"),
            ("failedOnly", false, "1 = 只看没看完的（有进度且进度 < 时长）"),
        ],
    },
];

pub fn json_line(line_type: &str, data: Option<Value>, extra: Vec<(String, Value)>) -> String {
    let mut fields = Vec::with_capacity(3 + extra.len());
    fields.push(("schema".to_string(), Value::Number(SCHEMA_VERSION.to_string())));
    fields.push(("type".to_string(), Value::String(line_type.to_string())));
    if let Some(d) = data {
        fields.push(("data".to_string(), d));
    }
    for (k, v) in extra {
        fields.push((k, v));
    }
    json::to_json_string(&Value::Object(fields))
}

pub fn error_json_line(
    error_code: &str,
    platform: Option<&str>,
    message: &str,
    retryable: bool,
    extra: Vec<(String, Value)>,
    requests: Option<i32>,
) -> String {
    let mut extra_fields = Vec::with_capacity(5 + extra.len());
    extra_fields.push(("error".to_string(), Value::String(error_code.to_string())));
    if let Some(p) = platform {
        extra_fields.push(("platform".to_string(), Value::String(p.to_string())));
    }
    extra_fields.push(("message".to_string(), Value::String(message.to_string())));
    extra_fields.push(("retryable".to_string(), Value::Bool(retryable)));
    for (k, v) in extra {
        extra_fields.push((k, v));
    }
    if let Some(reqs) = requests {
        extra_fields.push(("requests".to_string(), Value::Number(reqs.to_string())));
    }
    json_line("error", None, extra_fields)
}

pub fn render_capabilities(socket_path: &str, platform_json_arr: Value) -> String {
    let mut body_fields = Vec::new();
    body_fields.push(("schema".to_string(), Value::Number(SCHEMA_VERSION.to_string())));

    // socket
    let socket_obj = Value::Object(vec![
        ("path".to_string(), Value::String(socket_path.to_string())),
        ("transport".to_string(), Value::String("http-over-unix-socket".to_string())),
        ("readOnly".to_string(), Value::Bool(true)),
        ("permissions".to_string(), Value::String("socket 0600 / 目录 0700（不监听 TCP 端口）".to_string())),
    ]);
    body_fields.push(("socket".to_string(), socket_obj));

    // output
    let output_obj = Value::Object(vec![
        ("contentType".to_string(), Value::String("application/x-ndjson".to_string())),
        ("lines".to_string(), Value::String("一行一条 JSON，每行以换行结尾".to_string())),
        ("schemaVersion".to_string(), Value::Number(SCHEMA_VERSION.to_string())),
        ("envelope".to_string(), Value::String("schema / type / data（data 就是 App 的 FeedItem / FeedDetail / Comment 序列化）".to_string())),
        ("done".to_string(), Value::String("列表类接口最后一行是 type=done（count / requests / next …）".to_string())),
        ("error".to_string(), Value::String("失败时 HTTP 状态码 != 200，正文只有一行 type=error（也带 requests）".to_string())),
        ("requests".to_string(), Value::String("这次调用自己发起的平台 HTTP 请求数（不是进程级差值，App 自己刷流不算进来）".to_string())),
        ("image".to_string(), Value::String(
            "/image 是唯一一个不回 JSON 的接口：成功时正文就是 image/jpeg 图片本身，尺寸 / 段数等元信息在 X-Feedhub-Width / -Height / -Full-Width / -Full-Height / -Segments / -Seg / -Idx / -Image-Count / -Requests 这些响应头里；失败照旧是一行 type=error".to_string(),
        )),
    ]);
    body_fields.push(("output".to_string(), output_obj));

    // endpoints
    let endpoints_arr = Value::Array(
        ENDPOINTS
            .iter()
            .map(|e| {
                let params_arr = Value::Array(
                    e.params
                        .iter()
                        .map(|&(name, req, desc)| {
                            Value::Object(vec![
                                ("name".to_string(), Value::String(name.to_string())),
                                ("required".to_string(), Value::Bool(req)),
                                ("description".to_string(), Value::String(desc.to_string())),
                            ])
                        })
                        .collect(),
                );
                Value::Object(vec![
                    ("method".to_string(), Value::String(e.method.to_string())),
                    ("path".to_string(), Value::String(e.path.to_string())),
                    ("description".to_string(), Value::String(e.description.to_string())),
                    ("network".to_string(), Value::Bool(e.network)),
                    ("params".to_string(), params_arr),
                ])
            })
            .collect(),
    );
    body_fields.push(("endpoints".to_string(), endpoints_arr));

    // errorCodes
    let error_codes_arr = Value::Array(
        ERROR_CODES
            .iter()
            .map(|e| {
                Value::Object(vec![
                    ("error".to_string(), Value::String(e.code.to_string())),
                    ("httpStatus".to_string(), Value::Number(e.http_status.to_string())),
                    ("retryable".to_string(), Value::Bool(e.retryable)),
                ])
            })
            .collect(),
    );
    body_fields.push(("errorCodes".to_string(), error_codes_arr));

    // platforms
    body_fields.push(("platforms".to_string(), platform_json_arr));

    // limits
    let search_kinds_obj = Value::Object(vec![
        ("values".to_string(), Value::Array(vec![
            Value::String("all".to_string()),
            Value::String("video".to_string()),
            Value::String("post".to_string()),
            Value::String("user".to_string()),
            Value::String("topic".to_string()),
        ])),
        ("note".to_string(), Value::String("user / topic 目前没有平台支持（给了会 not_supported），视频用 video、帖子用 post".to_string())),
    ]);

    let image_limits_obj = Value::Object(vec![
        ("defaultMaxWidth".to_string(), Value::Number("1024".to_string())),
        ("maxMaxWidth".to_string(), Value::Number("2048".to_string())),
        ("jpegQuality".to_string(), Value::Number(JPEG_QUALITY.to_string())),
        ("longRatio".to_string(), Value::Number("3".to_string())),
        ("segmentHeight".to_string(), Value::String("长图每段高 ≈ 缩放后宽 × 1.5，段间重叠 8%（免得接缝切掉一行字）".to_string())),
        ("oneShot".to_string(), Value::String("一次只取一张，没有整帖批量取".to_string())),
        ("commentMaxPages".to_string(), Value::Number(MAX_COMMENT_SEARCH_PAGES.to_string())),
        ("noDiskWrites".to_string(), Value::String("接口自己不写任何文件：图直接写在 HTTP 响应里，agent 自己决定存哪（curl -o）".to_string())),
    ]);

    let limits_obj = Value::Object(vec![
        ("searchMaxPages".to_string(), Value::Number(PAGE_BUDGET.to_string())),
        ("pageMaxLimit".to_string(), Value::Number(MAX_PAGE_LIMIT.to_string())),
        ("libraryMaxLimit".to_string(), Value::Number(MAX_LIBRARY_LIMIT.to_string())),
        ("maxConcurrency".to_string(), Value::Number(MAX_CONCURRENCY.to_string())),
        ("queueDepth".to_string(), Value::Number(QUEUE_DEPTH.to_string())),
        ("requestBudget".to_string(), Value::String("没有：接口不自己限速 / 不记冷却，风控由 App 按它的规矩处理".to_string())),
        ("searchKinds".to_string(), search_kinds_obj),
        ("cursorEncoding".to_string(), Value::String("游标是不透明字符串，里面可能有 & % + = 等字符；当查询参数传时要 URL 编码（curl 用 --data-urlencode）".to_string())),
        ("image".to_string(), image_limits_obj),
    ]);
    body_fields.push(("limits".to_string(), limits_obj));

    let line = json_line("capabilities", Some(Value::Object(body_fields)), Vec::new());
    format!("{line}\n")
}

pub fn render_platforms(platform_items: &[Value], requests: i32) -> String {
    let mut sb = String::new();
    for p in platform_items {
        sb.push_str(&json_line("platform", Some(p.clone()), Vec::new()));
        sb.push('\n');
    }
    let done = json_line(
        "done",
        None,
        vec![
            ("count".to_string(), Value::Number(platform_items.len().to_string())),
            ("requests".to_string(), Value::Number(requests.to_string())),
        ],
    );
    sb.push_str(&done);
    sb.push('\n');
    sb
}
