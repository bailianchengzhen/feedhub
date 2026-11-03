package feedhub.app.sync

import android.Manifest
import android.content.pm.PackageManager
import android.os.SystemClock
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.common.HybridBinarizer
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/**
 * 同一进程里只在**第一次**打开扫码框时自动申请相机权限。用户拒绝过之后不再弹系统框
 * 只显示一行"去系统设置里打开"。重新打开 App 后才会再申请一次。
 */
@Volatile
private var permissionAutoAsked = false

/**
 * 安卓的扫码取景框：CameraX `Preview` + `ImageAnalysis`（YUV 的 Y 平面直接喂 zxing，
 * 不用先转 Bitmap）。只在有相机权限时绑定；离开组合就 `unbindAll`。
 */
@Composable
actual fun QrScannerView(onCode: (String) -> Unit, onError: (String) -> Unit, modifier: Modifier) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    var granted by remember {
        mutableStateOf(ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED)
    }
    var denialMessage by remember { mutableStateOf<String?>(null) }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { ok ->
        granted = ok
        if (!ok) {
            denialMessage = "没有相机权限，扫不了码。到系统设置 → 应用 → feedhub → 权限里打开相机，再回来。"
            onError(denialMessage!!)
        }
    }
    LaunchedEffect(Unit) {
        if (granted) return@LaunchedEffect
        if (!permissionAutoAsked) {
            permissionAutoAsked = true
            launcher.launch(Manifest.permission.CAMERA)
        } else {
            denialMessage = "没有相机权限，扫不了码。到系统设置 → 应用 → feedhub → 权限里打开相机，再回来。"
            onError(denialMessage!!)
        }
    }

    if (!granted) {
        Box(modifier, contentAlignment = Alignment.Center) {
            Text(
                denialMessage ?: "需要相机权限才能扫码",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        return
    }

    val previewView = remember {
        PreviewView(context).apply {
            scaleType = PreviewView.ScaleType.FILL_CENTER
            implementationMode = PreviewView.ImplementationMode.COMPATIBLE
        }
    }
    val found = remember { AtomicBoolean(false) }
    DisposableEffect(lifecycleOwner, previewView) {
        val executor = Executors.newSingleThreadExecutor()
        val providerFuture = ProcessCameraProvider.getInstance(context)
        var provider: ProcessCameraProvider? = null
        val reader = MultiFormatReader().apply {
            setHints(
                mapOf(
                    DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE),
                    DecodeHintType.TRY_HARDER to true,
                ),
            )
        }
        var lastAttempt = 0L
        val listener = Runnable {
            try {
                val cameraProvider = providerFuture.get()
                provider = cameraProvider
                val preview = Preview.Builder().build().also { it.surfaceProvider = previewView.surfaceProvider }
                val analysis = ImageAnalysis.Builder()
                    .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                    .build()
                analysis.setAnalyzer(executor) { image ->
                    try {
                        if (found.get()) return@setAnalyzer
                        val now = SystemClock.elapsedRealtime()
                        // 每帧都解太费电；二维码又不会动，~6 帧/秒足够
                        if (now - lastAttempt < 150) return@setAnalyzer
                        lastAttempt = now
                        val text = decodeQr(image, reader)
                        if (text != null) {
                            found.set(true)
                            previewView.post { onCode(text.trim()) }
                        }
                    } catch (e: Exception) {
                        // 某一帧解不出来很正常，继续下一帧
                    } finally {
                        image.close()
                    }
                }
                cameraProvider.unbindAll()
                cameraProvider.bindToLifecycle(
                    lifecycleOwner,
                    CameraSelector.DEFAULT_BACK_CAMERA,
                    preview,
                    analysis,
                )
            } catch (t: Throwable) {
                previewView.post { onError("相机起不来：${t.message ?: t::class.simpleName}") }
            }
        }
        providerFuture.addListener(listener, ContextCompat.getMainExecutor(context))
        onDispose {
            found.set(true)
            runCatching { provider?.unbindAll() }
            executor.shutdown()
        }
    }
    AndroidView(factory = { previewView }, modifier = modifier.fillMaxSize())
}

/**
 * 一帧 YUV → zxing。只用 Y 平面（二维码只需要亮度），`rowStride` 可能比宽度大（对齐），
 * 所以按 rowStride 建缓冲、再让 `PlanarYUVLuminanceSource` 裁掉右边的填充。
 */
private fun decodeQr(image: ImageProxy, reader: MultiFormatReader): String? {
    val plane = image.planes.firstOrNull() ?: return null
    val width = image.width
    val height = image.height
    val rowStride = plane.rowStride
    val pixelStride = plane.pixelStride
    if (width <= 0 || height <= 0 || rowStride < width || pixelStride != 1) return null
    val buffer = plane.buffer
    val data = ByteArray(rowStride * height)
    buffer.rewind()
    val read = minOf(buffer.remaining(), data.size)
    if (read < width) return null
    buffer.get(data, 0, read)
    return try {
        val source = PlanarYUVLuminanceSource(data, rowStride, height, 0, 0, width, height, false)
        reader.decodeWithState(BinaryBitmap(HybridBinarizer(source))).text
    } catch (e: Exception) {
        null
    } finally {
        reader.reset()
    }
}
