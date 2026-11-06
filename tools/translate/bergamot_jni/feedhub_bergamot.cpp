/*
 * FeedHub 本地轻量翻译引擎 JNI 桥接（Project Bergamot / Firefox Translations）。
 *
 * 设计要点：
 * 1. 进程内推理（BlockingService + replicas=1）：`.so` 内部不起后台常驻线程、不起定时器；
 *    并发排队与空闲/退后台释放由上层 Kotlin（TranslationScheduler + BergamotTranslationEngine）统一控制。
 * 2. 绕开 pathie-cpp 的 mbstowcs 路径坑：模型、词表、shortlist 全部通过标准 POSIX `fopen`/`fread`
 *    直接读入对齐的 `AlignedMemory`（模型 256 字节对齐、其余 64 字节对齐），支持任意含中文/UTF-8 路径。
 * 3. 异常兜底：开启 `marian::setThrowExceptionOnAbort(true)`，任何内部 `ABORT_IF` 转成 C++ 异常捕获并写入
 *    `nativeLastError()`，绝不调 `std::abort()` 把宿主 JVM / Android 进程直接撞死。
 * 4. 标准 UTF-8 字节进出：`nativeTranslate` 收发 `jbyteArray`（UTF-8），避免 JNI Modified UTF-8 在
 *    4 字节 Emoji（如 🎉🔥）上触发 PCRE2 报错或 Android ART CheckJNI 崩溃。
 */

#include <jni.h>

#if defined(__unix__) || defined(__ANDROID__)
#include <signal.h>
#endif

#include <clocale>
#include <cstdio>
#include <cstring>
#include <exception>
#include <memory>
#include <mutex>
#include <sstream>
#include <string>
#include <utility>
#include <vector>

#include "common/logging.h"
#include "translator/byte_array_util.h"
#include "translator/definitions.h"
#include "translator/parser.h"
#include "translator/response.h"
#include "translator/response_options.h"
#include "translator/service.h"
#include "translator/translation_model.h"

namespace {

std::once_flag g_init_once;
std::mutex g_err_mutex;
std::string g_last_error;

void setError(const std::string& msg) {
  std::lock_guard<std::mutex> lock(g_err_mutex);
  g_last_error = msg;
}

void initOnce() {
  // 让 Marian 内部的 ABORT / ABORT_IF 抛 MarianRuntimeException，不在进程内 std::abort()
  marian::setThrowExceptionOnAbort(true);
#if !defined(__ANDROID__)
  // 桌面端顺手切 UTF-8 C locale（不影响 JVM，防止第三方库调 mbstowcs 时退回 ASCII）
  if (!std::setlocale(LC_CTYPE, "C.UTF-8")) {
    std::setlocale(LC_CTYPE, "");
  }
#endif
}

std::string jstringToUtf8(JNIEnv* env, jstring jstr) {
  if (jstr == nullptr) return "";
  const char* chars = env->GetStringUTFChars(jstr, nullptr);
  if (chars == nullptr) return "";
  std::string out(chars, static_cast<size_t>(env->GetStringUTFLength(jstr)));
  env->ReleaseStringUTFChars(jstr, chars);
  return out;
}

bool readFileAligned(const std::string& path, size_t alignment, marian::bergamot::AlignedMemory& out,
                     std::string& err) {
  if (path.empty()) {
    err = "文件路径为空";
    return false;
  }
  std::FILE* fp = std::fopen(path.c_str(), "rb");
  if (!fp) {
    err = "无法打开文件: " + path;
    return false;
  }
  if (std::fseek(fp, 0, SEEK_END) != 0) {
    std::fclose(fp);
    err = "无法定位文件末尾: " + path;
    return false;
  }
  long len = std::ftell(fp);
  if (len <= 0) {
    std::fclose(fp);
    err = "文件为空或无法读取长度: " + path;
    return false;
  }
  if (std::fseek(fp, 0, SEEK_SET) != 0) {
    std::fclose(fp);
    err = "无法回到文件开头: " + path;
    return false;
  }
  const size_t size = static_cast<size_t>(len);
  marian::bergamot::AlignedMemory mem(size, alignment);
  size_t readTotal = 0;
  char* dst = reinterpret_cast<char*>(mem.begin());
  while (readTotal < size) {
    size_t n = std::fread(dst + readTotal, 1, size - readTotal, fp);
    if (n == 0) break;
    readTotal += n;
  }
  std::fclose(fp);
  if (readTotal != size) {
    err = "读取文件不完整: " + path;
    return false;
  }
  out = std::move(mem);
  return true;
}

std::string yamlSingleQuote(const std::string& s) {
  std::string out;
  out.reserve(s.size() + 4);
  out.push_back('\'');
  for (char c : s) {
    if (c == '\'') {
      out.append("''");
    } else {
      out.push_back(c);
    }
  }
  out.push_back('\'');
  return out;
}

bool fileReadable(const std::string& path) {
  if (path.empty()) return false;
  std::FILE* fp = std::fopen(path.c_str(), "rb");
  if (!fp) return false;
  std::fclose(fp);
  return true;
}

struct BergamotSession {
  std::mutex mutex;
  marian::bergamot::BlockingService service;
  std::shared_ptr<marian::bergamot::TranslationModel> model;

  explicit BergamotSession(const marian::bergamot::BlockingService::Config& cfg)
      : service(cfg) {}
};

}  // namespace

extern "C" {

#if defined(__unix__) || defined(__ANDROID__)
int __real_sigaction(int signum, const struct sigaction* act, struct sigaction* oldact);
int __wrap_sigaction(int signum, const struct sigaction* act, struct sigaction* oldact) {
  // 严禁 Marian 的 setErrorHandlers() 覆盖宿主 HotSpot JVM / Android ART 的 SIGSEGV 与 SIGFPE 处理器
  // （JVM 与 ART 均依赖隐式空指针检查 SIGSEGV 与轮询页 safepoint，被覆盖会导致宿主进程崩溃）。
  if (act != nullptr) {
    if (oldact != nullptr) {
      return __real_sigaction(signum, nullptr, oldact);
    }
    return 0;
  }
  return __real_sigaction(signum, act, oldact);
}
#endif

std::terminate_handler __wrap__ZSt13set_terminatePFvvE(std::terminate_handler /*func*/) noexcept {
  // 严禁替换宿主进程的全局 C++ terminate_handler
  return nullptr;
}

JNIEXPORT jstring JNICALL
Java_feedhub_app_translate_BergamotNative_nativeVersion(JNIEnv* env, jobject /*thiz*/) {
  return env->NewStringUTF("bergamot-0.6.0");
}

JNIEXPORT jstring JNICALL
Java_feedhub_app_translate_BergamotNative_nativeLastError(JNIEnv* env, jobject /*thiz*/) {
  std::lock_guard<std::mutex> lock(g_err_mutex);
  return env->NewStringUTF(g_last_error.c_str());
}

JNIEXPORT jlong JNICALL
Java_feedhub_app_translate_BergamotNative_nativeCreate(
    JNIEnv* env,
    jobject /*thiz*/,
    jstring jModelPath,
    jstring jSrcVocabPath,
    jstring jTrgVocabPath,
    jstring jShortlistPath,
    jstring jSsplitPrefixPath) {
  std::call_once(g_init_once, initOnce);

  const std::string modelPath = jstringToUtf8(env, jModelPath);
  const std::string srcVocabPath = jstringToUtf8(env, jSrcVocabPath);
  const std::string trgVocabPath = jstringToUtf8(env, jTrgVocabPath);
  const std::string shortlistPath = jstringToUtf8(env, jShortlistPath);
  const std::string ssplitPrefixPath = jstringToUtf8(env, jSsplitPrefixPath);

  try {
    marian::bergamot::MemoryBundle bundle;
    std::string ioErr;

    marian::bergamot::AlignedMemory modelMem;
    if (!readFileAligned(modelPath, 256, modelMem, ioErr)) {
      setError(ioErr);
      return 0;
    }
    if (!marian::bergamot::validateBinaryModel(modelMem, modelMem.size())) {
      setError("翻译模型文件头校验失败（文件可能损坏或不完整）: " + modelPath);
      return 0;
    }
    bundle.models.push_back(std::move(modelMem));

    auto srcVocabMem = std::make_shared<marian::bergamot::AlignedMemory>();
    if (!readFileAligned(srcVocabPath, 64, *srcVocabMem, ioErr)) {
      setError(ioErr);
      return 0;
    }
    if (srcVocabPath == trgVocabPath) {
      bundle.vocabs.push_back(srcVocabMem);
      bundle.vocabs.push_back(srcVocabMem);
    } else {
      auto trgVocabMem = std::make_shared<marian::bergamot::AlignedMemory>();
      if (!readFileAligned(trgVocabPath, 64, *trgVocabMem, ioErr)) {
        setError(ioErr);
        return 0;
      }
      bundle.vocabs.push_back(std::move(srcVocabMem));
      bundle.vocabs.push_back(std::move(trgVocabMem));
    }

    if (!shortlistPath.empty()) {
      marian::bergamot::AlignedMemory lexMem;
      if (!readFileAligned(shortlistPath, 64, lexMem, ioErr)) {
        setError(ioErr);
        return 0;
      }
      bundle.shortlist = std::move(lexMem);
    }

    // Firefox Translations 标准推理参数（base-memory intgemm8 + 段落内分句 + 64 MB 工作区上限）
    // 注：即便词表已通过 MemoryBundle 内存传入，Marian 的 createEmbeddingLayer / Transformer 仍会读取
    // options["vocabs"][batchIndex] 检查后缀是否为 .fsv/.fm（不读磁盘），故必须填入两个非 .fsv 占位名防止空 vector 越界。
    std::ostringstream yaml;
    yaml << "beam-size: 1\n"
         << "normalize: 1.0\n"
         << "word-penalty: 0\n"
         << "max-length-break: 128\n"
         << "mini-batch-words: 1024\n"
         << "workspace: 64\n"
         << "max-length-factor: 2.0\n"
         << "skip-cost: true\n"
         << "cpu-threads: 1\n"
         << "quiet: true\n"
         << "quiet-translation: true\n"
         << "gemm-precision: int8shiftAlphaAll\n"
         << "ssplit-mode: paragraph\n"
         << "vocabs:\n"
         << "  - memory.spm\n"
         << "  - memory.spm\n";
    if (fileReadable(ssplitPrefixPath)) {
      yaml << "ssplit-prefix-file: " << yamlSingleQuote(ssplitPrefixPath) << "\n";
    }

    marian::bergamot::BlockingService::Config svcCfg;
    svcCfg.cacheSize = 0;  // 缓存由 FeedHub Rust 层统一管理（translations.jsonl）
    svcCfg.logger.level = "off";

    // 先构造 BlockingService 以初始化静默 Logger，再构造 TranslationModel
    auto session = std::make_unique<BergamotSession>(svcCfg);
    auto options = marian::bergamot::parseOptionsFromString(yaml.str(), /*validate=*/false);
    session->model = std::make_shared<marian::bergamot::TranslationModel>(
        options, std::move(bundle), /*replicas=*/1);

    // 预热一次，触发 MarianBackend 懒加载权重解包，保证 READY 状态代表推理图真正就绪
    std::vector<std::string> warmSrc = {"Hi"};
    marian::bergamot::ResponseOptions warmOpt;
    warmOpt.HTML = false;
    warmOpt.qualityScores = false;
    warmOpt.alignment = false;
    std::vector<marian::bergamot::ResponseOptions> warmOpts = {warmOpt};
    auto warmResp = session->service.translateMultiple(session->model, std::move(warmSrc), warmOpts);
    if (warmResp.empty() || warmResp[0].target.text.empty()) {
      setError("翻译模型预热自检返回空结果");
      return 0;
    }

    setError("");
    return reinterpret_cast<jlong>(session.release());
  } catch (const std::exception& e) {
    setError(std::string("加载 Bergamot 模型失败: ") + e.what());
    return 0;
  } catch (...) {
    setError("加载 Bergamot 模型遇到未知 C++ 异常");
    return 0;
  }
}

JNIEXPORT jbyteArray JNICALL
Java_feedhub_app_translate_BergamotNative_nativeTranslate(
    JNIEnv* env,
    jobject /*thiz*/,
    jlong handle,
    jbyteArray jUtf8Input) {
  if (handle == 0 || jUtf8Input == nullptr) {
    setError("无效的 Bergamot 会话句柄或空输入");
    return nullptr;
  }
  auto* session = reinterpret_cast<BergamotSession*>(handle);
  const jsize len = env->GetArrayLength(jUtf8Input);
  if (len <= 0) {
    return env->NewByteArray(0);
  }

  std::string input(static_cast<size_t>(len), '\0');
  env->GetByteArrayRegion(jUtf8Input, 0, len, reinterpret_cast<jbyte*>(&input[0]));

  try {
    std::lock_guard<std::mutex> lock(session->mutex);
    std::vector<std::string> sources;
    sources.push_back(std::move(input));

    marian::bergamot::ResponseOptions ro;
    ro.HTML = false;
    ro.qualityScores = false;
    ro.alignment = false;
    std::vector<marian::bergamot::ResponseOptions> opts = {ro};

    auto responses = session->service.translateMultiple(session->model, std::move(sources), opts);
    if (responses.empty()) {
      setError("Bergamot 未返回翻译结果");
      return nullptr;
    }
    const std::string& out = responses[0].target.text;
    jbyteArray result = env->NewByteArray(static_cast<jsize>(out.size()));
    if (result == nullptr) {
      setError("分配译文数组失败（OOM）");
      return nullptr;
    }
    if (!out.empty()) {
      env->SetByteArrayRegion(
          result, 0, static_cast<jsize>(out.size()), reinterpret_cast<const jbyte*>(out.data()));
    }
    return result;
  } catch (const std::exception& e) {
    setError(std::string("Bergamot 翻译异常: ") + e.what());
    return nullptr;
  } catch (...) {
    setError("Bergamot 翻译遇到未知 C++ 异常");
    return nullptr;
  }
}

JNIEXPORT void JNICALL
Java_feedhub_app_translate_BergamotNative_nativeClose(JNIEnv* /*env*/, jobject /*thiz*/, jlong handle) {
  if (handle == 0) return;
  auto* session = reinterpret_cast<BergamotSession*>(handle);
  delete session;
}

}  // extern "C"
