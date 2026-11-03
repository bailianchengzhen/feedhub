#!/bin/bash
# 交叉编译安卓用的 llama-server，**带 Hexagon NPU 和 Adreno OpenCL 后端**。
#
# Gradle 的 `buildLlamaServerAndroid` 直接调这个脚本：`arm64-v8a`（CPU + OpenCL + Hexagon 一份二进制）+ `x86_64`（只 CPU，模拟器用）。
#
# 产物按 ABI 目录摆好，Gradle 的 `stageLlamaServerAndroid` 原样拷进 jniLibs。
#
# ## 产物（每个 ABI 一份）
#
# ```
# androidApp/build/llama-server-jniLibs/<abi>/libllamaserver.so   ← 可执行文件（静态链接，见下）
# androidApp/build/llama-server-jniLibs/<abi>/libggml-htp-v79.so  ← Hexagon 的 NPU 侧内核
# androidApp/build/llama-server-jniLibs/<abi>/libOpenCL.so        ← Khronos ICD loader（安卓系统那份在 /system/vendor）
# ```
#
# 为什么 `llama-server` 是个 `.so`：Android 只允许执行 `nativeLibraryDir` 下的文件
# （`androidApp/build.gradle.kts` 的 `jniLibs.useLegacyPackaging = true` 就是为了让它**真被解出来**）。
# 内容是可执行文件，`.so` 只是安装器认它的条件 —— 和 sing-box 一个办法。
#
# 为什么静态链接：llama.cpp 默认 `BUILD_SHARED_LIBS=ON`，会切出 libllama / libggml* / libllama-common
# 十几个 .so，而 Android 的可执行文件只能从 nativeLibraryDir 找依赖。静态链成一份就只有一个文件。
# `libggml-htp-v79.so` **不能**静态（它是给 DSP 侧 FastRPC 加载器的 skel，必须是共享库），
# 单独摆出来、靠 `ADSP_LIBRARY_PATH` 指过去（[`mobile_env`](rust/crates/feedhub-core/src/translate/server.rs)）。
#
# ## 两个后端各自的额外依赖
#
# - **Hexagon**：`HEXAGON_SDK_ROOT`（6.7.0.0 够用：`build/cmake/hexagon_arch.cmake` 里有 v79）
#   + `HEXAGON_TOOLS_ROOT`（SDK 里的 `tools/HEXAGON_Tools/<版本>`）。构建时用 QCC 交叉编 DSP 侧代码。
# - **OpenCL**：`OpenCL-Headers` 的 `CL/` + 一个链接期的 `libOpenCL.so` 桩。安卓运行时的
#   OpenCL 在 `/system/vendor/lib64/libOpenCL.so`（高通的），但 NDK sysroot 里**没有**它，
#   所以链接期用我们自己编的 Khronos ICD loader（`build_opencl_icd()`，也顺手进 APK——它按
#   `/system/vendor/Khronos/OpenCL/vendors/*.icd` 找厂商实现）。
#
# ## 用法
#
# 要 SDK + NDK + cmake/ninja，编一次比较久：
#
#     tools/translate/build-android.sh
#     tools/translate/build-android.sh arm64-v8a x86_64
#
# 找不到 llama.cpp 源码时**跳过**（不报错）：APK 照常出，只是不带翻译引擎。
#
# 可覆盖的环境变量：
#   LLAMA_CPP_SRC         llama.cpp 源码，tag b11700（默认 deps/llama.cpp-src）
#   NDK                   NDK 根目录（默认 deps/android-ndk）
#   HEXAGON_SDK_ROOT      默认 deps/hexagon-sdk
#   HEXAGON_TOOLS_ROOT    默认 $HEXAGON_SDK_ROOT/tools/HEXAGON_Tools/21.0.04
#   OPENCL_HEADERS        Khronos OpenCL-Headers（默认 deps/OpenCL-Headers）
#   OPENCL_ICD_SRC        Khronos OpenCL-ICD-Loader（默认 deps/OpenCL-ICD-Loader）
#   OUT                   jniLibs 输出目录（默认 androidApp/build/llama-server-jniLibs）
#   BUILD_ROOT            构建目录（默认 androidApp/build/llama-server-build）
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
LLAMA_CPP_SRC=${LLAMA_CPP_SRC:-$ROOT/deps/llama.cpp-src}
NDK=${NDK:-$ROOT/deps/android-ndk}
HEXAGON_SDK_ROOT=${HEXAGON_SDK_ROOT:-$ROOT/deps/hexagon-sdk}
HEXAGON_TOOLS_ROOT=${HEXAGON_TOOLS_ROOT:-$HEXAGON_SDK_ROOT/tools/HEXAGON_Tools/21.0.04}
OPENCL_HEADERS=${OPENCL_HEADERS:-$ROOT/deps/OpenCL-Headers}
OPENCL_ICD_SRC=${OPENCL_ICD_SRC:-$ROOT/deps/OpenCL-ICD-Loader}
OUT=${OUT:-$ROOT/androidApp/build/llama-server-jniLibs}
BUILD_ROOT=${BUILD_ROOT:-$ROOT/androidApp/build/llama-server-build}
VERSION_TAG=b11700
# 26 = Android 8.0：和 App 的 minSdk 一致；Hexagon 的 FastRPC / OpenCL 都在这之上。
ANDROID_API=${ANDROID_API:-26}
JOBS=${JOBS:-$(nproc)}
# 骁龙 8 Elite 是 **HTP v79** / Adreno 830；别的芯片用 HTP_ARCH 改。
HTP_ARCH=${HTP_ARCH:-v79}

[ -d "$LLAMA_CPP_SRC" ] || { echo "找不到 llama.cpp 源码：$LLAMA_CPP_SRC，跳过（APK 不带翻译引擎）" >&2; exit 0; }
[ -d "$NDK" ] || { echo "找不到 NDK：$NDK" >&2; exit 2; }
TOOLCHAIN=$NDK/build/cmake/android.toolchain.cmake
[ -f "$TOOLCHAIN" ] || { echo "NDK 里没有 $TOOLCHAIN" >&2; exit 2; }

# 版本只读地确认一眼（不 checkout、不改源码树）。源码树可能被别处共用，这里不切版本。
have=$(git -C "$LLAMA_CPP_SRC" describe --tags --exact-match 2>/dev/null || true)
if [ "$have" != "$VERSION_TAG" ]; then
  echo "llama.cpp 源码不在 $VERSION_TAG（现在是 '${have:-未知}'）：$LLAMA_CPP_SRC" >&2
  echo "  这个脚本不改第三方源码树；请先在那边 checkout $VERSION_TAG。" >&2
  exit 3
fi

if [ $# -gt 0 ]; then
  ABIS=("$@")
else
  ABIS=(arm64-v8a)
fi

BIN=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
STRIP=$BIN/llvm-strip
[ -x "$STRIP" ] || { echo "NDK 里没有 llvm-strip：$STRIP" >&2; exit 6; }

# ---------------------------------------------------------------------------
# OpenCL：头文件 + ICD loader（每个 ABI 一份，缓存在 BUILD_ROOT/opencl-<abi>）
# ---------------------------------------------------------------------------
build_opencl_icd() {
  local abi=$1 out=$2
  local dir=$BUILD_ROOT/opencl-icd-$abi
  echo "=== 编 Khronos OpenCL ICD loader（$abi）==="
  cmake -S "$OPENCL_ICD_SRC" -B "$dir" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_TOOLCHAIN_FILE="$TOOLCHAIN" \
    -DANDROID_ABI="$abi" \
    -DANDROID_PLATFORM="android-$ANDROID_API" \
    -DANDROID_STL=c++_shared \
    -DOPENCL_ICD_LOADER_HEADERS_DIR="$OPENCL_HEADERS" \
    -DBUILD_TESTING=OFF > "$dir.configure.log" 2>&1 \
    || { tail -20 "$dir.configure.log" >&2; return 9; }
  cmake --build "$dir" -j "$JOBS" > "$dir.build.log" 2>&1 \
    || { tail -20 "$dir.build.log" >&2; return 9; }
  cp -f "$dir/libOpenCL.so" "$out"
}

# ---------------------------------------------------------------------------
# llama-server
# ---------------------------------------------------------------------------
for ABI in "${ABIS[@]}"; do
  case "$ABI" in
    arm64-v8a)
      ARCH_FLAGS="-march=armv8.7a+fp16+dotprod+i8mm -fvectorize -ffp-model=fast -fno-finite-math-only -D_GNU_SOURCE"
      ;;
    x86_64)
      # 模拟器：没有 NPU / Adreno，只验 CPU 档和降级逻辑（脚本本身也只在 arm64 上开两个后端）。
      ARCH_FLAGS="-D_GNU_SOURCE"
      ;;
    *)
      echo "不认识的 ABI：$ABI（这个脚本只支持 arm64-v8a / x86_64）" >&2
      exit 7
      ;;
  esac

  BUILD=$BUILD_ROOT/$ABI
  DEST=$OUT/$ABI
  mkdir -p "$DEST"

  ARGS=(
    -S "$LLAMA_CPP_SRC" -B "$BUILD" -G Ninja
    -DCMAKE_BUILD_TYPE=Release
    -DCMAKE_TOOLCHAIN_FILE="$TOOLCHAIN"
    -DANDROID_ABI="$ABI"
    -DANDROID_PLATFORM="android-$ANDROID_API"
    -DCMAKE_C_FLAGS="$ARCH_FLAGS"
    -DCMAKE_CXX_FLAGS="$ARCH_FLAGS"
    -DCMAKE_C_FLAGS_RELEASE="-O3 -DNDEBUG"
    -DCMAKE_CXX_FLAGS_RELEASE="-O3 -DNDEBUG"
    # 单文件：静态链全部 llama / ggml 库（见文件头）
    -DBUILD_SHARED_LIBS=OFF
    -DGGML_STATIC=ON
    -DLLAMA_BUILD_TESTS=OFF
    -DLLAMA_BUILD_EXAMPLES=OFF
    -DLLAMA_BUILD_TOOLS=ON
    -DLLAMA_BUILD_SERVER=ON
    -DLLAMA_BUILD_APP=OFF
    # **不联网**：默认 LLAMA_USE_PREBUILT_UI=ON 会在构建时从 HF 下前端资源
    -DLLAMA_BUILD_UI=OFF
    -DLLAMA_USE_PREBUILT_UI=OFF
    -DLLAMA_OPENSSL=OFF
    -DGGML_OPENMP=OFF
    -DGGML_BLAS=OFF
    -DGGML_LLAMAFILE=OFF
    -DGGML_NATIVE=OFF
    -DGGML_VULKAN=OFF -DGGML_CUDA=OFF -DGGML_HIP=OFF -DGGML_METAL=OFF -DGGML_SYCL=OFF -DGGML_RPC=OFF
  )

  if [ "$ABI" = arm64-v8a ]; then
    for d in "$HEXAGON_SDK_ROOT" "$HEXAGON_TOOLS_ROOT" "$OPENCL_HEADERS/CL" "$OPENCL_ICD_SRC"; do
      [ -d "$d" ] || { echo "缺依赖：$d（Hexagon SDK / OpenCL 源码；见文件头）" >&2; exit 4; }
    done
    # OpenCL 的链接期桩：优先用已经编好的 ICD loader，没有就先编一个
    OPENCL_LIB=$BUILD_ROOT/opencl-prefix-$ABI/libOpenCL.so
    if [ ! -f "$OPENCL_LIB" ]; then
      mkdir -p "$(dirname "$OPENCL_LIB")"
      build_opencl_icd "$ABI" "$OPENCL_LIB" || exit 9
    fi
    ARGS+=(
      -DHEXAGON_SDK_ROOT="$HEXAGON_SDK_ROOT"
      -DHEXAGON_TOOLS_ROOT="$HEXAGON_TOOLS_ROOT"
      -DPREBUILT_LIB_DIR=android_aarch64
      -DGGML_HEXAGON=ON
      -DGGML_OPENCL=ON
      -DGGML_OPENCL_EMBED_KERNELS=ON
      -DGGML_OPENCL_USE_ADRENO_KERNELS=ON
      -DOpenCL_INCLUDE_DIR="$OPENCL_HEADERS"
      -DOpenCL_LIBRARY="$OPENCL_LIB"
    )
    echo "=== 编 llama-server（$ABI）b11700，Hexagon $HTP_ARCH + OpenCL，$JOBS 路并行 ==="
  else
    ARGS+=(-DGGML_HEXAGON=OFF -DGGML_OPENCL=OFF)
    echo "=== 编 llama-server（$ABI）b11700，只有 CPU（模拟器用），$JOBS 路并行 ==="
  fi

  cmake "${ARGS[@]}" > "$BUILD.configure.log" 2>&1 \
    || { tail -30 "$BUILD.configure.log" >&2; exit 8; }
  # htp-$HTP_ARCH 是 ExternalProject（DSP 侧要另外用 QCC 编），显式带上：
  # `llama-server` 本身不依赖它，不写在这里的话它永远不会被编出来。
  TARGETS=(llama-server)
  [ "$ABI" = arm64-v8a ] && TARGETS+=("htp-$HTP_ARCH")
  cmake --build "$BUILD" --target "${TARGETS[@]}" -j "$JOBS" > "$BUILD.build.log" 2>&1 \
    || { tail -40 "$BUILD.build.log" >&2; exit 8; }

  SRC=$BUILD/bin/llama-server
  [ -f "$SRC" ] || { echo "编完了但找不到 $SRC" >&2; exit 8; }
  cp -f "$SRC" "$DEST/libllamaserver.so"
  "$STRIP" --strip-unneeded "$DEST/libllamaserver.so" 2>/dev/null || true
  chmod 644 "$DEST/libllamaserver.so"

  if [ "$ABI" = arm64-v8a ]; then
    SKEL=$BUILD/ggml/src/ggml-hexagon/libggml-htp-$HTP_ARCH.so
    [ -f "$SKEL" ] || { echo "找不到 Hexagon 的 skel：$SKEL" >&2; exit 8; }
    cp -f "$SKEL" "$DEST/libggml-htp-$HTP_ARCH.so"
    # skel 不能 strip（DSP 侧要靠符号表定位入口 `htp_iface_skel_handle_invoke`）
    cp -f "$OPENCL_LIB" "$DEST/libOpenCL.so"
    chmod 644 "$DEST/libggml-htp-$HTP_ARCH.so" "$DEST/libOpenCL.so"
  fi

  echo "=== $ABI 好了：$DEST ==="
  ls -la "$DEST"
done

echo
echo "下一步：这些 .so / 可执行文件由 androidApp 的 jniLibs srcDir 打进 APK；"
echo "起进程时 Rust 侧（translate::server::mobile_env）会把 LD_LIBRARY_PATH / ADSP_LIBRARY_PATH"
echo "指到同一个 nativeLibraryDir，并按 translate::device 的候选顺序试 NPU → GPU → NPU+GPU → CPU。"
