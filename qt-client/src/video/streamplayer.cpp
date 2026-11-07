#include "streamplayer.h"

#include <chrono>
#include <cstring>

#include <d3d11_4.h>
#include <windows.h>

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_d3d11va.h>
}

namespace decibell::video {

namespace {

int64_t nowNs()
{
    return std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now().time_since_epoch())
        .count();
}

// Hardware only: a software fallback would hand the renderer system-memory pictures (not this path's job yet).
AVPixelFormat pickD3D11(AVCodecContext *, const AVPixelFormat *formats)
{
    for (const AVPixelFormat *p = formats; *p != AV_PIX_FMT_NONE; ++p) {
        if (*p == AV_PIX_FMT_D3D11)
            return *p;
    }
    return AV_PIX_FMT_NONE;
}

// FFmpeg brackets each decode submission (begin frame .. end frame) with these. The device is also used by the
// render thread (the video processor blit, Qt's own drawing): taking the device's multithread lock keeps a blit
// from landing inside a submission.
void lockDevice(void *mt) { static_cast<ID3D11Multithread *>(mt)->Enter(); }
void unlockDevice(void *mt) { static_cast<ID3D11Multithread *>(mt)->Leave(); }

constexpr size_t kQueueDepth = 16;

} // namespace

StreamPlayer::StreamPlayer(ID3D11Device *device, const QString &path, std::function<void()> onFrame)
    : m_onFrame(std::move(onFrame))
{
    if (!m_file.open(path.toStdWString())) {
        m_error = QStringLiteral("cannot read %1").arg(path);
        return;
    }
    const char *decoderName = m_file.codec == Codec::H264 ? "h264" : m_file.codec == Codec::Hevc ? "hevc" : "av1";
    // By name: FFmpeg's own AV1 decoder carries the hardware path; a libdav1d build would be picked first by id.
    const AVCodec *codec = avcodec_find_decoder_by_name(decoderName);
    if (!codec) {
        m_error = QStringLiteral("no %1 decoder").arg(QLatin1StringView(decoderName));
        return;
    }
    ID3D11DeviceContext *immediate = nullptr;
    device->GetImmediateContext(&immediate);
    immediate->QueryInterface(__uuidof(ID3D11Multithread), reinterpret_cast<void **>(&m_mt));
    if (qEnvironmentVariableIntValue("DECIBELL_VIDEO_FENCE") == 1) {
        ID3D11Device5 *device5 = nullptr;
        if (SUCCEEDED(device->QueryInterface(__uuidof(ID3D11Device5), reinterpret_cast<void **>(&device5)))) {
            device5->CreateFence(0, D3D11_FENCE_FLAG_NONE, __uuidof(ID3D11Fence), reinterpret_cast<void **>(&m_fence));
            device5->Release();
        }
        immediate->QueryInterface(__uuidof(ID3D11DeviceContext4), reinterpret_cast<void **>(&m_context4));
        if (m_fence && m_context4)
            m_fenceEvent = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    }
    immediate->Release();
    if (!m_mt) {
        m_error = QStringLiteral("device has no ID3D11Multithread");
        return;
    }

    AVBufferRef *hw = av_hwdevice_ctx_alloc(AV_HWDEVICE_TYPE_D3D11VA);
    auto *hwctx = static_cast<AVD3D11VADeviceContext *>(reinterpret_cast<AVHWDeviceContext *>(hw->data)->hwctx);
    device->AddRef(); // released by FFmpeg with the device context
    hwctx->device = device;
    hwctx->lock = lockDevice;
    hwctx->unlock = unlockDevice;
    hwctx->lock_ctx = m_mt;
    if (int r = av_hwdevice_ctx_init(hw); r < 0) {
        av_buffer_unref(&hw);
        m_error = QStringLiteral("D3D11VA device init failed (%1)").arg(r);
        return;
    }
    m_codec = codec;
    m_hw = hw;
    // Synthetic files carry their parameter sets in-band: open now, so a failure shows as an error. A recorded
    // stream's first keyframe may bring a description: the decoder thread opens it then.
    if (m_file.version == 1 && !openDecoder({})) {
        m_error = QStringLiteral("avcodec_open2 failed");
        return;
    }
    m_decoder = std::thread([this] { decodeLoop(); });
    m_source = std::thread([this] { sourceLoop(); });
}

StreamPlayer::~StreamPlayer()
{
    m_stop = true;
    m_queueCv.notify_all();
    if (m_source.joinable())
        m_source.join();
    if (m_decoder.joinable())
        m_decoder.join();
    for (Queued &q : m_queue)
        av_packet_free(&q.pkt);
    av_frame_free(&m_latest);
    avcodec_free_context(&m_ctx);
    av_buffer_unref(&m_hw);
    if (m_fenceEvent)
        CloseHandle(m_fenceEvent);
    if (m_fence)
        m_fence->Release();
    if (m_context4)
        m_context4->Release();
    if (m_mt)
        m_mt->Release();
}

bool StreamPlayer::openDecoder(const std::vector<uint8_t> &extradata)
{
    avcodec_free_context(&m_ctx);
    m_ctx = avcodec_alloc_context3(m_codec);
    m_ctx->hw_device_ctx = av_buffer_ref(m_hw);
    m_ctx->get_format = pickD3D11;
    m_ctx->flags |= AV_CODEC_FLAG_LOW_DELAY;
    m_ctx->thread_count = 1;       // frame threads would hold pictures back; the GPU does the work
    m_ctx->extra_hw_frames = 4;    // the picture waiting for the renderer + the one being blitted, with margin
    if (!extradata.empty()) {
        // hvcC / av1C (/ avcC): FFmpeg then reads the stream's NAL framing from it, as WebCodecs does.
        m_ctx->extradata = static_cast<uint8_t *>(av_mallocz(extradata.size() + AV_INPUT_BUFFER_PADDING_SIZE));
        memcpy(m_ctx->extradata, extradata.data(), extradata.size());
        m_ctx->extradata_size = int(extradata.size());
    }
    m_extradata = extradata;
    if (avcodec_open2(m_ctx, m_codec, nullptr) < 0) {
        avcodec_free_context(&m_ctx);
        return false;
    }
    return true;
}

const char *StreamPlayer::codecName() const
{
    switch (m_file.codec) {
    case Codec::H264: return "H.264";
    case Codec::Hevc: return "HEVC";
    case Codec::Av1: return "AV1";
    }
    return "?";
}

void StreamPlayer::sourceLoop()
{
    SetThreadDescription(GetCurrentThread(), L"spike-stream-source");
    HANDLE timer = CreateWaitableTimerExW(nullptr, nullptr, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS);
    // Synthetic files play at their fps; a recorded stream on its recorded arrival times (start of file = now).
    const int64_t period = 1'000'000'000LL / (m_file.fps ? m_file.fps : 60);
    int64_t next = nowNs();
    int64_t origin = nowNs();
    FrameHeader h;
    while (!m_stop) {
        if (!m_file.next(h))
            break;
        AVPacket *pkt = av_packet_alloc();
        if (av_new_packet(pkt, int(h.size)) < 0 || !m_file.read(pkt->data, h.size)) {
            av_packet_free(&pkt);
            break;
        }
        if (h.atUs >= 0) {
            if (h.wrapped)
                origin = nowNs() - h.atUs * 1000;
            const int64_t early = origin + h.atUs * 1000 - nowNs(); // wait for this frame's own arrival time
            if (early > 0) {
                LARGE_INTEGER due;
                due.QuadPart = -(early / 100);
                SetWaitableTimer(timer, &due, 0, nullptr, nullptr, FALSE);
                WaitForSingleObject(timer, INFINITE);
            }
        }
        if (h.keyframe)
            pkt->flags |= AV_PKT_FLAG_KEY;
        pkt->pts = nowNs(); // carried through the decoder to the picture: feed -> render latency
        {
            std::lock_guard lock(m_queueMutex);
            if (m_queue.size() >= kQueueDepth) {
                av_packet_free(&m_queue.front().pkt);
                m_queue.pop_front();
                ++queueDrops;
            }
            m_queue.push_back({pkt, std::move(h.description)});
        }
        ++fed;
        m_queueCv.notify_one();
        if (h.atUs >= 0)
            continue; // paced before it was sent, above

        next += period;
        const int64_t wait = next - nowNs();
        if (wait > 0) {
            LARGE_INTEGER due;
            due.QuadPart = -(wait / 100);
            SetWaitableTimer(timer, &due, 0, nullptr, nullptr, FALSE);
            WaitForSingleObject(timer, INFINITE);
        } else if (wait < -4 * period) {
            next = nowNs(); // far behind (a debugger, the machine asleep): resync instead of bursting
        }
    }
    CloseHandle(timer);
}

void StreamPlayer::decodeLoop()
{
    SetThreadDescription(GetCurrentThread(), L"decibell-video-decode");
    AVFrame *frame = av_frame_alloc();
    // A recording can start mid-GOP: nothing decodes before its first keyframe.
    bool awaitKeyframe = true;
    for (;;) {
        Queued q;
        {
            std::unique_lock lock(m_queueMutex);
            m_queueCv.wait(lock, [this] { return m_stop || !m_queue.empty(); });
            if (m_stop)
                break;
            q = std::move(m_queue.front());
            m_queue.pop_front();
        }
        AVPacket *pkt = q.pkt;
        if (awaitKeyframe && !(pkt->flags & AV_PKT_FLAG_KEY)) {
            av_packet_free(&pkt);
            continue;
        }
        // A new or changed configuration record (a recording's first keyframe, a resolution change): reopen with
        // it, as StreamVideoPlayer reconfigures WebCodecs.
        if (!m_ctx || (!q.description.empty() && q.description != m_extradata)) {
            if (!openDecoder(q.description.empty() ? m_extradata : q.description)) {
                ++errors;
                av_packet_free(&pkt);
                continue;
            }
        }
        const int64_t t0 = nowNs();
        int r = avcodec_send_packet(m_ctx, pkt);
        av_packet_free(&pkt);
        if (r < 0) {
            ++errors;
            awaitKeyframe = true; // a broken reference chain: the client asks the streamer for a keyframe here
            avcodec_flush_buffers(m_ctx);
            continue;
        }
        awaitKeyframe = false;
        while ((r = avcodec_receive_frame(m_ctx, frame)) == 0) {
            ++decoded;
            m_frameWidth = frame->width;
            m_frameHeight = frame->height;
            const int64_t t1 = nowNs();
            if (m_fenceEvent) {
                // Signalled once the GPU has run everything submitted so far, this picture's decode included.
                m_mt->Enter();
                m_context4->Signal(m_fence, ++m_fenceValue);
                m_mt->Leave();
                if (m_fence->GetCompletedValue() < m_fenceValue) {
                    m_fence->SetEventOnCompletion(m_fenceValue, m_fenceEvent);
                    WaitForSingleObject(m_fenceEvent, 500);
                }
            }
            {
                std::lock_guard lock(m_sampleMutex);
                if (m_sampling) {
                    m_decodeMs.push_back(float(t1 - t0) / 1e6f);
                    m_decodeDoneMs.push_back(float(nowNs() - t0) / 1e6f);
                }
            }
            AVFrame *out = av_frame_alloc();
            av_frame_move_ref(out, frame);
            publish(out);
        }
        if (r != AVERROR(EAGAIN) && r != AVERROR_EOF) {
            ++errors;
            awaitKeyframe = true;
            avcodec_flush_buffers(m_ctx);
        }
    }
    av_frame_free(&frame);
}

void StreamPlayer::publish(AVFrame *frame)
{
    {
        std::lock_guard lock(m_frameMutex);
        if (m_latest) {
            av_frame_free(&m_latest);
            ++replaced;
        }
        m_latest = frame;
    }
    if (!m_notified.exchange(true))
        m_onFrame();
}

AVFrame *StreamPlayer::takeLatest()
{
    std::lock_guard lock(m_frameMutex);
    AVFrame *f = m_latest;
    m_latest = nullptr;
    m_notified = false;
    return f;
}

void StreamPlayer::startSamples()
{
    std::lock_guard lock(m_sampleMutex);
    m_sampling = true;
    m_decodeMs.clear();
    m_decodeDoneMs.clear();
    m_latencyMs.clear();
}

void StreamPlayer::recordLatency(float ms)
{
    std::lock_guard lock(m_sampleMutex);
    if (m_sampling)
        m_latencyMs.push_back(ms);
}

std::vector<float> StreamPlayer::decodeSamples() const
{
    std::lock_guard lock(m_sampleMutex);
    return m_decodeMs;
}

std::vector<float> StreamPlayer::decodeDoneSamples() const
{
    std::lock_guard lock(m_sampleMutex);
    return m_decodeDoneMs;
}

std::vector<float> StreamPlayer::latencySamples() const
{
    std::lock_guard lock(m_sampleMutex);
    return m_latencyMs;
}

} // namespace decibell::video
