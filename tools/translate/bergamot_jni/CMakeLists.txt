cmake_minimum_required(VERSION 3.16)
project(feedhub_bergamot CXX C)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

if(NOT DEFINED BERGAMOT_INFERENCE_DIR)
  message(FATAL_ERROR "BERGAMOT_INFERENCE_DIR must point to mozilla/translations/inference")
endif()

# 隐藏静态库内部符号（protobuf / abseil / ruy / cpuinfo / sentencepiece），防止与同进程的
# libonnxruntime.so 或系统库发生全局符号劫持（ELF symbol interposition）；
# 同时开启按函数/数据段切分，配合链接期 --gc-sections 裁掉未用到的训练与量化器代码。
set(CMAKE_C_VISIBILITY_PRESET hidden)
set(CMAKE_CXX_VISIBILITY_PRESET hidden)
set(CMAKE_VISIBILITY_INLINES_HIDDEN ON)
add_compile_options(-ffunction-sections -fdata-sections)
add_compile_definitions(PATHIE_ASSUME_UTF8_ON_UNIX=1)

# 仅编译推理静态库，关闭训练/服务端/CUDA/MKL/测试；使用内置 intgemm + ruy + pcre2（与 Firefox Android 一致）
set(COMPILE_LIBRARY_ONLY ON CACHE BOOL "" FORCE)
set(COMPILE_CUDA OFF CACHE BOOL "" FORCE)
set(COMPILE_SERVER OFF CACHE BOOL "" FORCE)
set(COMPILE_TESTS OFF CACHE BOOL "" FORCE)
set(USE_MKL OFF CACHE BOOL "" FORCE)
set(USE_DOXYGEN OFF CACHE BOOL "" FORCE)
set(USE_STATIC_LIBS ON CACHE BOOL "" FORCE)
set(SSPLIT_COMPILE_LIBRARY_ONLY ON CACHE BOOL "" FORCE)
set(SSPLIT_USE_INTERNAL_PCRE2 ON CACHE BOOL "" FORCE)
set(GIT_SUBMODULE OFF CACHE BOOL "" FORCE)

if(ANDROID)
  if(ANDROID_ABI STREQUAL "arm64-v8a")
    set(BUILD_ARCH "armv8-a" CACHE STRING "" FORCE)
    set(USE_RUY ON CACHE BOOL "" FORCE)
    set(USE_RUY_SGEMM ON CACHE BOOL "" FORCE)
    # marian-fork 在子目录里才设这组 sse2neon 宏，提早到顶层让 inference/src 与 JNI 桥接包含 marian 头时一致生效
    add_compile_definitions(ARM FMA SSE)
    add_compile_options(-flax-vector-conversions)
  elseif(ANDROID_ABI STREQUAL "x86_64")
    set(BUILD_ARCH "x86-64-v2" CACHE STRING "" FORCE)
    set(USE_RUY ON CACHE BOOL "" FORCE)
    set(USE_RUY_SGEMM ON CACHE BOOL "" FORCE)
  else()
    message(FATAL_ERROR "Unsupported ANDROID_ABI: ${ANDROID_ABI}")
  endif()
else()
  # Linux x86_64 桌面版：AVX2 基线（x86-64-v3），避免 -march=native 绑死编译机微架构
  if(NOT DEFINED BUILD_ARCH)
    set(BUILD_ARCH "x86-64-v3" CACHE STRING "" FORCE)
  endif()
  set(USE_RUY ON CACHE BOOL "" FORCE)
  set(USE_RUY_SGEMM ON CACHE BOOL "" FORCE)
endif()

add_subdirectory("${BERGAMOT_INFERENCE_DIR}" "${CMAKE_CURRENT_BINARY_DIR}/bergamot" EXCLUDE_FROM_ALL)

# Marian 上游无条件把 sentencepiece_train-static 拉进依赖链（仅虚函数 SentencePieceVocab::create 引用），
# 但推理从不训练词表，且 trainer_interface.cc 在 NDK r29（Clang 21 + ARM 无符号 enum）下触发 constexpr 报错。
# 在构建目录生成单函数桩替换训练源码：不改第三方源码树，同时裁掉整套词表训练器体积。
if(TARGET sentencepiece_train-static)
  set(SPM_TRAIN_STUB "${CMAKE_CURRENT_BINARY_DIR}/spm_train_stub.cpp")
  file(WRITE "${SPM_TRAIN_STUB}" [=[
#include "sentencepiece_trainer.h"
namespace sentencepiece {
util::Status SentencePieceTrainer::Train(
    absl::string_view, SentenceIterator*, std::string*) {
  return util::Status(util::StatusCode::kUnimplemented, "training disabled in inference build");
}
}  // namespace sentencepiece
]=])
  target_include_directories(sentencepiece_train-static PRIVATE
    "${BERGAMOT_INFERENCE_DIR}/marian-fork/src/3rd_party/sentencepiece/src"
  )
  set_target_properties(sentencepiece_train-static PROPERTIES SOURCES "${SPM_TRAIN_STUB}")
endif()

# Android API 26（minSdk 26）的 Bionic 头里 iconv_open/iconv/iconv_close 与 glob/globfree 仅在 API 28+ 声明；
# 开启 PATHIE_ASSUME_UTF8_ON_UNIX=1 后运行期永不调这两组函数（且链接期 --gc-sections 会整段裁掉），
# 用内联桩补齐编译期声明即可在不抬高 minSdk、不引入 API 28 动态符号的前提下通过编译。
if(ANDROID AND TARGET pathie-cpp)
  set(ANDROID_PATHIE_SHIM "${CMAKE_CURRENT_BINARY_DIR}/android_pathie_shim.h")
  file(WRITE "${ANDROID_PATHIE_SHIM}" [=[
#pragma once
#include <android/api-level.h>
#if defined(__ANDROID_API__) && (__ANDROID_API__ < 28)
#include <errno.h>
#include <glob.h>
#include <iconv.h>
static inline iconv_t iconv_open(const char*, const char*) { errno = ENOSYS; return (iconv_t)-1; }
static inline size_t iconv(iconv_t, char**, size_t*, char**, size_t*) { errno = ENOSYS; return (size_t)-1; }
static inline int iconv_close(iconv_t) { return 0; }
static inline int glob(const char*, int, int (*)(const char*, int), glob_t*) { return GLOB_ABORTED; }
static inline void globfree(glob_t*) {}
#endif
]=])
  target_compile_options(pathie-cpp PRIVATE "SHELL:-include ${ANDROID_PATHIE_SHIM}")
endif()

# Android x86_64 下 faiss/VectorTransform.cpp 使用 __m128 但漏了显式包含 <immintrin.h>（Bionic 头不会间接带入）
if(ANDROID AND ANDROID_ABI STREQUAL "x86_64" AND TARGET faiss)
  target_compile_options(faiss PRIVATE "SHELL:-include immintrin.h")
endif()

add_library(feedhub_bergamot SHARED feedhub_bergamot.cpp)

target_include_directories(feedhub_bergamot PRIVATE
  "${BERGAMOT_INFERENCE_DIR}/src"
)

if(NOT ANDROID)
  if(DEFINED JNI_INCLUDE_DIR AND EXISTS "${JNI_INCLUDE_DIR}/jni.h")
    target_include_directories(feedhub_bergamot PRIVATE "${JNI_INCLUDE_DIR}" "${JNI_MD_INCLUDE_DIR}")
  else()
    find_package(JNI REQUIRED)
    target_include_directories(feedhub_bergamot PRIVATE ${JNI_INCLUDE_DIRS})
  endif()
endif()

target_link_libraries(feedhub_bergamot PRIVATE bergamot-translator-source)

# 仅导出 JNI 入口 + 16 KB 页对齐（Android 15+ 要求，Linux 上同样安全）+ 内部符号就近绑定
# 同时用 --wrap 拦截 Marian 内部对 sigaction / std::set_terminate 的调用，严禁覆盖 JVM / Android ART 信号处理器
set(VERSION_SCRIPT "${CMAKE_CURRENT_SOURCE_DIR}/bergamot_jni.map")
target_link_options(feedhub_bergamot PRIVATE
  "-Wl,--gc-sections"
  "-Wl,--version-script,${VERSION_SCRIPT}"
  "-Wl,-Bsymbolic"
  "-Wl,-z,max-page-size=16384"
  "-Wl,--wrap=sigaction"
  "-Wl,--wrap=_ZSt13set_terminatePFvvE"
)

if(ANDROID)
  target_link_libraries(feedhub_bergamot PRIVATE log)
endif()
