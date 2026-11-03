//! **宿主能力（"端口"）**：Rust 侧定义、宿主（桌面 / 安卓的 Kotlin + OkHttp）实现。
//!
//! 这是"平台层搬进 Rust"之后 Rust 还需要宿主的全部东西——**一共三件**：
//!
//! | trait | 宿主那边是什么 | 为什么留在宿主 |
//! | [`HttpHost`] | `OkHttpClient.newCall(...).execute()` | TLS 指纹：桌面是 JDK SunJSSE + OkHttp `MODERN_TLS`，安卓是 Conscrypt；换 rustls 会同时改 JA3 / 扩展顺序 / h2 SETTINGS，平台的连接特征必须**一个字节都不变** |
//! | [`StoreHost`] | `ctx.store`（`KeyValueStore`） | 里面存的是平台认设备 / 认签名的值（WBI 的 mixin key 之类），必须**原样沿用同一份**，不能有两份缓存 |
//! | [`CookieHost`] | `ctx.cookies`（`FeedCookieJar`） | 安卓上 `CookieManager` 是登录态的唯一真相，搬进 Rust 就要同步，多一个"用户以为登录了其实没有"的 bug 面 |
//!
//! 三件都是**同步阻塞**的：Kotlin 在 `Dispatchers.IO` 上调 Rust，Rust 同步回调宿主执行 OkHttp，宿主阻塞等响应。
//! 这样 `.so` 里**不需要 tokio**（没有 Future、没有执行器），线程模型只有"调用方那一个线程"。
//!
//! 这一层故意**不认识任何平台的类型**：只有请求 / 响应 / Cookie / 错误四样。加平台时只在这上面加"平台对象"。

/// 一个要发给平台的请求。**拼 URL / 选头 / 组 body 全在 Rust 里做**，宿主只负责发。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpRequest {
    /// 大写方法名（`GET` / `POST`）。
    pub method: String,
    /// **完整 URL**（带 query、已经签好名）。宿主不许再改。
    pub url: String,
    /// **有序**请求头：宿主必须按这个顺序写进去，且**不要**再补同名头。重复的名字允许出现多次（按顺序保留）。
    pub headers: Vec<(String, String)>,
    /// body 字节；空 = 没有 body（GET）。
    pub body: Vec<u8>,
}

/// 宿主的响应。状态码 / 头 / body 都不做解释，判定（412、`{code}` 外壳……）在 Rust 里做。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpResponse {
    pub status: u16,
    /// **有序**响应头（同名多次按原顺序保留）。
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// 跟随后得到的最终 URL（重定向之后）。排障用。
    pub final_url: String,
}

/// 宿主执行请求时可能遇到的错误。**必须分类带回来**：风控红线和退出码都靠这个区分。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// 连不上 / 超时 / 证书 / 连接被重置……（Kotlin 侧是 `java.io.IOException`）。
    #[error("网络错误：{detail}")]
    Network { detail: String },

    /// **fail-closed**：这个平台要走代理，但这次没有可用的代理，宿主**一个字节都没发**。
    /// 上层映射成 `ProxyUnavailableException`（cli 的退出码 6），和普通网络错误分开。
    #[error("代理不可用：{detail}")]
    ProxyUnavailable { detail: String },

    /// 宿主自己的意外（回调里抛了别的异常、编码问题）。别让它变成一次崩溃。
    #[error("宿主错误：{detail}")]
    Other { detail: String },
}

/// 发一个请求、拿一个响应。同步。
pub trait HttpHost: Send + Sync {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HostError>;
}

/// 键值存储（对应 `ctx.store`）。值和键都是原样的字符串，Rust 不解释宿主的格式。
pub trait StoreHost: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    fn put(&self, key: &str, value: Option<&str>);
}

/// 要种进宿主 Cookie 罐的一条 Cookie（B站 的 `buvid3` / `buvid4` 是接口发的，要自己种）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieSpec {
    pub name: String,
    pub value: String,
    /// `bilibili.com`（不带前导点 = host-only，和 Kotlin `Cookie.Builder` 的默认一致）。
    pub domain: String,
    pub path: String,
    /// 毫秒时间戳。
    pub expires_at_ms: i64,
}

/// 宿主 Cookie 罐的读写（对应 `ctx.cookies`）。
pub trait CookieHost: Send + Sync {
    /// 发往 [`url`] 的请求会带上的、名为 [`name`] 的值。判"登录态 / 设备标识在不在"只用它。
    fn find(&self, url: &str, name: &str) -> Option<String>;

    /// 把这几条 Cookie 种进罐里（不会顶掉别的域名）。
    fn put(&self, url: &str, cookies: Vec<CookieSpec>);
}

// ===========================================================================
// 扫码登录
// ===========================================================================

/// 一条**带完整属性**的 Cookie（`okhttp3.Cookie` 的字段一对一）。
///
/// 为什么不用 [`CookieSpec`]：扫码登录成功时要把临时罐里的 Cookie **原样**搬进 `ctx.cookies`
/// （`commitQrLogin` 的语义），`secure` / `httpOnly` / `hostOnly` 三个属性会影响
/// `Cookie.matches(url)`（`hostOnly=false` 的域 Cookie 才在 `www.` 子域上带得上）和之后写回 `cookies.json`
/// 的存档，丢一个就是"用户以为登录了其实没有"的 bug。Kotlin 侧的 `FeedCookieJar` 直接吃 `okhttp3.Cookie`，
/// 所以边界上按这几个字段原样过。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JarCookie {
    pub name: String,
    pub value: String,
    /// `bilibili.com`（不带前导点；`host_only` 决定它是不是只匹配这个主机）。
    pub domain: String,
    pub path: String,
    /// 毫秒时间戳。
    pub expires_at_ms: i64,
    pub secure: bool,
    pub http_only: bool,
    /// `true` = host-only（`Cookie.Builder.hostOnlyDomain`），`false` = 域 Cookie（`Cookie.Builder.domain`）。
    pub host_only: bool,
}

/// **扫码登录的宿主能力**：临时 Cookie 罐 + 落地。
///
/// 为什么不是第三条宿主端口就能做完：扫码流程和 `ctx.cookies` **必须隔离**（`core/QrLoginSupport.kt` 的
/// `QrSessions`：成功之前一格都不动，取消 / 过期不能影响现有登录态），而临时罐必须挂在**发请求的那个
/// OkHttp 客户端**上（`client.newBuilder().cookieJar(临时罐)`）才能收 Set-Cookie、也只带临时罐里的 Cookie。
/// 罐子在宿主实现（`MemoryCookieJar` / OkHttp 的 `CookieJar` 都在 Kotlin 那边），Rust 只按句柄用。
///
/// 落地（[`QrHost::commit`]）也是宿主：`remove`（清旧登录 Cookie，设备标识不动）+ `put`（写新的，顺序照传）。
/// 这一对必须原子，否则中途失败会留下"旧的删了新的没写"的半截登录态——Kotlin 的 `commitQrLogin` 是同一个语义。
///
/// 图解码（[`QrHost::decode_qr_image`]）也是宿主：百度通行证给的二维码是一张 PNG，内容是扫出来的文字
/// （`t` / `local` 由服务端生成，拼不出来），而 zxing 只有 Java 版。Rust 只负责把图取回来。
pub trait QrHost: Send + Sync {
    /// `ctx.cookies.all()`：知乎 / 抖音的扫码要从本机设备 Cookie 起步（换掉登录凭据）。
    fn all(&self) -> Vec<JarCookie>;

    /// 开一个**空的**临时罐，返回句柄（宿主内部编号，Rust 原样带回）。
    /// 语义同 `QrSessions.begin(token, MemoryCookieJar())`，但 token 由 Rust 记（宿主不认识平台 token）。
    fn begin(&self) -> String;

    /// 用临时罐发一个请求（宿主：`client.newBuilder().cookieJar(临时罐)` + 可选的读超时）。
    /// [`timeout_ms`]：贴吧的 `unicast` 是长轮询，只有那一跳把读超时调大（Kotlin 的
    /// `readTimeout(unicastReadTimeoutS, SECONDS)`）；`None` = 平台客户端自己的超时。
    fn execute(
        &self,
        session: &str,
        request: HttpRequest,
        timeout_ms: Option<i64>,
    ) -> Result<HttpResponse, HostError>;

    /// 临时罐里的全部 Cookie（`jar.all()` 的顺序），带完整属性。
    fn cookies(&self, session: &str) -> Vec<JarCookie>;

    /// `jar.find(url, name)`：知乎的 `d_c0` / `_xsrf`、抖音的 `msToken` 这些要从临时罐里读。
    fn find(&self, session: &str, url: &str, name: &str) -> Option<String>;

    /// `jar.put(url, cookies)`：扫码响应体里直接带的 Cookie（B站的 `crossDomain` 参数、知乎的 `z_c0`）补进临时罐。
    fn put(&self, session: &str, url: &str, cookies: Vec<JarCookie>);

    /// 丢掉临时罐（终态）。找不到就什么都不做。
    fn end(&self, session: &str);

    /// 登录成功后的落地：**先**删掉 [`remove_domain`]（含子域）下名字在 [`remove_names`] 里的旧登录 Cookie，
    /// **再**把 [`cookies`] 原样写进 `ctx.cookies`（同名同域同路径的会替换）。和 `commitQrLogin` 同一个顺序。
    fn commit(
        &self,
        url: &str,
        remove_domain: &str,
        remove_names: Vec<String>,
        cookies: Vec<JarCookie>,
    );

    /// 二维码图片 → 里面的文字（不支持 / 解不出来给 `None`，调用方按平台原来的报错处理）。
    fn decode_qr_image(&self, png: Vec<u8>) -> Option<String>;
}

// ===========================================================================
// 签名随机量 / 时钟：**一次调度里要发好几个请求**的平台才用得上
// ===========================================================================
//
// 别的平台是"Kotlin 先抽好一份随机量、调一次 Rust、发一个请求"，所以随机量和时间都是参数。
// 小红书关注流不一样：**一次刷新要发 N 个请求**（关注列表 1~30 页、作者主页每人 1 个、extra 每批 1 个），
// 每个请求的签名随机量都不一样，而且补位长度取决于那个请求的 body——所以只能让 Rust 在
// **要发请求的那一刻**问宿主。两个 trait 就是那个口子（`XhsHostedSource::with_drive` 挂上去）。
//
// 为什么要分 [`SignRandomHost::draw_head`] / [`SignRandomHost::draw_tail`] 两步：Kotlin `XhsSigner`
// 的随机取用顺序是
// `version → [xy-direction] → xray 尾巴 → b3 → (rap: 掩码 → 会话密钥 → 密钥 → 盐) →（这时才知道补位长度）→ 补位 → 加密耗时`，
// 而补位长度又要用掩码 + 会话密钥算（`x-rap-param` 的 gzip 长度）。所以宿主先抽前半段，
// Rust 纯算出补位长度、再回来抽后半段。**两步都用宿主那一个 `Random`**，顺序天然和 Kotlin 逐位对齐，
// 而且宿主不需要记"上一笔抽到哪了"这种中间状态。

/// [`SignRandomHost::draw_head`] 的结果：一次签名里**补位长度之前**的那几笔随机量。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignRandomHead {
    /// `random.nextInt()`：x3 里的 version。
    pub version: i32,
    /// 没有 user_id 时的 `xy-direction`（Kotlin 是 `10 + random.nextInt(91)`）；有 user_id 时忽略。
    pub xy_random: i32,
    /// `x-xray-traceid` 尾巴的 16 个十六进制字符。
    pub xray_tail: String,
    /// `x-b3-traceid` 的 16 个十六进制字符。
    pub b3_trace_id: String,
    /// `x-rap-param` 的掩码字节（网页从 `0-9a-z` 里挑一个字符的码点）。
    pub rap_mask: i32,
    /// 16 字节会话密钥（ASCII）。
    pub rap_session_key: String,
    /// 16 字节随机密钥（ASCII）。
    pub rap_key: String,
    /// 4~6 个 ASCII 字符的盐。
    pub rap_salt: String,
}

/// [`SignRandomHost::draw_tail`] 的结果：**补位长度之后**的那两笔。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignRandomTail {
    /// 补到 16 倍数的补位字符（长度由 Rust 算好传进来）。
    pub rap_pad: String,
    /// 信封里的"加密耗时"字段（Kotlin 是 `60 + random.nextInt(181)`）。
    pub rap_enc_time_ms: i32,
}

/// **签名随机量的来源**（宿主从自己的 `Random` 里按 `XhsSigner` 的取用顺序抽）。
///
/// 两个方法在**同一线程、同一次调用里**先后被调，宿主实现里那个 `Random` 必须是同一个实例
/// （否则和 Kotlin 参照实现的随机流对不齐）。
pub trait SignRandomHost: Send + Sync {
    /// 抽"补位长度之前"的那几笔。[`xy`] 为真且 [`xy_seed`] 是空串 / 没有时才抽 `xy_random`
    /// （有种子时 `xy-direction` 是 murmur3 算出来的，不占随机流）。
    fn draw_head(&self, xy: bool, xy_seed: Option<&str>, rap: bool) -> SignRandomHead;

    /// 抽"补位长度之后"的那两笔：`pad_len` 个补位字符 + 加密耗时。
    fn draw_tail(&self, pad_len: usize) -> SignRandomTail;
}

/// **时钟**（宿主给，`System.currentTimeMillis()`）。
///
/// 为什么不让 Rust 自己读：一次刷新里 Kotlin 会调很多次 `now()`（刷新预算的 deadline、每个作者的
/// `atMs`、`extraAt`），对拍要注入同一个假钟才能逐格比 `fetchedAt` / `extraAt` / `closedUntil`。
pub trait ClockHost: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// **"我自己是谁"**：`user_id` 这类的"要发请求才能知道"的身份。
///
/// 谁在用：小红书关注流（`whoami` 没搬进 Rust，而 `xhs.user_id` 没存过时要现问一次 `user/me`）。
/// Rust 在**真要拉关注列表的那一刻**才问（翻页 / 缓存还新鲜的那一轮一个请求都不发）。
pub trait SelfIdHost: Send + Sync {
    fn self_id(&self) -> Option<String>;
}

// ===========================================================================
// 签名随机量
// ===========================================================================

/// **a_bogus 的随机数来源**（宿主从自己的 `Random` 里按签名时的顺序抽）。
///
/// 为什么要有它：以前调用方"先问个数、再取"，一次调用只签一个请求，所以数量是确定的。
/// 要不要发、发几次只有当 Rust 解析完响应才知道。硬让调用方多抽几个会**改掉随机流**
/// （和 Kotlin 参照实现从第二个请求起就对不齐），所以改成"签的那一刻问宿主抽"——抽的个数、顺序
/// 天然和 Kotlin 的 `AbogusSigner` 逐位一致。
pub trait RandomHost: Send + Sync {
    /// 抽 `count` 个 `[0, 1)` 的随机数（`Math.random()` 的口径）。
    fn draw(&self, count: usize) -> Vec<f64>;
}

// ===========================================================================
// JS 求解
// ===========================================================================

/// **在宿主里跑外来 JS 的签名引擎**（现在只有 YouTube 的 n/sig：yt-dlp 的 EJS 跑在 quickjs-kt 里）。
///
/// 为什么留在宿主：EJS 是几 MB 的 JS、要在 QuickJS 里跑（桌面是 JNI、安卓是 AAR 里的 `libquickjs.so`），
/// 而 `.so` 里既不带 JS 引擎也不想带（体积 + 一段解释执行的 CPU 不该在 Rust 这侧占着）。
/// Rust 只负责"**要解什么**"（扫 `signatureCipher` 的 `s`、直链里的 `n`）和"**答案拼回哪里**"。
///
/// **整批一次**：一次播放可能有两组（`sig` 一次、`n` 一次），也可能只有一组；别一条挑战过一次边界。
/// 调用是**同步**的（和 HTTP 端口一样，宿主在 `Dispatchers.IO` 上阻塞等 QuickJS 跑完）。
///
/// 失败语义：宿主解不出来就返回**空** `Vec`（= 一个答案都没有），Rust 那边照 Kotlin 的
/// `catch (e: Exception)` 处理（这个客户端算失败、换下一个），**不重试、不换参数**。
pub trait ScriptHost: Send + Sync {
    /// `requests` 是 `(类型, 挑战列表)`，类型只有 `"sig"` / `"n"`；返回 `(挑战, 答案)`。
    /// [`player_key`] 是播放器 hash（同一个脚本只编译一次，编译结果由宿主缓存）。
    fn solve(
        &self,
        player_js: &str,
        player_key: &str,
        requests: Vec<(String, Vec<String>)>,
    ) -> Vec<(String, String)>;
}
