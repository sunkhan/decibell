#pragma once
#include <QObject>
#include <QString>
#include <QStringList>
#include <QVariantList>
#include <QVariantMap>
#include <thread>
#include <mutex>
#include <unordered_map>
#include <memory>
#include <deque>
#include <boost/asio.hpp>
#include <boost/asio/ssl.hpp>
#include "../common/net_utils.hpp"
#include "messages.pb.h"
#include "audio_engine.hpp"
#include "video_engine.hpp"
#include <QUdpSocket>
#include <QVideoSink>
#include <QVideoFrame>

using boost::asio::ip::tcp;
namespace ssl = boost::asio::ssl;

struct ConnectionState {
    std::shared_ptr<ssl::stream<tcp::socket>> socket;
    uint32_t inbound_header = 0;
    std::vector<uint8_t> inbound_body;
    bool is_central = false;
    int server_id = -1;
    QString host; // Store the host for UDP connections
    int port = 0; // Store the port for reconnection

    std::deque<std::shared_ptr<std::vector<uint8_t>>> write_queue;
    std::mutex write_mutex;
};

class ChatBackend : public QObject {
    Q_OBJECT
    Q_PROPERTY(QString username READ getUsername NOTIFY loginSucceeded)

public:
    explicit ChatBackend(QObject* parent = nullptr);
    ~ChatBackend();
    QString getUsername() const { return my_username_; }

public slots:
    void attemptLogin(const QString& username, const QString& password);
    void attemptRegister(const QString& username, const QString& email, const QString& password);
    void sendPrivateMessage(const QString& recipient, const QString& message);
    void requestServerList();
    void logout();

    void connectToCommunityServer(int serverId, const QString& host, int port);
    void joinChannel(int serverId, const QString& channelId);
    void sendChannelMessage(int serverId, const QString& channelId, const QString& message);
    void disconnectFromCommunityServer(int serverId);

    // Voice System
    void joinVoiceChannel(int serverId, const QString& channelId);
    void leaveVoiceChannel();

    // Audio Mute
    Q_INVOKABLE void toggleMute();
    Q_INVOKABLE bool isMuted();

    // Video System
    void startVideoStream(int serverId, const QString& channelId, int fps, int bitrateKbps, bool includeAudio, const QString& sourceType, const QString& sourceId, int resWidth, int resHeight, bool adaptiveBitrate = true);
    Q_INVOKABLE QVariantList getCaptureSources();
    Q_INVOKABLE void registerVideoSink(const QString& username, QObject* sink);
    Q_INVOKABLE void unregisterVideoSink(const QString& username, QObject* sink);
    void requestFriendList();
    void sendFriendAction(int action, const QString& targetUsername);

signals:
    void statusMessageChanged(const QString& newMessage);
    void messageReceived(const QString& context, const QString& sender, const QString& content, qint64 timestamp);
    void loginSucceeded();
    void userListUpdated(const QStringList& users);
    void registerResponded(bool success, const QString& message);
    void loggedOut();
    void connectionLost(const QString& errorMsg);
    void serverListReceived(const QVariantList& servers);
    void communityAuthResponded(int serverId, bool success, const QString& message, const QVariantList& channels = QVariantList());
    void voicePresenceUpdated(const QString& channelId, const QStringList& users);
    void streamPresenceUpdated(const QString& channelId, const QVariantList& streams);
    
    // Friend System
    void friendListReceived(const QVariantList& friends);
    void friendActionResponded(bool success, const QString& message);

    // Audio System
    void localAudioLevelChanged(qreal level);
    void muteChanged(bool muted);
    void remoteUserSpeaking(const QString& username, qreal level);

    // Video System (sink-based, no signal needed)

private slots:
    void processUdpDatagrams();

private:
    void connectToCentralServer();
    void disconnectFromCentralServer();
    void sendCentralPacket(chatproj::Packet packet);
    void sendCommunityPacket(int serverId, chatproj::Packet packet);

    void startAsyncRead(std::shared_ptr<ConnectionState> state);
    void handlePacket(std::shared_ptr<ConnectionState> state, const chatproj::Packet& packet);
    void handleCommunityDisconnect(std::shared_ptr<ConnectionState> state);
    void asyncWrite(std::shared_ptr<ConnectionState> state, std::shared_ptr<std::vector<uint8_t>> framed);
    void doWrite(std::shared_ptr<ConnectionState> state);

    boost::asio::io_context io_context_;
    boost::asio::executor_work_guard<boost::asio::io_context::executor_type> work_guard_;
    ssl::context ssl_context_;
    std::thread network_thread_;

    std::mutex connection_mutex_;
    std::shared_ptr<ConnectionState> central_connection_;
    std::unordered_map<int, std::shared_ptr<ConnectionState>> community_connections_;

    QString my_username_;
    std::string jwt_token_; 

    QUdpSocket* udp_socket_ = nullptr;
    std::unique_ptr<AudioEngine> audio_engine_;
    std::unique_ptr<VideoEngine> video_engine_;
    std::unordered_map<QString, std::vector<QVideoSink*>> video_sinks_;

    // Stored so we can send PLI keyframe requests back to the community server
    QHostAddress voice_server_host_;
    quint16 voice_server_udp_port_ = 0;
};