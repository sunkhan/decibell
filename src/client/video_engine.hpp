#pragma once
#include <QObject>
#include <QImage>
#include <QThread>
#include <QMutex>
#include <QWaitCondition>
#include <QScreen>
#include <QGuiApplication>
#include <QTimer>
#include <QHostAddress>
#include <QVideoFrame>
#include <QVideoFrameFormat>
#include <vpx/vpx_encoder.h>
#include <vpx/vp8cx.h>
#include <vpx/vpx_decoder.h>
#include <vpx/vp8dx.h>
#include <libyuv.h>
#include <condition_variable>
#include <atomic>
#include <chrono>
#include "../common/udp_packet.hpp"

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <d3d11.h>
#include <dxgi1_2.h>
#include <mfapi.h>
#include <mftransform.h>
#include <mfidl.h>
#include <mferror.h>
#include <strmif.h>
#include <codecapi.h>
#endif

class VideoEngine : public QObject {
    Q_OBJECT
public:
    explicit VideoEngine(QObject* parent = nullptr);
    ~VideoEngine();

    void startStream(const QString& token, const QString& host, quint16 port, int fps, int bitrate, int width, int height, int captureType, quintptr captureId, bool adaptiveBitrate = true);
    void stopStream();

    void processDatagram(const QByteArray& datagram);
    void processFecPacket(const QByteArray& datagram);
    void processNack(const QByteArray& datagram);
    void requestKeyframe();

signals:
    // Pushes decoded frame back to UI for the specific user
    void remoteFrameReceived(const QString& username, const QVideoFrame& frame);
    void localFrameCaptured(const QVideoFrame& frame);
    void sendUdpData(const QByteArray& data, const QHostAddress& host, quint16 port);
    void keyframeNeeded(const QString& streamerUsername);

private slots:
    void grabScreen();

private:
    void initEncoder(int width, int height, int fps, int bitrate);
    void destroyEncoder();
    void captureAndEncodeLoop();

#ifdef _WIN32
    // DXGI Desktop Duplication (hardware-accelerated screen capture)
    bool initDxgiCapture(int screenIndex);
    void cleanupDxgiCapture();
    void captureAndEncodeDxgi();
    void captureAndEncodeWindow();

    ID3D11Device* d3d_device_ = nullptr;
    ID3D11DeviceContext* d3d_context_ = nullptr;
    IDXGIOutputDuplication* desk_dupl_ = nullptr;
    ID3D11Texture2D* staging_texture_ = nullptr;

    // MFT Hardware H.264 Encoder
    bool initHardwareEncoder(int width, int height, int fps, int bitrate);
    void cleanupHardwareEncoder();
    void hardwareEncodeAndSendFrame(const uint8_t* bgra_data, int stride);

    IMFTransform* hw_encoder_ = nullptr;
    ICodecAPI* hw_codec_api_ = nullptr;
    IMFMediaEventGenerator* hw_event_gen_ = nullptr;  // For async MFT event handling
    bool hw_encoder_initialized_ = false;
    bool hw_encoder_async_ = false;  // True if MFT uses async event model
    bool mf_started_ = false;
    std::vector<uint8_t> nv12_buffer_;
    int64_t sample_time_ = 0;

    // --- Zero-copy GPU encoding pipeline ---
    // D3D11 Video Processor: BGRA→NV12 color conversion + scaling on GPU
    bool initVideoProcessor(int input_w, int input_h);
    void cleanupVideoProcessor();
    // Encode a DXGI-captured texture without any CPU readback
    bool gpuEncodeAndSendFrame(ID3D11Texture2D* desktop_texture, int cap_w, int cap_h);

    ID3D11VideoDevice* video_device_ = nullptr;
    ID3D11VideoContext* video_context_ = nullptr;
    ID3D11VideoProcessor* video_processor_ = nullptr;
    ID3D11VideoProcessorEnumerator* vp_enum_ = nullptr;
    ID3D11Texture2D* gpu_bgra_texture_ = nullptr;  // Persistent GPU BGRA copy target
    ID3D11Texture2D* nv12_texture_ = nullptr;       // NV12 output for encoder (GPU-only)
    ID3D11VideoProcessorInputView* vp_input_view_ = nullptr;
    ID3D11VideoProcessorOutputView* vp_output_view_ = nullptr;
    int vp_input_width_ = 0;
    int vp_input_height_ = 0;

    // D3D11-aware MFT: accepts texture-backed IMFSamples (no CPU buffers)
    IMFDXGIDeviceManager* dxgi_device_manager_ = nullptr;
    UINT dxgi_manager_token_ = 0;
    bool hw_d3d11_aware_ = false;

    // Windows Graphics Capture (WGC) — modern window/screen capture API
    struct WgcState;  // PIMPL: defined in .cpp to isolate WinRT headers
    std::unique_ptr<WgcState> wgc_;
    bool initWgcCapture(HWND hwnd);
    void cleanupWgcCapture();
    void captureAndEncodeWgc();
#endif

    // Shared encode+send logic used by all capture paths
    void encodeAndSendFrame();
    // Shared UDP fragmentation for both VP9 and H.264 bitstreams
    void fragmentAndSend(const uint8_t* data, size_t size, bool is_keyframe, uint8_t codec);
#ifdef _WIN32
    // Drain all pending output from the MFT encoder and send it
    void drainEncoderOutput();
#endif

    // VPX
    vpx_codec_ctx_t encoder_;
    vpx_image_t raw_img_;
    bool encoder_initialized_ = false;

    // Decoding state per user
    struct DecoderState {
        // VP9 software decoder
        vpx_codec_ctx_t vpx_decoder;
        bool vpx_initialized = false;

#ifdef _WIN32
        // MFT H.264 hardware decoder
        IMFTransform* mft_decoder = nullptr;
        bool mft_initialized = false;
#endif

        // Common
        uint8_t codec = 0;  // VideoCodec from last received packet
        bool awaiting_keyframe = false;
        uint32_t last_decoded_frame_id = 0;
        bool has_decoded_any = false;
        std::chrono::steady_clock::time_point last_pli_time;
    };
    std::unordered_map<std::string, DecoderState> decoders_;

#ifdef _WIN32
    // MFT H.264 decoder helpers
    bool initMftDecoder(DecoderState& ds);
    bool decodeMftFrame(DecoderState& ds, const uint8_t* data, size_t size, QVideoFrame& out);
#endif

    // Networking
    QString token_;
    std::string udp_id_;  // Last 31 chars of JWT — fits in SENDER_ID_SIZE, unique per user
    QHostAddress server_host_;
    quint16 server_port_ = 0;

    // Threading and Capture
    bool is_streaming_ = false;
    std::thread capture_thread_;
    QTimer* capture_timer_ = nullptr;

    QImage latest_frame_;
    bool frame_ready_ = false;
    std::mutex capture_mutex_;
    std::condition_variable capture_cv_;

    // Sequence
    uint32_t frame_id_ = 0;
    std::atomic<bool> force_keyframe_{false};
    uint8_t active_codec_ = 0;  // CODEC_VP9 or CODEC_H264

    int current_fps_ = 30;
    int current_bitrate_ = 2500;

    // Capture source
    int capture_type_ = 0; // 0 = screen, 1 = window
    int capture_screen_index_ = 0;
    quintptr capture_window_id_ = 0;
    int target_width_ = 1280;
    int target_height_ = 720;

    // --- NACK retransmission (sender side) ---
    // Ring buffer of recently sent packets, keyed by (frame_id << 16 | packet_index).
    // Allows retransmitting lost packets on NACK without re-encoding.
    static constexpr int RETX_BUFFER_MAX_FRAMES = 30;  // Keep ~1 second at 30fps
    std::unordered_map<uint64_t, QByteArray> retx_buffer_;
    std::mutex retx_mutex_;
    uint32_t retx_oldest_frame_ = 0;

    // --- NACK retransmission (receiver side) ---
    void sendNack(const std::string& streamer, uint32_t frame_id,
                  const std::vector<uint16_t>& missing);

    // --- Adaptive bitrate (sender side) ---
    bool adaptive_bitrate_enabled_ = true;
    int configured_bitrate_ = 2500;    // User-selected bitrate (kbps) — upper bound
    int min_bitrate_ = 300;            // Lower bound (kbps)
    std::atomic<int> nack_packets_received_{0};  // NACKs received since last evaluation
    std::atomic<int> total_packets_sent_{0};     // Packets sent since last evaluation
    void evaluateAndAdaptBitrate();
    void applyBitrate(int new_bitrate_kbps);
};
