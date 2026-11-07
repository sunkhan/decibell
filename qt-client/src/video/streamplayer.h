// One watched stream's decode side: FFmpeg's hardware decoder (D3D11VA) on the window's own D3D11 device, so
// decoded pictures are GPU textures the scene graph can read without a copy to system memory (design doc §5).
//
// Two threads per stream:
//  - the source: in the client, the core's receive thread hands over reassembled frames; in the spike, a thread
//    reads a .dbv file and feeds its frames at the stream's frame rate (a high-resolution waitable timer);
//  - the decoder ("decibell-video-decode"): sends each frame to FFmpeg and publishes the newest decoded picture.
// The render thread takes the newest picture at its next sync (VideoItem); a picture it never took is replaced
// (counted as `replaced`): the screen shows the newest, as WebCodecs + a canvas does.
#pragma once

#include <QString>

#include <atomic>
#include <condition_variable>
#include <deque>
#include <functional>
#include <mutex>
#include <thread>
#include <vector>

#include "streamfile.h"

struct AVCodecContext;
struct AVFrame;
struct AVPacket;
struct ID3D11Device;
struct ID3D11DeviceContext4;
struct ID3D11Fence;
struct ID3D11Multithread;

namespace decibell::video {

class StreamPlayer
{
public:
    // `onFrame` runs on the decoder thread when a picture is waiting and none was waiting before.
    StreamPlayer(ID3D11Device *device, const QString &path, std::function<void()> onFrame);
    ~StreamPlayer();

    bool ok() const { return m_error.isEmpty(); }
    QString error() const { return m_error; }
    const char *codecName() const;
    uint32_t width() const { return m_file.width; }
    uint32_t height() const { return m_file.height; }
    uint32_t fps() const { return m_file.fps; }

    // Render thread: the newest decoded picture (AV_PIX_FMT_D3D11) or null. The caller frees it.
    AVFrame *takeLatest();

    std::atomic<uint64_t> fed{0}, decoded{0}, replaced{0}, errors{0}, queueDrops{0};

    // Benchmark samples (ms): the decode call, the decode until the GPU finished it, and feed -> picture handed to
    // the renderer.
    void startSamples();
    void recordLatency(float ms);
    std::vector<float> decodeSamples() const;
    std::vector<float> decodeDoneSamples() const;
    std::vector<float> latencySamples() const;

private:
    void sourceLoop();
    void decodeLoop();
    void publish(AVFrame *frame);

    StreamFile m_file;
    QString m_error;
    AVCodecContext *m_ctx = nullptr;
    ID3D11Multithread *m_mt = nullptr;
    // Diagnostic (DECIBELL_VIDEO_FENCE=1): wait until the GPU has finished each picture before publishing it, and
    // record how long that took (decode_done_ms). Off by default: it made things worse, not better (measured with a
    // game holding the GPU: decode-done p50 4-8 ms, p99 24-29 ms, and the decoder fell behind, 888 of 960 frames);
    // the GPU orders the blit after the decode by itself.
    ID3D11DeviceContext4 *m_context4 = nullptr;
    ID3D11Fence *m_fence = nullptr;
    void *m_fenceEvent = nullptr;
    uint64_t m_fenceValue = 0;
    std::function<void()> m_onFrame;

    std::mutex m_queueMutex;
    std::condition_variable m_queueCv;
    std::deque<AVPacket *> m_queue;
    std::atomic<bool> m_stop{false};

    std::mutex m_frameMutex;
    AVFrame *m_latest = nullptr;
    std::atomic<bool> m_notified{false};

    mutable std::mutex m_sampleMutex;
    bool m_sampling = false;
    std::vector<float> m_decodeMs, m_decodeDoneMs, m_latencyMs;

    std::thread m_source, m_decoder;
};

} // namespace decibell::video
