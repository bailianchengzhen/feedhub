# 第三方声明

本仓库自己的代码按 [MIT](LICENSE) 发布。下面这些第三方的代码、数据和图片**保留各自的许可与版权**，
不受本仓库 MIT 许可覆盖。通过 Gradle / Cargo 下载的依赖库各自带许可，这里不一一列出；
App 的「设置 → 关于 → 开源许可」里列了主要的运行时依赖。

## 随仓库分发的第三方代码

| 位置 | 上游 | 许可 |
|---|---|---|
| `rust/crates/feedhub-net/vendor/http2/` | [h2](https://github.com/hyperium/h2)（打了一个补丁，见 `rust/crates/feedhub-net/patches/`） | MIT（目录里的 `LICENSE`） |
| `rust/crates/feedhub-net/vendor/wreq-proto/` | [wreq-proto](https://crates.io/crates/wreq-proto) | Apache-2.0（目录里的 `LICENSE`） |
| `source-youtube/src/main/resources/yt_ejs/` | [yt-dlp/ejs](https://github.com/yt-dlp/ejs) 0.8.0 发布产物，内含 meriyah 6.1.4、astring 1.9.0 | Unlicense；meriyah、astring 为 MIT（见同目录 `PROVENANCE.md`） |

打包时会把 [sing-box](https://github.com/SagerNet/sing-box)（GPL-3.0）的发布二进制原样放进 deb / APK，
作为独立进程运行；它不在本仓库里，源码见上游。

## 移植 / 参考的第三方实现

下面这些文件里的算法移植自第三方项目，文件头写明了出处。

| 文件 | 上游 | 许可 |
|---|---|---|
| `source-tiktok/.../TiktokSign.kt`、`rust/crates/feedhub-core/src/tiktok/sign.rs` | [Johnserf-Seed/f2](https://github.com/Johnserf-Seed/f2) `xgnarly.py` | Apache-2.0 |
| `source-douyin/.../AbogusSigner.kt`、`rust/crates/feedhub-core/src/douyin/sign.rs`（a_bogus 部分） | [ylcangel/douyin_sign](https://github.com/ylcangel/douyin_sign)，Copyright AngelToms | Apache-2.0 |
| `rust/crates/feedhub-core/src/heybox/sign.rs` 及 `source-heybox` 的 hkey 签名 | xiaoheihe-cli `xhh_operator/signing.py`，Copyright (c) 2025 Li Fangyu | MIT |
| `rust/crates/feedhub-core/src/tieba/sign.rs` 等贴吧协议字段 | [aiotieba](https://github.com/lumina37/aiotieba) | Unlicense |
| `source-douyin/src/main/resources/.../passport_profile.json` | juelianxyt/douyin-web-qr-login | MIT |
| `rust/crates/feedhub-core/src/douyin/sign.rs`（secsdk 部分）、`source-douyin/.../SecSdkSigner.kt` | [cv-cat/catbus](https://github.com/cv-cat/catbus) `src/platforms/douyin/web/sign.ts`（最初按同一作者的 DouYin_Spider 写，后来作者以 MIT 收进 catbus） | MIT |
| `rust/crates/feedhub-core/src/xhs/{sign,codec,aes}.rs`、`source-xiaohongshu/.../Xhs{Signer,Codec,Aes,Rap}.kt` | [cv-cat/catbus](https://github.com/cv-cat/catbus) `static/xhs/`（同一作者 Spider_XHS 的脚本）；[Cloxl/xhshow](https://github.com/Cloxl/xhshow) | MIT |
| `source-xiaohongshu/src/main/resources/.../sign_profile.json` | cv-cat/catbus `static/xhs/xhs_pc/js/reference_profile.json`、`rap_fingerprint_template.json`；密钥流同 xhshow 的 `HEX_KEY` | MIT |
| `rust/crates/feedhub-core/src/zhihu/sign.rs` | [zhihulite/Hydrogen](https://github.com/zhihulite/Hydrogen) `services/api/zse96.lua` | MIT |
| `rust/crates/feedhub-core/src/coolapk/token.rs`、`source-coolapk/.../AppToken.kt`（v2） | [Coolapk-UWP/Coolapk-API-Collect](https://github.com/Coolapk-UWP/Coolapk-API-Collect) `Coolapk-API/授权/HTTP 首部.md` | CC BY 4.0 |
| 同上（v3）、`source-coolapk/src/main/resources/.../auth_blob.b64` | coolapk-desktop `src-tauri/src/coolapk/auth.rs`；YuleBest/coolapk-cli `internal/auth`（`auth_blob.b64` 原样取自这里） | MIT |
| `source-coolapk/.../CoolapkCaptchaPage.kt` 的验证码参数 | coolapk-desktop `src/utils/neteaseCaptcha.ts` | MIT |
| `rust/crates/feedhub-core/src/x/txid.rs`、`source-x/src/main/resources/.../x_client_profile.json` | [Teskann/QuaX](https://github.com/Teskann/QuaX)，Copyright (c) 2024 Harley Jones | MIT |

## 平台表情图片

`source-coolapk/src/main/resources/feedhub-emoji/`、`source-zhihu/src/main/resources/feedhub-emoji/` 下的表情图片
**版权归各自平台所有**，只用来在本地把表情代码显示成图片，不在本仓库许可范围内。
权利人如有异议请提 issue，会尽快移除。

## 平台接口

本项目与 B站、知乎、百度贴吧、小黑盒、酷安、小红书、抖音、X、YouTube、Reddit、TikTok 均无关联，
所有商标归各自所有者。
