package feedhub.app.baselineprofile

import androidx.benchmark.macro.BaselineProfileMode
import androidx.benchmark.macro.CompilationMode
import androidx.benchmark.macro.FrameTimingMetric
import androidx.benchmark.macro.StartupMode
import androidx.benchmark.macro.StartupTimingMetric
import androidx.benchmark.macro.junit4.MacrobenchmarkRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.uiautomator.By
import androidx.test.uiautomator.Until
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * 量 R8 / Baseline Profile 的效果。跑的是 `benchmark` 构建类型（= release 的 R8 配置、
 * `isDebuggable = false`），**断网 + 演示数据**（`--ez demo true`，一个请求也不发）。
 *
 * - [coldStartup]：`StartupTimingMetric` 的 `timeToInitialDisplay`，冷启动 5 次；
 * - [homeScroll]：`FrameTimingMetric` 的 `frameDurationCpuMs` p50 / p90，首页上下滑各 3 屏，5 次。
 *
 * `CompilationMode.Partial(baselineProfileMode = Require)`：APK 里没有 baseline profile 就直接失败，
 * 这样量到的"改后"确实是 profile 生效的数字，不会悄悄退化成 None。
 *
 * 跑法（模拟器 / 真机，需要 root）：
 * ```
 * ./gradlew :baselineprofile:assembleBenchmark
 * adb -s emulator-5554 install -r androidApp/build/outputs/apk/benchmark/androidApp-benchmark.apk
 * adb -s emulator-5554 install -r baselineprofile/build/outputs/apk/benchmark/baselineprofile-benchmark.apk
 * adb -s emulator-5554 shell am instrument -w \
 *   -e class feedhub.app.baselineprofile.FeedhubBenchmark#coldStartup \
 *   feedhub.app.baselineprofile/androidx.test.runner.AndroidJUnitRunner
 * ```
 */
@RunWith(AndroidJUnit4::class)
class FeedhubBenchmark {
    @get:Rule
    val rule = MacrobenchmarkRule()

    @Test
    fun coldStartup() = rule.measureRepeated(
        packageName = "feedhub.app",
        metrics = listOf(StartupTimingMetric()),
        iterations = 5,
        startupMode = StartupMode.COLD,
        compilationMode = CompilationMode.Partial(baselineProfileMode = BaselineProfileMode.Require),
    ) {
        pressHome()
        startActivityAndWait { it.putExtra("demo", true) }
        device.wait(Until.hasObject(By.pkg("feedhub.app").depth(0)), 10_000)
    }

    @Test
    fun homeScroll() = rule.measureRepeated(
        packageName = "feedhub.app",
        metrics = listOf(FrameTimingMetric()),
        iterations = 5,
        startupMode = StartupMode.COLD,
        compilationMode = CompilationMode.Partial(baselineProfileMode = BaselineProfileMode.Require),
    ) {
        startActivityAndWait { it.putExtra("demo", true) }
        device.wait(Until.hasObject(By.pkg("feedhub.app").depth(0)), 10_000)
        val w = device.displayWidth
        val h = device.displayHeight
        // 首页往下滑 3 屏，再滑回顶部：一屏一屏地滑，帧耗时都算在这一次迭代里
        repeat(3) {
            device.swipe(w / 2, (h * 0.80f).toInt(), w / 2, (h * 0.30f).toInt(), 30)
            device.waitForIdle()
        }
        repeat(3) {
            device.swipe(w / 2, (h * 0.30f).toInt(), w / 2, (h * 0.80f).toInt(), 30)
            device.waitForIdle()
        }
    }
}
