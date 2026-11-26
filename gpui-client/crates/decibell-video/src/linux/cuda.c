// The CUDA half of the NVDEC path: FFmpeg's nvdec hwaccel leaves each decoded picture in CUDA device memory;
// this copies it, on the GPU, into Vulkan images of the renderer's device that were imported into CUDA through
// their memory's file descriptor (VK_KHR_external_memory_fd). No picture data reaches the CPU.
//
// Everything is loaded at run time: `cuda_load_functions` (nv-codec-headers' dynlink_loader.h, MIT) dlopens
// libcuda.so.1, so a machine without the NVIDIA driver simply does not offer this decoder (dbv_cuda_open fails
// with a message). FFmpeg loads the same library the same way for its own CUDA device context; libnvcuvid.so.1 is
// loaded by its nvdec hwaccel when the decoder is opened.
// The loader reports a missing library on stderr by default; the caller logs our own message instead.
#define FFNV_LOG_FUNC(logctx, msg, ...) ((void)0)
#define FFNV_DEBUG_LOG_FUNC(logctx, msg, ...) ((void)0)
#include <ffnvcodec/dynlink_loader.h>
#include <libavutil/buffer.h>
#include <libavutil/frame.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_cuda.h>
#include <stdio.h>
#include <string.h>

#ifndef CUDA_EXTERNAL_MEMORY_DEDICATED
#define CUDA_EXTERNAL_MEMORY_DEDICATED 0x1
#endif

typedef struct DbvCuda {
    CudaFunctions *cu;      // our own table of the library's entry points (FFmpeg's is private to it)
    AVBufferRef *device;    // FFmpeg's CUDA device context: owns the CUDA context the decoder runs in
    CUcontext ctx;
    CUstream stream;
} DbvCuda;

typedef struct DbvCudaPlane {
    CUexternalMemory memory;
    CUmipmappedArray mipmap;
    CUarray array;
} DbvCudaPlane;

// CUDA's result codes that explain a failure here (cuda.h; nv-codec-headers declares only a few of them).
static const char *cu_name(CUresult result)
{
    switch ((int)result) {
    case 0: return "success";
    case 1: return "invalid value";
    case 2: return "out of memory";
    case 3: return "not initialized";
    case 100: return "no CUDA device";
    case 101: return "invalid device";
    case 201: return "invalid context";
    case 304: return "the operating system refused the call";
    case 400: return "invalid handle";
    case 801: return "not supported";
    case 999: return "unknown error";
    default: return "error";
    }
}

static int fail(char *err, int err_len, const char *what, CUresult result)
{
    snprintf(err, (size_t)err_len, "%s: CUDA %s (%d)", what, cu_name(result), (int)result);
    return -1;
}

// The CUDA device context on the GPU whose Vulkan deviceUUID is `uuid` (VkPhysicalDeviceIDProperties): the
// device the renderer draws with. NULL with a message in `err` when CUDA is not installed, the GPU is not a CUDA
// device, or the driver lacks the external-memory entry points (older than 2019's 418 series).
DbvCuda *dbv_cuda_open(const uint8_t uuid[16], char *err, int err_len)
{
    CudaFunctions *cu = NULL;
    if (cuda_load_functions(&cu, NULL) < 0 || !cu) {
        snprintf(err, (size_t)err_len, "the CUDA library (libcuda.so.1) could not be loaded");
        return NULL;
    }
    if (!cu->cuImportExternalMemory || !cu->cuExternalMemoryGetMappedMipmappedArray ||
        !cu->cuMipmappedArrayGetLevel || !cu->cuDestroyExternalMemory || !cu->cuMipmappedArrayDestroy) {
        snprintf(err, (size_t)err_len, "this CUDA library has no external memory support");
        cuda_free_functions(&cu);
        return NULL;
    }
    CUresult result = cu->cuInit(0);
    int count = 0, index = -1;
    if (result == CUDA_SUCCESS)
        result = cu->cuDeviceGetCount(&count);
    if (result != CUDA_SUCCESS) {
        fail(err, err_len, "cuInit", result);
        cuda_free_functions(&cu);
        return NULL;
    }
    for (int i = 0; i < count && index < 0; ++i) {
        CUdevice device;
        CUuuid id;
        if (cu->cuDeviceGet(&device, i) != CUDA_SUCCESS)
            continue;
        // The Vulkan deviceUUID of an NVIDIA GPU is its CUDA UUID (what cuDeviceGetUuid returns; the _v2
        // variant differs only for MIG instances).
        tcuDeviceGetUuid *get = cu->cuDeviceGetUuid ? cu->cuDeviceGetUuid : cu->cuDeviceGetUuid_v2;
        if (get && get(&id, device) == CUDA_SUCCESS && !memcmp(id.bytes, uuid, 16))
            index = i;
    }
    if (index < 0) {
        snprintf(err, (size_t)err_len, "none of the %d CUDA devices is the GPU the window is drawn with", count);
        cuda_free_functions(&cu);
        return NULL;
    }
    char name[16];
    snprintf(name, sizeof name, "%d", index);
    AVBufferRef *device = NULL;
    // FFmpeg creates the context with CU_CTX_SCHED_BLOCKING_SYNC: a thread waiting for the GPU sleeps.
    int ret = av_hwdevice_ctx_create(&device, AV_HWDEVICE_TYPE_CUDA, name, NULL, 0);
    if (ret < 0) {
        char message[128];
        av_strerror(ret, message, sizeof message);
        snprintf(err, (size_t)err_len, "creating the CUDA device context: %s", message);
        cuda_free_functions(&cu);
        return NULL;
    }
    DbvCuda *cuda = av_mallocz(sizeof *cuda);
    if (!cuda) {
        av_buffer_unref(&device);
        cuda_free_functions(&cu);
        snprintf(err, (size_t)err_len, "out of memory");
        return NULL;
    }
    AVCUDADeviceContext *hwctx = ((AVHWDeviceContext *)device->data)->hwctx;
    cuda->cu = cu;
    cuda->device = device;
    cuda->ctx = hwctx->cuda_ctx;
    cuda->stream = hwctx->stream;
    return cuda;
}

void dbv_cuda_close(DbvCuda *cuda)
{
    if (!cuda)
        return;
    av_buffer_unref(&cuda->device);
    cuda_free_functions(&cuda->cu);
    av_free(cuda);
}

// A new reference to FFmpeg's device context, for AVCodecContext.hw_device_ctx (the caller unrefs it).
AVBufferRef *dbv_cuda_device(DbvCuda *cuda) { return av_buffer_ref(cuda->device); }

int dbv_cuda_has_semaphores(DbvCuda *cuda)
{
    return cuda->cu->cuImportExternalSemaphore && cuda->cu->cuSignalExternalSemaphoresAsync &&
           cuda->cu->cuDestroyExternalSemaphore;
}

// Imports the memory behind one plane image (an R8 or R8G8 VkImage with a dedicated allocation of `size` bytes,
// exported as an opaque fd) and maps it as a CUDA array of `width` x `height` texels of `channels` bytes. On
// success CUDA owns `fd`; on failure the caller still does.
int dbv_cuda_import_plane(DbvCuda *cuda, int fd, uint64_t size, int width, int height, int channels,
                          DbvCudaPlane *out, char *err, int err_len)
{
    CudaFunctions *cu = cuda->cu;
    CUcontext dummy;
    memset(out, 0, sizeof *out);
    CUresult result = cu->cuCtxPushCurrent(cuda->ctx);
    if (result != CUDA_SUCCESS)
        return fail(err, err_len, "cuCtxPushCurrent", result);
    CUDA_EXTERNAL_MEMORY_HANDLE_DESC memory = {
        .type = CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD,
        .handle.fd = fd,
        .size = size,
        .flags = CUDA_EXTERNAL_MEMORY_DEDICATED,
    };
    result = cu->cuImportExternalMemory(&out->memory, &memory);
    if (result != CUDA_SUCCESS) {
        cu->cuCtxPopCurrent(&dummy);
        return fail(err, err_len, "cuImportExternalMemory", result);
    }
    CUDA_EXTERNAL_MEMORY_MIPMAPPED_ARRAY_DESC array = {
        .offset = 0,
        .arrayDesc = { .Width = (size_t)width, .Height = (size_t)height, .Depth = 0,
                       .Format = CU_AD_FORMAT_UNSIGNED_INT8, .NumChannels = (unsigned)channels, .Flags = 0 },
        .numLevels = 1,
    };
    result = cu->cuExternalMemoryGetMappedMipmappedArray(&out->mipmap, out->memory, &array);
    const char *what = "cuExternalMemoryGetMappedMipmappedArray";
    if (result == CUDA_SUCCESS) {
        result = cu->cuMipmappedArrayGetLevel(&out->array, out->mipmap, 0);
        what = "cuMipmappedArrayGetLevel";
    }
    if (result != CUDA_SUCCESS) {
        // The fd went to CUDA with the import: destroying the import releases it.
        if (out->mipmap)
            cu->cuMipmappedArrayDestroy(out->mipmap);
        cu->cuDestroyExternalMemory(out->memory);
        memset(out, 0, sizeof *out);
        cu->cuCtxPopCurrent(&dummy);
        fail(err, err_len, what, result);
        return -2;      // the fd is gone
    }
    cu->cuCtxPopCurrent(&dummy);
    return 0;
}

void dbv_cuda_free_plane(DbvCuda *cuda, DbvCudaPlane *plane)
{
    CudaFunctions *cu = cuda->cu;
    CUcontext dummy;
    if (!plane->memory || cu->cuCtxPushCurrent(cuda->ctx) != CUDA_SUCCESS)
        return;
    if (plane->mipmap)
        cu->cuMipmappedArrayDestroy(plane->mipmap);
    cu->cuDestroyExternalMemory(plane->memory);
    cu->cuCtxPopCurrent(&dummy);
    memset(plane, 0, sizeof *plane);
}

// Imports a binary Vulkan semaphore (exported as an opaque fd). On success CUDA owns `fd`.
int dbv_cuda_import_semaphore(DbvCuda *cuda, int fd, CUexternalSemaphore *out, char *err, int err_len)
{
    CudaFunctions *cu = cuda->cu;
    CUcontext dummy;
    CUresult result = cu->cuCtxPushCurrent(cuda->ctx);
    if (result != CUDA_SUCCESS)
        return fail(err, err_len, "cuCtxPushCurrent", result);
    CUDA_EXTERNAL_SEMAPHORE_HANDLE_DESC desc = {
        .type = CU_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD,
        .handle.fd = fd,
    };
    result = cu->cuImportExternalSemaphore(out, &desc);
    cu->cuCtxPopCurrent(&dummy);
    return result == CUDA_SUCCESS ? 0 : fail(err, err_len, "cuImportExternalSemaphore", result);
}

void dbv_cuda_free_semaphore(DbvCuda *cuda, CUexternalSemaphore semaphore)
{
    CudaFunctions *cu = cuda->cu;
    CUcontext dummy;
    if (!semaphore || cu->cuCtxPushCurrent(cuda->ctx) != CUDA_SUCCESS)
        return;
    cu->cuDestroyExternalSemaphore(semaphore);
    cu->cuCtxPopCurrent(&dummy);
}

// Copies the picture of an NV12 AV_PIX_FMT_CUDA frame (`width` x `height`; the frame's planes are pitch-linear
// device memory, data[0] luma and data[1] interleaved chroma) into the two plane arrays, device to device.
// With `semaphore`: the copies and then the semaphore's signal are queued on the context's stream and this
// returns at once; the Vulkan side waits on the semaphore, so the GPU orders "copy, then anything that samples
// the image". Without (a CUDA library that cannot import semaphores): waits here until the copies have run.
int dbv_cuda_copy(DbvCuda *cuda, const AVFrame *frame, int width, int height, const DbvCudaPlane *luma,
                  const DbvCudaPlane *chroma, CUexternalSemaphore semaphore, char *err, int err_len)
{
    CudaFunctions *cu = cuda->cu;
    CUcontext dummy;
    CUresult result = cu->cuCtxPushCurrent(cuda->ctx);
    if (result != CUDA_SUCCESS)
        return fail(err, err_len, "cuCtxPushCurrent", result);
    const char *what = "cuMemcpy2DAsync";
    for (int plane = 0; plane < 2 && result == CUDA_SUCCESS; ++plane) {
        CUDA_MEMCPY2D copy = {
            .srcMemoryType = CU_MEMORYTYPE_DEVICE,
            .srcDevice = (CUdeviceptr)frame->data[plane],
            .srcPitch = (size_t)frame->linesize[plane],
            .dstMemoryType = CU_MEMORYTYPE_ARRAY,
            .dstArray = plane ? chroma->array : luma->array,
            // Chroma: (width + 1) / 2 pairs of two bytes.
            .WidthInBytes = plane ? (size_t)((width + 1) / 2) * 2 : (size_t)width,
            .Height = plane ? (size_t)((height + 1) / 2) : (size_t)height,
        };
        result = cu->cuMemcpy2DAsync(&copy, cuda->stream);
    }
    if (result == CUDA_SUCCESS) {
        if (semaphore) {
            CUDA_EXTERNAL_SEMAPHORE_SIGNAL_PARAMS params = { 0 };
            what = "cuSignalExternalSemaphoresAsync";
            result = cu->cuSignalExternalSemaphoresAsync(&semaphore, &params, 1, cuda->stream);
        } else {
            what = "cuStreamSynchronize";
            result = cu->cuStreamSynchronize(cuda->stream);
        }
    }
    cu->cuCtxPopCurrent(&dummy);
    return result == CUDA_SUCCESS ? 0 : fail(err, err_len, what, result);
}
