// Reads FFmpeg's description of a DRM PRIME frame (what av_hwframe_map gives for a VA-API surface) with the
// struct layouts of the FFmpeg that is linked.
#include <libavutil/frame.h>
#include <libavutil/hwcontext_drm.h>

typedef struct DbvDrmPlane {
    int fd;             // the DMA-BUF the plane lives in (owned by the frame: valid until it is unreferenced)
    uint32_t format;    // the DRM fourcc of the plane's layer (R8, GR88, or NV12 for a two-plane layer)
    uint64_t size;      // of the DMA-BUF; 0 when the exporter did not say
    uint64_t modifier;  // DRM format modifier of the DMA-BUF (DRM_FORMAT_MOD_INVALID: unknown)
    uint64_t offset;    // of the plane's first byte in the DMA-BUF
    uint64_t pitch;     // bytes per row
} DbvDrmPlane;

// The planes of an AV_PIX_FMT_DRM_PRIME frame in layer order (for NV12: luma, then chroma, whether the exporter
// gave one layer per plane or one layer with two planes). Returns their number, or -1 if there are more than `max`.
int dbv_drm_planes(const AVFrame *frame, DbvDrmPlane *out, int max)
{
    const AVDRMFrameDescriptor *desc = (const AVDRMFrameDescriptor *)frame->data[0];
    int count = 0;
    for (int l = 0; l < desc->nb_layers; ++l) {
        const AVDRMLayerDescriptor *layer = &desc->layers[l];
        for (int p = 0; p < layer->nb_planes; ++p) {
            const AVDRMPlaneDescriptor *plane = &layer->planes[p];
            if (count == max || plane->object_index < 0 || plane->object_index >= desc->nb_objects)
                return -1;
            const AVDRMObjectDescriptor *object = &desc->objects[plane->object_index];
            out[count++] = (DbvDrmPlane){ object->fd, layer->format, object->size, object->format_modifier,
                                          (uint64_t)plane->offset, (uint64_t)plane->pitch };
        }
    }
    return count;
}
