/*
 * 桌面本地向量的推理入口：
 * **一层很薄的 JNI shim，直接调 ONNX Runtime 的 C API**，不依赖 ORT 的 JVM 绑定（`ai.onnxruntime`）。
 *
 * 为什么不引 `com.microsoft.onnxruntime:onnxruntime`（JVM 版）：这台机器 / 这个仓库的构建是**离线**的
 * （`./gradlew test --offline`），Gradle 缓存里只有安卓的 AAR（`onnxruntime-android`），JVM 版的 jar 拿不到；
 * 而任务的红线是"不发在线请求"。所以这里用**已经在本机的** ORT 1.31.0 linux-x64 运行库
 * （`libonnxruntime.so.1.31.0`，和安卓那版同一个版本号），自己编译一层 300 行的 JNI 转发（5 个方法）。
 *
 * 头文件用的是 **`onnxruntime-android:1.31.0` AAR 里那份 `headers/onnxruntime_c_api.h`**（同一个 release，
 * 构建时从 Gradle 缓存里解出来）：C API 的版本和运行库对得上，函数指针表由编译器解析，不手写偏移。
 *
 * 这层只做四件事：建会话（线程数 / 图优化 / 关自旋）、喂一批 `input_ids` + `attention_mask`、取
 * `sentence_embedding`、释放。**分词 / 归一化 / 截断都不在这里**（和安卓走同一套 Kotlin 实现）。
 *
 * 线程模型：调用方（`DesktopOnnxTextEmbedder`）用一把锁把一批一批串起来；这里不再加锁（除了错误串）。
 */

#include <jni.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "onnxruntime_c_api.h"

/** `OrtApi` / `OrtEnv` 是进程级的（ORT 自己也会复用 env），只建一次。 */
static pthread_once_t g_env_once = PTHREAD_ONCE_INIT;
static const OrtApi* g_api = NULL;
static OrtEnv* g_env = NULL;
static const char* g_env_error = NULL;
/** 初始化失败的原因要留到之后（`OrtStatus` 的 message 在 `ReleaseStatus` 之后就不归我们了），先拷一份。 */
static char g_env_error_buf[1024] = {0};

/** 最近一次失败的说明（只有一条；调用方是串行的，再用一把小锁兜住多线程读）。 */
static pthread_mutex_t g_err_lock = PTHREAD_MUTEX_INITIALIZER;
static char g_error[2048] = {0};

static void set_error(const char* what, const char* detail) {
    pthread_mutex_lock(&g_err_lock);
    if (detail != NULL && detail[0] != '\0') {
        snprintf(g_error, sizeof(g_error), "%s: %s", what, detail);
    } else {
        snprintf(g_error, sizeof(g_error), "%s", what);
    }
    pthread_mutex_unlock(&g_err_lock);
}

/** 把一个 `OrtStatus*` 变成错误串并释放它；返回 0 表示成功。 */
static int check_status(OrtStatus* status, const char* what) {
    if (status == NULL) return 0;
    set_error(what, g_api != NULL ? g_api->GetErrorMessage(status) : "unknown");
    if (g_api != NULL) g_api->ReleaseStatus(status);
    return -1;
}

static void init_env_once(void) {
    const OrtApiBase* base = OrtGetApiBase();
    if (base == NULL) {
        g_env_error = "OrtGetApiBase() 返回空（libonnxruntime 没加载对？）";
        return;
    }
    // 和安卓那边写的版本一致才用：换运行库就和换模型一样，必须重新验一遍数值
    g_api = base->GetApi(ORT_API_VERSION);
    if (g_api == NULL) {
        g_env_error = "这个 libonnxruntime 不支持我们的 C API 版本";
        return;
    }
    OrtStatus* status = g_api->CreateEnv(ORT_LOGGING_LEVEL_ERROR, "feedhub-desktop", &g_env);
    if (status != NULL) {
        snprintf(g_env_error_buf, sizeof(g_env_error_buf), "%s", g_api->GetErrorMessage(status));
        g_env_error = g_env_error_buf;
        g_api->ReleaseStatus(status);
        g_api = NULL;
        g_env = NULL;
        return;
    }
    // 遥测关掉（和安卓的 `OrtEnvironment.setTelemetry(false)` 一个意思）：这个 App 不外发任何东西
    g_api->DisableTelemetryEvents(g_env);
}

/** 一个会话 + 三张"文字输入时必须给"的空多模态张量（`[0, 512]` float，建一次重复用）。 */
typedef struct {
    OrtSession* session;
    OrtMemoryInfo* mem_info;
    OrtValue* empty_features[3];
    size_t dim; /* 第一次 Run 之后才知道（这个图是 768） */
} EmbedSession;

static void release_value(OrtValue** value) {
    if (*value != NULL && g_api != NULL) {
        g_api->ReleaseValue(*value);
        *value = NULL;
    }
}

static void free_session(EmbedSession* s) {
    if (s == NULL) return;
    if (g_api != NULL) {
        for (int i = 0; i < 3; i++) release_value(&s->empty_features[i]);
        if (s->mem_info != NULL) g_api->ReleaseMemoryInfo(s->mem_info);
        if (s->session != NULL) g_api->ReleaseSession(s->session);
    }
    free(s);
}

/** 建三个 0 元素的 `[0, 512]` float 张量（ORT 允许 data_len = 0）。 */
static int create_empty_features(EmbedSession* s) {
    int64_t shape[2] = {0, 512};
    for (int i = 0; i < 3; i++) {
        OrtStatus* status = g_api->CreateTensorWithDataAsOrtValue(
            s->mem_info, NULL, 0, shape, 2, ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT, &s->empty_features[i]);
        if (check_status(status, "建空的多模态输入失败") != 0) return -1;
    }
    return 0;
}

JNIEXPORT jstring JNICALL Java_feedhub_app_embed_OrtNative_nativeVersion(JNIEnv* env, jclass clazz) {
    (void)clazz;
    const char* version = OrtGetApiBase() != NULL ? OrtGetApiBase()->GetVersionString() : "missing";
    return (*env)->NewStringUTF(env, version);
}

JNIEXPORT jstring JNICALL Java_feedhub_app_embed_OrtNative_nativeLastError(JNIEnv* env, jclass clazz) {
    (void)clazz;
    pthread_mutex_lock(&g_err_lock);
    char copy[sizeof(g_error)];
    memcpy(copy, g_error, sizeof(copy));
    pthread_mutex_unlock(&g_err_lock);
    return (*env)->NewStringUTF(env, copy);
}

/**
 * 建会话。返回 0 表示失败（原因用 `nativeLastError` 取）。
 *
 * @param model_path  `.onnx` 的绝对路径；外部权重（`.onnx_data`）必须和它**同一个目录**
 * @param threads     intra-op 线程数（桌面默认 3，给界面/别的进程留核）
 */
JNIEXPORT jlong JNICALL Java_feedhub_app_embed_OrtNative_nativeCreate(
    JNIEnv* env, jclass clazz, jstring model_path, jint threads, jint inter_threads) {
    (void)clazz;
    pthread_once(&g_env_once, init_env_once);
    if (g_api == NULL || g_env == NULL) {
        set_error(g_env_error != NULL ? g_env_error : "ONNX Runtime 初始化失败", NULL);
        return 0;
    }
    if (model_path == NULL) {
        set_error("模型路径是空的", NULL);
        return 0;
    }
    const char* path = (*env)->GetStringUTFChars(env, model_path, NULL);
    if (path == NULL) {
        set_error("模型路径读不出来", NULL);
        return 0;
    }
    EmbedSession* s = (EmbedSession*)calloc(1, sizeof(EmbedSession));
    if (s == NULL) {
        (*env)->ReleaseStringUTFChars(env, model_path, path);
        set_error("内存不够", NULL);
        return 0;
    }
    OrtSessionOptions* options = NULL;
    OrtStatus* status = g_api->CreateSessionOptions(&options);
    if (check_status(status, "建会话选项失败") != 0) goto fail;
    check_status(g_api->SetSessionGraphOptimizationLevel(options, ORT_ENABLE_ALL), "设置图优化失败");
    // SPINNING：一批算完不要自旋等下一批（和安卓那边一样，省电）
    check_status(g_api->SetIntraOpNumThreads(options, (int)threads), "设置 intra-op 线程数失败");
    check_status(g_api->SetInterOpNumThreads(options, (int)(inter_threads > 0 ? inter_threads : 1)),
                 "设置 inter-op 线程数失败");
    check_status(g_api->AddSessionConfigEntry(options, "session.intra_op.allow_spinning", "0"), "关自旋失败");
    check_status(g_api->AddSessionConfigEntry(options, "session.inter_op.allow_spinning", "0"), "关自旋失败");
    status = g_api->CreateSession(g_env, path, options, &s->session);
    if (check_status(status, "加载 ONNX 图失败") != 0) goto fail;
    status = g_api->CreateCpuMemoryInfo(OrtDeviceAllocator, OrtMemTypeCPU, &s->mem_info);
    if (check_status(status, "建 CPU 内存信息失败") != 0) goto fail;
    if (create_empty_features(s) != 0) goto fail;
    g_api->ReleaseSessionOptions(options);
    (*env)->ReleaseStringUTFChars(env, model_path, path);
    return (jlong)(intptr_t)s;

fail:
    if (options != NULL && g_api != NULL) g_api->ReleaseSessionOptions(options);
    (*env)->ReleaseStringUTFChars(env, model_path, path);
    free_session(s);
    return 0;
}

/**
 * 算一批：`ids` / `mask` 是**已经补好**的 `batch × len` 的 int64 平铺数组。
 * 返回 `batch × dim` 的 float 数组（**没有归一化**，归一化和 MRL 截断由 Kotlin 侧按安卓同一条路做）；
 * 失败返回 NULL。
 */
JNIEXPORT jfloatArray JNICALL Java_feedhub_app_embed_OrtNative_nativeEmbed(
    JNIEnv* env, jclass clazz, jlong handle, jlongArray ids, jlongArray mask, jint batch, jint len) {
    (void)clazz;
    EmbedSession* s = (EmbedSession*)(intptr_t)handle;
    if (s == NULL || ids == NULL || mask == NULL || batch <= 0 || len <= 0) {
        set_error("参数不对（会话 / 输入 / 批大小）", NULL);
        return NULL;
    }
    const jsize need = (jsize)((int64_t)batch * (int64_t)len);
    if ((*env)->GetArrayLength(env, ids) < need || (*env)->GetArrayLength(env, mask) < need) {
        set_error("输入数组比 batch × len 短", NULL);
        return NULL;
    }
    jlong* ids_data = (*env)->GetLongArrayElements(env, ids, NULL);
    jlong* mask_data = (*env)->GetLongArrayElements(env, mask, NULL);
    if (ids_data == NULL || mask_data == NULL) {
        if (ids_data != NULL) (*env)->ReleaseLongArrayElements(env, ids, ids_data, JNI_ABORT);
        if (mask_data != NULL) (*env)->ReleaseLongArrayElements(env, mask, mask_data, JNI_ABORT);
        set_error("拿不到输入数组", NULL);
        return NULL;
    }
    jfloatArray result = NULL;
    OrtValue* ids_tensor = NULL;
    OrtValue* mask_tensor = NULL;
    OrtValue* output = NULL;
    int64_t shape[2] = {(int64_t)batch, (int64_t)len};
    do {
        OrtStatus* status = g_api->CreateTensorWithDataAsOrtValue(
            s->mem_info, ids_data, (size_t)need * sizeof(int64_t), shape, 2,
            ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64, &ids_tensor);
        if (check_status(status, "建 input_ids 张量失败") != 0) break;
        status = g_api->CreateTensorWithDataAsOrtValue(
            s->mem_info, mask_data, (size_t)need * sizeof(int64_t), shape, 2,
            ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64, &mask_tensor);
        if (check_status(status, "建 attention_mask 张量失败") != 0) break;

        const char* input_names[5] = {"input_ids", "attention_mask", "image_features", "video_features",
                                      "audio_features"};
        const OrtValue* inputs[5] = {ids_tensor, mask_tensor, s->empty_features[0], s->empty_features[1],
                                     s->empty_features[2]};
        const char* output_names[1] = {"sentence_embedding"};
        status = g_api->Run(s->session, NULL, input_names, inputs, 5, output_names, 1, &output);
        if (check_status(status, "推理失败") != 0) break;

        float* data = NULL;
        status = g_api->GetTensorMutableData(output, (void**)&data);
        if (check_status(status, "取输出数据失败") != 0) break;
        OrtTensorTypeAndShapeInfo* info = NULL;
        status = g_api->GetTensorTypeAndShape(output, &info);
        if (check_status(status, "取输出形状失败") != 0) break;
        size_t dims_count = 0;
        status = g_api->GetDimensionsCount(info, &dims_count);
        if (check_status(status, "取输出维数个数失败") != 0) {
            g_api->ReleaseTensorTypeAndShapeInfo(info);
            break;
        }
        int64_t dims[8];
        if (dims_count > 8) dims_count = 8;
        status = g_api->GetDimensions(info, dims, dims_count);
        if (check_status(status, "取输出维数失败") != 0) {
            g_api->ReleaseTensorTypeAndShapeInfo(info);
            break;
        }
        size_t dim = (size_t)dims[dims_count - 1];
        g_api->ReleaseTensorTypeAndShapeInfo(info);
        if (dim == 0) {
            set_error("输出维数是 0", NULL);
            break;
        }
        s->dim = dim;
        jfloatArray out = (*env)->NewFloatArray(env, (jsize)((int64_t)batch * (int64_t)dim));
        if (out == NULL) {
            set_error("分配输出数组失败", NULL);
            break;
        }
        (*env)->SetFloatArrayRegion(env, out, 0, (jsize)((int64_t)batch * (int64_t)dim), data);
        result = out;
    } while (0);

    release_value(&output);
    release_value(&mask_tensor);
    release_value(&ids_tensor);
    (*env)->ReleaseLongArrayElements(env, mask, mask_data, JNI_ABORT);
    (*env)->ReleaseLongArrayElements(env, ids, ids_data, JNI_ABORT);
    return result;
}

/** 输出维数（第一次 Run 之前是 0）。 */
JNIEXPORT jint JNICALL Java_feedhub_app_embed_OrtNative_nativeDim(JNIEnv* env, jclass clazz, jlong handle) {
    (void)env;
    (void)clazz;
    EmbedSession* s = (EmbedSession*)(intptr_t)handle;
    return s == NULL ? 0 : (jint)s->dim;
}

JNIEXPORT void JNICALL Java_feedhub_app_embed_OrtNative_nativeClose(JNIEnv* env, jclass clazz, jlong handle) {
    (void)env;
    (void)clazz;
    free_session((EmbedSession*)(intptr_t)handle);
}
