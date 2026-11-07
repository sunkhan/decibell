// streamgen: a synthetic screen-share bitstream for the video spike, encoded the way the streamer does it
// (native/src/media/bitrate_preset.rs: NVENC p4, tune ull, CBR, no B-frames, zero latency, forced IDR), written
// one encoded frame per record in a small container (.dbv, below): Annex B for H.264 / HEVC with the parameter sets
// in-band, a temporal unit of OBUs for AV1 — whole frames, as the receive path's reassembler hands them on.
//
//   streamgen <out.dbv> <h264_nvenc|hevc_nvenc|av1_nvenc> <width> <height> <fps> <kbps> <seconds>
//
// .dbv: "DBV1", u8 codec (1 H.264, 2 HEVC, 3 AV1), 3 zero bytes, u32 width, u32 height, u32 fps; then per frame
// u32 size, u32 flags (bit 0: keyframe), the bytes. Little-endian. Read by qt-client/src/video/streamfile.h.
//
// The picture: a gradient, scrolling coloured bars and moving fine detail, so the encoder spends its bitrate and the
// decoder has real work; the frame number as a barcode band at the top (read back by tools/perf/framecount). Frames are made on the CPU in NV12 and NVENC uploads them (fine for a generator; the
// streamer itself encodes from GPU textures).
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/opt.h>
#include <libavutil/imgutils.h>
}

// The frame number as a barcode across the top 1/18 of the picture: 16 cells, most significant bit first, white for 1.
// tools/perf/framecount reads it back off the screen to tell exactly which frames were displayed.
static void barcode(AVFrame *f, int n, int w, int h)
{
    const int bandH = h / 18;
    for (int y = 0; y < bandH; ++y) {
        uint8_t *row = f->data[0] + size_t(y) * f->linesize[0];
        for (int x = 0; x < w; ++x)
            row[x] = ((n >> (15 - x * 16 / w)) & 1) ? 235 : 16;
    }
    for (int y = 0; y < (bandH + 1) / 2; ++y)
        memset(f->data[1] + size_t(y) * f->linesize[1], 128, size_t(w));
}

static void fill(AVFrame *f, int n, int w, int h)
{
    // Luma: horizontal gradient + vertical bars scrolling 8 px/frame + a moving checker for fine detail.
    for (int y = 0; y < h; ++y) {
        uint8_t *row = f->data[0] + size_t(y) * f->linesize[0];
        for (int x = 0; x < w; ++x) {
            const int bar = ((x + n * 8) / 64) & 1;
            const int checker = (((x + n * 3) >> 2) ^ ((y + n * 2) >> 2)) & 1;
            const int v = 32 + (x * 160) / w + bar * 30 + (y < h / 3 ? checker * 24 : 0);
            row[x] = uint8_t(v > 235 ? 235 : v);
        }
    }
    // Chroma: slow hue drift.
    for (int y = 0; y < h / 2; ++y) {
        uint8_t *row = f->data[1] + size_t(y) * f->linesize[1];
        for (int x = 0; x < w / 2; ++x) {
            row[2 * x] = uint8_t(128 + 60 * std::sin((x + n) * 0.01));
            row[2 * x + 1] = uint8_t(128 + 60 * std::cos((y + n) * 0.013));
        }
    }
}

int main(int argc, char **argv)
{
    if (argc != 8) {
        fprintf(stderr, "usage: streamgen <out> <h264_nvenc|hevc_nvenc|av1_nvenc> <w> <h> <fps> <kbps> <seconds>\n");
        return 2;
    }
    const char *out = argv[1], *enc = argv[2];
    const int w = atoi(argv[3]), h = atoi(argv[4]), fps = atoi(argv[5]), kbps = atoi(argv[6]), secs = atoi(argv[7]);
    const AVCodec *codec = avcodec_find_encoder_by_name(enc);
    if (!codec) { fprintf(stderr, "no encoder %s\n", enc); return 1; }
    AVCodecContext *c = avcodec_alloc_context3(codec);
    c->width = w; c->height = h;
    c->time_base = AVRational{1, fps};
    c->framerate = AVRational{fps, 1};
    c->pix_fmt = AV_PIX_FMT_NV12;
    c->bit_rate = int64_t(kbps) * 1000;
    c->rc_max_rate = c->bit_rate;
    c->rc_buffer_size = int(c->bit_rate / fps * 2);
    c->gop_size = fps * 4;
    c->max_b_frames = 0;
    // The streamer's NVENC options (bitrate_preset.rs preset_for).
    av_opt_set(c->priv_data, "preset", "p4", 0);
    av_opt_set(c->priv_data, "tune", "ull", 0);
    av_opt_set(c->priv_data, "rc", "cbr", 0);
    av_opt_set(c->priv_data, "forced-idr", "1", 0);
    if (strcmp(enc, "av1_nvenc") == 0) {
        av_opt_set(c->priv_data, "tile_columns", "2", 0);
        av_opt_set(c->priv_data, "tile_rows", "1", 0);
    } else {
        av_opt_set(c->priv_data, "b_ref_mode", "disabled", 0);
        av_opt_set(c->priv_data, "zerolatency", "1", 0);
    }
    if (int r = avcodec_open2(c, codec, nullptr); r < 0) { fprintf(stderr, "avcodec_open2: %d\n", r); return 1; }
    FILE *f = nullptr;
    if (fopen_s(&f, out, "wb") != 0 || !f) { fprintf(stderr, "cannot write %s\n", out); return 1; }
    const uint8_t codecId = strcmp(enc, "h264_nvenc") == 0 ? 1 : strcmp(enc, "hevc_nvenc") == 0 ? 2 : 3;
    const uint8_t magic[8] = {'D', 'B', 'V', '1', codecId, 0, 0, 0};
    const uint32_t dims[3] = {uint32_t(w), uint32_t(h), uint32_t(fps)};
    fwrite(magic, 1, sizeof magic, f);
    fwrite(dims, 4, 3, f);
    AVFrame *frame = av_frame_alloc();
    frame->format = c->pix_fmt; frame->width = w; frame->height = h;
    av_frame_get_buffer(frame, 32);
    AVPacket *pkt = av_packet_alloc();
    int64_t bytes = 0, packets = 0;
    auto drain = [&](bool flush) {
        while (true) {
            const int r = avcodec_receive_packet(c, pkt);
            if (r == AVERROR(EAGAIN) || r == AVERROR_EOF) break;
            if (r < 0) { fprintf(stderr, "receive_packet: %d\n", r); exit(1); }
            const uint32_t head[2] = {uint32_t(pkt->size), (pkt->flags & AV_PKT_FLAG_KEY) ? 1u : 0u};
            fwrite(head, 4, 2, f);
            fwrite(pkt->data, 1, size_t(pkt->size), f);
            bytes += pkt->size; ++packets;
            av_packet_unref(pkt);
        }
        (void)flush;
    };
    for (int n = 0; n < fps * secs; ++n) {
        av_frame_make_writable(frame);
        fill(frame, n, w, h);
        barcode(frame, n, w, h);
        frame->pts = n;
        if (avcodec_send_frame(c, frame) < 0) { fprintf(stderr, "send_frame failed\n"); return 1; }
        drain(false);
    }
    avcodec_send_frame(c, nullptr);
    drain(true);
    fclose(f);
    printf("%s: %lld packets, %.1f MB, %.1f Mbit/s\n", out, (long long)packets, bytes / 1e6, bytes * 8.0 / 1e6 / secs);
    av_packet_free(&pkt); av_frame_free(&frame); avcodec_free_context(&c);
    return 0;
}
