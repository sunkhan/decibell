#pragma once
#include <QObject>
#include <QAudioSource>
#include <QAudioSink>
#include <QMediaDevices>
#include <QHostAddress>
#include <opus/opus.h>
#include "../common/udp_packet.hpp"
#include <thread>
#include <mutex>
#include <deque>
#include <atomic>

class AudioEngine : public QObject {
    Q_OBJECT
public:
    explicit AudioEngine(QObject* parent = nullptr);
    ~AudioEngine();

    void startVoice(const QString& token, const QString& host, quint16 port);
    void stopVoice();

    void setMuted(bool muted) { is_muted_ = muted; }
    bool isMuted() const { return is_muted_; }

    void processDatagram(const QByteArray& datagram);

    // System audio capture for screen sharing
    void startSystemAudioCapture();
    void stopSystemAudioCapture();

signals:
    void localAudioLevelChanged(qreal level);
    void remoteUserSpeaking(const QString& username, qreal level);
    void sendUdpData(const QByteArray& data, const QHostAddress& host, quint16 port);

private slots:
    void handleAudioReady();

private:
    void initOpus();
    void captureSystemAudioLoop();

    QAudioSource* audio_source_ = nullptr;
    QAudioSink* audio_sink_ = nullptr;
    QIODevice* audio_input_device_ = nullptr;
    QIODevice* audio_output_device_ = nullptr;

    OpusEncoder* encoder_ = nullptr;
    OpusDecoder* decoder_ = nullptr;

    QString token_;
    std::string udp_id_;  // Last 31 chars of JWT for compact UDP identification
    QHostAddress server_host_;
    quint16 server_port_ = 0;

    bool is_active_ = false;
    bool is_muted_ = false;
    uint16_t sequence_number_ = 0;
    QByteArray input_buffer_;

    // System audio (WASAPI loopback)
    std::atomic<bool> system_audio_active_{false};
    std::thread system_audio_thread_;
    std::mutex sys_audio_mutex_;
    std::deque<int16_t> sys_audio_fifo_;
};
