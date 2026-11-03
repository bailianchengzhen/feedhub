// Baseline Profile 的生成器 + benchmark。
//
// 为什么没有用 AndroidX 的 `androidx.baselineprofile` Gradle 插件：本机 AGP 是 **9.4.0**，试过插件 1.4.1 和 1.5.0，
// 两者在 apply 时都会崩在 `Extension of type 'TestExtension' does not exist`（AGP 9 把测试扩展注册成了
// `TestExtensionImpl`，插件按旧类型取）。所以这里用它底下同一套东西：`com.android.test` 模块 + macrobenchmark 的
// `BaselineProfileRule`。生成结果从设备的测试 APK 私有目录 pull 回来，再覆盖
// `androidApp/src/main/baselineProfiles/baseline-prof.txt`。
//
// **`benchmark` 构建类型**：测试模块自己也有一个叫 `benchmark` 的构建类型，AGP 会把它和
// `:androidApp` 的 `benchmark` 变体对上（那个变体 = release 的 R8 配置 + debug 签名 + 演示数据开关），
// 所以这里量的是 release 形状的包，而不是 debug 包：
//   ./gradlew :baselineprofile:connectedBenchmarkAndroidTest
// 模拟器上要先 `adb -s emulator-5554 root`（BaselineProfileRule 要读 /data/misc/profiles 下的 profile）。
plugins {
    // 只有仪器化测试、没有 App 代码的模块
    alias(libs.plugins.android.test)
}

android {
    namespace = "feedhub.app.baselineprofile"
    compileSdk = libs.versions.android.compile.sdk.get().toInt()

    defaultConfig {
        // 和 :androidApp 一致（生成器要装在同一台设备上跑）
        minSdk = libs.versions.android.min.sdk.get().toInt()
        targetSdk = libs.versions.android.target.sdk.get().toInt()
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        // emulator-5554 是模拟器：macrobenchmark 默认把"在模拟器上跑"当成错误拦下来，
        // 这里是**故意**在模拟器上量（用户手机不能拿来跑仪器化测试），所以显式放行这一条。
        // 也放行 LOW-BATTERY / UNLOCKED 之类的环境噪音，免得跑一半被环境检查打断。
        testInstrumentationRunnerArguments["androidx.benchmark.suppressErrors"] = "EMULATOR,LOW-BATTERY,UNLOCKED"
    }

    buildTypes {
        // 和 :androidApp 的 benchmark 变体同名，AGP 靠名字把两边对上（test 的 benchmark → app 的 benchmark）。
        // 测试 APK 自己可调试无所谓；被驱动的 feedhub.app 那个是不可调试的 benchmark 包。
        create("benchmark") {
            isDebuggable = true
            signingConfig = signingConfigs.getByName("debug")
            // 依赖里没有 benchmark 变体时回退到 release（AndroidX 模板同款写法）
            matchingFallbacks += listOf("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    // 要测的 App（测试装上这台设备、由 macrobenchmark 驱动它）
    targetProjectPath = ":androidApp"

    // **自己插桩自己**：默认 com.android.test 的 instrumentation 是"插进被测 App 的进程"的，
    // 而我们的被测包是 R8 压过的 release 形状——测试代码和 App 共用进程时，第一次就崩在
    // `NoSuchMethodError: kotlin.jvm.internal.Intrinsics.checkNotNullParameter`（App 里这个方法被 R8 内联掉了，
    // 测试 APK 的类还要调它）。AndroidX 的 macrobenchmark 模块都用这个实验开关把 targetPackage 改成测试包自己。
    experimentalProperties["android.experimental.self-instrumenting"] = true
}

// AGP 的 `checkTestedAppObfuscationBenchmark`：目标 App 开了 R8 就要求测试模块也开压缩，否则构建失败。
// 这个检查是为"测试代码直接调用被测 App 的类、需要同一份 mapping"准备的；我们两个测试**一行 App 代码都不调**
// （全部通过 UI 自动化 + `am start` 的 extra 驱动），不需要 mapping，所以把这个检查关掉，测试 APK 保持不压缩
// ——比给测试 APK 硬塞一套 R8 规则（还要为 macrobenchmark 的一堆可选依赖写 -dontwarn）更干净。
tasks.matching { it.name.startsWith("checkTestedAppObfuscation") }.configureEach { enabled = false }

dependencies {
    // macrobenchmark 的 @Rule：BaselineProfileRule 负责"记下这次跑过的类 / 方法"，
    // MacrobenchmarkRule 用来量冷启动 / 帧耗时
    implementation(libs.androidx.benchmark.macro.junit4)
    implementation(libs.androidx.test.ext.junit)
    // 找控件、滑动、返回
    implementation(libs.androidx.test.uiautomator)
}
