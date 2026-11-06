# feedhub

把多个平台的推荐流 / 关注流聚合到一个 App 里看。桌面（Linux）和安卓两个端，后端逻辑是同一份 Rust。

支持的平台：B站、知乎、贴吧、小黑盒、酷安、小红书、抖音、X、YouTube、Reddit、TikTok。

> 非官方客户端，与上述任何平台无关。只读为主：除了关注 / 取关，不做评论、点赞、签到之类的写操作。
> 请遵守各平台的用户协议；账号被风控、被限制的风险由使用者自己承担。

## 能做什么

- **信息流**：各平台的推荐流、关注流按批穿插混排；可以按平台开关、按内容类型过滤、屏蔽关键词。
- **详情和评论**：图文、回答、动态、帖子原生渲染，评论和楼中楼。
- **视频**：桌面用 GStreamer（支持 VA-API 硬解），安卓用 Media3；B站弹幕。
- **搜索**：在选中的平台里一起搜。
- **本地资料库**：收藏、浏览历史存在本机 SQLite；两台设备之间可以在局域网里扫码同步。
- **登录**：大部分国内平台支持扫码登录；桌面也可以从本机 Chrome 导入 Cookie。Cookie 只存在本机。
- **代理**：可以按平台指定代理；内置 sing-box 核心（订阅 / 节点 / 测速），境外平台走代理、代理不可用时不回退直连。
- **本地翻译**（可选）：Project Bergamot + Firefox 离线英译中模型，在本机翻译英文内容，不联网。
- **本地个性化**（可选）：EmbeddingGemma 在本机算向量，只用来调整排序，不删内容。
- **给 agent 用的接口**（桌面，可选，默认关）：本机 unix socket 上的 HTTP 接口，另有命令行工具 `./feedhub`。

## 目录

| 路径 | 内容 |
|---|---|
| `rust/crates/feedhub-core` | 核心：各平台的请求、签名、解析，信息流调度，资料库，播放器决策，同步，翻译 / 向量的调度 |
| `rust/crates/feedhub-net` | HTTP 客户端（带浏览器 / App 的 TLS 与 HTTP/2 指纹） |
| `rust/crates/feedhub-ffi` | 给 Kotlin 用的 UniFFI 绑定 |
| `rust/crates/feedhub-gst` | 桌面播放引擎（GStreamer） |
| `core/`、`rust-bridge/`、`rust-gst/` | Kotlin 侧的模型和对 Rust 的封装 |
| `source-*/` | 平台插件（启用哪些见 `plugins.toml`） |
| `app/` | Compose Multiplatform 界面（桌面 + 安卓共用） |
| `androidApp/` | 安卓壳 |
| `cli/` | 命令行工具 |

## 构建

需要：

- JDK 21
- Rust stable（`cargo`）；编安卓包还要 `rustup target add aarch64-linux-android x86_64-linux-android`
- 桌面播放：GStreamer 1.26 的开发包（Debian / Ubuntu：`libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libgstreamer-plugins-bad1.0-dev`）
- 安卓：Android SDK（compileSdk 37）和 NDK r29
- [sing-box](https://github.com/SagerNet/sing-box) 1.14.3 的发布二进制（GPL-3.0，独立进程运行，只随包分发）：
  `linux-amd64` 那份放到 `deps/sing-box/linux-amd64/sing-box`，`android-arm64` 那份放到
  `deps/sing-box/android-arm64/sing-box`。打包时会校验 sha256；换版本要同时给 `-Pfeedhub.singbox.sha256`
  （安卓是 `-Pfeedhub.singbox.sha256.android`），或者用 `-Pfeedhub.singbox.dir` 指到别的目录。

```bash
./gradlew :app:run                     # 直接运行桌面版
./gradlew :app:packageDeb              # 打 deb 包
./gradlew :androidApp:assembleBenchmark  # 安卓：release 优化 + debug 签名，可以直接装
./feedhub --help                       # 命令行工具（先增量构建）
```

正式发布用的安卓 release 包需要自己配置签名。

### 可选依赖：`deps/`

默认构建不打包本地翻译与本地个性化模型（体积小、零后台模型开销）；需要时传 `-Pfeedhub.translate=true` 或 `-Pfeedhub.embed=true` 启用。大文件（二进制、模型）不进仓库，约定放在仓库根目录的 `deps/` 下（已在 `.gitignore` 里，放软链接也行），也可以用括号里的 Gradle 属性指到别处。下面这些**缺哪样都照常构建**，只是对应功能在 App 里显示"未包含"。

| 放在 | 是什么 | 用来 |
|---|---|---|
| `deps/embeddinggemma` | EmbeddingGemma 的 ONNX 导出（`-Pfeedhub.embedModelDir`） | 本地个性化（`-Pfeedhub.embed=true`） |
| `deps/onnxruntime-linux-x64` | ONNX Runtime 1.31.0（`-Pfeedhub.ort.dir`；还需要 gcc 编一个 JNI 小模块） | 桌面的本地个性化（`-Pfeedhub.embed=true`） |
| `deps/bergamot-src` | [mozilla/translations](https://github.com/mozilla/translations) 源码（含 `inference/` 及子模块，`-Pfeedhub.bergamotSrcDir`；由 `tools/translate/build-bergamot.sh` 编译 JNI 库） | 本地翻译引擎（`-Pfeedhub.translate=true`） |
| `deps/translate/bergamot-enzh` | Firefox 离线英译中模型文件（`-Pfeedhub.translateModelDir`） | 本地翻译模型（`-Pfeedhub.translate=true`） |
| `deps/android-ndk` | NDK r29（也可以用 `ANDROID_NDK_HOME` 或 `-Pfeedhub.ndk.dir`） | 安卓 |
| `deps/gst-prefix` | 离线的 GStreamer 开发前缀（`-Pfeedhub.gstPrefix`） | 系统没装 GStreamer 开发包时 |

## 许可

本仓库的代码按 [MIT](LICENSE) 发布。随仓库带的第三方代码、移植的算法和平台表情图片保留各自的许可与版权，
见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
