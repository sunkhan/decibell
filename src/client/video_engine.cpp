#include "video_engine.hpp"
#include <QDebug>
#include <QPixmap>
#include <chrono>

#ifdef _WIN32
#include <windows.h>
#pragma comment(lib, "winmm.lib")  // For timeBeginPeriod/timeEndPeriod

// Windows Graphics Capture (WGC) — requires Windows 10 1803+
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Graphics.Capture.h>
#include <winrt/Windows.Graphics.DirectX.h>
#include <winrt/Windows.Graphics.DirectX.Direct3D11.h>
#include <windows.graphics.capture.interop.h>
#include <windows.graphics.directx.direct3d11.interop.h>
#include <inspectable.h>
#include <dwmapi.h>
#endif

#ifdef _WIN32
// PIMPL struct for Windows Graphics Capture — isolates WinRT types from Qt headers
struct VideoEngine::WgcState {
    winrt::Windows::Graphics::Capture::GraphicsCaptureItem item{nullptr};
    winrt::Windows::Graphics::Capture::Direct3D11CaptureFramePool frame_pool{nullptr};
    winrt::Windows::Graphics::Capture::GraphicsCaptureSession session{nullptr};
    winrt::Windows::Graphics::DirectX::Direct3D11::IDirect3DDevice winrt_device{nullptr};
    ID3D11Texture2D* staging_texture = nullptr;

    ~WgcState() {
        if (session) { session.Close(); session = nullptr; }
        if (frame_pool) { frame_pool.Close(); frame_pool = nullptr; }
        item = nullptr;
        winrt_device = nullptr;
        if (staging_texture) { staging_texture->Release(); staging_texture = nullptr; }
    }
};
#endif

// SIMD-optimized ARGB scaling via libyuv — 5-10x faster than QImage::scaled()
static QImage scaleArgb(const uint8_t* src, int src_stride, int src_w, int src_h,
                        int dst_w, int dst_h) {
    QImage dst(dst_w, dst_h, QImage::Format_ARGB32);
    libyuv::ARGBScale(
        src, src_stride, src_w, src_h,
        dst.bits(), dst.bytesPerLine(), dst_w, dst_h,
        libyuv::kFilterBilinear
    );
    return dst;
}

// Convert a QImage (Format_ARGB32) to QVideoFrame for efficient QML rendering.
// QImage::Format_ARGB32 is BGRA byte-order on little-endian, matching Format_BGRA8888.
static QVideoFrame imageToVideoFrame(const QImage& img) {
    QVideoFrameFormat fmt(img.size(), QVideoFrameFormat::Format_BGRA8888);
    QVideoFrame frame(fmt);
    frame.map(QVideoFrame::WriteOnly);
    memcpy(frame.bits(0), img.constBits(), img.sizeInBytes());
    frame.unmap();
    return frame;
}

VideoEngine::VideoEngine(QObject* parent) : QObject(parent) {
    capture_timer_ = new QTimer(this);
    connect(capture_timer_, &QTimer::timeout, this, &VideoEngine::grabScreen);
}

VideoEngine::~VideoEngine() {
    stopStream();

    for (auto& pair : decoders_) {
        if (pair.second.vpx_initialized) {
            vpx_codec_destroy(&pair.second.vpx_decoder);
        }
#ifdef _WIN32
        if (pair.second.mft_decoder) {
            pair.second.mft_decoder->Release();
        }
#endif
    }

#ifdef _WIN32
    if (mf_started_) {
        MFShutdown();
        mf_started_ = false;
    }
#endif
}

void VideoEngine::initEncoder(int width, int height, int fps, int bitrate) {
    if (encoder_initialized_) destroyEncoder();

    vpx_codec_enc_cfg_t cfg;
    vpx_codec_err_t res = vpx_codec_enc_config_default(vpx_codec_vp9_cx(), &cfg, 0);
    if (res != VPX_CODEC_OK) {
        qCritical() << "Failed to get default VP9 config.";
        return;
    }

    cfg.g_w = width;
    cfg.g_h = height;
    cfg.rc_target_bitrate = bitrate;
    cfg.g_timebase.num = 1;
    cfg.g_timebase.den = fps;
    cfg.g_error_resilient = VPX_ERROR_RESILIENT_DEFAULT;
    cfg.rc_end_usage = VPX_CBR;
    cfg.g_lag_in_frames = 0;
    cfg.g_threads = 4;
    cfg.kf_max_dist = fps * 2;

    // VP9 CBR tuning — slightly relaxed to let the encoder allocate more bits
    // to complex scenes and fewer to static ones, improving perceived quality.
    cfg.rc_buf_initial_sz = 500;
    cfg.rc_buf_optimal_sz = 600;
    cfg.rc_buf_sz = 1000;
    cfg.rc_undershoot_pct = 50;   // Don't force encoder to always hit target; save bits for motion
    cfg.rc_overshoot_pct = 50;    // Allow short bursts above target for sharp detail
    cfg.rc_min_quantizer = 2;     // Don't let quantizer go so low it wastes bits on imperceptible detail
    cfg.rc_max_quantizer = 52;    // Floor: prevent encoder from dropping quality too far

    if (!vpx_img_alloc(&raw_img_, VPX_IMG_FMT_I420, width, height, 1)) {
        qCritical() << "Failed to allocate image.";
        return;
    }

    if (vpx_codec_enc_init(&encoder_, vpx_codec_vp9_cx(), &cfg, 0)) {
        qCritical() << "Failed to initialize VP9 encoder.";
        vpx_img_free(&raw_img_);
        return;
    }

    // VP9 speed 6: good balance of quality and CPU usage for real-time streaming.
    // Speed 9 skips too many RD optimizations and produces noticeably worse output
    // at the same bitrate.  Speed 6 keeps motion smooth while giving the encoder
    // enough room to produce sharp detail.
    vpx_codec_control(&encoder_, VP8E_SET_CPUUSED, 6);
    vpx_codec_control(&encoder_, VP9E_SET_AQ_MODE, 3);      // Cyclic refresh AQ
    vpx_codec_control(&encoder_, VP9E_SET_TILE_COLUMNS, 2);  // 4 tile columns for parallelism
    vpx_codec_control(&encoder_, VP9E_SET_ROW_MT, 1);        // Row-based multi-threading
    vpx_codec_control(&encoder_, VP8E_SET_STATIC_THRESHOLD, 0);
    vpx_codec_control(&encoder_, VP9E_SET_FRAME_PARALLEL_DECODING, 1);  // Help decoders
    vpx_codec_control(&encoder_, VP8E_SET_MAX_INTRA_BITRATE_PCT, 300);  // Cap keyframe size

    encoder_initialized_ = true;
    force_keyframe_ = false;
    qDebug() << "Initialized VP9 Encoder:" << width << "x" << height << "@" << fps << "fps," << bitrate << "kbps";
}

void VideoEngine::destroyEncoder() {
    if (encoder_initialized_) {
        vpx_codec_destroy(&encoder_);
        vpx_img_free(&raw_img_);
        encoder_initialized_ = false;
    }
}

void VideoEngine::startStream(const QString& token, const QString& host, quint16 port, int fps, int bitrate, int width, int height, int captureType, quintptr captureId, bool adaptiveBitrate) {
    stopStream();

    token_ = token;
    // Use last 31 chars of JWT as compact UDP identifier (signature portion, unique per user)
    std::string full_token = token.toStdString();
    size_t id_len = std::min(full_token.size(), size_t(chatproj::SENDER_ID_SIZE - 1));
    udp_id_ = full_token.substr(full_token.size() - id_len);
    server_host_ = QHostAddress(host);
    server_port_ = port;
    current_fps_ = fps;
    current_bitrate_ = bitrate;
    configured_bitrate_ = bitrate;
    adaptive_bitrate_enabled_ = adaptiveBitrate;
    nack_packets_received_ = 0;
    total_packets_sent_ = 0;
    frame_id_ = 0;

    capture_type_ = captureType;
    if (captureType == 1) {
        capture_window_id_ = captureId;
        capture_screen_index_ = 0;
    } else {
        capture_screen_index_ = static_cast<int>(captureId);
        capture_window_id_ = 0;
    }
    // VP9 requires even dimensions
    target_width_ = (width + 1) & ~1;
    target_height_ = (height + 1) & ~1;

    is_streaming_ = true;
    frame_ready_ = false;

#ifdef _WIN32
    // Encoder init is deferred to the capture thread for both paths:
    // - Screen capture (DXGI): init in captureAndEncodeDxgi() after D3D11 device creation
    // - Window capture (WGC): init in captureAndEncodeWgc() after WGC creates D3D11 device
#else
    initEncoder(target_width_, target_height_, current_fps_, current_bitrate_);
#endif

    capture_thread_ = std::thread(&VideoEngine::captureAndEncodeLoop, this);

#ifndef _WIN32
    // Non-Windows: use QTimer + grabScreen (main-thread capture) as fallback
    capture_timer_->start(1000 / current_fps_);
#endif
}

void VideoEngine::stopStream() {
    if (is_streaming_) {
        is_streaming_ = false;
        if (capture_timer_) capture_timer_->stop();
        capture_cv_.notify_all();
        if (capture_thread_.joinable()) {
            capture_thread_.join();
        }
    }
#ifdef _WIN32
    cleanupWgcCapture();
    cleanupDxgiCapture();
    cleanupHardwareEncoder();
#endif
    destroyEncoder();
}

void VideoEngine::requestKeyframe() {
    force_keyframe_ = true;
}

// grabScreen() is only used as a fallback on non-Windows platforms.
// On Windows, capture happens directly in the worker thread via DXGI/PrintWindow.
void VideoEngine::grabScreen() {
    if (!is_streaming_) return;

    QPixmap pixmap;
    if (capture_type_ == 1) {
        QScreen* screen = QGuiApplication::primaryScreen();
        if (!screen) return;
        pixmap = screen->grabWindow(capture_window_id_);
    } else {
        auto screens = QGuiApplication::screens();
        int idx = capture_screen_index_;
        if (idx < 0 || idx >= screens.size()) idx = 0;
        QScreen* screen = screens.at(idx);
        pixmap = screen->grabWindow(0);
    }

    if (pixmap.isNull()) return;
    QImage image = pixmap.toImage();

    {
        std::lock_guard<std::mutex> lock(capture_mutex_);
        latest_frame_ = image;
        frame_ready_ = true;
    }
    capture_cv_.notify_one();
    emit localFrameCaptured(imageToVideoFrame(image));
}

// Pace UDP sends: for N packets, insert a short delay between each to avoid
// burst-induced packet loss at the NIC/router.  Uses ~50% of the frame budget
// so the pacing finishes well before the next capture.
static void pacePackets(int packet_index, int total_packets, int fps) {
    if (total_packets <= 1 || packet_index == 0) return;
    // Spread across half the frame interval
    auto budget = std::chrono::microseconds(500000 / fps);  // 50% of frame interval
    auto gap = budget / total_packets;
    // Only pace if gap is meaningful (>50us) to avoid busy-waiting overhead
    if (gap > std::chrono::microseconds(50)) {
        std::this_thread::sleep_for(gap);
    }
}

// Shared UDP fragmentation for any encoded bitstream (VP9 or H.264)
void VideoEngine::fragmentAndSend(const uint8_t* data, size_t size, bool is_keyframe, uint8_t codec) {
    if (size == 0) return;

    const uint8_t* read_ptr = data;
    size_t remaining = size;
    uint16_t packet_index = 0;
    uint16_t total_packets = static_cast<uint16_t>((remaining + chatproj::UDP_MAX_PAYLOAD - 1) / chatproj::UDP_MAX_PAYLOAD);

    // FEC accumulator: XOR payload buffer and payload_size XOR for current group
    uint8_t fec_payload[chatproj::UDP_MAX_PAYLOAD] = {};
    uint16_t fec_size_xor = 0;
    uint16_t fec_group_start = 0;
    uint16_t fec_group_count = 0;

    while (remaining > 0) {
        pacePackets(packet_index, total_packets, current_fps_);
        size_t chunk_size = std::min(remaining, static_cast<size_t>(chatproj::UDP_MAX_PAYLOAD));

        chatproj::UdpVideoPacket vp;
        vp.packet_type = chatproj::UdpPacketType::VIDEO;
        std::memset(vp.sender_id, 0, chatproj::SENDER_ID_SIZE);
        std::memcpy(vp.sender_id, udp_id_.c_str(), std::min(udp_id_.size(), size_t(chatproj::SENDER_ID_SIZE - 1)));
        vp.frame_id = frame_id_;
        vp.packet_index = packet_index;
        vp.total_packets = total_packets;
        vp.payload_size = static_cast<uint16_t>(chunk_size);
        vp.is_keyframe = is_keyframe;
        vp.codec = codec;
        std::memcpy(vp.payload, read_ptr, chunk_size);

        size_t packet_total_size = sizeof(chatproj::UdpVideoPacket) - chatproj::UDP_MAX_PAYLOAD + chunk_size;
        QByteArray out_data(reinterpret_cast<const char*>(&vp), static_cast<int>(packet_total_size));
        emit sendUdpData(out_data, server_host_, server_port_);
        total_packets_sent_++;

        // Save to retransmission buffer for NACK recovery
        {
            uint64_t key = (static_cast<uint64_t>(frame_id_) << 16) | packet_index;
            std::lock_guard<std::mutex> lock(retx_mutex_);
            retx_buffer_[key] = out_data;
        }

        // Accumulate FEC: XOR this packet's payload (zero-padded) and size
        for (size_t i = 0; i < chunk_size; i++)
            fec_payload[i] ^= vp.payload[i];
        // Zero-pad region is already 0 in fec_payload, XOR with 0 is no-op
        fec_size_xor ^= vp.payload_size;
        fec_group_count++;

        // Emit FEC packet when group is full
        if (fec_group_count == chatproj::FEC_GROUP_SIZE) {
            chatproj::UdpFecPacket fec;
            fec.packet_type = chatproj::UdpPacketType::FEC;
            std::memset(fec.sender_id, 0, chatproj::SENDER_ID_SIZE);
            std::memcpy(fec.sender_id, udp_id_.c_str(), std::min(udp_id_.size(), size_t(chatproj::SENDER_ID_SIZE - 1)));
            fec.frame_id = frame_id_;
            fec.group_start = fec_group_start;
            fec.group_count = fec_group_count;
            fec.payload_size_xor = fec_size_xor;
            std::memcpy(fec.payload, fec_payload, chatproj::UDP_MAX_PAYLOAD);

            QByteArray fec_data(reinterpret_cast<const char*>(&fec), sizeof(fec));
            emit sendUdpData(fec_data, server_host_, server_port_);

            // Reset for next group
            std::memset(fec_payload, 0, chatproj::UDP_MAX_PAYLOAD);
            fec_size_xor = 0;
            fec_group_start = packet_index + 1;
            fec_group_count = 0;
        }

        read_ptr += chunk_size;
        remaining -= chunk_size;
        packet_index++;
    }

    // Emit FEC for the trailing partial group (if any packets remain)
    if (fec_group_count > 1) {
        chatproj::UdpFecPacket fec;
        fec.packet_type = chatproj::UdpPacketType::FEC;
        std::memset(fec.sender_id, 0, chatproj::SENDER_ID_SIZE);
        std::memcpy(fec.sender_id, udp_id_.c_str(), std::min(udp_id_.size(), size_t(chatproj::SENDER_ID_SIZE - 1)));
        fec.frame_id = frame_id_;
        fec.group_start = fec_group_start;
        fec.group_count = fec_group_count;
        fec.payload_size_xor = fec_size_xor;
        std::memcpy(fec.payload, fec_payload, chatproj::UDP_MAX_PAYLOAD);

        QByteArray fec_data(reinterpret_cast<const char*>(&fec), sizeof(fec));
        emit sendUdpData(fec_data, server_host_, server_port_);
    }

    // Evict old frames from retransmission buffer
    {
        std::lock_guard<std::mutex> lock(retx_mutex_);
        if (frame_id_ >= RETX_BUFFER_MAX_FRAMES) {
            uint32_t evict_before = frame_id_ - RETX_BUFFER_MAX_FRAMES + 1;
            while (retx_oldest_frame_ < evict_before) {
                // Remove all packets for the oldest frame
                for (uint16_t i = 0; i < 1000; i++) {
                    uint64_t key = (static_cast<uint64_t>(retx_oldest_frame_) << 16) | i;
                    if (retx_buffer_.erase(key) == 0) break;
                }
                retx_oldest_frame_++;
            }
        }
    }

    frame_id_++;
}

// Shared encode+send logic: assumes raw_img_ is already filled with I420 data
void VideoEngine::encodeAndSendFrame() {
    int flags = 0;
    if (force_keyframe_.exchange(false) || frame_id_ % (current_fps_ * 2) == 0) {
        flags |= VPX_EFLAG_FORCE_KF;
    }

    if (vpx_codec_encode(&encoder_, &raw_img_, frame_id_, 1, flags, VPX_DL_REALTIME)) {
        qWarning() << "Failed to encode video frame.";
        return;
    }

    vpx_codec_iter_t iter = nullptr;
    const vpx_codec_cx_pkt_t *pkt = nullptr;

    std::vector<uint8_t> frame_buffer;
    bool is_keyframe = false;

    while ((pkt = vpx_codec_get_cx_data(&encoder_, &iter)) != nullptr) {
        if (pkt->kind == VPX_CODEC_CX_FRAME_PKT) {
            const uint8_t* raw_buffer = static_cast<const uint8_t*>(pkt->data.frame.buf);
            size_t sz = pkt->data.frame.sz;
            frame_buffer.insert(frame_buffer.end(), raw_buffer, raw_buffer + sz);
            if (pkt->data.frame.flags & VPX_FRAME_IS_KEY) {
                is_keyframe = true;
            }
        }
    }

    if (!frame_buffer.empty()) {
        fragmentAndSend(frame_buffer.data(), frame_buffer.size(), is_keyframe, active_codec_);
    }
}

// ---------------------- DXGI DESKTOP DUPLICATION (Windows) ----------------------

#ifdef _WIN32

bool VideoEngine::initDxgiCapture(int screenIndex) {
    cleanupDxgiCapture();

    // Create D3D11 device with video support (required for D3D11 Video Processor)
    D3D_FEATURE_LEVEL featureLevel;
    HRESULT hr = D3D11CreateDevice(
        nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr,
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT, nullptr, 0,
        D3D11_SDK_VERSION,
        &d3d_device_, &featureLevel, &d3d_context_
    );
    if (FAILED(hr)) {
        qWarning() << "Failed to create D3D11 device. HRESULT:" << Qt::hex << hr;
        return false;
    }

    // Enable multithread protection — the video processor and encoder may
    // issue GPU work that overlaps with DXGI operations
    ID3D10Multithread* mt = nullptr;
    if (SUCCEEDED(d3d_device_->QueryInterface(__uuidof(ID3D10Multithread), (void**)&mt))) {
        mt->SetMultithreadProtected(TRUE);
        mt->Release();
    }

    // Get DXGI device -> adapter -> output
    IDXGIDevice* dxgiDevice = nullptr;
    hr = d3d_device_->QueryInterface(__uuidof(IDXGIDevice), reinterpret_cast<void**>(&dxgiDevice));
    if (FAILED(hr)) {
        qWarning() << "Failed to get IDXGIDevice.";
        cleanupDxgiCapture();
        return false;
    }

    IDXGIAdapter* adapter = nullptr;
    hr = dxgiDevice->GetAdapter(&adapter);
    dxgiDevice->Release();
    if (FAILED(hr)) {
        qWarning() << "Failed to get DXGI adapter.";
        cleanupDxgiCapture();
        return false;
    }

    IDXGIOutput* output = nullptr;
    hr = adapter->EnumOutputs(screenIndex, &output);
    adapter->Release();
    if (FAILED(hr)) {
        qWarning() << "Failed to enumerate DXGI output" << screenIndex;
        cleanupDxgiCapture();
        return false;
    }

    IDXGIOutput1* output1 = nullptr;
    hr = output->QueryInterface(__uuidof(IDXGIOutput1), reinterpret_cast<void**>(&output1));
    output->Release();
    if (FAILED(hr)) {
        qWarning() << "Failed to get IDXGIOutput1 (requires Windows 8+).";
        cleanupDxgiCapture();
        return false;
    }

    hr = output1->DuplicateOutput(d3d_device_, &desk_dupl_);
    output1->Release();
    if (FAILED(hr)) {
        qWarning() << "Failed to duplicate output. HRESULT:" << Qt::hex << hr;
        cleanupDxgiCapture();
        return false;
    }

    // Create a staging texture for CPU readback
    DXGI_OUTDUPL_DESC duplDesc;
    desk_dupl_->GetDesc(&duplDesc);

    D3D11_TEXTURE2D_DESC texDesc = {};
    texDesc.Width = duplDesc.ModeDesc.Width;
    texDesc.Height = duplDesc.ModeDesc.Height;
    texDesc.MipLevels = 1;
    texDesc.ArraySize = 1;
    texDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    texDesc.SampleDesc.Count = 1;
    texDesc.Usage = D3D11_USAGE_STAGING;
    texDesc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;

    hr = d3d_device_->CreateTexture2D(&texDesc, nullptr, &staging_texture_);
    if (FAILED(hr)) {
        qWarning() << "Failed to create staging texture.";
        cleanupDxgiCapture();
        return false;
    }

    qDebug() << "DXGI Desktop Duplication initialized:" << texDesc.Width << "x" << texDesc.Height;
    return true;
}

void VideoEngine::cleanupDxgiCapture() {
    cleanupVideoProcessor();
    if (staging_texture_) { staging_texture_->Release(); staging_texture_ = nullptr; }
    if (desk_dupl_) { desk_dupl_->Release(); desk_dupl_ = nullptr; }
    if (d3d_context_) { d3d_context_->Release(); d3d_context_ = nullptr; }
    if (d3d_device_) { d3d_device_->Release(); d3d_device_ = nullptr; }
}

// ---------------------- D3D11 VIDEO PROCESSOR (GPU COLOR CONVERSION) ----------------------

bool VideoEngine::initVideoProcessor(int input_w, int input_h) {
    cleanupVideoProcessor();
    if (!d3d_device_ || !d3d_context_) return false;

    // Get video device and context interfaces
    HRESULT hr = d3d_device_->QueryInterface(__uuidof(ID3D11VideoDevice), (void**)&video_device_);
    if (FAILED(hr)) { qDebug() << "GPU has no video device interface"; return false; }

    hr = d3d_context_->QueryInterface(__uuidof(ID3D11VideoContext), (void**)&video_context_);
    if (FAILED(hr)) { cleanupVideoProcessor(); return false; }

    // Create enumerator describing the conversion: BGRA input → NV12 output
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC desc = {};
    desc.InputFrameFormat = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
    desc.InputWidth = input_w;
    desc.InputHeight = input_h;
    desc.OutputWidth = target_width_;
    desc.OutputHeight = target_height_;
    desc.Usage = D3D11_VIDEO_USAGE_PLAYBACK_NORMAL;

    hr = video_device_->CreateVideoProcessorEnumerator(&desc, &vp_enum_);
    if (FAILED(hr)) { cleanupVideoProcessor(); return false; }

    hr = video_device_->CreateVideoProcessor(vp_enum_, 0, &video_processor_);
    if (FAILED(hr)) { cleanupVideoProcessor(); return false; }

    // Create persistent GPU BGRA texture (CopyResource target from desktop texture)
    D3D11_TEXTURE2D_DESC bgra_desc = {};
    bgra_desc.Width = input_w;
    bgra_desc.Height = input_h;
    bgra_desc.MipLevels = 1;
    bgra_desc.ArraySize = 1;
    bgra_desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    bgra_desc.SampleDesc.Count = 1;
    bgra_desc.Usage = D3D11_USAGE_DEFAULT;
    bgra_desc.BindFlags = D3D11_BIND_RENDER_TARGET;

    hr = d3d_device_->CreateTexture2D(&bgra_desc, nullptr, &gpu_bgra_texture_);
    if (FAILED(hr)) { cleanupVideoProcessor(); return false; }

    // Create NV12 output texture for the encoder
    D3D11_TEXTURE2D_DESC nv12_desc = {};
    nv12_desc.Width = target_width_;
    nv12_desc.Height = target_height_;
    nv12_desc.MipLevels = 1;
    nv12_desc.ArraySize = 1;
    nv12_desc.Format = DXGI_FORMAT_NV12;
    nv12_desc.SampleDesc.Count = 1;
    nv12_desc.Usage = D3D11_USAGE_DEFAULT;
    nv12_desc.BindFlags = D3D11_BIND_RENDER_TARGET;

    hr = d3d_device_->CreateTexture2D(&nv12_desc, nullptr, &nv12_texture_);
    if (FAILED(hr)) {
        qDebug() << "GPU does not support NV12 render target textures";
        cleanupVideoProcessor();
        return false;
    }

    // Create video processor input view on the BGRA texture
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC ivd = {};
    ivd.FourCC = 0;
    ivd.ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D;
    ivd.Texture2D.MipSlice = 0;
    hr = video_device_->CreateVideoProcessorInputView(gpu_bgra_texture_, vp_enum_, &ivd, &vp_input_view_);
    if (FAILED(hr)) { cleanupVideoProcessor(); return false; }

    // Create video processor output view on the NV12 texture
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC ovd = {};
    ovd.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
    ovd.Texture2D.MipSlice = 0;
    hr = video_device_->CreateVideoProcessorOutputView(nv12_texture_, vp_enum_, &ovd, &vp_output_view_);
    if (FAILED(hr)) { cleanupVideoProcessor(); return false; }

    vp_input_width_ = input_w;
    vp_input_height_ = input_h;
    qDebug() << "D3D11 Video Processor initialized:" << input_w << "x" << input_h
             << "→" << target_width_ << "x" << target_height_ << "(BGRA→NV12)";
    return true;
}

void VideoEngine::cleanupVideoProcessor() {
    if (vp_output_view_) { vp_output_view_->Release(); vp_output_view_ = nullptr; }
    if (vp_input_view_) { vp_input_view_->Release(); vp_input_view_ = nullptr; }
    if (nv12_texture_) { nv12_texture_->Release(); nv12_texture_ = nullptr; }
    if (gpu_bgra_texture_) { gpu_bgra_texture_->Release(); gpu_bgra_texture_ = nullptr; }
    if (video_processor_) { video_processor_->Release(); video_processor_ = nullptr; }
    if (vp_enum_) { vp_enum_->Release(); vp_enum_ = nullptr; }
    if (video_context_) { video_context_->Release(); video_context_ = nullptr; }
    if (video_device_) { video_device_->Release(); video_device_ = nullptr; }
    if (dxgi_device_manager_) { dxgi_device_manager_->Release(); dxgi_device_manager_ = nullptr; }
    hw_d3d11_aware_ = false;
    vp_input_width_ = 0;
    vp_input_height_ = 0;
}

bool VideoEngine::gpuEncodeAndSendFrame(ID3D11Texture2D* desktop_texture, int cap_w, int cap_h) {
    if (!video_processor_ || !hw_encoder_initialized_ || !hw_d3d11_aware_) return false;

    // If capture dimensions changed (resolution change), reinit the video processor
    if (cap_w != vp_input_width_ || cap_h != vp_input_height_) {
        if (!initVideoProcessor(cap_w, cap_h)) return false;
    }

    // Step 1: Drain pending output and wait for input readiness
    drainEncoderOutput();

    // For async MFTs, wait for METransformNeedInput (with timeout)
    if (hw_encoder_async_ && hw_event_gen_) {
        bool input_ready = false;
        for (int attempt = 0; attempt < 50; attempt++) {
            IMFMediaEvent* event = nullptr;
            HRESULT evtHr = hw_event_gen_->GetEvent(MF_EVENT_FLAG_NO_WAIT, &event);
            if (FAILED(evtHr) || !event) {
                std::this_thread::sleep_for(std::chrono::microseconds(200));
                continue;
            }
            MediaEventType eventType;
            event->GetType(&eventType);
            event->Release();

            if (eventType == METransformNeedInput) {
                input_ready = true;
                break;
            } else if (eventType == METransformHaveOutput) {
                // Collect straggling output first, then keep waiting for NeedInput
                MFT_OUTPUT_DATA_BUFFER outputData = {};
                MFT_OUTPUT_STREAM_INFO streamInfo = {};
                hw_encoder_->GetOutputStreamInfo(0, &streamInfo);
                IMFSample* outSample = nullptr;
                if (!(streamInfo.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES)) {
                    MFCreateSample(&outSample);
                    IMFMediaBuffer* outBuf = nullptr;
                    MFCreateMemoryBuffer(streamInfo.cbSize > 0 ? streamInfo.cbSize : 1024 * 1024, &outBuf);
                    outSample->AddBuffer(outBuf);
                    outBuf->Release();
                    outputData.pSample = outSample;
                }
                DWORD status = 0;
                if (SUCCEEDED(hw_encoder_->ProcessOutput(0, 1, &outputData, &status)) && outputData.pSample) {
                    bool kf = false;
                    UINT32 cp = 0;
                    if (SUCCEEDED(outputData.pSample->GetUINT32(MFSampleExtension_CleanPoint, &cp))) kf = (cp != 0);
                    IMFMediaBuffer* ob = nullptr;
                    outputData.pSample->ConvertToContiguousBuffer(&ob);
                    if (ob) {
                        BYTE* d; DWORD s;
                        ob->Lock(&d, nullptr, &s);
                        if (s > 0) fragmentAndSend(d, s, kf, chatproj::CODEC_H264);
                        ob->Unlock();
                        ob->Release();
                    }
                    outputData.pSample->Release();
                    if (outSample && outSample != outputData.pSample) outSample->Release();
                } else {
                    if (outSample) outSample->Release();
                }
            }
        }
        if (!input_ready) return false;
    }

    // Step 2: Copy desktop texture to our persistent BGRA texture (fast GPU→GPU DMA)
    d3d_context_->CopyResource(gpu_bgra_texture_, desktop_texture);

    // Step 3: BGRA→NV12 + scaling on GPU via Video Processor
    D3D11_VIDEO_PROCESSOR_STREAM stream = {};
    stream.Enable = TRUE;
    stream.pInputSurface = vp_input_view_;
    HRESULT hr = video_context_->VideoProcessorBlt(video_processor_, vp_output_view_, 0, 1, &stream);
    if (FAILED(hr)) {
        qWarning() << "VideoProcessorBlt failed:" << Qt::hex << hr;
        return false;
    }

    // Step 4: Force keyframe if needed
    if (force_keyframe_.exchange(false) && hw_codec_api_) {
        VARIANT var;
        VariantInit(&var);
        var.vt = VT_UI4;
        var.ulVal = 1;
        hw_codec_api_->SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &var);
    }

    // Step 5: Create texture-backed IMFSample from the NV12 output (zero-copy)
    IMFMediaBuffer* dxgi_buffer = nullptr;
    hr = MFCreateDXGISurfaceBuffer(__uuidof(ID3D11Texture2D), nv12_texture_, 0, FALSE, &dxgi_buffer);
    if (FAILED(hr)) {
        qWarning() << "MFCreateDXGISurfaceBuffer failed:" << Qt::hex << hr;
        return false;
    }

    dxgi_buffer->SetCurrentLength(target_width_ * target_height_ * 3 / 2);

    IMFSample* sample = nullptr;
    MFCreateSample(&sample);
    sample->AddBuffer(dxgi_buffer);
    dxgi_buffer->Release();

    LONGLONG duration = 10000000LL / current_fps_;
    sample->SetSampleTime(sample_time_);
    sample->SetSampleDuration(duration);
    sample_time_ += duration;

    // Step 6: Feed texture-backed sample to MFT encoder
    hr = hw_encoder_->ProcessInput(0, sample, 0);
    sample->Release();
    if (FAILED(hr)) {
        qWarning() << "MFT ProcessInput (D3D11) failed:" << Qt::hex << hr;
        return false;
    }

    // Step 7: Collect encoded H.264 bitstream
    drainEncoderOutput();
    return true;
}

void VideoEngine::captureAndEncodeDxgi() {
    // Lower thread priority so the game/application keeps its frame rate
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    // Request 1ms timer resolution so sleep_for actually sleeps ~1ms instead of ~15ms
    timeBeginPeriod(1);

    if (!initDxgiCapture(capture_screen_index_)) {
        qWarning() << "DXGI init failed, falling back to Qt capture.";
        // No D3D11 device available — init CPU-backed encoder
        if (!initHardwareEncoder(target_width_, target_height_, current_fps_, current_bitrate_)) {
            initEncoder(target_width_, target_height_, current_fps_, current_bitrate_);
        }
        QMetaObject::invokeMethod(capture_timer_, "start", Qt::QueuedConnection,
            Q_ARG(int, 1000 / current_fps_));
        while (is_streaming_) {
            QImage image;
            {
                std::unique_lock<std::mutex> lock(capture_mutex_);
                capture_cv_.wait(lock, [this] { return frame_ready_ || !is_streaming_; });
                if (!is_streaming_) break;
                image = latest_frame_;
                frame_ready_ = false;
            }
            if (image.format() != QImage::Format_ARGB32)
                image = image.convertToFormat(QImage::Format_ARGB32);
            if (image.width() != target_width_ || image.height() != target_height_)
                image = scaleArgb(image.constBits(), image.bytesPerLine(),
                                  image.width(), image.height(), target_width_, target_height_);

            if (hw_encoder_initialized_) {
                hardwareEncodeAndSendFrame(image.constBits(), image.bytesPerLine());
            } else {
                libyuv::ARGBToI420(
                    image.constBits(), image.bytesPerLine(),
                    raw_img_.planes[VPX_PLANE_Y], raw_img_.stride[VPX_PLANE_Y],
                    raw_img_.planes[VPX_PLANE_U], raw_img_.stride[VPX_PLANE_U],
                    raw_img_.planes[VPX_PLANE_V], raw_img_.stride[VPX_PLANE_V],
                    target_width_, target_height_
                );
                encodeAndSendFrame();
            }
        }
        return;
    }

    // DXGI succeeded — D3D11 device is available.
    // Try zero-copy path: Video Processor (BGRA→NV12 on GPU) + D3D11-aware MFT.
    DXGI_OUTDUPL_DESC duplDesc;
    desk_dupl_->GetDesc(&duplDesc);
    int capture_w = static_cast<int>(duplDesc.ModeDesc.Width);
    int capture_h = static_cast<int>(duplDesc.ModeDesc.Height);

    bool gpu_zero_copy = false;
    if (initVideoProcessor(capture_w, capture_h)) {
        if (initHardwareEncoder(target_width_, target_height_, current_fps_, current_bitrate_)) {
            if (hw_d3d11_aware_) {
                gpu_zero_copy = true;
                qDebug() << "*** Zero-copy GPU encoding pipeline active ***";
            }
        }
    }

    if (!gpu_zero_copy) {
        // D3D11-aware not available — fall back to CPU-backed encoder
        if (!hw_encoder_initialized_) {
            if (!initHardwareEncoder(target_width_, target_height_, current_fps_, current_bitrate_)) {
                initEncoder(target_width_, target_height_, current_fps_, current_bitrate_);
            }
        }
        qDebug() << "Using CPU-readback encoding path";
    }

    auto frame_interval = std::chrono::microseconds(1000000 / current_fps_);
    int consecutive_slow_frames = 0;
    int preview_counter = 0;
    auto last_abr_eval = std::chrono::steady_clock::now();

    while (is_streaming_) {
        auto frame_start = std::chrono::steady_clock::now();

        // Adaptive bitrate evaluation every 2 seconds
        if (adaptive_bitrate_enabled_) {
            auto abr_elapsed = std::chrono::duration_cast<std::chrono::seconds>(frame_start - last_abr_eval);
            if (abr_elapsed.count() >= 2) {
                evaluateAndAdaptBitrate();
                last_abr_eval = frame_start;
            }
        }

        DXGI_OUTDUPL_FRAME_INFO frameInfo;
        IDXGIResource* desktopResource = nullptr;

        // Wait for up to the frame interval for a new desktop frame
        int timeout_ms = std::max(1, static_cast<int>(std::chrono::duration_cast<std::chrono::milliseconds>(frame_interval).count()));
        HRESULT hr = desk_dupl_->AcquireNextFrame(timeout_ms, &frameInfo, &desktopResource);

        if (hr == DXGI_ERROR_WAIT_TIMEOUT) {
            consecutive_slow_frames = 0;
            continue;
        }

        if (hr == DXGI_ERROR_ACCESS_LOST) {
            // Desktop switch, resolution change, etc. — reinitialize
            qDebug() << "DXGI access lost, reinitializing...";
            if (desktopResource) desktopResource->Release();
            if (!initDxgiCapture(capture_screen_index_)) {
                qWarning() << "DXGI reinit failed, stopping stream.";
                break;
            }
            continue;
        }

        if (FAILED(hr)) {
            if (desktopResource) desktopResource->Release();
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
            continue;
        }

        // Get the desktop texture
        ID3D11Texture2D* desktopTexture = nullptr;
        hr = desktopResource->QueryInterface(__uuidof(ID3D11Texture2D), reinterpret_cast<void**>(&desktopTexture));
        desktopResource->Release();

        if (FAILED(hr)) {
            desk_dupl_->ReleaseFrame();
            continue;
        }

        D3D11_TEXTURE2D_DESC texDesc;
        desktopTexture->GetDesc(&texDesc);
        int cap_w = static_cast<int>(texDesc.Width);
        int cap_h = static_cast<int>(texDesc.Height);

        // ===== ZERO-COPY GPU PATH =====
        // Desktop texture stays on GPU → Video Processor converts BGRA→NV12 on GPU →
        // MFT reads NV12 texture directly → only the compressed H.264 bitstream touches CPU.
        if (hw_d3d11_aware_ && video_processor_) {
            // Local preview every 3rd frame (only time we touch the staging texture)
            if (++preview_counter % 3 == 0) {
                d3d_context_->CopyResource(staging_texture_, desktopTexture);
                D3D11_MAPPED_SUBRESOURCE mapped;
                if (SUCCEEDED(d3d_context_->Map(staging_texture_, 0, D3D11_MAP_READ, 0, &mapped))) {
                    QImage preview(static_cast<const uchar*>(mapped.pData), cap_w, cap_h,
                                   mapped.RowPitch, QImage::Format_ARGB32);
                    emit localFrameCaptured(imageToVideoFrame(preview));
                    d3d_context_->Unmap(staging_texture_, 0);
                }
            }

            gpuEncodeAndSendFrame(desktopTexture, cap_w, cap_h);
            desktopTexture->Release();
            desk_dupl_->ReleaseFrame();
        }
        // ===== CPU FALLBACK PATH =====
        // Used when D3D11-aware MFT is not available (VP9 software or CPU-backed MFT)
        else {
            d3d_context_->CopyResource(staging_texture_, desktopTexture);
            desktopTexture->Release();
            desk_dupl_->ReleaseFrame();

            D3D11_MAPPED_SUBRESOURCE mapped;
            hr = d3d_context_->Map(staging_texture_, 0, D3D11_MAP_READ, 0, &mapped);
            if (FAILED(hr)) continue;

            if (cap_w != target_width_ || cap_h != target_height_) {
                QImage scaled = scaleArgb(
                    static_cast<const uint8_t*>(mapped.pData), mapped.RowPitch,
                    cap_w, cap_h, target_width_, target_height_);
                d3d_context_->Unmap(staging_texture_, 0);

                if (hw_encoder_initialized_) {
                    hardwareEncodeAndSendFrame(scaled.constBits(), scaled.bytesPerLine());
                } else {
                    libyuv::ARGBToI420(
                        scaled.constBits(), scaled.bytesPerLine(),
                        raw_img_.planes[VPX_PLANE_Y], raw_img_.stride[VPX_PLANE_Y],
                        raw_img_.planes[VPX_PLANE_U], raw_img_.stride[VPX_PLANE_U],
                        raw_img_.planes[VPX_PLANE_V], raw_img_.stride[VPX_PLANE_V],
                        target_width_, target_height_
                    );
                    encodeAndSendFrame();
                }

                if (++preview_counter % 3 == 0) emit localFrameCaptured(imageToVideoFrame(scaled));
            } else {
                if (hw_encoder_initialized_) {
                    hardwareEncodeAndSendFrame(static_cast<const uint8_t*>(mapped.pData), mapped.RowPitch);
                } else {
                    libyuv::ARGBToI420(
                        static_cast<const uint8_t*>(mapped.pData), mapped.RowPitch,
                        raw_img_.planes[VPX_PLANE_Y], raw_img_.stride[VPX_PLANE_Y],
                        raw_img_.planes[VPX_PLANE_U], raw_img_.stride[VPX_PLANE_U],
                        raw_img_.planes[VPX_PLANE_V], raw_img_.stride[VPX_PLANE_V],
                        target_width_, target_height_
                    );
                    encodeAndSendFrame();
                }

                if (++preview_counter % 3 == 0) {
                    QImage preview(static_cast<const uchar*>(mapped.pData), cap_w, cap_h,
                                   mapped.RowPitch, QImage::Format_ARGB32);
                    emit localFrameCaptured(imageToVideoFrame(preview));
                }
                d3d_context_->Unmap(staging_texture_, 0);
            }
        }

        // Frame pacing + overload detection
        auto encode_time = std::chrono::steady_clock::now() - frame_start;
        if (encode_time > frame_interval) {
            // Encoding took longer than the frame budget — don't sleep,
            // and if consistently slow, skip a frame to let the CPU breathe
            consecutive_slow_frames++;
            if (consecutive_slow_frames >= 3) {
                // Skip one frame by sleeping for a frame interval
                std::this_thread::sleep_for(frame_interval);
                consecutive_slow_frames = 0;
            }
        } else {
            consecutive_slow_frames = 0;
            auto remaining = frame_interval - encode_time;
            std::this_thread::sleep_for(remaining);
        }
    }

    cleanupDxgiCapture();
    timeEndPeriod(1);
}

void VideoEngine::captureAndEncodeWindow() {
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    timeBeginPeriod(1);

    // Ensure encoder is initialized (may not be if called as WGC fallback)
    if (!hw_encoder_initialized_ && !encoder_initialized_) {
        if (!d3d_device_) {
            D3D_FEATURE_LEVEL fl;
            D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr,
                              D3D11_CREATE_DEVICE_VIDEO_SUPPORT, nullptr, 0,
                              D3D11_SDK_VERSION, &d3d_device_, &fl, &d3d_context_);
        }
        if (!initHardwareEncoder(target_width_, target_height_, current_fps_, current_bitrate_)) {
            initEncoder(target_width_, target_height_, current_fps_, current_bitrate_);
        }
    }

    HWND hwnd = reinterpret_cast<HWND>(capture_window_id_);
    auto frame_interval = std::chrono::microseconds(1000000 / current_fps_);
    int consecutive_slow_frames = 0;
    int preview_counter = 0;
    auto last_abr_eval = std::chrono::steady_clock::now();

    while (is_streaming_) {
        auto frame_start = std::chrono::steady_clock::now();

        // Adaptive bitrate evaluation every 2 seconds
        if (adaptive_bitrate_enabled_) {
            auto abr_elapsed = std::chrono::duration_cast<std::chrono::seconds>(frame_start - last_abr_eval);
            if (abr_elapsed.count() >= 2) {
                evaluateAndAdaptBitrate();
                last_abr_eval = frame_start;
            }
        }

        RECT clientRect;
        if (!GetClientRect(hwnd, &clientRect) || !IsWindow(hwnd)) {
            qWarning() << "Target window no longer valid.";
            break;
        }
        int w = clientRect.right - clientRect.left;
        int h = clientRect.bottom - clientRect.top;
        if (w <= 0 || h <= 0) {
            std::this_thread::sleep_for(std::chrono::milliseconds(100));
            continue;
        }

        HDC hdcWindow = GetDC(hwnd);
        HDC hdcMem = CreateCompatibleDC(hdcWindow);
        HBITMAP hBmp = CreateCompatibleBitmap(hdcWindow, w, h);
        HGDIOBJ hOld = SelectObject(hdcMem, hBmp);

        PrintWindow(hwnd, hdcMem, PW_CLIENTONLY | PW_RENDERFULLCONTENT);

        BITMAPINFOHEADER bi = {};
        bi.biSize = sizeof(bi);
        bi.biWidth = w;
        bi.biHeight = -h;
        bi.biPlanes = 1;
        bi.biBitCount = 32;
        bi.biCompression = BI_RGB;

        QImage captured(w, h, QImage::Format_ARGB32);
        GetDIBits(hdcMem, hBmp, 0, h, captured.bits(),
                  reinterpret_cast<BITMAPINFO*>(&bi), DIB_RGB_COLORS);

        SelectObject(hdcMem, hOld);
        DeleteObject(hBmp);
        DeleteDC(hdcMem);
        ReleaseDC(hwnd, hdcWindow);

        if (captured.format() != QImage::Format_ARGB32) {
            captured = captured.convertToFormat(QImage::Format_ARGB32);
        }
        if (w != target_width_ || h != target_height_) {
            captured = scaleArgb(captured.constBits(), captured.bytesPerLine(),
                                 w, h, target_width_, target_height_);
        }

        if (hw_encoder_initialized_) {
            hardwareEncodeAndSendFrame(captured.constBits(), captured.bytesPerLine());
        } else {
            libyuv::ARGBToI420(
                captured.constBits(), captured.bytesPerLine(),
                raw_img_.planes[VPX_PLANE_Y], raw_img_.stride[VPX_PLANE_Y],
                raw_img_.planes[VPX_PLANE_U], raw_img_.stride[VPX_PLANE_U],
                raw_img_.planes[VPX_PLANE_V], raw_img_.stride[VPX_PLANE_V],
                target_width_, target_height_
            );
            encodeAndSendFrame();
        }

        if (++preview_counter % 3 == 0) {
            emit localFrameCaptured(imageToVideoFrame(captured));
        }

        // Frame pacing with overload detection
        auto encode_time = std::chrono::steady_clock::now() - frame_start;
        if (encode_time > frame_interval) {
            consecutive_slow_frames++;
            if (consecutive_slow_frames >= 3) {
                std::this_thread::sleep_for(frame_interval);
                consecutive_slow_frames = 0;
            }
        } else {
            consecutive_slow_frames = 0;
            std::this_thread::sleep_for(frame_interval - encode_time);
        }
    }
    timeEndPeriod(1);
}

// ---------------------- WINDOWS GRAPHICS CAPTURE (WGC) ----------------------

bool VideoEngine::initWgcCapture(HWND hwnd) {
    cleanupWgcCapture();

    // Need a D3D11 device — reuse existing or create one
    if (!d3d_device_) {
        D3D_FEATURE_LEVEL fl;
        HRESULT hr = D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr,
                                       D3D11_CREATE_DEVICE_VIDEO_SUPPORT, nullptr, 0,
                                       D3D11_SDK_VERSION, &d3d_device_, &fl, &d3d_context_);
        if (FAILED(hr)) {
            qWarning() << "WGC: Failed to create D3D11 device:" << Qt::hex << hr;
            return false;
        }

        // Enable multithread protection
        ID3D10Multithread* mt = nullptr;
        if (SUCCEEDED(d3d_device_->QueryInterface(__uuidof(ID3D10Multithread), (void**)&mt))) {
            mt->SetMultithreadProtected(TRUE);
            mt->Release();
        }
    }

    try {
        winrt::init_apartment(winrt::apartment_type::multi_threaded);
    } catch (const winrt::hresult_error&) {
        // Already initialized — that's fine
    }

    wgc_ = std::make_unique<WgcState>();

    // Wrap D3D11 device as WinRT IDirect3DDevice
    IDXGIDevice* dxgi_dev = nullptr;
    HRESULT hr = d3d_device_->QueryInterface(__uuidof(IDXGIDevice), (void**)&dxgi_dev);
    if (FAILED(hr)) {
        qWarning() << "WGC: Failed to get IDXGIDevice";
        cleanupWgcCapture();
        return false;
    }

    winrt::com_ptr<::IInspectable> inspectable;
    hr = CreateDirect3D11DeviceFromDXGIDevice(dxgi_dev, inspectable.put());
    dxgi_dev->Release();
    if (FAILED(hr)) {
        qWarning() << "WGC: Failed to create WinRT D3D device:" << Qt::hex << hr;
        cleanupWgcCapture();
        return false;
    }
    wgc_->winrt_device = inspectable.as<winrt::Windows::Graphics::DirectX::Direct3D11::IDirect3DDevice>();

    // Create GraphicsCaptureItem from HWND via interop
    auto interop = winrt::get_activation_factory<
        winrt::Windows::Graphics::Capture::GraphicsCaptureItem,
        IGraphicsCaptureItemInterop>();

    hr = interop->CreateForWindow(
        hwnd,
        winrt::guid_of<winrt::Windows::Graphics::Capture::IGraphicsCaptureItem>(),
        winrt::put_abi(wgc_->item));
    if (FAILED(hr) || !wgc_->item) {
        qWarning() << "WGC: Failed to create capture item for window:" << Qt::hex << hr;
        cleanupWgcCapture();
        return false;
    }

    auto size = wgc_->item.Size();
    qDebug() << "WGC: Capture item size:" << size.Width << "x" << size.Height;

    // Create frame pool (free-threaded — no DispatcherQueue needed)
    wgc_->frame_pool = winrt::Windows::Graphics::Capture::Direct3D11CaptureFramePool::CreateFreeThreaded(
        wgc_->winrt_device,
        winrt::Windows::Graphics::DirectX::DirectXPixelFormat::B8G8R8A8UIntNormalized,
        1,  // buffer count
        size);

    wgc_->session = wgc_->frame_pool.CreateCaptureSession(wgc_->item);

    // Disable the yellow capture border (Windows 11+)
    try {
        wgc_->session.IsBorderRequired(false);
    } catch (...) {
        // Not available on older Windows — ignore
    }

    // Disable mouse cursor in the capture
    try {
        wgc_->session.IsCursorCaptureEnabled(false);
    } catch (...) {
        // Older API — ignore
    }

    wgc_->session.StartCapture();

    // Create staging texture for CPU readback (local preview + fallback encode)
    D3D11_TEXTURE2D_DESC stagingDesc = {};
    stagingDesc.Width = size.Width;
    stagingDesc.Height = size.Height;
    stagingDesc.MipLevels = 1;
    stagingDesc.ArraySize = 1;
    stagingDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    stagingDesc.SampleDesc.Count = 1;
    stagingDesc.Usage = D3D11_USAGE_STAGING;
    stagingDesc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    d3d_device_->CreateTexture2D(&stagingDesc, nullptr, &wgc_->staging_texture);

    qDebug() << "WGC: Capture session started successfully";
    return true;
}

void VideoEngine::cleanupWgcCapture() {
    wgc_.reset();
}

void VideoEngine::captureAndEncodeWgc() {
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    timeBeginPeriod(1);

    HWND hwnd = reinterpret_cast<HWND>(capture_window_id_);
    if (!initWgcCapture(hwnd)) {
        qWarning() << "WGC init failed, falling back to PrintWindow.";
        cleanupWgcCapture();
        captureAndEncodeWindow();
        return;
    }

    // Set up encoder — try zero-copy GPU path first, then CPU fallback
    auto item_size = wgc_->item.Size();
    int capture_w = item_size.Width;
    int capture_h = item_size.Height;

    bool gpu_zero_copy = false;
    if (initVideoProcessor(capture_w, capture_h)) {
        if (initHardwareEncoder(target_width_, target_height_, current_fps_, current_bitrate_)) {
            if (hw_d3d11_aware_) {
                gpu_zero_copy = true;
                qDebug() << "WGC: Zero-copy GPU encoding pipeline active";
            }
        }
    }
    if (!gpu_zero_copy) {
        if (!hw_encoder_initialized_) {
            if (!initHardwareEncoder(target_width_, target_height_, current_fps_, current_bitrate_)) {
                initEncoder(target_width_, target_height_, current_fps_, current_bitrate_);
            }
        }
        qDebug() << "WGC: Using CPU-readback encoding path";
    }

    auto frame_interval = std::chrono::microseconds(1000000 / current_fps_);
    int consecutive_slow_frames = 0;
    int preview_counter = 0;
    auto last_abr_eval = std::chrono::steady_clock::now();

    // Handle window close
    auto item_closed_token = wgc_->item.Closed([this](auto&&, auto&&) {
        qDebug() << "WGC: Capture item closed (window destroyed)";
        is_streaming_ = false;
    });

    while (is_streaming_) {
        auto frame_start = std::chrono::steady_clock::now();

        // Adaptive bitrate evaluation every 2 seconds
        if (adaptive_bitrate_enabled_) {
            auto abr_elapsed = std::chrono::duration_cast<std::chrono::seconds>(frame_start - last_abr_eval);
            if (abr_elapsed.count() >= 2) {
                evaluateAndAdaptBitrate();
                last_abr_eval = frame_start;
            }
        }

        // Check if window is still valid
        if (!IsWindow(hwnd)) {
            qWarning() << "WGC: Target window no longer valid.";
            break;
        }

        // Try to get a frame (non-blocking)
        auto wgc_frame = wgc_->frame_pool.TryGetNextFrame();
        if (!wgc_frame) {
            // No new frame available — sleep briefly and retry
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
            continue;
        }

        auto frame_size = wgc_frame.ContentSize();
        int frame_w = frame_size.Width;
        int frame_h = frame_size.Height;

        // Handle resize: recreate frame pool if window dimensions changed
        if (frame_w != capture_w || frame_h != capture_h) {
            qDebug() << "WGC: Window resized to" << frame_w << "x" << frame_h;
            capture_w = frame_w;
            capture_h = frame_h;
            wgc_->frame_pool.Recreate(
                wgc_->winrt_device,
                winrt::Windows::Graphics::DirectX::DirectXPixelFormat::B8G8R8A8UIntNormalized,
                1,
                {capture_w, capture_h});

            // Recreate staging texture for new size
            if (wgc_->staging_texture) {
                wgc_->staging_texture->Release();
                wgc_->staging_texture = nullptr;
            }
            D3D11_TEXTURE2D_DESC stagingDesc = {};
            stagingDesc.Width = capture_w;
            stagingDesc.Height = capture_h;
            stagingDesc.MipLevels = 1;
            stagingDesc.ArraySize = 1;
            stagingDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
            stagingDesc.SampleDesc.Count = 1;
            stagingDesc.Usage = D3D11_USAGE_STAGING;
            stagingDesc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
            d3d_device_->CreateTexture2D(&stagingDesc, nullptr, &wgc_->staging_texture);

            // Reinit video processor for new input dimensions
            if (gpu_zero_copy) {
                cleanupVideoProcessor();
                if (!initVideoProcessor(capture_w, capture_h)) {
                    gpu_zero_copy = false;
                }
            }

            wgc_frame.Close();
            continue;
        }

        // Extract the D3D11 texture from the WGC frame
        auto surface = wgc_frame.Surface();
        auto access = surface.as<::Windows::Graphics::DirectX::Direct3D11::IDirect3DDxgiInterfaceAccess>();
        ID3D11Texture2D* frame_texture = nullptr;
        HRESULT tex_hr = access->GetInterface(__uuidof(ID3D11Texture2D), (void**)&frame_texture);
        wgc_frame.Close();

        if (FAILED(tex_hr) || !frame_texture) {
            continue;
        }

        // ===== ZERO-COPY GPU PATH =====
        if (gpu_zero_copy && hw_d3d11_aware_ && video_processor_) {
            // Local preview every 3rd frame
            if (++preview_counter % 3 == 0) {
                d3d_context_->CopyResource(wgc_->staging_texture, frame_texture);
                D3D11_MAPPED_SUBRESOURCE mapped;
                if (SUCCEEDED(d3d_context_->Map(wgc_->staging_texture, 0, D3D11_MAP_READ, 0, &mapped))) {
                    QImage preview(static_cast<const uchar*>(mapped.pData), capture_w, capture_h,
                                   mapped.RowPitch, QImage::Format_ARGB32);
                    emit localFrameCaptured(imageToVideoFrame(preview));
                    d3d_context_->Unmap(wgc_->staging_texture, 0);
                }
            }

            gpuEncodeAndSendFrame(frame_texture, capture_w, capture_h);
            frame_texture->Release();
        }
        // ===== CPU FALLBACK PATH =====
        else {
            d3d_context_->CopyResource(wgc_->staging_texture, frame_texture);
            frame_texture->Release();

            D3D11_MAPPED_SUBRESOURCE mapped;
            HRESULT map_hr = d3d_context_->Map(wgc_->staging_texture, 0, D3D11_MAP_READ, 0, &mapped);
            if (FAILED(map_hr)) continue;

            if (capture_w != target_width_ || capture_h != target_height_) {
                QImage scaled = scaleArgb(
                    static_cast<const uint8_t*>(mapped.pData), mapped.RowPitch,
                    capture_w, capture_h, target_width_, target_height_);
                d3d_context_->Unmap(wgc_->staging_texture, 0);

                if (hw_encoder_initialized_) {
                    hardwareEncodeAndSendFrame(scaled.constBits(), scaled.bytesPerLine());
                } else {
                    libyuv::ARGBToI420(
                        scaled.constBits(), scaled.bytesPerLine(),
                        raw_img_.planes[VPX_PLANE_Y], raw_img_.stride[VPX_PLANE_Y],
                        raw_img_.planes[VPX_PLANE_U], raw_img_.stride[VPX_PLANE_U],
                        raw_img_.planes[VPX_PLANE_V], raw_img_.stride[VPX_PLANE_V],
                        target_width_, target_height_);
                    encodeAndSendFrame();
                }

                if (++preview_counter % 3 == 0) emit localFrameCaptured(imageToVideoFrame(scaled));
            } else {
                if (hw_encoder_initialized_) {
                    hardwareEncodeAndSendFrame(static_cast<const uint8_t*>(mapped.pData), mapped.RowPitch);
                } else {
                    libyuv::ARGBToI420(
                        static_cast<const uint8_t*>(mapped.pData), mapped.RowPitch,
                        raw_img_.planes[VPX_PLANE_Y], raw_img_.stride[VPX_PLANE_Y],
                        raw_img_.planes[VPX_PLANE_U], raw_img_.stride[VPX_PLANE_U],
                        raw_img_.planes[VPX_PLANE_V], raw_img_.stride[VPX_PLANE_V],
                        target_width_, target_height_);
                    encodeAndSendFrame();
                }

                if (++preview_counter % 3 == 0) {
                    QImage preview(static_cast<const uchar*>(mapped.pData), capture_w, capture_h,
                                   mapped.RowPitch, QImage::Format_ARGB32);
                    emit localFrameCaptured(imageToVideoFrame(preview));
                }
                d3d_context_->Unmap(wgc_->staging_texture, 0);
            }
        }

        // Frame pacing
        auto encode_time = std::chrono::steady_clock::now() - frame_start;
        if (encode_time > frame_interval) {
            consecutive_slow_frames++;
            if (consecutive_slow_frames >= 3) {
                std::this_thread::sleep_for(frame_interval);
                consecutive_slow_frames = 0;
            }
        } else {
            consecutive_slow_frames = 0;
            std::this_thread::sleep_for(frame_interval - encode_time);
        }
    }

    wgc_->item.Closed(item_closed_token);
    cleanupWgcCapture();
    timeEndPeriod(1);
}

// ---------------------- MFT HARDWARE H.264 ENCODER ----------------------

bool VideoEngine::initHardwareEncoder(int width, int height, int fps, int bitrate) {
    // Start Media Foundation
    if (!mf_started_) {
        HRESULT hr = MFStartup(MF_VERSION);
        if (FAILED(hr)) {
            qWarning() << "MFStartup failed:" << Qt::hex << hr;
            return false;
        }
        mf_started_ = true;
    }

    // Find a hardware H.264 encoder
    MFT_REGISTER_TYPE_INFO inputType = { MFMediaType_Video, MFVideoFormat_NV12 };
    MFT_REGISTER_TYPE_INFO outputType = { MFMediaType_Video, MFVideoFormat_H264 };

    IMFActivate** activates = nullptr;
    UINT32 count = 0;
    HRESULT hr = MFTEnumEx(
        MFT_CATEGORY_VIDEO_ENCODER,
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
        &inputType, &outputType,
        &activates, &count
    );

    if (FAILED(hr) || count == 0) {
        qDebug() << "No hardware H.264 encoder found (count=" << count << ")";
        if (activates) CoTaskMemFree(activates);
        return false;
    }

    // Log which encoder we're using
    WCHAR name[256] = {};
    UINT32 nameLen = 0;
    activates[0]->GetString(MFT_FRIENDLY_NAME_Attribute, name, 256, &nameLen);
    qDebug() << "Found hardware encoder:" << QString::fromWCharArray(name, nameLen);

    // Activate the encoder
    hr = activates[0]->ActivateObject(IID_PPV_ARGS(&hw_encoder_));
    for (UINT32 i = 0; i < count; i++) activates[i]->Release();
    CoTaskMemFree(activates);

    if (FAILED(hr)) {
        qWarning() << "Failed to activate hardware encoder:" << Qt::hex << hr;
        return false;
    }

    // Check if MFT is async and unlock it if so
    hw_encoder_async_ = false;
    hw_event_gen_ = nullptr;
    IMFAttributes* mftAttrs = nullptr;
    if (SUCCEEDED(hw_encoder_->GetAttributes(&mftAttrs))) {
        UINT32 isAsync = 0;
        if (SUCCEEDED(mftAttrs->GetUINT32(MF_TRANSFORM_ASYNC, &isAsync)) && isAsync) {
            mftAttrs->SetUINT32(MF_TRANSFORM_ASYNC_UNLOCK, TRUE);
            hw_encoder_async_ = true;
            hw_encoder_->QueryInterface(IID_PPV_ARGS(&hw_event_gen_));
            qDebug() << "MFT encoder is async — using event-driven model";
        }
        mftAttrs->Release();
    }

    // Enable D3D11-aware mode if we have a D3D11 device.
    // This lets the MFT accept texture-backed IMFSamples (zero-copy GPU encoding).
    // Must be done BEFORE setting media types, as available types may change.
    hw_d3d11_aware_ = false;
    if (d3d_device_) {
        if (!dxgi_device_manager_) {
            hr = MFCreateDXGIDeviceManager(&dxgi_manager_token_, &dxgi_device_manager_);
            if (SUCCEEDED(hr)) {
                dxgi_device_manager_->ResetDevice(d3d_device_, dxgi_manager_token_);
            }
        }
        if (dxgi_device_manager_) {
            hr = hw_encoder_->ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER,
                                             reinterpret_cast<ULONG_PTR>(dxgi_device_manager_));
            if (SUCCEEDED(hr)) {
                hw_d3d11_aware_ = true;
                qDebug() << "MFT encoder is D3D11-aware (zero-copy enabled)";
            } else {
                qDebug() << "MFT D3D11 manager rejected (HRESULT:" << Qt::hex << hr << "), using CPU buffers";
            }
        }
    }

    // Set output type: H.264
    IMFMediaType* outType = nullptr;
    MFCreateMediaType(&outType);
    outType->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
    outType->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_H264);
    outType->SetUINT32(MF_MT_AVG_BITRATE, bitrate * 1000);
    MFSetAttributeSize(outType, MF_MT_FRAME_SIZE, width, height);
    MFSetAttributeRatio(outType, MF_MT_FRAME_RATE, fps, 1);
    outType->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);
    outType->SetUINT32(MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main);

    hr = hw_encoder_->SetOutputType(0, outType, 0);
    outType->Release();
    if (FAILED(hr)) {
        qWarning() << "Failed to set encoder output type:" << Qt::hex << hr;
        hw_encoder_->Release(); hw_encoder_ = nullptr;
        return false;
    }

    // Set input type: enumerate the encoder's available input types and pick NV12.
    // Using the encoder's own type as a base ensures all required attributes are present
    // (NVIDIA MFTs reject manually-constructed types that lack internal attributes).
    bool input_set = false;
    for (DWORD i = 0; ; i++) {
        IMFMediaType* availType = nullptr;
        hr = hw_encoder_->GetInputAvailableType(0, i, &availType);
        if (FAILED(hr)) break;

        GUID subtype;
        availType->GetGUID(MF_MT_SUBTYPE, &subtype);

        if (subtype == MFVideoFormat_NV12) {
            MFSetAttributeSize(availType, MF_MT_FRAME_SIZE, width, height);
            MFSetAttributeRatio(availType, MF_MT_FRAME_RATE, fps, 1);
            availType->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);

            hr = hw_encoder_->SetInputType(0, availType, 0);
            availType->Release();
            if (SUCCEEDED(hr)) {
                input_set = true;
                break;
            }
            qDebug() << "NV12 input type rejected at index" << i << ":" << Qt::hex << hr;
        } else {
            availType->Release();
        }
    }

    if (!input_set) {
        qWarning() << "No compatible NV12 input type accepted by encoder";
        hw_encoder_->Release(); hw_encoder_ = nullptr;
        return false;
    }

    // Configure low-latency, quality, and CBR via ICodecAPI
    hr = hw_encoder_->QueryInterface(IID_PPV_ARGS(&hw_codec_api_));
    if (SUCCEEDED(hr) && hw_codec_api_) {
        VARIANT var;
        VariantInit(&var);

        // Low latency mode — critical for real-time streaming
        var.vt = VT_BOOL;
        var.boolVal = VARIANT_TRUE;
        hw_codec_api_->SetValue(&CODECAPI_AVLowLatencyMode, &var);

        // CBR rate control
        var.vt = VT_UI4;
        var.ulVal = eAVEncCommonRateControlMode_CBR;
        hw_codec_api_->SetValue(&CODECAPI_AVEncCommonRateControlMode, &var);

        // Target bitrate
        var.vt = VT_UI4;
        var.ulVal = bitrate * 1000;
        hw_codec_api_->SetValue(&CODECAPI_AVEncCommonMeanBitRate, &var);

        // GOP size — keyframe every 2 seconds (same as VP9 path)
        var.vt = VT_UI4;
        var.ulVal = static_cast<ULONG>(fps * 2);
        hw_codec_api_->SetValue(&CODECAPI_AVEncMPVGOPSize, &var);

        // Disable B-frames — they add latency with no benefit for real-time streaming
        var.vt = VT_UI4;
        var.ulVal = 0;
        hw_codec_api_->SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &var);

        // Max QP — prevent quality from dropping too low during motion
        var.vt = VT_UI4;
        var.ulVal = 36;
        hw_codec_api_->SetValue(&CODECAPI_AVEncVideoMaxQP, &var);

        // Buffer size — 2x bitrate for 1 second of buffering
        var.vt = VT_UI4;
        var.ulVal = bitrate * 2000;
        hw_codec_api_->SetValue(&CODECAPI_AVEncCommonBufferSize, &var);
    }

    // Start streaming
    hr = hw_encoder_->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
    if (FAILED(hr)) {
        qWarning() << "Failed to begin streaming on encoder:" << Qt::hex << hr;
        cleanupHardwareEncoder();
        return false;
    }
    hw_encoder_->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);

    // Allocate NV12 buffer
    nv12_buffer_.resize(width * height * 3 / 2);
    sample_time_ = 0;

    hw_encoder_initialized_ = true;
    active_codec_ = chatproj::CODEC_H264;
    qDebug() << "H.264 hardware encoder initialized:" << width << "x" << height
             << "@" << fps << "fps," << bitrate << "kbps";
    return true;
}

void VideoEngine::cleanupHardwareEncoder() {
    if (hw_encoder_) {
        hw_encoder_->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        hw_encoder_->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        hw_encoder_->Release();
        hw_encoder_ = nullptr;
    }
    if (hw_event_gen_) {
        hw_event_gen_->Release();
        hw_event_gen_ = nullptr;
    }
    if (hw_codec_api_) {
        hw_codec_api_->Release();
        hw_codec_api_ = nullptr;
    }
    hw_encoder_initialized_ = false;
    hw_encoder_async_ = false;
    active_codec_ = chatproj::CODEC_VP9;
    nv12_buffer_.clear();
}

void VideoEngine::drainEncoderOutput() {
    if (!hw_encoder_) return;

    // For async MFTs, poll for METransformHaveOutput events (non-blocking)
    if (hw_encoder_async_ && hw_event_gen_) {
        while (true) {
            IMFMediaEvent* event = nullptr;
            // 0ms timeout = non-blocking check
            HRESULT hr = hw_event_gen_->GetEvent(MF_EVENT_FLAG_NO_WAIT, &event);
            if (FAILED(hr) || !event) break;

            MediaEventType eventType;
            event->GetType(&eventType);
            event->Release();

            if (eventType == METransformHaveOutput) {
                // Output is available — collect it
                MFT_OUTPUT_DATA_BUFFER outputData = {};
                MFT_OUTPUT_STREAM_INFO streamInfo = {};
                hw_encoder_->GetOutputStreamInfo(0, &streamInfo);

                IMFSample* outSample = nullptr;
                if (!(streamInfo.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES)) {
                    MFCreateSample(&outSample);
                    IMFMediaBuffer* outBuf = nullptr;
                    DWORD bufSize = streamInfo.cbSize > 0 ? streamInfo.cbSize : 1024 * 1024;
                    MFCreateMemoryBuffer(bufSize, &outBuf);
                    outSample->AddBuffer(outBuf);
                    outBuf->Release();
                    outputData.pSample = outSample;
                }

                DWORD status = 0;
                hr = hw_encoder_->ProcessOutput(0, 1, &outputData, &status);
                if (SUCCEEDED(hr) && outputData.pSample) {
                    IMFSample* resultSample = outputData.pSample;

                    bool is_keyframe = false;
                    UINT32 cleanPoint = 0;
                    if (SUCCEEDED(resultSample->GetUINT32(MFSampleExtension_CleanPoint, &cleanPoint)))
                        is_keyframe = (cleanPoint != 0);

                    IMFMediaBuffer* outBuffer = nullptr;
                    resultSample->ConvertToContiguousBuffer(&outBuffer);
                    if (outBuffer) {
                        BYTE* h264_data = nullptr;
                        DWORD h264_size = 0;
                        outBuffer->Lock(&h264_data, nullptr, &h264_size);
                        if (h264_size > 0)
                            fragmentAndSend(h264_data, h264_size, is_keyframe, chatproj::CODEC_H264);
                        outBuffer->Unlock();
                        outBuffer->Release();
                    }

                    resultSample->Release();
                    if (outSample && outSample != resultSample) outSample->Release();
                } else {
                    if (outSample) outSample->Release();
                }
            } else if (eventType == METransformNeedInput) {
                // Input slot is available — stop draining, caller can feed input
                break;
            }
            // METransformDrainComplete, etc. — just continue
        }
        return;
    }

    // Synchronous MFT path
    while (true) {
        MFT_OUTPUT_DATA_BUFFER outputData = {};
        DWORD status = 0;
        MFT_OUTPUT_STREAM_INFO streamInfo = {};
        hw_encoder_->GetOutputStreamInfo(0, &streamInfo);

        IMFSample* outSample = nullptr;
        if (!(streamInfo.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES)) {
            MFCreateSample(&outSample);
            IMFMediaBuffer* outBuf = nullptr;
            DWORD bufSize = streamInfo.cbSize > 0 ? streamInfo.cbSize : 1024 * 1024;
            MFCreateMemoryBuffer(bufSize, &outBuf);
            outSample->AddBuffer(outBuf);
            outBuf->Release();
            outputData.pSample = outSample;
        }

        HRESULT hr = hw_encoder_->ProcessOutput(0, 1, &outputData, &status);

        if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) {
            if (outSample) outSample->Release();
            break;
        }
        if (FAILED(hr)) {
            if (outSample) outSample->Release();
            if (outputData.pSample && outputData.pSample != outSample)
                outputData.pSample->Release();
            break;
        }

        IMFSample* resultSample = outputData.pSample;
        if (!resultSample) {
            if (outSample) outSample->Release();
            break;
        }

        bool is_keyframe = false;
        UINT32 cleanPoint = 0;
        if (SUCCEEDED(resultSample->GetUINT32(MFSampleExtension_CleanPoint, &cleanPoint)))
            is_keyframe = (cleanPoint != 0);

        IMFMediaBuffer* outBuffer = nullptr;
        resultSample->ConvertToContiguousBuffer(&outBuffer);
        if (outBuffer) {
            BYTE* h264_data = nullptr;
            DWORD h264_size = 0;
            outBuffer->Lock(&h264_data, nullptr, &h264_size);
            if (h264_size > 0)
                fragmentAndSend(h264_data, h264_size, is_keyframe, chatproj::CODEC_H264);
            outBuffer->Unlock();
            outBuffer->Release();
        }

        resultSample->Release();
        if (outSample && outSample != resultSample) outSample->Release();
    }
}

void VideoEngine::hardwareEncodeAndSendFrame(const uint8_t* bgra_data, int stride) {
    if (!hw_encoder_initialized_) return;

    // Drain pending output and wait for input readiness
    drainEncoderOutput();

    // For async MFTs, wait for METransformNeedInput
    if (hw_encoder_async_ && hw_event_gen_) {
        bool input_ready = false;
        for (int attempt = 0; attempt < 50; attempt++) {
            IMFMediaEvent* event = nullptr;
            HRESULT evtHr = hw_event_gen_->GetEvent(MF_EVENT_FLAG_NO_WAIT, &event);
            if (FAILED(evtHr) || !event) {
                std::this_thread::sleep_for(std::chrono::microseconds(200));
                continue;
            }
            MediaEventType eventType;
            event->GetType(&eventType);
            event->Release();

            if (eventType == METransformNeedInput) {
                input_ready = true;
                break;
            } else if (eventType == METransformHaveOutput) {
                // Collect output, then keep waiting
                MFT_OUTPUT_DATA_BUFFER outputData = {};
                MFT_OUTPUT_STREAM_INFO streamInfo = {};
                hw_encoder_->GetOutputStreamInfo(0, &streamInfo);
                IMFSample* outSample = nullptr;
                if (!(streamInfo.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES)) {
                    MFCreateSample(&outSample);
                    IMFMediaBuffer* outBuf = nullptr;
                    MFCreateMemoryBuffer(streamInfo.cbSize > 0 ? streamInfo.cbSize : 1024 * 1024, &outBuf);
                    outSample->AddBuffer(outBuf);
                    outBuf->Release();
                    outputData.pSample = outSample;
                }
                DWORD status = 0;
                if (SUCCEEDED(hw_encoder_->ProcessOutput(0, 1, &outputData, &status)) && outputData.pSample) {
                    bool kf = false;
                    UINT32 cp = 0;
                    if (SUCCEEDED(outputData.pSample->GetUINT32(MFSampleExtension_CleanPoint, &cp))) kf = (cp != 0);
                    IMFMediaBuffer* ob = nullptr;
                    outputData.pSample->ConvertToContiguousBuffer(&ob);
                    if (ob) {
                        BYTE* d; DWORD s;
                        ob->Lock(&d, nullptr, &s);
                        if (s > 0) fragmentAndSend(d, s, kf, chatproj::CODEC_H264);
                        ob->Unlock();
                        ob->Release();
                    }
                    outputData.pSample->Release();
                    if (outSample && outSample != outputData.pSample) outSample->Release();
                } else {
                    if (outSample) outSample->Release();
                }
            }
        }
        if (!input_ready) return;
    }

    int w = target_width_;
    int h = target_height_;

    // Convert BGRA → NV12
    uint8_t* nv12_y = nv12_buffer_.data();
    uint8_t* nv12_uv = nv12_buffer_.data() + w * h;
    libyuv::ARGBToNV12(
        bgra_data, stride,
        nv12_y, w,
        nv12_uv, w,
        w, h
    );

    // Force keyframe if requested
    if (force_keyframe_.exchange(false) && hw_codec_api_) {
        VARIANT var;
        VariantInit(&var);
        var.vt = VT_UI4;
        var.ulVal = 1;
        hw_codec_api_->SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &var);
    }

    // Create IMFMediaBuffer from NV12 data
    IMFMediaBuffer* buffer = nullptr;
    DWORD nv12_size = static_cast<DWORD>(nv12_buffer_.size());
    HRESULT hr = MFCreateMemoryBuffer(nv12_size, &buffer);
    if (FAILED(hr)) return;

    BYTE* buf_ptr = nullptr;
    buffer->Lock(&buf_ptr, nullptr, nullptr);
    memcpy(buf_ptr, nv12_buffer_.data(), nv12_size);
    buffer->Unlock();
    buffer->SetCurrentLength(nv12_size);

    // Create IMFSample
    IMFSample* sample = nullptr;
    MFCreateSample(&sample);
    sample->AddBuffer(buffer);

    // Set timestamps
    LONGLONG duration = 10000000LL / current_fps_;  // 100ns units
    sample->SetSampleTime(sample_time_);
    sample->SetSampleDuration(duration);
    sample_time_ += duration;

    // Feed to encoder
    hr = hw_encoder_->ProcessInput(0, sample, 0);
    sample->Release();
    buffer->Release();

    if (FAILED(hr)) {
        qWarning() << "MFT ProcessInput failed:" << Qt::hex << hr;
        return;
    }

    // Collect encoded output
    drainEncoderOutput();
}

// ---------------------- MFT HARDWARE H.264 DECODER ----------------------

bool VideoEngine::initMftDecoder(DecoderState& ds) {
    if (!mf_started_) {
        HRESULT hr = MFStartup(MF_VERSION);
        if (FAILED(hr)) return false;
        mf_started_ = true;
    }

    MFT_REGISTER_TYPE_INFO inputType = { MFMediaType_Video, MFVideoFormat_H264 };
    MFT_REGISTER_TYPE_INFO outputType = { MFMediaType_Video, MFVideoFormat_NV12 };

    IMFActivate** activates = nullptr;
    UINT32 count = 0;

    // Try hardware decoder first
    HRESULT hr = MFTEnumEx(
        MFT_CATEGORY_VIDEO_DECODER,
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
        &inputType, &outputType,
        &activates, &count
    );

    // Fallback to software H.264 decoder (e.g., Microsoft H264 Video Decoder)
    if (FAILED(hr) || count == 0) {
        if (activates) CoTaskMemFree(activates);
        hr = MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            &inputType, &outputType,
            &activates, &count
        );
    }

    if (FAILED(hr) || count == 0) {
        qWarning() << "No H.264 decoder found";
        if (activates) CoTaskMemFree(activates);
        return false;
    }

    hr = activates[0]->ActivateObject(IID_PPV_ARGS(&ds.mft_decoder));
    for (UINT32 i = 0; i < count; i++) activates[i]->Release();
    CoTaskMemFree(activates);

    if (FAILED(hr)) {
        qWarning() << "Failed to activate H.264 decoder:" << Qt::hex << hr;
        return false;
    }

    // Set input type: H.264
    IMFMediaType* inType = nullptr;
    MFCreateMediaType(&inType);
    inType->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
    inType->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_H264);
    hr = ds.mft_decoder->SetInputType(0, inType, 0);
    inType->Release();

    if (FAILED(hr)) {
        qWarning() << "Failed to set decoder input type:" << Qt::hex << hr;
        ds.mft_decoder->Release();
        ds.mft_decoder = nullptr;
        return false;
    }

    // Negotiate output type — prefer NV12 but accept what the decoder offers
    for (DWORD i = 0; ; i++) {
        IMFMediaType* availType = nullptr;
        hr = ds.mft_decoder->GetOutputAvailableType(0, i, &availType);
        if (FAILED(hr)) break;

        GUID subtype;
        availType->GetGUID(MF_MT_SUBTYPE, &subtype);
        if (subtype == MFVideoFormat_NV12 || subtype == MFVideoFormat_RGB32) {
            hr = ds.mft_decoder->SetOutputType(0, availType, 0);
            availType->Release();
            if (SUCCEEDED(hr)) break;
        } else {
            availType->Release();
        }
    }

    // Enable low-latency decoding
    ICodecAPI* codecApi = nullptr;
    if (SUCCEEDED(ds.mft_decoder->QueryInterface(IID_PPV_ARGS(&codecApi)))) {
        VARIANT var;
        VariantInit(&var);
        var.vt = VT_BOOL;
        var.boolVal = VARIANT_TRUE;
        codecApi->SetValue(&CODECAPI_AVLowLatencyMode, &var);
        codecApi->Release();
    }

    ds.mft_decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
    ds.mft_decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);

    ds.mft_initialized = true;
    qDebug() << "H.264 MFT decoder initialized";
    return true;
}

bool VideoEngine::decodeMftFrame(DecoderState& ds, const uint8_t* data, size_t size, QVideoFrame& out) {
    if (!ds.mft_decoder) return false;

    // Create input sample
    IMFMediaBuffer* inBuf = nullptr;
    HRESULT hr = MFCreateMemoryBuffer(static_cast<DWORD>(size), &inBuf);
    if (FAILED(hr)) return false;

    BYTE* ptr = nullptr;
    inBuf->Lock(&ptr, nullptr, nullptr);
    memcpy(ptr, data, size);
    inBuf->Unlock();
    inBuf->SetCurrentLength(static_cast<DWORD>(size));

    IMFSample* inSample = nullptr;
    MFCreateSample(&inSample);
    inSample->AddBuffer(inBuf);
    inBuf->Release();

    hr = ds.mft_decoder->ProcessInput(0, inSample, 0);
    inSample->Release();

    if (FAILED(hr)) {
        return false;
    }

    // Get output
    MFT_OUTPUT_DATA_BUFFER outputData = {};
    DWORD status = 0;

    MFT_OUTPUT_STREAM_INFO streamInfo = {};
    ds.mft_decoder->GetOutputStreamInfo(0, &streamInfo);

    IMFSample* outSample = nullptr;
    if (!(streamInfo.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES)) {
        MFCreateSample(&outSample);
        IMFMediaBuffer* outBuf = nullptr;
        DWORD outBufSize = streamInfo.cbSize > 0 ? streamInfo.cbSize : target_width_ * target_height_ * 4;
        MFCreateMemoryBuffer(outBufSize, &outBuf);
        outSample->AddBuffer(outBuf);
        outBuf->Release();
        outputData.pSample = outSample;
    }

    hr = ds.mft_decoder->ProcessOutput(0, 1, &outputData, &status);

    if (FAILED(hr)) {
        if (outSample) outSample->Release();
        if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) return false;
        if (hr == MF_E_TRANSFORM_STREAM_CHANGE) {
            // Output format changed — renegotiate
            for (DWORD i = 0; ; i++) {
                IMFMediaType* availType = nullptr;
                HRESULT hr2 = ds.mft_decoder->GetOutputAvailableType(0, i, &availType);
                if (FAILED(hr2)) break;
                hr2 = ds.mft_decoder->SetOutputType(0, availType, 0);
                availType->Release();
                if (SUCCEEDED(hr2)) break;
            }
            return false;  // Caller should retry next frame
        }
        return false;
    }

    IMFSample* resultSample = outputData.pSample;
    if (!resultSample) {
        if (outSample) outSample->Release();
        return false;
    }

    // Extract decoded frame
    IMFMediaBuffer* outBuffer = nullptr;
    resultSample->ConvertToContiguousBuffer(&outBuffer);
    if (!outBuffer) {
        resultSample->Release();
        if (outSample && outSample != resultSample) outSample->Release();
        return false;
    }

    BYTE* decoded_data = nullptr;
    DWORD decoded_size = 0;
    outBuffer->Lock(&decoded_data, nullptr, &decoded_size);

    // Determine output format from the decoder's current output type
    IMFMediaType* currentOutType = nullptr;
    ds.mft_decoder->GetOutputCurrentType(0, &currentOutType);

    UINT32 out_w = 0, out_h = 0;
    MFGetAttributeSize(currentOutType, MF_MT_FRAME_SIZE, &out_w, &out_h);

    GUID outSubtype;
    currentOutType->GetGUID(MF_MT_SUBTYPE, &outSubtype);
    currentOutType->Release();

    if (out_w == 0 || out_h == 0) {
        out_w = target_width_;
        out_h = target_height_;
    }

    if (outSubtype == MFVideoFormat_NV12) {
        // Pass NV12 directly to QVideoFrame — GPU does YUV→RGB during rendering
        QVideoFrameFormat fmt(QSize(out_w, out_h), QVideoFrameFormat::Format_NV12);
        QVideoFrame vf(fmt);
        vf.map(QVideoFrame::WriteOnly);
        int y_stride = vf.bytesPerLine(0);
        for (UINT32 row = 0; row < out_h; ++row)
            memcpy(vf.bits(0) + row * y_stride, decoded_data + row * out_w, out_w);
        int uv_stride = vf.bytesPerLine(1);
        const uint8_t* uv_src = decoded_data + out_w * out_h;
        for (UINT32 row = 0; row < out_h / 2; ++row)
            memcpy(vf.bits(1) + row * uv_stride, uv_src + row * out_w, out_w);
        vf.unmap();
        out = vf;
    } else if (outSubtype == MFVideoFormat_RGB32) {
        QVideoFrameFormat fmt(QSize(out_w, out_h), QVideoFrameFormat::Format_BGRX8888);
        QVideoFrame vf(fmt);
        vf.map(QVideoFrame::WriteOnly);
        int dst_stride = vf.bytesPerLine(0);
        int src_stride = out_w * 4;
        for (UINT32 row = 0; row < out_h; ++row)
            memcpy(vf.bits(0) + row * dst_stride, decoded_data + row * src_stride, out_w * 4);
        vf.unmap();
        out = vf;
    }

    outBuffer->Unlock();
    outBuffer->Release();
    resultSample->Release();
    if (outSample && outSample != resultSample) outSample->Release();

    return true;
}

#endif // _WIN32

void VideoEngine::captureAndEncodeLoop() {
#ifdef _WIN32
    if (capture_type_ == 0) {
        // Screen capture: encoder init is deferred to captureAndEncodeDxgi()
        captureAndEncodeDxgi();
    } else {
        // Window capture: try WGC first (handles its own encoder init + PrintWindow fallback)
        captureAndEncodeWgc();
    }
#else
    // Non-Windows fallback: QTimer fires grabScreen() on the main thread,
    // frames arrive via condition variable.
    while (is_streaming_) {
        QImage image;
        {
            std::unique_lock<std::mutex> lock(capture_mutex_);
            capture_cv_.wait(lock, [this] { return frame_ready_ || !is_streaming_; });
            if (!is_streaming_) break;
            image = latest_frame_;
            frame_ready_ = false;
        }

        if (image.format() != QImage::Format_ARGB32)
            image = image.convertToFormat(QImage::Format_ARGB32);
        if (image.width() != target_width_ || image.height() != target_height_)
            image = scaleArgb(image.constBits(), image.bytesPerLine(),
                              image.width(), image.height(), target_width_, target_height_);

        libyuv::ARGBToI420(
            image.constBits(), image.bytesPerLine(),
            raw_img_.planes[VPX_PLANE_Y], raw_img_.stride[VPX_PLANE_Y],
            raw_img_.planes[VPX_PLANE_U], raw_img_.stride[VPX_PLANE_U],
            raw_img_.planes[VPX_PLANE_V], raw_img_.stride[VPX_PLANE_V],
            target_width_, target_height_
        );
        encodeAndSendFrame();
    }
#endif // !_WIN32
}

// --------------------------- DECODING ---------------------------

struct FecGroup {
    uint16_t group_start = 0;
    uint16_t group_count = 0;
    uint16_t payload_size_xor = 0;
    uint8_t payload[chatproj::UDP_MAX_PAYLOAD] = {};
};

struct FrameBuffer {
    uint32_t frame_id = 0;
    uint16_t received_packets = 0;
    uint16_t total_packets = 0;
    bool is_keyframe = false;
    uint8_t codec = 0;
    size_t actual_size = 0;
    std::vector<uint8_t> data;
    std::vector<bool> received_mask;        // Which packet indices have arrived
    std::vector<uint16_t> payload_sizes;    // Per-packet payload sizes (for FEC recovery)
    uint16_t highest_received_index = 0;    // Highest packet_index seen so far
    bool nack_sent = false;                 // Rate-limit: only one NACK per frame
    bool fec_recovered = false;             // Already attempted FEC recovery
    std::chrono::steady_clock::time_point first_packet_time;
    std::vector<FecGroup> fec_groups;       // Received FEC packets for this frame
};
static std::unordered_map<std::string, std::unordered_map<uint32_t, FrameBuffer>> frame_assemblers;

void VideoEngine::processDatagram(const QByteArray& datagram) {
    if (datagram.size() < sizeof(chatproj::UdpVideoPacket) - chatproj::UDP_MAX_PAYLOAD) return;

    const chatproj::UdpVideoPacket* pkt = reinterpret_cast<const chatproj::UdpVideoPacket*>(datagram.constData());
    std::string sender(pkt->sender_id);

    auto& user_assembler = frame_assemblers[sender];

    // Evict stale incomplete frames
    if (pkt->frame_id > 5) {
        uint32_t threshold = pkt->frame_id - 5;
        auto it = user_assembler.begin();
        while (it != user_assembler.end()) {
            if (it->first < threshold) {
                it = user_assembler.erase(it);
            } else {
                ++it;
            }
        }
    }

    auto& fb = user_assembler[pkt->frame_id];

    if (fb.received_packets == 0) {
        fb.frame_id = pkt->frame_id;
        fb.total_packets = pkt->total_packets;
        fb.is_keyframe = pkt->is_keyframe;
        fb.codec = pkt->codec;
        fb.data.resize(fb.total_packets * chatproj::UDP_MAX_PAYLOAD);
        fb.received_mask.resize(fb.total_packets, false);
        fb.payload_sizes.resize(fb.total_packets, 0);
        fb.first_packet_time = std::chrono::steady_clock::now();
    }

    if (fb.total_packets != pkt->total_packets) {
        user_assembler.erase(pkt->frame_id);
        return;
    }

    // Insert packet data (skip duplicates)
    bool is_new_packet = false;
    if (pkt->packet_index < fb.total_packets && !fb.received_mask[pkt->packet_index]) {
        size_t offset = pkt->packet_index * chatproj::UDP_MAX_PAYLOAD;
        if (offset + pkt->payload_size <= fb.data.size()) {
            std::memcpy(fb.data.data() + offset, pkt->payload, pkt->payload_size);
            fb.received_mask[pkt->packet_index] = true;
            fb.payload_sizes[pkt->packet_index] = pkt->payload_size;
            fb.received_packets++;
            if (pkt->packet_index > fb.highest_received_index)
                fb.highest_received_index = pkt->packet_index;
            size_t end = offset + pkt->payload_size;
            if (end > fb.actual_size) fb.actual_size = end;
            is_new_packet = true;
        }
    }

    // If duplicate and frame isn't complete, nothing to do
    if (!is_new_packet && fb.received_packets < fb.total_packets) return;

    // NACK: if we received a higher-indexed packet but have gaps below it,
    // and we haven't already sent a NACK for this frame, request retransmission
    if (!fb.nack_sent && fb.received_packets < fb.total_packets &&
        fb.highest_received_index > 0) {
        // Check if enough time has passed since first packet (allow reordering window)
        auto elapsed = std::chrono::steady_clock::now() - fb.first_packet_time;
        if (elapsed > std::chrono::milliseconds(5)) {
            std::vector<uint16_t> missing;
            for (uint16_t i = 0; i <= fb.highest_received_index && i < fb.total_packets; i++) {
                if (!fb.received_mask[i]) {
                    missing.push_back(i);
                }
            }
            if (!missing.empty()) {
                sendNack(sender, pkt->frame_id, missing);
                fb.nack_sent = true;
            }
        }
    }

    // FEC recovery: try to reconstruct missing packets from FEC groups
    if (!fb.fec_recovered && fb.received_packets < fb.total_packets && !fb.fec_groups.empty()) {
        for (const auto& fec : fb.fec_groups) {
            // Count missing packets in this FEC group
            int missing_count = 0;
            uint16_t missing_idx = 0;
            for (uint16_t i = fec.group_start; i < fec.group_start + fec.group_count && i < fb.total_packets; i++) {
                if (!fb.received_mask[i]) {
                    missing_count++;
                    missing_idx = i;
                }
            }
            // Can only recover if exactly 1 packet is missing in the group
            if (missing_count != 1) continue;

            // Reconstruct: XOR the FEC payload with all received packets in the group
            uint8_t recovered[chatproj::UDP_MAX_PAYLOAD];
            std::memcpy(recovered, fec.payload, chatproj::UDP_MAX_PAYLOAD);
            uint16_t recovered_size = fec.payload_size_xor;

            for (uint16_t i = fec.group_start; i < fec.group_start + fec.group_count && i < fb.total_packets; i++) {
                if (i == missing_idx) continue;
                const uint8_t* pkt_data = fb.data.data() + i * chatproj::UDP_MAX_PAYLOAD;
                for (size_t b = 0; b < chatproj::UDP_MAX_PAYLOAD; b++)
                    recovered[b] ^= pkt_data[b];
                recovered_size ^= fb.payload_sizes[i];
            }

            // Write recovered packet into frame buffer
            size_t offset = missing_idx * chatproj::UDP_MAX_PAYLOAD;
            if (recovered_size <= chatproj::UDP_MAX_PAYLOAD && offset + recovered_size <= fb.data.size()) {
                std::memcpy(fb.data.data() + offset, recovered, recovered_size);
                fb.received_mask[missing_idx] = true;
                fb.payload_sizes[missing_idx] = recovered_size;
                fb.received_packets++;
                size_t end = offset + recovered_size;
                if (end > fb.actual_size) fb.actual_size = end;
                fb.fec_recovered = true;
            }
        }
    }

    // If frame is complete, decode it
    if (fb.received_packets == fb.total_packets) {
        size_t actual_size = fb.actual_size;

        auto& state = decoders_[sender];
        state.codec = fb.codec;

        // Detect frame gaps — request keyframe recovery but DON'T immediately freeze.
        if (state.has_decoded_any && !fb.is_keyframe &&
            fb.frame_id > state.last_decoded_frame_id + 1) {
            auto now = std::chrono::steady_clock::now();
            auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(now - state.last_pli_time);
            if (elapsed.count() > 500) { // Debounce: max 1 PLI per 500ms
                emit keyframeNeeded(QString::fromStdString(sender));
                state.last_pli_time = now;
            }
        }

        // Only skip P-frames if we're in hard recovery (a previous decode actually FAILED)
        if (state.awaiting_keyframe && !fb.is_keyframe) {
            user_assembler.erase(pkt->frame_id);
            return;
        }

        bool decode_ok = false;
        QVideoFrame decoded_frame;

#ifdef _WIN32
        if (fb.codec == chatproj::CODEC_H264) {
            // H.264 hardware decode path
            if (!state.mft_initialized) {
                if (!initMftDecoder(state)) {
                    qWarning() << "Failed to init MFT H.264 decoder for:" << QString::fromStdString(sender);
                    user_assembler.erase(pkt->frame_id);
                    return;
                }
            }
            decode_ok = decodeMftFrame(state, fb.data.data(), actual_size, decoded_frame);
        } else
#endif
        {
            // VP9 software decode path
            if (!state.vpx_initialized) {
                if (vpx_codec_dec_init(&state.vpx_decoder, vpx_codec_vp9_dx(), nullptr, 0)) {
                    qWarning() << "Failed to init VP9 decoder for:" << QString::fromStdString(sender);
                    user_assembler.erase(pkt->frame_id);
                    return;
                }
                state.vpx_initialized = true;
            }

            if (vpx_codec_decode(&state.vpx_decoder, fb.data.data(), static_cast<unsigned int>(actual_size), nullptr, 0)) {
                qWarning() << "VP9 decode failed from:" << QString::fromStdString(sender);
                state.awaiting_keyframe = true;
                emit keyframeNeeded(QString::fromStdString(sender));
                state.last_pli_time = std::chrono::steady_clock::now();
                user_assembler.erase(pkt->frame_id);
                return;
            }

            vpx_codec_iter_t iter = nullptr;
            vpx_image_t *img = vpx_codec_get_frame(&state.vpx_decoder, &iter);
            if (img) {
                // Pass I420 directly to QVideoFrame — GPU does YUV→RGB during rendering
                QVideoFrameFormat fmt(QSize(img->d_w, img->d_h), QVideoFrameFormat::Format_YUV420P);
                QVideoFrame vf(fmt);
                vf.map(QVideoFrame::WriteOnly);
                int y_stride = vf.bytesPerLine(0);
                for (unsigned row = 0; row < img->d_h; ++row)
                    memcpy(vf.bits(0) + row * y_stride,
                           img->planes[VPX_PLANE_Y] + row * img->stride[VPX_PLANE_Y], img->d_w);
                int u_stride = vf.bytesPerLine(1);
                for (unsigned row = 0; row < img->d_h / 2; ++row)
                    memcpy(vf.bits(1) + row * u_stride,
                           img->planes[VPX_PLANE_U] + row * img->stride[VPX_PLANE_U], (img->d_w + 1) / 2);
                int v_stride = vf.bytesPerLine(2);
                for (unsigned row = 0; row < img->d_h / 2; ++row)
                    memcpy(vf.bits(2) + row * v_stride,
                           img->planes[VPX_PLANE_V] + row * img->stride[VPX_PLANE_V], (img->d_w + 1) / 2);
                vf.unmap();
                decoded_frame = vf;
                decode_ok = true;
            }
        }

        if (!decode_ok) {
            // Decode failed — enter hard recovery
            if (!fb.is_keyframe) {
                state.awaiting_keyframe = true;
                emit keyframeNeeded(QString::fromStdString(sender));
                state.last_pli_time = std::chrono::steady_clock::now();
            }
            user_assembler.erase(pkt->frame_id);
            return;
        }

        // Successful decode
        state.last_decoded_frame_id = fb.frame_id;
        state.has_decoded_any = true;
        if (fb.is_keyframe) {
            state.awaiting_keyframe = false;
        }

        if (decoded_frame.isValid()) {
            emit remoteFrameReceived(QString::fromStdString(sender), decoded_frame);
        }

        // Cleanup completed and all older frames
        auto it = user_assembler.begin();
        while (it != user_assembler.end()) {
            if (it->first <= pkt->frame_id) {
                it = user_assembler.erase(it);
            } else {
                ++it;
            }
        }
    }
}

// --- FEC: Receiver side — store FEC packets and attempt recovery ---
void VideoEngine::processFecPacket(const QByteArray& datagram) {
    if (datagram.size() < static_cast<int>(sizeof(chatproj::UdpFecPacket) - chatproj::UDP_MAX_PAYLOAD)) return;

    const chatproj::UdpFecPacket* fec = reinterpret_cast<const chatproj::UdpFecPacket*>(datagram.constData());
    std::string sender(fec->sender_id);

    auto& user_assembler = frame_assemblers[sender];
    auto it = user_assembler.find(fec->frame_id);
    if (it == user_assembler.end()) return;  // No frame buffer yet — packet arrived too early or too late

    auto& fb = it->second;

    // Store FEC group data
    FecGroup group;
    group.group_start = fec->group_start;
    group.group_count = fec->group_count;
    group.payload_size_xor = fec->payload_size_xor;
    std::memcpy(group.payload, fec->payload, chatproj::UDP_MAX_PAYLOAD);
    fb.fec_groups.push_back(std::move(group));

    // Immediately attempt recovery if frame is incomplete
    if (fb.received_packets < fb.total_packets) {
        const auto& fg = fb.fec_groups.back();
        int missing_count = 0;
        uint16_t missing_idx = 0;
        for (uint16_t i = fg.group_start; i < fg.group_start + fg.group_count && i < fb.total_packets; i++) {
            if (!fb.received_mask[i]) {
                missing_count++;
                missing_idx = i;
            }
        }
        if (missing_count == 1) {
            uint8_t recovered[chatproj::UDP_MAX_PAYLOAD];
            std::memcpy(recovered, fg.payload, chatproj::UDP_MAX_PAYLOAD);
            uint16_t recovered_size = fg.payload_size_xor;

            for (uint16_t i = fg.group_start; i < fg.group_start + fg.group_count && i < fb.total_packets; i++) {
                if (i == missing_idx) continue;
                const uint8_t* pkt_data = fb.data.data() + i * chatproj::UDP_MAX_PAYLOAD;
                for (size_t b = 0; b < chatproj::UDP_MAX_PAYLOAD; b++)
                    recovered[b] ^= pkt_data[b];
                recovered_size ^= fb.payload_sizes[i];
            }

            size_t offset = missing_idx * chatproj::UDP_MAX_PAYLOAD;
            if (recovered_size <= chatproj::UDP_MAX_PAYLOAD && offset + recovered_size <= fb.data.size()) {
                std::memcpy(fb.data.data() + offset, recovered, recovered_size);
                fb.received_mask[missing_idx] = true;
                fb.payload_sizes[missing_idx] = recovered_size;
                fb.received_packets++;
                size_t end = offset + recovered_size;
                if (end > fb.actual_size) fb.actual_size = end;
            }
        }
    }

    // If frame is now complete after FEC recovery, trigger decode by re-calling processDatagram
    // with a dummy packet isn't ideal. Instead, inline the decode check here.
    // However, the simplest approach: the next video packet arrival will trigger the decode.
    // FEC recovery without a subsequent packet is an edge case (all packets arrived except
    // the very last one, and FEC fixes it). To handle it, we check completion here.
    if (fb.received_packets == fb.total_packets) {
        // Build a minimal QByteArray that looks like the last received video packet
        // to re-enter processDatagram's decode path. Instead, just emit a signal
        // or call the decode logic directly. For simplicity, we create a synthetic
        // VIDEO packet with the sender info and frame_id to trigger decode.
        chatproj::UdpVideoPacket synthetic;
        synthetic.packet_type = chatproj::UdpPacketType::VIDEO;
        std::memcpy(synthetic.sender_id, fec->sender_id, chatproj::SENDER_ID_SIZE);
        synthetic.frame_id = fec->frame_id;
        synthetic.packet_index = 0;  // Already received, will hit duplicate check
        synthetic.total_packets = fb.total_packets;
        synthetic.payload_size = fb.payload_sizes[0];
        synthetic.is_keyframe = fb.is_keyframe;
        synthetic.codec = fb.codec;
        // Don't need to copy actual payload — processDatagram will see it as duplicate
        // and skip to the frame-complete check
        QByteArray synth_data(reinterpret_cast<const char*>(&synthetic),
                              sizeof(chatproj::UdpVideoPacket) - chatproj::UDP_MAX_PAYLOAD + synthetic.payload_size);
        processDatagram(synth_data);
    }
}

// --- NACK: Sender side — retransmit requested packets from the ring buffer ---
void VideoEngine::processNack(const QByteArray& datagram) {
    if (datagram.size() < static_cast<int>(sizeof(chatproj::UdpNackPacket) -
        sizeof(uint16_t) * chatproj::NACK_MAX_ENTRIES)) return;

    const chatproj::UdpNackPacket* nack = reinterpret_cast<const chatproj::UdpNackPacket*>(datagram.constData());

    uint16_t count = std::min(nack->nack_count, chatproj::NACK_MAX_ENTRIES);
    nack_packets_received_ += count;  // Track for adaptive bitrate

    std::lock_guard<std::mutex> lock(retx_mutex_);
    for (uint16_t i = 0; i < count; i++) {
        uint64_t key = (static_cast<uint64_t>(nack->frame_id) << 16) | nack->missing_indices[i];
        auto it = retx_buffer_.find(key);
        if (it != retx_buffer_.end()) {
            emit sendUdpData(it->second, server_host_, server_port_);
        }
    }
}

// --- NACK: Receiver side — send NACK to the streamer via the server ---
void VideoEngine::sendNack(const std::string& streamer, uint32_t frame_id,
                           const std::vector<uint16_t>& missing) {
    if (missing.empty()) return;

    // Send in batches of NACK_MAX_ENTRIES
    for (size_t offset = 0; offset < missing.size(); offset += chatproj::NACK_MAX_ENTRIES) {
        chatproj::UdpNackPacket nack;
        nack.packet_type = chatproj::UdpPacketType::NACK;
        std::memset(nack.sender_id, 0, chatproj::SENDER_ID_SIZE);
        std::memcpy(nack.sender_id, udp_id_.c_str(),
                    std::min(udp_id_.size(), size_t(chatproj::SENDER_ID_SIZE - 1)));
        std::memset(nack.target_username, 0, chatproj::SENDER_ID_SIZE);
        std::memcpy(nack.target_username, streamer.c_str(),
                    std::min(streamer.size(), size_t(chatproj::SENDER_ID_SIZE - 1)));
        nack.frame_id = frame_id;

        uint16_t count = static_cast<uint16_t>(
            std::min(missing.size() - offset, size_t(chatproj::NACK_MAX_ENTRIES)));
        nack.nack_count = count;
        std::memset(nack.missing_indices, 0, sizeof(nack.missing_indices));
        for (uint16_t i = 0; i < count; i++) {
            nack.missing_indices[i] = missing[offset + i];
        }

        size_t pkt_size = sizeof(chatproj::UdpNackPacket) -
            sizeof(uint16_t) * (chatproj::NACK_MAX_ENTRIES - count);
        QByteArray data(reinterpret_cast<const char*>(&nack), static_cast<int>(pkt_size));
        emit sendUdpData(data, server_host_, server_port_);
    }
}

// --- Adaptive Bitrate ---
void VideoEngine::evaluateAndAdaptBitrate() {
    if (!adaptive_bitrate_enabled_) return;

    int sent = total_packets_sent_.exchange(0);
    int nacked = nack_packets_received_.exchange(0);

    if (sent == 0) return;

    // Loss ratio = NACKed packets / total sent packets
    double loss_ratio = static_cast<double>(nacked) / sent;

    int new_bitrate = current_bitrate_;

    if (loss_ratio > 0.10) {
        // Heavy loss (>10%): aggressive reduction — halve bitrate
        new_bitrate = current_bitrate_ * 50 / 100;
    } else if (loss_ratio > 0.03) {
        // Moderate loss (3-10%): reduce by 20%
        new_bitrate = current_bitrate_ * 80 / 100;
    } else if (loss_ratio < 0.01 && current_bitrate_ < configured_bitrate_) {
        // Very low loss (<1%): gradually increase by 10%, up to configured max
        new_bitrate = std::min(current_bitrate_ * 110 / 100, configured_bitrate_);
    }

    // Clamp to bounds
    new_bitrate = std::max(new_bitrate, min_bitrate_);
    new_bitrate = std::min(new_bitrate, configured_bitrate_);

    if (new_bitrate != current_bitrate_) {
        qDebug() << "Adaptive bitrate:" << current_bitrate_ << "kbps ->"
                 << new_bitrate << "kbps (loss:" << QString::number(loss_ratio * 100, 'f', 1) << "%)";
        applyBitrate(new_bitrate);
    }
}

void VideoEngine::applyBitrate(int new_bitrate_kbps) {
    current_bitrate_ = new_bitrate_kbps;

#ifdef _WIN32
    // Update MFT H.264 encoder bitrate at runtime via ICodecAPI
    if (hw_codec_api_) {
        VARIANT var;
        VariantInit(&var);
        var.vt = VT_UI4;
        var.ulVal = new_bitrate_kbps * 1000;
        hw_codec_api_->SetValue(&CODECAPI_AVEncCommonMeanBitRate, &var);

        // Also update buffer size (2x bitrate)
        var.ulVal = new_bitrate_kbps * 2000;
        hw_codec_api_->SetValue(&CODECAPI_AVEncCommonBufferSize, &var);
    }
#endif

    // Update VP9 encoder bitrate at runtime
    if (encoder_initialized_) {
        vpx_codec_enc_cfg_t cfg;
        if (vpx_codec_enc_config_default(vpx_codec_vp9_cx(), &cfg, 0) == VPX_CODEC_OK) {
            cfg.rc_target_bitrate = new_bitrate_kbps;
            cfg.g_w = target_width_;
            cfg.g_h = target_height_;
            cfg.g_timebase.num = 1;
            cfg.g_timebase.den = current_fps_;
            cfg.rc_end_usage = VPX_CBR;
            cfg.g_lag_in_frames = 0;
            cfg.g_error_resilient = VPX_ERROR_RESILIENT_DEFAULT;
            vpx_codec_enc_config_set(&encoder_, &cfg);
        }
    }
}
