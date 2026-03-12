import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtMultimedia

Item {
    id: root

    signal userSpeakingSignal(string username, real level)
    signal fullscreenRequested(string username)

    property alias gridView: participantsGrid
    property alias channelNameText: headerTitle.text
    property alias disconnectBtn: disconnectBtnArea
    property alias cameraBtn: cameraBtnArea
    property alias screenShareBtn: screenShareBtnArea
    property alias muteBtn: muteBtnArea
    property bool isMuted: false
    property string focusedStreamUser: ""

    onFocusedStreamUserChanged: {
        // Re-register expanded view sink when focused user changes
        if (expandedView._registeredUser !== "") {
            backend.unregisterVideoSink(expandedView._registeredUser, expandedVideo.videoSink)
            expandedView._registeredUser = ""
        }
        if (focusedStreamUser !== "") {
            backend.registerVideoSink(focusedStreamUser, expandedVideo.videoSink)
            expandedView._registeredUser = focusedStreamUser
        }
    }

    FontLoader {
        id: faIcons
        source: "assets/FontAwesome7Free-Solid-900.otf"
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // Header
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 48
            color: "#1A1B1E"

            Rectangle {
                anchors.bottom: parent.bottom
                anchors.left: parent.left
                anchors.right: parent.right
                height: 1
                color: "#242528"
            }

            Text {
                id: headerTitle
                anchors.verticalCenter: parent.verticalCenter
                anchors.left: parent.left
                anchors.leftMargin: 24
                color: "white"
                font.pixelSize: 16
                font.weight: Font.Bold
                text: "Voice Channel"
            }
        }

        // Main Grid / Focused Area
        Rectangle {
            Layout.fillWidth: true
            Layout.fillHeight: true
            color: "#1A1B1E"

            GridView {
                id: participantsGrid
                anchors.fill: parent
                anchors.margins: 24
                cellWidth: 320
                cellHeight: 240
                clip: true
                visible: root.focusedStreamUser === ""

                // Temporary mockup delegate
                delegate: Rectangle {
                    id: gridDelegate
                    width: 300
                    height: 220
                    color: "#1A1B1E"
                    radius: 8
                    border.color: isSpeaking ? "#2CA3E8" : "#2D3245"
                    border.width: isSpeaking ? 2 : 1

                    property bool isSpeaking: false
                    property string streamUsername: modelData.username
                    property bool hasStream: modelData.hasStream

                    Component.onCompleted: {
                        if (gridDelegate.hasStream)
                            backend.registerVideoSink(gridDelegate.streamUsername, streamVideo.videoSink)
                    }
                    Component.onDestruction: {
                        backend.unregisterVideoSink(gridDelegate.streamUsername, streamVideo.videoSink)
                    }

                    Connections {
                        target: root
                        function onUserSpeakingSignal(username, level) {
                            if (username === gridDelegate.streamUsername && level > 0.02) {
                                gridDelegate.isSpeaking = true
                                speakTimer.restart()
                            }
                        }
                    }

                    Timer {
                        id: speakTimer
                        interval: 300
                        onTriggered: gridDelegate.isSpeaking = false
                    }

                    // Placeholder for video stream
                    Rectangle {
                        anchors.fill: parent
                        anchors.margins: 4
                        color: "#0C0E13"
                        radius: 6
                        clip: true // Keep the video inside the rounded borders

                        Text {
                            anchors.centerIn: parent
                            text: gridDelegate.hasStream ? "📺 Connecting..." : "🎥 " + gridDelegate.streamUsername
                            color: gridDelegate.isSpeaking ? "white" : "#884f6a86"
                            font.pixelSize: 18
                            visible: !streamVideo.visible
                        }

                        VideoOutput {
                            id: streamVideo
                            anchors.fill: parent
                            visible: gridDelegate.hasStream
                            fillMode: VideoOutput.PreserveAspectFit
                        }

                        // Click to expand stream
                        MouseArea {
                            anchors.fill: parent
                            enabled: gridDelegate.hasStream
                            cursorShape: gridDelegate.hasStream ? Qt.PointingHandCursor : Qt.ArrowCursor
                            onClicked: root.focusedStreamUser = gridDelegate.streamUsername
                        }

                        // Fullscreen button overlay
                        Rectangle {
                            anchors.top: parent.top
                            anchors.right: parent.right
                            anchors.margins: 8
                            width: 28
                            height: 28
                            radius: 4
                            color: fullscreenTileBtn.containsMouse ? "#80000000" : "#40000000"
                            visible: gridDelegate.hasStream
                            z: 2

                            Text {
                                anchors.centerIn: parent
                                text: "\uf065"
                                font.family: faIcons.name
                                color: "white"
                                font.pixelSize: 12
                            }

                            MouseArea {
                                id: fullscreenTileBtn
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: root.fullscreenRequested(gridDelegate.streamUsername)
                            }
                        }
                    }

                    // User name plate
                    Rectangle {
                        anchors.bottom: parent.bottom
                        anchors.left: parent.left
                        anchors.right: parent.right
                        anchors.margins: 8
                        height: 32
                        color: "#242528"
                        radius: 6
                        opacity: 0.8

                        RowLayout {
                            anchors.fill: parent
                            anchors.leftMargin: 12
                            anchors.rightMargin: 12

                            Text {
                                text: gridDelegate.streamUsername
                                color: gridDelegate.isSpeaking ? "white" : "#b9bbbe"
                                font.pixelSize: 14
                                Layout.fillWidth: true
                            }

                            Rectangle {
                                width: 12
                                height: 12
                                radius: 6
                                color: "#FF4C4C"
                                visible: gridDelegate.hasStream

                                Text {
                                    anchors.centerIn: parent
                                    text: "LIVE"
                                    color: "white"
                                    font.pixelSize: 8
                                    font.weight: Font.Bold
                                    anchors.horizontalCenterOffset: 20
                                }
                            }
                        }
                    }
                }
            }

            // Expanded / Focused Stream View
            Rectangle {
                id: expandedView
                anchors.fill: parent
                anchors.margins: 8
                color: "#0C0E13"
                radius: 8
                visible: root.focusedStreamUser !== ""
                clip: true

                property string _registeredUser: ""

                VideoOutput {
                    id: expandedVideo
                    anchors.fill: parent
                    anchors.margins: 4
                    fillMode: VideoOutput.PreserveAspectFit
                }

                // Click background to go back to grid
                MouseArea {
                    anchors.fill: parent
                    onClicked: root.focusedStreamUser = ""
                    cursorShape: Qt.PointingHandCursor
                }

                // Back button (top-left)
                Rectangle {
                    anchors.top: parent.top
                    anchors.left: parent.left
                    anchors.margins: 12
                    width: 36
                    height: 36
                    radius: 18
                    color: backBtn.containsMouse ? "#B0000000" : "#60000000"
                    z: 2

                    Text {
                        anchors.centerIn: parent
                        text: "\uf060"
                        font.family: faIcons.name
                        color: "white"
                        font.pixelSize: 14
                    }

                    MouseArea {
                        id: backBtn
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.focusedStreamUser = ""
                    }
                }

                // Fullscreen button (top-right)
                Rectangle {
                    anchors.top: parent.top
                    anchors.right: parent.right
                    anchors.margins: 12
                    width: 36
                    height: 36
                    radius: 18
                    color: expandedFullscreenBtn.containsMouse ? "#B0000000" : "#60000000"
                    z: 2

                    Text {
                        anchors.centerIn: parent
                        text: "\uf065"
                        font.family: faIcons.name
                        color: "white"
                        font.pixelSize: 14
                    }

                    MouseArea {
                        id: expandedFullscreenBtn
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.fullscreenRequested(root.focusedStreamUser)
                    }
                }

                // Username label (bottom-left)
                Rectangle {
                    anchors.bottom: parent.bottom
                    anchors.left: parent.left
                    anchors.margins: 12
                    height: 32
                    width: expandedUsername.implicitWidth + 24
                    color: "#80000000"
                    radius: 6
                    z: 2

                    Text {
                        id: expandedUsername
                        anchors.centerIn: parent
                        text: root.focusedStreamUser
                        color: "white"
                        font.pixelSize: 14
                    }
                }
            }
        }

        // Bottom Controls
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 80
            color: "#0C0D0F"

            Rectangle {
                anchors.top: parent.top
                anchors.left: parent.left
                anchors.right: parent.right
                height: 1
                color: "#242528"
            }

            RowLayout {
                anchors.centerIn: parent
                spacing: 24

                Rectangle {
                    width: 48
                    height: 48
                    radius: 24
                    color: root.isMuted
                        ? (muteBtnArea.containsMouse ? "#FF6B6B" : "#FF4C4C")
                        : (muteBtnArea.containsMouse ? "#3A405A" : "#2D3245")

                    Text {
                        anchors.centerIn: parent
                        text: root.isMuted ? "\uf131" : "\uf130"
                        font.family: faIcons.name
                        color: "white"
                        font.pixelSize: 18
                    }

                    MouseArea {
                        id: muteBtnArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                    }
                }

                Rectangle {
                    width: 48
                    height: 48
                    radius: 24
                    color: cameraBtnArea.containsMouse ? "#3A405A" : "#2D3245"

                    Text {
                        anchors.centerIn: parent
                        text: "📹"
                        color: "white"
                        font.pixelSize: 20
                    }

                    MouseArea {
                        id: cameraBtnArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                    }
                }

                Rectangle {
                    width: 48
                    height: 48
                    radius: 24
                    color: screenShareBtnArea.containsMouse ? "#3A405A" : "#2D3245"

                    Text {
                        anchors.centerIn: parent
                        text: "💻"
                        color: "white"
                        font.pixelSize: 20
                    }

                    MouseArea {
                        id: screenShareBtnArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                    }
                }

                Rectangle {
                    width: 64
                    height: 48
                    radius: 24
                    color: disconnectBtnArea.containsMouse ? "#FF6B6B" : "#FF4C4C"

                    Text {
                        anchors.centerIn: parent
                        text: "📞"
                        color: "white"
                        font.pixelSize: 20
                    }

                    MouseArea {
                        id: disconnectBtnArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                    }
                }
            }
        }
    }
}
