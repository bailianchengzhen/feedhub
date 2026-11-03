package feedhub.app.sync

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/**
 * 扫码取景框：安卓用 CameraX 预览 + 逐帧 zxing 解码（不依赖 Google Play 服务，国行 ROM没有）；
 * 桌面没有这个入口，实现是空占位。
 *
 * 只在手机端「登录互传」的弹框里、且还在扫码阶段时组合；一旦 [onCode] 拿到内容，弹框切到"正在同步"，
 * 这个 Composable 离开组合，相机随之解绑（不会后台一直开着相机）。
 *
 * @param onCode 解出一条二维码内容（**已经是我们这种码**才会回调；普通二维码由调用方过滤）。
 * @param onError 相机权限被拒 / 相机起不来时的一行中文原因（权限只在第一次自动申请一次，不反复弹）。
 */
@Composable
expect fun QrScannerView(
    onCode: (String) -> Unit,
    onError: (String) -> Unit,
    modifier: Modifier = Modifier,
)
