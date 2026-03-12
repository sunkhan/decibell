import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: root

    signal channelClicked(string channelId, string channelName)
    signal joinVoiceChannel(string channelId, string channelName)
    signal leaveVoiceChannel()
    signal userSpeakingSignal(string username, real level)
    signal voiceUserClicked(string username, real clickX, real clickY)

    property alias channelsModel: channelsModel
    property alias channelsListView: channelsListView

    property bool inVoiceChannel: false
    property string activeVoiceChannelName: ""
    property real audioLevel: 0.0
    property var activeVoiceUsers: ({})

    Rectangle {
        anchors.fill: parent
        color: "#0C0D0F"

        Rectangle {
            anchors.right: parent.right
            anchors.top: parent.top
            anchors.bottom: parent.bottom
            width: 1
            color: "#0C0E13"
        }

        ColumnLayout {
            anchors.fill: parent
            anchors.margins: 16
            spacing: 16

            Text {
                text: "CHANNELS"
                color: "#884f6a86"
                font.pixelSize: 12
                font.weight: Font.Bold
                Layout.topMargin: 8
            }

            ListView {
                id: channelsListView
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                spacing: 4

                model: ListModel {
                    id: channelsModel
                }

                delegate: Column {
                    id: delegateRoot
                    width: ListView.view.width
                    spacing: 2

                    property string myChannelId: channelId
                    property string myChannelName: channelName
                    property int myType: type !== undefined ? type : 0

                    Rectangle {
                        width: parent.width
                        height: 32
                        color: delegateMouseArea.containsMouse ? "#242528" : "transparent"
                        radius: 4

                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            anchors.left: parent.left
                            anchors.leftMargin: 8
                            text: (delegateRoot.myType === 1 ? "🔊 " : "# ") + delegateRoot.myChannelName
                            color: "#884f6a86"
                            font.pixelSize: 14
                        }

                        MouseArea {
                            id: delegateMouseArea
                            anchors.fill: parent
                            hoverEnabled: true
                            onClicked: {
                                if (delegateRoot.myType === 1) {
                                    root.joinVoiceChannel(delegateRoot.myChannelId, delegateRoot.myChannelName)
                                } else {
                                    root.channelClicked(delegateRoot.myChannelId, delegateRoot.myChannelName)
                                }
                            }
                        }
                    }

                    Repeater {
                        model: root.activeVoiceUsers[delegateRoot.myChannelId] || []
                        delegate: Item {
                            id: voiceUserItem
                            width: delegateRoot.width
                            height: 28
                            
                            property bool isSpeaking: false

                            Connections {
                                target: root
                                function onUserSpeakingSignal(username, level) {
                                    if (username === modelData && level > 0.02) {
                                        voiceUserItem.isSpeaking = true
                                        speakTimer.restart()
                                    }
                                }
                            }

                            Timer {
                                id: speakTimer
                                interval: 300
                                onTriggered: voiceUserItem.isSpeaking = false
                            }

                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 24
                                spacing: 8

                                Rectangle {
                                    Layout.preferredWidth: 20
                                    Layout.preferredHeight: 20
                                    radius: 10
                                    color: "#2D3245"
                                    border.width: voiceUserItem.isSpeaking ? 2 : 0
                                    border.color: "#2CA3E8"
                                }

                                Text {
                                    text: modelData
                                    color: voiceUserItem.isSpeaking ? "white" : "#884f6a86"
                                    font.pixelSize: 13
                                }
                            }

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: function(mouse) {
                                    var pos = voiceUserItem.mapToItem(root, mouse.x, mouse.y)
                                    root.voiceUserClicked(modelData, pos.x, pos.y)
                                }
                            }
                        }
                    }
                }
            }

            // Voice Connection Status Panel
            Rectangle {
                Layout.fillWidth: true
                Layout.preferredHeight: 64
                color: "#242528"
                radius: 8
                visible: root.inVoiceChannel

                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: 8
                    spacing: 4

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 8

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2

                            Text {
                                text: "Voice Connected"
                                color: "#2CA3E8"
                                font.pixelSize: 12
                                font.weight: Font.Bold
                            }

                            Text {
                                text: root.activeVoiceChannelName
                                color: "#884f6a86"
                                font.pixelSize: 11
                                elide: Text.ElideRight
                                Layout.fillWidth: true
                            }
                        }

                        Rectangle {
                            Layout.preferredWidth: 32
                            Layout.preferredHeight: 32
                            radius: 16
                            color: "#2D3245"

                            Text {
                                anchors.centerIn: parent
                                text: "✖"
                                color: "#FF4C4C"
                                font.pixelSize: 14
                            }

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onEntered: parent.color = "#FF4C4C"; 
                                onExited: parent.color = "#2D3245";
                                onClicked: root.leaveVoiceChannel()
                            }
                        }
                    }

                    // Audio Level Meter
                    Rectangle {
                        Layout.fillWidth: true
                        Layout.preferredHeight: 4
                        color: "#2D3245"
                        radius: 2
                        clip: true

                        Rectangle {
                            anchors.left: parent.left
                            anchors.top: parent.top
                            anchors.bottom: parent.bottom
                            width: parent.width * Math.min(1.0, root.audioLevel * 3.0) // Boost visual scale
                            color: "#2CA3E8"
                            radius: 2
                            
                            Behavior on width {
                                NumberAnimation { duration: 50; easing.type: Easing.OutQuad }
                            }
                        }
                    }
                }
            }
        }
    }
}