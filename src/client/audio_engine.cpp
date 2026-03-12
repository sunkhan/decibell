#include "audio_engine.hpp"
#include <QDebug>
#include <QAudioDevice>
#include <algorithm>
#include <cmath>

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <audioclient.h>
#include <mmdeviceapi.h>
#endif

constexpr int SAMPLE_RATE = 48000;
constexpr int CHANNELS = 1;
constexpr int FRAME_SIZE = 960; // 20ms at 48kHz
constexpr int MAX_PACKET_SIZE = chatproj::UDP_MAX_PAYLOAD;

AudioEngine::AudioEngine(QObject* parent) : QObject(parent) {
    initOpus();
    if (audio_source_) {
        audio_input_device_ = audio_source_->start();
        if (audio_input_device_) {
            connect(audio_input_device_, &QIODevice::readyRead, this, &AudioEngine::handleAudioReady);
        } else {
            qWarning() << "Failed to start audio source.";
        }
    }
    if (audio_sink_) {
        audio_output_device_ = audio_sink_->start();
        if (!audio_output_device_) {
            qWarning() << "Failed to start audio sink.";
        }
    }
}

AudioEngine::~AudioEngine() {
    stopSystemAudioCapture();
    stopVoice();
    if (encoder_) opus_encoder_destroy(encoder_);
    if (decoder_) opus_decoder_destroy(decoder_);
}

void AudioEngine::initOpus() {
    int err;
    encoder_ = opus_encoder_create(SAMPLE_RATE, CHANNELS, OPUS_APPLICATION_VOIP, &err);
    if (err != OPUS_OK) qCritical() << "Failed to create Opus encoder:" << opus_strerror(err);

    decoder_ = opus_decoder_create(SAMPLE_RATE, CHANNELS, &err);
    if (err != OPUS_OK) qCritical() << "Failed to create Opus decoder:" << opus_strerror(err);

    // Initialize audio devices once
    QAudioFormat format;
    format.setSampleRate(SAMPLE_RATE);
    format.setChannelCount(CHANNELS);
    format.setSampleFormat(QAudioFormat::Int16);

    QAudioDevice infoIn = QMediaDevices::defaultAudioInput();
    if (!infoIn.isFormatSupported(format)) {
        qWarning() << "Default audio input format not supported by OS.";
    }
    audio_source_ = new QAudioSource(infoIn, format, this);
    audio_source_->setBufferSize(FRAME_SIZE * sizeof(int16_t) * 4);

    QAudioDevice infoOut = QMediaDevices::defaultAudioOutput();
    audio_sink_ = new QAudioSink(infoOut, format, this);
}

void AudioEngine::startVoice(const QString& token, const QString& host, quint16 port) {
    token_ = token;
    // Use last 31 chars of JWT as compact UDP identifier
    std::string full_token = token.toStdString();
    size_t id_len = std::min(full_token.size(), size_t(chatproj::SENDER_ID_SIZE - 1));
    udp_id_ = full_token.substr(full_token.size() - id_len);
    server_host_ = QHostAddress(host);
    server_port_ = port;
    is_active_ = true;
    sequence_number_ = 0;
    input_buffer_.clear();

    opus_encoder_ctl(encoder_, OPUS_RESET_STATE);
    opus_decoder_ctl(decoder_, OPUS_RESET_STATE);
}

void AudioEngine::stopVoice() {
    is_active_ = false;
    sequence_number_ = 0;
    emit localAudioLevelChanged(0.0);
}

void AudioEngine::handleAudioReady() {
    if (!audio_input_device_) return;

    QByteArray data = audio_input_device_->readAll();

    if (!is_active_) {
        input_buffer_.clear();
        return;
    }

    bool has_system_audio = system_audio_active_.load(std::memory_order_relaxed);

    // If muted AND no system audio, nothing to send
    if (is_muted_ && !has_system_audio) {
        input_buffer_.clear();
        emit localAudioLevelChanged(0.0);
        return;
    }

    // Append newly captured mic bytes for frame timing
    input_buffer_.append(data);
    int bytes_per_frame = FRAME_SIZE * sizeof(int16_t);

    while (input_buffer_.size() >= bytes_per_frame) {
        const int16_t* mic_data = reinterpret_cast<const int16_t*>(input_buffer_.constData());

        // Build the frame: mic audio (or silence if muted) + system audio
        int16_t frame[FRAME_SIZE];
        for (int i = 0; i < FRAME_SIZE; ++i) {
            frame[i] = is_muted_ ? 0 : mic_data[i];
        }

        // Mix in system audio if available
        if (has_system_audio) {
            std::lock_guard<std::mutex> lock(sys_audio_mutex_);
            for (int i = 0; i < FRAME_SIZE; ++i) {
                if (sys_audio_fifo_.empty()) break;
                int32_t mixed = static_cast<int32_t>(frame[i]) + static_cast<int32_t>(sys_audio_fifo_.front());
                frame[i] = static_cast<int16_t>(std::clamp(mixed, -32768, 32767));
                sys_audio_fifo_.pop_front();
            }
        }

        // Calculate peak amplitude for the volume meter
        int16_t max_val = 0;
        for (int j = 0; j < FRAME_SIZE; ++j) {
            int16_t val = std::abs(frame[j]);
            if (val > max_val) max_val = val;
        }
        qreal level = static_cast<qreal>(max_val) / 32768.0;
        emit localAudioLevelChanged(level);

        // Encode and send
        chatproj::UdpAudioPacket packet;
        packet.packet_type = chatproj::UdpPacketType::AUDIO;
        memset(packet.sender_id, 0, chatproj::SENDER_ID_SIZE);
        memcpy(packet.sender_id, udp_id_.c_str(), std::min(udp_id_.size(), size_t(chatproj::SENDER_ID_SIZE - 1)));
        packet.sequence = ++sequence_number_;

        int bytes = opus_encode(encoder_, frame, FRAME_SIZE, packet.payload, MAX_PACKET_SIZE);
        if (bytes > 0) {
            packet.payload_size = bytes;
            size_t total_size = sizeof(chatproj::UdpAudioPacket) - chatproj::UDP_MAX_PAYLOAD + bytes;

            QByteArray out_data(reinterpret_cast<const char*>(&packet), static_cast<int>(total_size));
            emit sendUdpData(out_data, server_host_, server_port_);
        }

        input_buffer_.remove(0, bytes_per_frame);
    }
}

void AudioEngine::processDatagram(const QByteArray& datagram) {
    if (!is_active_) return;

    if (datagram.size() < sizeof(chatproj::UdpAudioPacket) - chatproj::UDP_MAX_PAYLOAD) return;

    const chatproj::UdpAudioPacket* packet = reinterpret_cast<const chatproj::UdpAudioPacket*>(datagram.constData());

    int16_t pcm_data[FRAME_SIZE];
    int samples = opus_decode(decoder_, packet->payload, packet->payload_size, pcm_data, FRAME_SIZE, 0);

    if (samples > 0 && audio_output_device_) {
        audio_output_device_->write(reinterpret_cast<const char*>(pcm_data), samples * sizeof(int16_t));

        int16_t max_val = 0;
        for (int j = 0; j < samples; ++j) {
            int16_t val = std::abs(pcm_data[j]);
            if (val > max_val) max_val = val;
        }
        qreal level = static_cast<qreal>(max_val) / 32768.0;

        QString senderUsername = QString::fromStdString(packet->sender_id);
        emit remoteUserSpeaking(senderUsername, level);
    }
}

// ---------------------- System Audio Capture (WASAPI Loopback) ----------------------

void AudioEngine::startSystemAudioCapture() {
    if (system_audio_active_.load()) return;
    system_audio_active_ = true;
    system_audio_thread_ = std::thread(&AudioEngine::captureSystemAudioLoop, this);
    qDebug() << "System audio capture started.";
}

void AudioEngine::stopSystemAudioCapture() {
    if (!system_audio_active_.load()) return;
    system_audio_active_ = false;
    if (system_audio_thread_.joinable()) {
        system_audio_thread_.join();
    }
    std::lock_guard<std::mutex> lock(sys_audio_mutex_);
    sys_audio_fifo_.clear();
    qDebug() << "System audio capture stopped.";
}

void AudioEngine::captureSystemAudioLoop() {
#ifdef _WIN32
    CoInitializeEx(nullptr, COINIT_MULTITHREADED);

    IMMDeviceEnumerator* enumerator = nullptr;
    HRESULT hr = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL,
                                  __uuidof(IMMDeviceEnumerator), reinterpret_cast<void**>(&enumerator));
    if (FAILED(hr)) {
        qWarning() << "Failed to create device enumerator for loopback capture.";
        CoUninitialize();
        system_audio_active_ = false;
        return;
    }

    IMMDevice* device = nullptr;
    hr = enumerator->GetDefaultAudioEndpoint(eRender, eConsole, &device);
    if (FAILED(hr)) {
        qWarning() << "Failed to get default audio render endpoint.";
        enumerator->Release();
        CoUninitialize();
        system_audio_active_ = false;
        return;
    }

    IAudioClient* audioClient = nullptr;
    hr = device->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr, reinterpret_cast<void**>(&audioClient));
    device->Release();
    if (FAILED(hr)) {
        qWarning() << "Failed to activate audio client for loopback.";
        enumerator->Release();
        CoUninitialize();
        system_audio_active_ = false;
        return;
    }

    WAVEFORMATEX* mixFormat = nullptr;
    hr = audioClient->GetMixFormat(&mixFormat);
    if (FAILED(hr)) {
        qWarning() << "Failed to get system mix format.";
        audioClient->Release();
        enumerator->Release();
        CoUninitialize();
        system_audio_active_ = false;
        return;
    }

    int sys_channels = mixFormat->nChannels;
    int sys_sample_rate = mixFormat->nSamplesPerSec;
    int bits_per_sample = mixFormat->wBitsPerSample;

    // Detect float format (system mixer on modern Windows is always float32)
    bool is_float = (mixFormat->wFormatTag == WAVE_FORMAT_IEEE_FLOAT);
    if (mixFormat->wFormatTag == WAVE_FORMAT_EXTENSIBLE && bits_per_sample == 32) {
        is_float = true;
    }

    qDebug() << "System audio:" << sys_sample_rate << "Hz," << sys_channels << "ch,"
             << bits_per_sample << "bit," << (is_float ? "float" : "int");

    // Initialize loopback capture (20ms buffer)
    REFERENCE_TIME bufferDuration = 200000;
    hr = audioClient->Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
                                  bufferDuration, 0, mixFormat, nullptr);
    CoTaskMemFree(mixFormat);

    if (FAILED(hr)) {
        qWarning() << "Failed to initialize loopback capture. HRESULT:" << Qt::hex << hr;
        audioClient->Release();
        enumerator->Release();
        CoUninitialize();
        system_audio_active_ = false;
        return;
    }

    IAudioCaptureClient* captureClient = nullptr;
    hr = audioClient->GetService(__uuidof(IAudioCaptureClient), reinterpret_cast<void**>(&captureClient));
    if (FAILED(hr)) {
        qWarning() << "Failed to get capture client service.";
        audioClient->Release();
        enumerator->Release();
        CoUninitialize();
        system_audio_active_ = false;
        return;
    }

    hr = audioClient->Start();
    if (FAILED(hr)) {
        captureClient->Release();
        audioClient->Release();
        enumerator->Release();
        CoUninitialize();
        system_audio_active_ = false;
        return;
    }

    // Resampling state for sample rate conversion (e.g. 44100 -> 48000)
    double input_step = static_cast<double>(sys_sample_rate) / 48000.0;
    double resample_pos = 0.0;
    int16_t prev_sample = 0;

    while (system_audio_active_.load(std::memory_order_relaxed)) {
        UINT32 packetLength = 0;
        hr = captureClient->GetNextPacketSize(&packetLength);

        while (SUCCEEDED(hr) && packetLength != 0) {
            BYTE* data = nullptr;
            UINT32 numFrames = 0;
            DWORD flags = 0;

            hr = captureClient->GetBuffer(&data, &numFrames, &flags, nullptr, nullptr);
            if (FAILED(hr)) break;

            bool silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT);

            std::lock_guard<std::mutex> lock(sys_audio_mutex_);

            for (UINT32 i = 0; i < numFrames; ++i) {
                int16_t mono_sample = 0;

                if (!silent && data) {
                    if (is_float) {
                        const float* samples = reinterpret_cast<const float*>(data) + i * sys_channels;
                        float mono = 0.0f;
                        for (int ch = 0; ch < sys_channels; ++ch) {
                            mono += samples[ch];
                        }
                        mono /= static_cast<float>(sys_channels);
                        mono_sample = static_cast<int16_t>(std::clamp(mono * 32767.0f, -32768.0f, 32767.0f));
                    } else {
                        const int16_t* samples = reinterpret_cast<const int16_t*>(data) + i * sys_channels;
                        int32_t sum = 0;
                        for (int ch = 0; ch < sys_channels; ++ch) {
                            sum += samples[ch];
                        }
                        mono_sample = static_cast<int16_t>(sum / sys_channels);
                    }
                }

                // Resample to 48kHz
                if (sys_sample_rate == 48000) {
                    sys_audio_fifo_.push_back(mono_sample);
                } else {
                    while (resample_pos < 1.0) {
                        int16_t interpolated = static_cast<int16_t>(
                            prev_sample + (mono_sample - prev_sample) * resample_pos);
                        sys_audio_fifo_.push_back(interpolated);
                        resample_pos += input_step;
                    }
                    resample_pos -= 1.0;
                    prev_sample = mono_sample;
                }
            }

            // Cap FIFO at 2 seconds to prevent unbounded growth
            while (sys_audio_fifo_.size() > 96000) {
                sys_audio_fifo_.pop_front();
            }

            captureClient->ReleaseBuffer(numFrames);
            hr = captureClient->GetNextPacketSize(&packetLength);
        }

        std::this_thread::sleep_for(std::chrono::milliseconds(5));
    }

    // Cleanup
    audioClient->Stop();
    captureClient->Release();
    audioClient->Release();
    enumerator->Release();
    CoUninitialize();
#else
    qWarning() << "System audio capture is only supported on Windows.";
    system_audio_active_ = false;
#endif
}
