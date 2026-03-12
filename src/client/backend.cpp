#include "backend.hpp"
#include <QDebug>
#include <QGuiApplication>
#include <QScreen>

ChatBackend::ChatBackend(QObject* parent) 
    : QObject(parent), 
      ssl_context_(ssl::context::tlsv12_client),
      work_guard_(boost::asio::make_work_guard(io_context_)) {
    ssl_context_.set_verify_mode(ssl::verify_none);

    udp_socket_ = new QUdpSocket(this);
    udp_socket_->bind(QHostAddress::Any, 0);
    udp_socket_->setSocketOption(QAbstractSocket::ReceiveBufferSizeSocketOption, 2 * 1024 * 1024);
    udp_socket_->setSocketOption(QAbstractSocket::SendBufferSizeSocketOption, 2 * 1024 * 1024);
    connect(udp_socket_, &QUdpSocket::readyRead, this, &ChatBackend::processUdpDatagrams);
    
    audio_engine_ = std::make_unique<AudioEngine>();
    connect(audio_engine_.get(), &AudioEngine::localAudioLevelChanged, this, &ChatBackend::localAudioLevelChanged);
    connect(audio_engine_.get(), &AudioEngine::remoteUserSpeaking, this, &ChatBackend::remoteUserSpeaking);
    connect(audio_engine_.get(), &AudioEngine::sendUdpData, this, [this](const QByteArray& data, const QHostAddress& host, quint16 port) {
        udp_socket_->writeDatagram(data, host, port);
    });

    video_engine_ = std::make_unique<VideoEngine>();
    connect(video_engine_.get(), &VideoEngine::remoteFrameReceived, this, [this](const QString& username, const QVideoFrame& frame) {
        auto it = video_sinks_.find(username);
        if (it != video_sinks_.end()) {
            for (auto* sink : it->second)
                sink->setVideoFrame(frame);
        }
    });
    connect(video_engine_.get(), &VideoEngine::localFrameCaptured, this, [this](const QVideoFrame& frame) {
        auto it = video_sinks_.find(my_username_);
        if (it != video_sinks_.end()) {
            for (auto* sink : it->second)
                sink->setVideoFrame(frame);
        }
    });
    connect(video_engine_.get(), &VideoEngine::sendUdpData, this, [this](const QByteArray& data, const QHostAddress& host, quint16 port) {
        udp_socket_->writeDatagram(data, host, port);
    });
    connect(video_engine_.get(), &VideoEngine::keyframeNeeded, this, [this](const QString& streamerUsername) {
        if (voice_server_udp_port_ == 0) return;
        chatproj::UdpKeyframeRequest req;
        req.packet_type = chatproj::UdpPacketType::KEYFRAME_REQUEST;
        std::memset(req.sender_id, 0, chatproj::SENDER_ID_SIZE);
        // Use last 31 chars of JWT as compact UDP identifier
        size_t id_len = std::min(jwt_token_.size(), size_t(chatproj::SENDER_ID_SIZE - 1));
        std::string udp_id = jwt_token_.substr(jwt_token_.size() - id_len);
        std::memcpy(req.sender_id, udp_id.c_str(), udp_id.size());
        std::memset(req.target_username, 0, chatproj::SENDER_ID_SIZE);
        auto target = streamerUsername.toStdString();
        std::memcpy(req.target_username, target.c_str(), std::min(target.size(), size_t(chatproj::SENDER_ID_SIZE - 1)));
        QByteArray data(reinterpret_cast<const char*>(&req), sizeof(req));
        udp_socket_->writeDatagram(data, voice_server_host_, voice_server_udp_port_);
    });

    network_thread_ = std::thread([this]() { io_context_.run(); });
    connectToCentralServer();
}

ChatBackend::~ChatBackend() {
    logout();
    work_guard_.reset();
    io_context_.stop();
    if (network_thread_.joinable()) network_thread_.join();
}

void ChatBackend::connectToCentralServer() {
    std::lock_guard<std::mutex> lock(connection_mutex_);
    if (central_connection_) return;

    central_connection_ = std::make_shared<ConnectionState>();
    central_connection_->socket = std::make_shared<ssl::stream<tcp::socket>>(io_context_, ssl_context_);
    central_connection_->is_central = true;

    auto resolver = std::make_shared<tcp::resolver>(io_context_);
    resolver->async_resolve("93.131.204.246", "8080", 
        [this, resolver](const boost::system::error_code& ec, tcp::resolver::results_type results) {
            if (!ec) {
                boost::asio::async_connect(central_connection_->socket->lowest_layer(), results,
                    [this](const boost::system::error_code& ec2, const tcp::endpoint&) {
                        if (!ec2) {
                            central_connection_->socket->async_handshake(ssl::stream_base::client,
                                [this](const boost::system::error_code& ec3) {
                                    if (!ec3) {
                                        startAsyncRead(central_connection_);
                                    } else {
                                        emit connectionLost("Central TLS Handshake failed.");
                                    }
                                });
                        } else {
                            emit connectionLost("Central Connection failed.");
                        }
                    });
            } else {
                emit connectionLost("Central Resolution failed.");
            }
        });
}

void ChatBackend::disconnectFromCentralServer() {
    std::lock_guard<std::mutex> lock(connection_mutex_);
    if (central_connection_ && central_connection_->socket) {
        boost::system::error_code ec;
        central_connection_->socket->lowest_layer().close(ec);
        central_connection_.reset();
    }
}

void ChatBackend::attemptLogin(const QString& username, const QString& password) {
    if (!central_connection_) connectToCentralServer();
    my_username_ = username;
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::LOGIN_REQ);
    auto* login = packet.mutable_login_req();
    login->set_username(username.toStdString());
    login->set_password(password.toStdString());
    sendCentralPacket(packet);
}

void ChatBackend::attemptRegister(const QString& username, const QString& email, const QString& password) {
    if (!central_connection_) connectToCentralServer();
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::REGISTER_REQ);
    auto* reg_req = packet.mutable_register_req();
    reg_req->set_username(username.toStdString());
    reg_req->set_email(email.toStdString());
    reg_req->set_password(password.toStdString());
    sendCentralPacket(packet);
}

void ChatBackend::sendPrivateMessage(const QString& recipient, const QString& message) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::DIRECT_MSG);
    auto* msg = packet.mutable_direct_msg();
    msg->set_recipient(recipient.toStdString());
    msg->set_content(message.toStdString());
    sendCentralPacket(packet);
}

void ChatBackend::requestServerList() {
    qDebug() << "[Client] Formatting SERVER_LIST_REQ packet...";
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::SERVER_LIST_REQ);
    packet.mutable_server_list_req();
    sendCentralPacket(packet);
    qDebug() << "[Client C++] SERVER_LIST_REQ dispatched to network queue.";
}

void ChatBackend::requestFriendList() {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::FRIEND_LIST_REQ);
    packet.mutable_friend_list_req();
    sendCentralPacket(packet);
}

void ChatBackend::sendFriendAction(int action, const QString& targetUsername) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::FRIEND_ACTION_REQ);
    auto* req = packet.mutable_friend_action_req();
    req->set_action(static_cast<chatproj::FriendActionType>(action));
    req->set_target_username(targetUsername.toStdString());
    sendCentralPacket(packet);
}

void ChatBackend::logout() {
    disconnectFromCentralServer();
    
    std::lock_guard<std::mutex> lock(connection_mutex_);
    for (auto& pair : community_connections_) {
        boost::system::error_code ec;
        pair.second->socket->lowest_layer().close(ec);
    }
    community_connections_.clear();

    my_username_.clear();
    jwt_token_.clear();
    emit loggedOut();
}

void ChatBackend::sendCentralPacket(chatproj::Packet packet) {
    std::lock_guard<std::mutex> lock(connection_mutex_);
    if (!central_connection_) return;

    if (packet.type() != chatproj::Packet::LOGIN_REQ && 
        packet.type() != chatproj::Packet::REGISTER_REQ) {
        packet.set_auth_token(jwt_token_);
    }

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
    asyncWrite(central_connection_, framed);
}

// --- COMMUNITY SERVERS ---

void ChatBackend::connectToCommunityServer(int serverId, const QString& host, int port) {
    bool already_connected = false;
    {
        std::lock_guard<std::mutex> lock(connection_mutex_);
        if (community_connections_.find(serverId) != community_connections_.end()) {
            already_connected = true;
        }
    }

    if (already_connected) {
        // Force the QML UI to navigate to the chat view without opening a new socket
        emit communityAuthResponded(serverId, true, "Already connected.");
        return;
    }

    std::lock_guard<std::mutex> lock(connection_mutex_);
    
    auto state = std::make_shared<ConnectionState>();
    state->socket = std::make_shared<ssl::stream<tcp::socket>>(io_context_, ssl_context_);
    state->is_central = false;
    state->server_id = serverId;
    state->host = host;
    state->port = port;
    community_connections_[serverId] = state;

    auto resolver = std::make_shared<tcp::resolver>(io_context_);
    resolver->async_resolve(host.toStdString(), std::to_string(port), 
        [this, resolver, state, serverId](const boost::system::error_code& ec, tcp::resolver::results_type results) {
            if (!ec) {
                boost::asio::async_connect(state->socket->lowest_layer(), results,
                    [this, state, serverId](const boost::system::error_code& ec2, const tcp::endpoint&) {
                        if (!ec2) {
                            // Enable TCP keepalive to detect dead connections
                            state->socket->lowest_layer().set_option(boost::asio::socket_base::keep_alive(true));
                            state->socket->async_handshake(ssl::stream_base::client,
                                [this, state, serverId](const boost::system::error_code& ec3) {
                                    if (!ec3) {
                                        startAsyncRead(state);
                                        
                                        chatproj::Packet auth_packet;
                                        auth_packet.set_type(chatproj::Packet::COMMUNITY_AUTH_REQ);
                                        auth_packet.mutable_community_auth_req()->set_jwt_token(jwt_token_);
                                        
                                        std::string serialized;
                                        auth_packet.SerializeToString(&serialized);
                                        auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
                                        asyncWrite(state, framed);
                                    }
                                });
                        }
                    });
            }
        });
}

void ChatBackend::disconnectFromCommunityServer(int serverId) {
    std::lock_guard<std::mutex> lock(connection_mutex_);
    auto it = community_connections_.find(serverId);
    if (it != community_connections_.end()) {
        boost::system::error_code ec;
        it->second->socket->lowest_layer().close(ec);
        community_connections_.erase(it);
    }
}

void ChatBackend::joinChannel(int serverId, const QString& channelId) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::JOIN_CHANNEL_REQ);
    packet.mutable_join_channel_req()->set_channel_id(channelId.toStdString());
    sendCommunityPacket(serverId, packet);
}

void ChatBackend::joinVoiceChannel(int serverId, const QString& channelId) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::JOIN_VOICE_REQ);
    packet.mutable_join_voice_req()->set_channel_id(channelId.toStdString());
    sendCommunityPacket(serverId, packet);

    // Start audio engine and store voice server endpoint for PLI requests
    std::lock_guard<std::mutex> lock(connection_mutex_);
    auto it = community_connections_.find(serverId);
    if (it != community_connections_.end()) {
        QString host = it->second->host;
        voice_server_host_ = QHostAddress(host);
        voice_server_udp_port_ = 8083;
        audio_engine_->startVoice(QString::fromStdString(jwt_token_), host, 8083);
    }
}

void ChatBackend::leaveVoiceChannel() {
    audio_engine_->stopSystemAudioCapture();
    audio_engine_->stopVoice();
    video_engine_->stopStream();

    std::vector<int> active_servers;
    {
        std::lock_guard<std::mutex> lock(connection_mutex_);
        for (auto& pair : community_connections_) {
            active_servers.push_back(pair.first);
        }
    }

    for (int serverId : active_servers) {
        chatproj::Packet packet;
        packet.set_type(chatproj::Packet::LEAVE_VOICE_REQ);
        packet.mutable_leave_voice_req();
        sendCommunityPacket(serverId, packet);
    }
}

#ifdef _WIN32
struct WindowEnumInfo {
    HWND hwnd;
    QString title;
    int width, height;
};

static BOOL CALLBACK EnumWindowsProc(HWND hwnd, LPARAM lParam) {
    auto* windows = reinterpret_cast<std::vector<WindowEnumInfo>*>(lParam);

    if (!IsWindowVisible(hwnd)) return TRUE;
    if (IsIconic(hwnd)) return TRUE;
    if (GetWindow(hwnd, GW_OWNER) != nullptr) return TRUE;

    int len = GetWindowTextLengthW(hwnd);
    if (len == 0) return TRUE;

    LONG exStyle = GetWindowLongW(hwnd, GWL_EXSTYLE);
    if (exStyle & WS_EX_TOOLWINDOW) return TRUE;

    RECT rect;
    GetWindowRect(hwnd, &rect);
    int w = rect.right - rect.left;
    int h = rect.bottom - rect.top;
    if (w < 100 || h < 100) return TRUE;

    WCHAR title[512];
    GetWindowTextW(hwnd, title, 512);

    windows->push_back({hwnd, QString::fromWCharArray(title), w, h});
    return TRUE;
}
#endif

QVariantList ChatBackend::getCaptureSources() {
    QVariantList sources;

    // Enumerate screens
    auto screens = QGuiApplication::screens();
    for (int i = 0; i < screens.size(); ++i) {
        QScreen* screen = screens[i];
        QVariantMap map;
        map["type"] = "screen";
        map["name"] = QString("Screen %1 (%2x%3)").arg(i + 1).arg(screen->size().width()).arg(screen->size().height());
        map["id"] = QString::number(i);
        map["width"] = screen->size().width();
        map["height"] = screen->size().height();
        sources.append(map);
    }

#ifdef _WIN32
    // Enumerate visible application windows
    std::vector<WindowEnumInfo> windows;
    EnumWindows(EnumWindowsProc, reinterpret_cast<LPARAM>(&windows));

    for (const auto& win : windows) {
        QVariantMap map;
        map["type"] = "window";
        map["name"] = win.title;
        map["id"] = QString::number(reinterpret_cast<quintptr>(win.hwnd));
        map["width"] = win.width;
        map["height"] = win.height;
        sources.append(map);
    }
#endif

    return sources;
}

void ChatBackend::startVideoStream(int serverId, const QString& channelId, int fps, int bitrateKbps, bool includeAudio, const QString& sourceType, const QString& sourceId, int resWidth, int resHeight, bool adaptiveBitrate) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::START_STREAM_REQ);
    auto* req = packet.mutable_start_stream_req();
    req->set_channel_id(channelId.toStdString());
    req->set_target_fps(fps);
    req->set_target_bitrate_kbps(bitrateKbps);
    req->set_has_audio(includeAudio);

    sendCommunityPacket(serverId, packet);

    int captureType = (sourceType == "window") ? 1 : 0;
    quintptr captureId = sourceId.toULongLong();

    std::lock_guard<std::mutex> lock(connection_mutex_);
    auto it = community_connections_.find(serverId);
    if (it != community_connections_.end()) {
        QString host = it->second->host;
        video_engine_->startStream(QString::fromStdString(jwt_token_), host, 8083, fps, bitrateKbps, resWidth, resHeight, captureType, captureId, adaptiveBitrate);
        if (includeAudio) {
            audio_engine_->startSystemAudioCapture();
        }
    }
}

void ChatBackend::toggleMute() {
    bool muted = !audio_engine_->isMuted();
    audio_engine_->setMuted(muted);
    emit muteChanged(muted);
}

bool ChatBackend::isMuted() {
    return audio_engine_->isMuted();
}

void ChatBackend::registerVideoSink(const QString& username, QObject* sinkObj) {
    auto* sink = qobject_cast<QVideoSink*>(sinkObj);
    if (!sink) return;
    video_sinks_[username].push_back(sink);
}

void ChatBackend::unregisterVideoSink(const QString& username, QObject* sinkObj) {
    auto* sink = qobject_cast<QVideoSink*>(sinkObj);
    if (!sink) return;
    auto it = video_sinks_.find(username);
    if (it != video_sinks_.end()) {
        auto& vec = it->second;
        vec.erase(std::remove(vec.begin(), vec.end(), sink), vec.end());
        if (vec.empty()) video_sinks_.erase(it);
    }
}

void ChatBackend::sendChannelMessage(int serverId, const QString& channelId, const QString& message) {
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::CHANNEL_MSG);
    auto* msg = packet.mutable_channel_msg();
    msg->set_channel_id(channelId.toStdString());
    msg->set_content(message.toStdString());
    sendCommunityPacket(serverId, packet);
}

void ChatBackend::sendCommunityPacket(int serverId, chatproj::Packet packet) {
    std::lock_guard<std::mutex> lock(connection_mutex_);
    auto it = community_connections_.find(serverId);
    if (it == community_connections_.end()) return;

    packet.set_auth_token(jwt_token_);
    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
    asyncWrite(it->second, framed);
}

void ChatBackend::processUdpDatagrams() {
    // Drain all pending datagrams from the kernel buffer FIRST, before doing
    // any expensive processing (VP8 decode).  This prevents the OS from
    // dropping packets while we're busy decoding a previous frame.
    QList<QByteArray> datagrams;
    while (udp_socket_->hasPendingDatagrams()) {
        QByteArray datagram;
        datagram.resize(static_cast<int>(udp_socket_->pendingDatagramSize()));
        udp_socket_->readDatagram(datagram.data(), datagram.size());
        if (!datagram.isEmpty()) {
            datagrams.append(std::move(datagram));
        }
    }

    for (const auto& datagram : datagrams) {
        uint8_t packet_type = static_cast<uint8_t>(datagram.at(0));

        if (packet_type == chatproj::UdpPacketType::AUDIO) {
            audio_engine_->processDatagram(datagram);
        } else if (packet_type == chatproj::UdpPacketType::VIDEO) {
            video_engine_->processDatagram(datagram);
        } else if (packet_type == chatproj::UdpPacketType::KEYFRAME_REQUEST) {
            // A viewer is requesting us (the streamer) to emit a keyframe
            video_engine_->requestKeyframe();
        } else if (packet_type == chatproj::UdpPacketType::NACK) {
            // A viewer is requesting retransmission of specific missing packets
            video_engine_->processNack(datagram);
        } else if (packet_type == chatproj::UdpPacketType::FEC) {
            // Forward Error Correction packet — used to recover lost video packets
            video_engine_->processFecPacket(datagram);
        }
    }
}

// --- ASYNC EVENT HANDLERS ---

void ChatBackend::startAsyncRead(std::shared_ptr<ConnectionState> state) {
    boost::asio::async_read(*(state->socket), boost::asio::buffer(&(state->inbound_header), 4),
        [this, state](boost::system::error_code ec, std::size_t) {
            if (!ec) {
                uint32_t length = ntohl(state->inbound_header);
                if (length > 2 * 1024 * 1024) return; 
                state->inbound_body.resize(length);
                
                boost::asio::async_read(*(state->socket), boost::asio::buffer(state->inbound_body.data(), length),
                    [this, state](boost::system::error_code ec2, std::size_t) {
                        if (!ec2) {
                            chatproj::Packet packet;
                            if (packet.ParseFromArray(state->inbound_body.data(), state->inbound_body.size())) {
                                handlePacket(state, packet);
                            }
                            startAsyncRead(state);
                        } else if (state->is_central) {
                            emit connectionLost("Connection body read error.");
                        } else {
                            handleCommunityDisconnect(state);
                        }
                    });
            } else if (state->is_central) {
                emit connectionLost("Connection header read error.");
            } else {
                handleCommunityDisconnect(state);
            }
        });
}

void ChatBackend::handleCommunityDisconnect(std::shared_ptr<ConnectionState> state) {
    int serverId = state->server_id;
    QString host = state->host;
    int port = state->port;

    // Remove the broken connection
    {
        std::lock_guard<std::mutex> lock(connection_mutex_);
        auto it = community_connections_.find(serverId);
        if (it != community_connections_.end() && it->second == state) {
            boost::system::error_code ec;
            state->socket->lowest_layer().close(ec);
            community_connections_.erase(it);
        } else {
            return; // Already cleaned up (e.g. user manually disconnected)
        }
    }

    qDebug() << "[Backend] Community server" << serverId << "disconnected, reconnecting in 2s...";

    // Attempt reconnect after a short delay
    auto timer = std::make_shared<boost::asio::steady_timer>(io_context_);
    timer->expires_after(std::chrono::seconds(2));
    timer->async_wait([this, timer, serverId, host, port](boost::system::error_code ec) {
        if (!ec) {
            connectToCommunityServer(serverId, host, port);
        }
    });
}

void ChatBackend::handlePacket(std::shared_ptr<ConnectionState> state, const chatproj::Packet& msg_packet) {
    if (state->is_central) {
        if (msg_packet.type() == chatproj::Packet::LOGIN_RES) {
            bool success = msg_packet.login_res().success();
            QString msg = success ? "Login successful" : QString::fromStdString(msg_packet.login_res().message());
            emit statusMessageChanged(msg);
            if (success) {
                jwt_token_ = msg_packet.login_res().jwt_token(); 
                emit loginSucceeded();  
            }
        }
        else if (msg_packet.type() == chatproj::Packet::REGISTER_RES) {
            emit registerResponded(msg_packet.register_res().success(), QString::fromStdString(msg_packet.register_res().message()));
        }
        else if (msg_packet.type() == chatproj::Packet::DIRECT_MSG) {
            const auto& dmsg = msg_packet.direct_msg();
            emit messageReceived(
                "@" + QString::fromStdString(dmsg.sender()),
                QString::fromStdString(dmsg.sender()), 
                QString::fromStdString(dmsg.content()),
                static_cast<qint64>(dmsg.timestamp())
            );
        }
        else if (msg_packet.type() == chatproj::Packet::PRESENCE_UPDATE) {
            QStringList userList;
            for (int i = 0; i < msg_packet.presence_update().online_users_size(); ++i) {
                userList << QString::fromStdString(msg_packet.presence_update().online_users(i));
            }
            emit userListUpdated(userList);
        }
        else if (msg_packet.type() == chatproj::Packet::SERVER_LIST_RES) {
            QVariantList serverList;
            const auto& res = msg_packet.server_list_res();
            for (int i = 0; i < res.servers_size(); ++i) {
                const auto& srv = res.servers(i);
                QVariantMap map;
                map["id"] = srv.id();
                map["name"] = QString::fromStdString(srv.name());
                map["description"] = QString::fromStdString(srv.description());
                map["host_ip"] = QString::fromStdString(srv.host_ip());
                map["port"] = srv.port();
                map["member_count"] = srv.member_count();
                serverList.append(map);
            }
            emit serverListReceived(serverList);
        }
        else if (msg_packet.type() == chatproj::Packet::FRIEND_ACTION_RES) {
            emit friendActionResponded(msg_packet.friend_action_res().success(), QString::fromStdString(msg_packet.friend_action_res().message()));
        }
        else if (msg_packet.type() == chatproj::Packet::FRIEND_LIST_RES) {
            QVariantList friendList;
            const auto& res = msg_packet.friend_list_res();
            for (int i = 0; i < res.friends_size(); ++i) {
                const auto& f = res.friends(i);
                QVariantMap map;
                map["usernameLabel"] = QString::fromStdString(f.username());
                map["status"] = static_cast<int>(f.status());
                friendList.append(map);
            }
            emit friendListReceived(friendList);
        }
    } else {
        if (msg_packet.type() == chatproj::Packet::COMMUNITY_AUTH_RES) {
            QVariantList channelList;
            const auto& res = msg_packet.community_auth_res();
            for (int i = 0; i < res.channels_size(); ++i) {
                const auto& ch = res.channels(i);
                QVariantMap map;
                map["channelId"] = QString::fromStdString(ch.id());
                map["channelName"] = QString::fromStdString(ch.name());
                map["type"] = static_cast<int>(ch.type());
                channelList.append(map);
            }
            emit communityAuthResponded(state->server_id, res.success(), QString::fromStdString(res.message()), channelList);
        }
        else if (msg_packet.type() == chatproj::Packet::CHANNEL_MSG) {
            const auto& cmsg = msg_packet.channel_msg();
            emit messageReceived(
                "#" + QString::fromStdString(cmsg.channel_id()),
                QString::fromStdString(cmsg.sender()),
                QString::fromStdString(cmsg.content()),
                static_cast<qint64>(cmsg.timestamp())
            );
        }
        else if (msg_packet.type() == chatproj::Packet::VOICE_PRESENCE_UPDATE) {
            const auto& update = msg_packet.voice_presence_update();
            QStringList users;
            for (int i = 0; i < update.active_users_size(); ++i) {
                users.append(QString::fromStdString(update.active_users(i)));
            }
            emit voicePresenceUpdated(QString::fromStdString(update.channel_id()), users);
        }
        else if (msg_packet.type() == chatproj::Packet::STREAM_PRESENCE_UPDATE) {
            const auto& update = msg_packet.stream_presence_update();
            QVariantList streams;
            for (int i = 0; i < update.active_streams_size(); ++i) {
                const auto& info = update.active_streams(i);
                QVariantMap map;
                map["streamId"] = QString::fromStdString(info.stream_id());
                map["owner"] = QString::fromStdString(info.owner_username());
                map["hasAudio"] = info.has_audio();
                streams.append(map);
            }
            emit streamPresenceUpdated(QString::fromStdString(update.channel_id()), streams);
        }
    }
}

void ChatBackend::asyncWrite(std::shared_ptr<ConnectionState> state, std::shared_ptr<std::vector<uint8_t>> framed) {
    bool write_in_progress = false;
    {
        std::lock_guard<std::mutex> lock(state->write_mutex);
        write_in_progress = !state->write_queue.empty();
        state->write_queue.push_back(framed);
    }

    if (!write_in_progress) {
        boost::asio::post(io_context_, [this, state]() {
            doWrite(state);
        });
    }
}

void ChatBackend::doWrite(std::shared_ptr<ConnectionState> state) {
    std::shared_ptr<std::vector<uint8_t>> next_msg;
    {
        std::lock_guard<std::mutex> lock(state->write_mutex);
        if (state->write_queue.empty()) return;
        next_msg = state->write_queue.front();
    }

    boost::asio::async_write(*(state->socket), boost::asio::buffer(*next_msg),
        [this, state](boost::system::error_code ec, std::size_t) {
            if (!ec) {
                std::lock_guard<std::mutex> lock(state->write_mutex);
                state->write_queue.pop_front();
                if (!state->write_queue.empty()) {
                    boost::asio::post(io_context_, [this, state]() {
                        doWrite(state);
                    });
                }
            }
        });
}
