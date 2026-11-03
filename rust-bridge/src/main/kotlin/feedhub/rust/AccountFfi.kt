package feedhub.rust

import feedhub.core.FollowKind
import feedhub.core.FollowTarget
import feedhub.core.QrLogin
import uniffi.feedhub_ffi.FfiFollowKind
import uniffi.feedhub_ffi.FfiFollowTarget
import uniffi.feedhub_ffi.FfiQrLogin
import uniffi.feedhub_ffi.FfiQrParam

/**
 * **账号层的 Kotlin → Rust 方向的转换**：`FollowTarget` / `QrLogin` → `Ffi*`。
 *
 * 反方向的（`Ffi*` → 模型）在 [RustModel]（`toFollowTarget` / `toQrLogin` / `toQrState` / `toFollowPage`），
 * 那几样是几家共用的，放一起；这两个只有"写操作 / 轮询把二维码交回去"用得上，单独一个文件。
 * 单独一个文件还有个好处：平台门面（`Rust<平台>Hosted`）里不用重复写这两段映射。
 */
internal fun FollowTarget.toFfi(): FfiFollowTarget = FfiFollowTarget(
    platform = platform.key,
    kind = when (kind) {
        FollowKind.USER -> FfiFollowKind.USER
        FollowKind.FORUM -> FfiFollowKind.FORUM
    },
    id = id,
    name = name,
    avatar = avatar,
    description = description,
    url = url,
)

/** 轮询时把前端拿着的二维码原样交回 Rust（`extra` 里的 `gid` 也要带回去，贴吧要用）。 */
internal fun QrLogin.toFfi(): FfiQrLogin = FfiQrLogin(
    platform = platform.key,
    content = content,
    token = token,
    extra = extra.map { (k, v) -> FfiQrParam(k, v) },
    expiresAtMs = expiresAtMs,
    pollIntervalMs = pollIntervalMs,
    hint = hint,
)

