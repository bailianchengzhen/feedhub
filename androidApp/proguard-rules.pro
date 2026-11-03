# R8 规则
#
# 目标：**只压缩 + 优化，不混淆**。保留类名 / 字段名 / 方法名，崩溃栈直接能读，反射 / 序列化 / JNI 按名字找
# 东西的地方也都还成立；体积主要靠"删掉没人用的代码 + 内联 / 改写成更小的形状"来省，混淆本来也不太省 dex。
# 优化（R8 full mode）照开，不写 -dontoptimize。
-dontobfuscate

# 崩溃栈要能读：保留源文件名和行号（不混淆时类名已经是原名，缺了这两样只剩 "SourceFile:0"）。
# 注解 / Signature / InnerClasses 是 kotlinx.serialization、Compose、反射查泛型信息时要用的；
# 这几个属性一共只占几十 KB。
-keepattributes Signature,InnerClasses,EnclosingMethod,*Annotation*,SourceFile,LineNumberTable

# ---------------------------------------------------------------------------
# 我们自己的代码：收紧 R8 keep 规则（第 6 节构建调优）
# ---------------------------------------------------------------------------
# 不再对 `feedhub.**` 全量 `-keep`，让 R8 full mode 对 `feedhub.app.**`、`feedhub.source.**`、
# `feedhub.core.**`、`feedhub.rust.**` 等做死代码裁剪、去虚化与方法内联；只精确保留真正走反射 / 清单入口的类：
# 1. 清单入口（AAPT2 也会生成，这里显式兜底）：
-keep class feedhub.app.FeedhubApplication { <init>(); }
-keep class feedhub.app.MainActivity { <init>(); }
# 2. `core` 通过 `Class.forName("feedhub.cookies.RustCookieWiring")` 反射桥接 `:rust-bridge`：
-keep class feedhub.cookies.RustCookieWiring { *; }
# 3. `@Serializable` 数据模型保留类头与构造器，配合下方 kotlinx.serialization 规则保证序列化器稳定工作：
-keep @kotlinx.serialization.Serializable class feedhub.** {
    <init>(...);
    <fields>;
}

# Kotlin 的元数据（kotlin.Metadata 注解）在"不混淆"时仍然可能被删；反射 / kotlin-reflect 用到时要有。
-keep class kotlin.Metadata { *; }
-keepclassmembers class **$WhenMappings { <fields>; }

# ---------------------------------------------------------------------------
# 会被反射 / 按名字找的第三方库（各库自带的 consumer rules 一般够，这里只补缺的）
# ---------------------------------------------------------------------------
# kotlinx.serialization：@Serializable 类生成的 `Companion.serializer()` / `$$serializer` 是按名字 / 泛型反射取的，
# 库自带的 consumer rules 已经覆盖 `**$$serializer` 和 `serializer(...)` 方法；这里再显式写一遍，
# 免得哪天库的 rules 没打进 APK 时静默坏掉（我们的 @Serializable 都在 feedhub.** 里，上面已经全留）。
-keepclassmembers @kotlinx.serialization.Serializable class ** {
    *** Companion;
    *** INSTANCE;
    kotlinx.serialization.KSerializer serializer(...);
}
-keepclasseswithmembers class **$$serializer { *; }

# quickjs（YouTube 的 n/sig 用）：native 方法按 JNI 名字（Java_io_github_dokar3_...）注册 / 查找，
# 类名、方法名都必须原样保留。`-dontobfuscate` 已经保住了名字，这里再 keep 掉类本身，防止被压缩掉。
-keep class io.github.dokar3.quickjs.** { *; }

# androidx.sqlite 的 bundled 驱动：JNI（libsqliteJni.so）按类名 / 方法名找 Java 侧，同理保留。
-keep class androidx.sqlite.driver.bundled.** { *; }

# profileinstaller：ProfileInstallReceiver 由 manifest 按类名启动（库的 consumer rules 有 keep，
# 这里写一条是防它被 shrinkResources / manifest 合并后的某个变体绕开），Baseline Profile 全指着它。
-keep class androidx.profileinstaller.ProfileInstallReceiver { *; }

# 本地向量：ONNX Runtime 的 Java 层
# ORT 的 native 层在 JNI_OnLoad 里按类名 / 方法名找 Java 类（`ai.onnxruntime.*`），R8 的 full mode 优化
# 有可能把"只被 native 用"的成员当成死代码删掉。这个包总共几百 KB，整包保留最省心（AAR 自带的
# proguard.txt 只保留了 telemetry 那几个类）。
-keep class ai.onnxruntime.** { *; }
-dontwarn ai.onnxruntime.**

# 它靠 JNA 的反射 + JNI 约定找符号：
# - `UniffiLib` 这个 interface 的方法名就是 Rust 侧导出的符号名（`uniffi_feedhub_ffi_fn_func_*`），
#   被压缩 / 改名就 `UnsatisfiedLinkError`；
# - 序列化用的 `FfiItem` / `FfiPage` / `FfiMedia` 等 record 由生成代码按字段名构造，同样要保。
# JNA 自己的类（Native / Pointer / Structure / Callback）也是按名字找 native 方法。
-keep class uniffi.feedhub_ffi.** { *; }
-keepclassmembers class uniffi.feedhub_ffi.** { *; }
-keep interface uniffi.feedhub_ffi.** { *; }
-dontwarn uniffi.feedhub_ffi.**

-keep class com.sun.jna.** { *; }
-keepclassmembers class com.sun.jna.** { *; }
-keep interface com.sun.jna.** { *; }
# JNA 通过 `com.sun.jna.Library` 的运行时实现（Native.load 生成的代理）调用 .so
-keep class * implements com.sun.jna.Library { *; }
-keepclassmembers class * extends com.sun.jna.Structure { *; }
-dontwarn com.sun.jna.**
# JNA 在安卓上走 libjnidispatch.so（aar 里的 jniLibs），不要因为"看起来没用到"把它裁掉
