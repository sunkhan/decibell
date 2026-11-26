// The part of decibell-video that needs FFmpeg's own struct layouts: wrapping the renderer's VkDevice in an
// AVHWDeviceContext and reading / updating an AVVkFrame under its lock. Everything else is Rust.
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_vulkan.h>
#include <string.h>

// Every submission to a queue of the renderer's device holds the renderer's device-wide queue lock
// (gpui_wgpu::device_queue_lock, reached through `queue_lock`): FFmpeg's decode and pool-image submissions through
// these callbacks, our own through dbv_vk_lock_queue. The renderer holds it when it submits, presents and
// reconfigures a surface (wgpu calls vkDeviceWaitIdle there, which excludes submissions on every queue).
typedef void (*dbv_queue_lock_fn)(int lock);
static dbv_queue_lock_fn queue_lock;

static void lock_any_queue(AVHWDeviceContext *ctx, uint32_t family, uint32_t index) { queue_lock(1); }
static void unlock_any_queue(AVHWDeviceContext *ctx, uint32_t family, uint32_t index) { queue_lock(0); }

// An AVHWDeviceContext around a VkDevice somebody else created (wgpu's). `aux_family` is a queue family with
// COMPUTE and TRANSFER (FFmpeg prepares pool images there and we copy pictures there): one the renderer never
// submits on when the device has one, else the renderer's own; `decode_family` has VIDEO_DECODE. FFmpeg uses
// queue 0 of each. The extension name arrays must outlive the device context.
AVBufferRef *dbv_vk_device(VkInstance inst, PFN_vkGetInstanceProcAddr gipa, VkPhysicalDevice phys, VkDevice dev,
                           const char *const *inst_ext, int nb_inst_ext, const char *const *dev_ext, int nb_dev_ext,
                           int aux_family, int decode_family, dbv_queue_lock_fn device_queue_lock, int *err)
{
    AVBufferRef *ref = av_hwdevice_ctx_alloc(AV_HWDEVICE_TYPE_VULKAN);
    if (!ref) { *err = AVERROR(ENOMEM); return NULL; }
    AVHWDeviceContext *ctx = (AVHWDeviceContext *)ref->data;
    AVVulkanDeviceContext *vk = ctx->hwctx;
    vk->get_proc_addr = gipa;
    vk->inst = inst;
    vk->phys_dev = phys;
    vk->act_dev = dev;
    vk->device_features.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2;
    vk->enabled_inst_extensions = inst_ext;
    vk->nb_enabled_inst_extensions = nb_inst_ext;
    vk->enabled_dev_extensions = dev_ext;
    vk->nb_enabled_dev_extensions = nb_dev_ext;
    vk->qf[0] = (AVVulkanDeviceQueueFamily){ .idx = aux_family, .num = 1,
                                             .flags = VK_QUEUE_COMPUTE_BIT | VK_QUEUE_TRANSFER_BIT };
    vk->qf[1] = (AVVulkanDeviceQueueFamily){ .idx = decode_family, .num = 1, .flags = VK_QUEUE_VIDEO_DECODE_BIT_KHR };
    vk->nb_qf = 2;
#if FF_API_VULKAN_FIXED_QUEUES
    // libavutil < 62 still validates the old fixed fields ("compute" and "upload" are required there).
    vk->queue_family_index = -1;        vk->nb_graphics_queues = 0;
    vk->queue_family_comp_index = aux_family;   vk->nb_comp_queues = 1;
    vk->queue_family_tx_index = aux_family;     vk->nb_tx_queues = 1;
    vk->queue_family_decode_index = decode_family; vk->nb_decode_queues = 1;
    vk->queue_family_encode_index = -1; vk->nb_encode_queues = 0;
#endif
    queue_lock = device_queue_lock;
    vk->lock_queue = lock_any_queue;
    vk->unlock_queue = unlock_any_queue;
    *err = av_hwdevice_ctx_init(ref);
    if (*err < 0) { av_buffer_unref(&ref); return NULL; }
    return ref;
}

typedef struct DbvVkFrame {
    VkImage image;          // one multi-planar image (FFmpeg's default; disable_multiplane is not set)
    VkFormat format;
    VkImageLayout layout;
    VkSemaphore sem;        // timeline: wait for `sem_value`, signal `sem_value + 1`
    uint64_t sem_value;
    uint32_t queue_family;  // VK_QUEUE_FAMILY_IGNORED when the image is shared between families
    int nb_images;          // > 1: one image per plane (not handled)
} DbvVkFrame;

// Locks a Vulkan frame and reads what a submission touching it needs. Pair with dbv_vkframe_unlock.
void dbv_vkframe_lock(AVFrame *f, DbvVkFrame *out)
{
    AVHWFramesContext *fc = (AVHWFramesContext *)f->hw_frames_ctx->data;
    AVVulkanFramesContext *vkfc = fc->hwctx;
    AVVkFrame *vkf = (AVVkFrame *)f->data[0];
    vkfc->lock_frame(fc, vkf);
    out->image = vkf->img[0];
    out->format = vkfc->format[0];
    out->layout = vkf->layout[0];
    out->sem = vkf->sem[0];
    out->sem_value = vkf->sem_value[0];
    out->queue_family = vkf->queue_family[0];
    out->nb_images = 0;
    for (int i = 0; i < AV_NUM_DATA_POINTERS && vkf->img[i]; ++i)
        out->nb_images++;
}

// `signalled`: the submission signalled the semaphore to sem_value + 1 (the image is left in the layout it had).
void dbv_vkframe_unlock(AVFrame *f, int signalled)
{
    AVHWFramesContext *fc = (AVHWFramesContext *)f->hw_frames_ctx->data;
    AVVulkanFramesContext *vkfc = fc->hwctx;
    AVVkFrame *vkf = (AVVkFrame *)f->data[0];
    if (signalled)
        vkf->sem_value[0] += 1;
    vkfc->unlock_frame(fc, vkf);
}
