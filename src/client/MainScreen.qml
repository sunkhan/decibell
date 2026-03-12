import QtQuick 2.15
import QtQuick.Window 2.15
import QtMultimedia

MainScreenForm {
    id: form

    property string activeRecipient: "" 
    property int activeServerId: -1
    property string activeChannelId: "" 
    property string activeVoiceChannelId: ""
    property var activeStreamsMap: ({})

    function updateVoiceUI(channelId) {
        var users = form.channelsSidebar.activeVoiceUsers[channelId] || []
        var streams = form.activeStreamsMap[channelId] || []
        
        var combinedList = []
        for (var i = 0; i < users.length; i++) {
            var uname = users[i]
            var hasStream = false
            for (var j = 0; j < streams.length; j++) {
                if (streams[j].owner === uname) {
                    hasStream = true
                    break
                }
            }
            combinedList.push({ "username": uname, "hasStream": hasStream })
        }

        if (form.activeVoiceChannelId === channelId) {
            form.voiceView.gridView.model = combinedList
        }
    }

    ListModel { id: friendsListModel }
    ListModel { id: publicServersModel }
    ListModel { id: activeServersModel }

    Component.onCompleted: {
        form.friendsListView.model = friendsListModel
        form.publicServersGrid.model = publicServersModel
        form.serverListView.model = activeServersModel 

        console.log("Requesting server and friend list from backend...")
        backend.requestServerList()
        backend.requestFriendList()
    }

    Connections {
        target: form.serverListWheelArea
        function onWheel(wheel) {
            if (form.serverListView.contentWidth > form.serverListView.width) {
                var newContentX = form.serverListView.contentX - wheel.angleDelta.y;
                form.serverListView.contentX = Math.max(0, Math.min(newContentX, form.serverListView.contentWidth - form.serverListView.width));
            }
        }
    }

    Connections {
        target: form.friendInput
        function onAccepted() {
            var target = form.friendInput.text.trim()
            if (target !== "") {
                backend.sendFriendAction(0, target) // 0 = ADD
                form.friendInput.text = ""
            }
        }
    }

    Connections {
        target: form.channelsSidebar
        function onChannelClicked(channelId, channelName) {
            if (form.activeServerId !== -1) {
                form.activeChannelId = channelId
                backend.joinChannel(form.activeServerId, channelId)
                form.chatView.channelName = "# " + channelName
                form.chatView.chatModel.clear()
                form.primaryStackLayout.currentIndex = 0
            }
        }
        function onJoinVoiceChannel(channelId, channelName) {
            if (form.activeServerId !== -1) {
                // If already in this channel, just switch to the voice view
                if (form.channelsSidebar.inVoiceChannel && form.channelsSidebar.activeVoiceChannelName === channelName) {
                    form.primaryStackLayout.currentIndex = 1
                } else {
                    // Otherwise, join it and optionally switch to it
                    form.voiceView.focusedStreamUser = ""
                    fullscreenWindow.close()
                    form.channelsSidebar.inVoiceChannel = true
                    form.channelsSidebar.activeVoiceChannelName = channelName
                    form.activeVoiceChannelId = channelId
                    form.voiceView.channelNameText = channelName
                    updateVoiceUI(channelId)
                    backend.joinVoiceChannel(form.activeServerId, channelId) 
                    form.primaryStackLayout.currentIndex = 1
                }
            }
        }
        function onLeaveVoiceChannel() {
            form.voiceView.focusedStreamUser = ""
            fullscreenWindow.close()
            form.channelsSidebar.inVoiceChannel = false
            form.channelsSidebar.activeVoiceChannelName = ""
            form.activeVoiceChannelId = ""
            backend.leaveVoiceChannel()
            form.primaryStackLayout.currentIndex = 0
        }
    }

    ProfilePopup {
        id: profilePopup
        parent: form

        onMessageSent: (username, message) => {
            backend.sendPrivateMessage(username, message)
            form.activeRecipient = username
            form.activeServerId = -1
            form.mainStackLayout.currentIndex = 1
            form.chatView.channelName = "@" + username
            form.chatView.chatModel.clear()
        }
    }

    Connections {
        target: form
        function onProfileRequested(username, clickX, clickY) {
            profilePopup.showForUser(username, clickX, clickY)
        }
    }

    Connections {
        target: form.chatView
        function onUsernameClicked(username, clickX, clickY) {
            var mapped = form.chatView.mapToItem(form, clickX, clickY)
            profilePopup.showForUser(username, mapped.x, mapped.y)
        }
    }

    Connections {
        target: form.channelsSidebar
        function onVoiceUserClicked(username, clickX, clickY) {
            var mapped = form.channelsSidebar.mapToItem(form, clickX, clickY)
            profilePopup.showForUser(username, mapped.x, mapped.y)
        }
    }

    StreamConfigDialog {
        id: streamConfigDialog
        onStartStream: (fps, bitrateKbps, includeAudio, sourceType, sourceId, resWidth, resHeight, adaptiveBitrate) => {
            if (form.activeServerId !== -1 && form.activeVoiceChannelId !== "") {
                backend.startVideoStream(form.activeServerId, form.activeVoiceChannelId, fps, bitrateKbps, includeAudio, sourceType, sourceId, resWidth, resHeight, adaptiveBitrate)
                console.log("Starting stream: " + resWidth + "x" + resHeight + " @ " + fps + " FPS, " + bitrateKbps + " kbps. Source: " + sourceType + ". Audio: " + includeAudio + ". Adaptive: " + adaptiveBitrate)
            }
        }
    }

    // Fullscreen stream viewer window
    Window {
        id: fullscreenWindow
        visible: false
        color: "black"
        title: "Stream - " + fullscreenWindow.streamUser

        property string streamUser: ""
        property string _registeredUser: ""

        onStreamUserChanged: {
            if (fullscreenWindow._registeredUser !== "")
                backend.unregisterVideoSink(fullscreenWindow._registeredUser, fullscreenVideo.videoSink)
            if (streamUser !== "") {
                backend.registerVideoSink(streamUser, fullscreenVideo.videoSink)
                fullscreenWindow._registeredUser = streamUser
            } else {
                fullscreenWindow._registeredUser = ""
            }
        }

        onClosing: {
            if (fullscreenWindow._registeredUser !== "") {
                backend.unregisterVideoSink(fullscreenWindow._registeredUser, fullscreenVideo.videoSink)
                fullscreenWindow._registeredUser = ""
            }
        }

        VideoOutput {
            id: fullscreenVideo
            anchors.fill: parent
            fillMode: VideoOutput.PreserveAspectFit
        }

        // Close button
        Rectangle {
            anchors.top: parent.top
            anchors.right: parent.right
            anchors.margins: 24
            width: 40
            height: 40
            radius: 20
            color: fsCloseBtn.containsMouse ? "#B0000000" : "#80000000"
            z: 2

            Text {
                anchors.centerIn: parent
                text: "\u2715"
                color: "white"
                font.pixelSize: 20
                font.weight: Font.Bold
            }

            MouseArea {
                id: fsCloseBtn
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: fullscreenWindow.close()
            }
        }

        Shortcut {
            sequence: "Escape"
            onActivated: fullscreenWindow.close()
        }
    }

    Connections {
        target: form.voiceView
        function onFullscreenRequested(username) {
            fullscreenWindow.streamUser = username
            fullscreenWindow.showFullScreen()
        }
    }

    Connections {
        target: form.voiceView.disconnectBtn
        function onClicked() {
            form.voiceView.focusedStreamUser = ""
            fullscreenWindow.close()
            form.channelsSidebar.inVoiceChannel = false
            form.channelsSidebar.activeVoiceChannelName = ""
            form.activeVoiceChannelId = ""
            backend.leaveVoiceChannel()
            form.primaryStackLayout.currentIndex = 0
        }
    }

    Connections {
        target: form.voiceView.muteBtn
        function onClicked() {
            backend.toggleMute()
            form.voiceView.isMuted = backend.isMuted()
        }
    }

    Connections {
        target: form.voiceView.screenShareBtn
        function onClicked() {
            streamConfigDialog.captureSources = backend.getCaptureSources()
            streamConfigDialog.open()
        }
    }

    Connections {
        target: form.chatView.messageInput
        function onAccepted() {
            var msg = form.chatView.messageInput.text.trim()
            
            if (msg !== "") {
                if (form.activeServerId !== -1) {
                    backend.sendChannelMessage(form.activeServerId, form.activeChannelId, msg)
                } else if (form.activeRecipient !== "") {
                    backend.sendPrivateMessage(form.activeRecipient, msg)
                }
                
                form.chatView.messageInput.text = ""
            }
        }
    }

    Connections {
        target: backend

        function onFriendListReceived(friends) {
            friendsListModel.clear()
            for (var i = 0; i < friends.length; i++) {
                var f = friends[i]
                var c = "#747F8D" // OFFLINE or BLOCKED
                if (f.status === 0) c = "#43B581" // ONLINE
                else if (f.status === 2 || f.status === 3) c = "#FAA61A" // PENDING

                friendsListModel.append({
                    "usernameLabel": f.usernameLabel,
                    "statusColor": c,
                    "status": f.status
                })
            }
        }

        function onFriendActionResponded(success, message) {
            console.log("Friend Action: " + message)
            backend.requestFriendList()
        }

        function onUserListUpdated(users) {
            backend.requestFriendList()
        }

        function onServerListReceived(servers) {
            console.log("Client received " + servers.length + " servers from backend.")
            publicServersModel.clear()
            for (var i = 0; i < servers.length; i++) {
                publicServersModel.append(servers[i])
            }
        }

        function onCommunityAuthResponded(serverId, success, message, channels) {
            if (success) {
                var exists = false
                var cachedChannels = []
                for (var i = 0; i < activeServersModel.count; i++) {
                    if (activeServersModel.get(i).serverId === serverId) {
                        exists = true
                        var srv = activeServersModel.get(i)
                        if (srv.channelsList) {
                            for (var c = 0; c < srv.channelsList.count; c++) {
                                cachedChannels.push({
                                    "channelId": srv.channelsList.get(c).channelId,
                                    "channelName": srv.channelsList.get(c).channelName,
                                    "type": srv.channelsList.get(c).type
                                })
                            }
                        }
                        break
                    }
                }

                var finalChannels = (channels && channels.length > 0) ? channels : cachedChannels

                if (!exists) {
                    var sName = "Server"
                    for (var j = 0; j < publicServersModel.count; j++) {
                        if (publicServersModel.get(j).id === serverId) {
                            sName = publicServersModel.get(j).name
                            break
                        }
                    }
                    
                    activeServersModel.append({
                        "serverId": serverId,
                        "serverName": sName,
                        "isHome": false,
                        "channelsList": finalChannels
                    })
                }

                form.activeServerId = serverId

                form.channelsSidebar.channelsModel.clear()
                var firstChannelId = "general"
                var firstChannelName = "general"

                if (finalChannels && finalChannels.length > 0) {
                    for (var k = 0; k < finalChannels.length; k++) {
                        form.channelsSidebar.channelsModel.append(finalChannels[k])
                    }
                    firstChannelId = finalChannels[0].channelId
                    firstChannelName = finalChannels[0].channelName
                } else {
                    form.channelsSidebar.channelsModel.append({"channelId": "general", "channelName": "general"})
                }

                form.activeChannelId = firstChannelId
                backend.joinChannel(serverId, firstChannelId)
                
                form.chatView.channelName = "# " + firstChannelName
                form.mainStackLayout.currentIndex = 1
                form.chatView.chatModel.clear()
            } else {
                console.error("Community Auth Failed: " + message)
            }
        }

        function onMessageReceived(context, sender, content, timestamp) {
            var d = new Date(timestamp * 1000)
            var timeStr = d.toLocaleTimeString(Qt.locale(), Locale.ShortFormat)
            var renderHeader = true

            if (form.chatView.chatModel.count > 0) {
                var lastMsg = form.chatView.chatModel.get(0)
                if (lastMsg.username === sender) {
                    var timeDifference = timestamp - lastMsg.rawTime
                    if (timeDifference < 300) {
                        renderHeader = false
                    }
                }
            }

            form.chatView.chatModel.insert(0, {
                "username": sender,
                "timestamp": timeStr,
                "messageText": content,
                "showHeader": renderHeader,
                "rawTime": timestamp
            })
        }
        
        function onConnectionLost(errorMsg) {
            console.error("Connection lost: " + errorMsg)
        }

        function onLocalAudioLevelChanged(level) {
            form.channelsSidebar.audioLevel = level
            form.channelsSidebar.userSpeakingSignal(backend.username, level)
            form.voiceView.userSpeakingSignal(backend.username, level)
        }

        function onVoicePresenceUpdated(channelId, users) {
            console.log("Voice presence updated for channel: " + channelId + " with users: " + users.length)
            var newObj = Object.assign({}, form.channelsSidebar.activeVoiceUsers)
            var arr = []
            for (var i = 0; i < users.length; i++) {
                arr.push(users[i])
            }
            newObj[channelId] = arr
            form.channelsSidebar.activeVoiceUsers = newObj
            
            updateVoiceUI(channelId)
        }

        function onStreamPresenceUpdated(channelId, streams) {
            console.log("Stream presence updated for channel: " + channelId + " with streams: " + streams.length)
            var newObj = Object.assign({}, form.activeStreamsMap)
            var arr = []
            for (var i = 0; i < streams.length; i++) {
                arr.push(streams[i])
            }
            newObj[channelId] = arr
            form.activeStreamsMap = newObj

            updateVoiceUI(channelId)
        }

        function onRemoteUserSpeaking(username, level) {
            form.channelsSidebar.userSpeakingSignal(username, level)
            form.voiceView.userSpeakingSignal(username, level)
        }
    }

}