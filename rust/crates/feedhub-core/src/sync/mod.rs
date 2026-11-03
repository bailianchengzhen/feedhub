//! 资料库同步的**纯逻辑**（Kotlin `core/src/main/kotlin/feedhub/core/sync/` 的 Rust 版）。

pub mod b64;
pub mod crypto;
pub mod file;
// 手动导出 / 导入的**编排**：
// 挑收件人 / 密码长度 / 文件头 → 要不要问密码 / 文件名 / 保留期。文件选择器和密钥落盘仍在宿主。
pub mod flow;
pub mod json;
// 登录互传：打包 / 解包 / 一次性局域网服务 / 取与推。
pub mod login;
pub mod merge;
pub mod offer;
pub mod pairing;
pub mod snapshot;
pub mod text;
