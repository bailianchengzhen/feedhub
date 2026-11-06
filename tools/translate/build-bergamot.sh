#!/bin/bash
# 编译 FeedHub 的 Bergamot 本地翻译 JNI 库（`libfeedhub_bergamot.so`）。
#
# 支持三种目标：
#   - `linux-x86_64`：桌面 Linux 版（输出到 `$OUT_LINUX/libfeedhub_bergamot.so`）
#   - `arm64-v8a`   ：安卓真机（NDK r29 交叉编译，16 KB 页对齐，输出到 `$OUT_ANDROID/arm64-v8a/libfeedhub_bergamot.so`）
#   - `x86_64`      ：安卓模拟器（NDK r29 交叉编译，16 KB 页对齐，输出到 `$OUT_ANDROID/x86_64/libfeedhub_bergamot.so`）
#
# 第三方源码（mozilla/translations 的 inference 目录）不进 git，默认从 `deps/bergamot-src` 读取；
# 找不到源码时打印提示并退出 0（照常构建无翻译包，不让别的开发机编译挂掉）。
#
# 用法：
#   tools/translate/build-bergamot.sh linux-x86_64
#   tools/translate/build-bergamot.sh arm64-v8a x86_64
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null || (cd "$SCRIPT_DIR/../.." && pwd))
JNI_SRC="$ROOT/tools/translate/bergamot_jni"
BERGAMOT_SRC=${BERGAMOT_SRC:-$ROOT/deps/bergamot-src}
INFERENCE_DIR="$BERGAMOT_SRC/inference"
NDK=${NDK:-$ROOT/deps/android-ndk}
ANDROID_API=${ANDROID_API:-26}
JOBS=${JOBS:-$(nproc)}

OUT_LINUX=${OUT_LINUX:-$ROOT/app/build/bergamot-native/linux-x86_64}
OUT_ANDROID=${OUT_ANDROID:-$ROOT/androidApp/build/bergamot-jniLibs}
BUILD_ROOT=${BUILD_ROOT:-$ROOT/build/bergamot-cmake}

if [ ! -f "$INFERENCE_DIR/CMakeLists.txt" ]; then
  echo "找不到 Bergamot 源码（$INFERENCE_DIR），跳过编译 libfeedhub_bergamot.so" >&2
  exit 0
fi

if [ $# -eq 0 ]; then
  echo "用法: $0 <linux-x86_64 | arm64-v8a | x86_64>..." >&2
  exit 2
fi

# marian-fork/CMakeLists.txt 内部有一条无条件的 `git submodule update --init`，离线构建会撞墙；
# 放一个薄包装跳过 submodule 子命令（子模块已在克隆时就位，不触网、不改第三方源码树）。
GIT_WRAP_DIR="$BUILD_ROOT/git-wrap"
mkdir -p "$GIT_WRAP_DIR"
REAL_GIT=$(command -v git)
cat > "$GIT_WRAP_DIR/git" <<EOF
#!/bin/sh
if [ "\${1:-}" = "submodule" ]; then
  exit 0
fi
exec "$REAL_GIT" "\$@"
EOF
chmod +x "$GIT_WRAP_DIR/git"
export PATH="$GIT_WRAP_DIR:$PATH"

GEN_ARGS=()
if command -v ninja >/dev/null 2>&1; then
  GEN_ARGS=(-G Ninja)
fi

build_linux() {
  local dir="$BUILD_ROOT/linux-x86_64"
  local jhome="${JAVA_HOME:-}"
  if [ -z "$jhome" ] || [ ! -f "$jhome/include/jni.h" ]; then
    jhome=$(java -XshowSettings:properties -version 2>&1 | awk -F'= ' '/java\.home/{print $2; exit}')
  fi
  [ -f "$jhome/include/jni.h" ] || { echo "找不到 jni.h（JAVA_HOME=$jhome）" >&2; exit 4; }

  echo "=== 编译 libfeedhub_bergamot.so（linux-x86_64）==="
  cmake -S "$JNI_SRC" -B "$dir" "${GEN_ARGS[@]}" \
    -DCMAKE_BUILD_TYPE=Release \
    -DBERGAMOT_INFERENCE_DIR="$INFERENCE_DIR" \
    -DJNI_INCLUDE_DIR="$jhome/include" \
    -DJNI_MD_INCLUDE_DIR="$jhome/include/linux" \
    > "$dir.configure.log" 2>&1 || { tail -40 "$dir.configure.log" >&2; exit 5; }

  cmake --build "$dir" --target feedhub_bergamot -j "$JOBS" \
    > "$dir.build.log" 2>&1 || { tail -60 "$dir.build.log" >&2; exit 5; }

  mkdir -p "$OUT_LINUX"
  cp -f "$dir/libfeedhub_bergamot.so" "$OUT_LINUX/libfeedhub_bergamot.so"
  strip --strip-unneeded "$OUT_LINUX/libfeedhub_bergamot.so"
  echo "产物: $OUT_LINUX/libfeedhub_bergamot.so ($(stat -c%s "$OUT_LINUX/libfeedhub_bergamot.so") 字节)"
}

build_android() {
  local abi=$1
  [ -d "$NDK" ] || { echo "找不到 Android NDK：$NDK" >&2; exit 3; }
  local toolchain="$NDK/build/cmake/android.toolchain.cmake"
  local strip_bin="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip"
  [ -f "$toolchain" ] || { echo "找不到 NDK toolchain：$toolchain" >&2; exit 3; }
  [ -x "$strip_bin" ] || { echo "找不到 llvm-strip：$strip_bin" >&2; exit 3; }

  local dir="$BUILD_ROOT/android-$abi"
  echo "=== 交叉编译 libfeedhub_bergamot.so（Android $abi）==="
  cmake -S "$JNI_SRC" -B "$dir" "${GEN_ARGS[@]}" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_TOOLCHAIN_FILE="$toolchain" \
    -DANDROID_ABI="$abi" \
    -DANDROID_PLATFORM="android-$ANDROID_API" \
    -DANDROID_STL=c++_static \
    -DBERGAMOT_INFERENCE_DIR="$INFERENCE_DIR" \
    > "$dir.configure.log" 2>&1 || { tail -40 "$dir.configure.log" >&2; exit 6; }

  cmake --build "$dir" --target feedhub_bergamot -j "$JOBS" \
    > "$dir.build.log" 2>&1 || { tail -60 "$dir.build.log" >&2; exit 6; }

  mkdir -p "$OUT_ANDROID/$abi"
  cp -f "$dir/libfeedhub_bergamot.so" "$OUT_ANDROID/$abi/libfeedhub_bergamot.so"
  "$strip_bin" --strip-all "$OUT_ANDROID/$abi/libfeedhub_bergamot.so"
  echo "产物: $OUT_ANDROID/$abi/libfeedhub_bergamot.so ($(stat -c%s "$OUT_ANDROID/$abi/libfeedhub_bergamot.so") 字节)"
}

for target in "$@"; do
  case "$target" in
    linux-x86_64) build_linux ;;
    arm64-v8a|x86_64) build_android "$target" ;;
    *) echo "不支持的目标架构: $target" >&2; exit 2 ;;
  esac
done
