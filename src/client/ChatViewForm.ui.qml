import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: root

    property alias channelName: headerText.text
    property alias messageListView: messageList
    property alias messageInput: inputField
    property alias chatModel: chatModel

    signal usernameClicked(string username, real clickX, real clickY)

    // Chat Area
    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // Chat Header
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 48
            color: "#1A1B1E"

            Rectangle {
                anchors.bottom: parent.bottom
                anchors.left: parent.left
                anchors.right: parent.right
                height: 1
                color: "#242528" // Separator line
            }

            Text {
                id: headerText
                anchors.verticalCenter: parent.verticalCenter
                anchors.left: parent.left
                anchors.leftMargin: 24
                color: "white"
                font.pixelSize: 16
                font.weight: Font.Bold
                text: "# general"
            }
        }

        // Message History List
        ListView {
            id: messageList
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            spacing: 0

            verticalLayoutDirection: ListView.BottomToTop

            model: ListModel {
                id: chatModel
            }

            delegate: MessageDelegate {
                id: msgDel
                username: model.username
                timestamp: model.timestamp
                messageText: model.messageText
                showHeader: model.showHeader
                onUsernameClicked: (username, cx, cy) => {
                    var pos = msgDel.mapToItem(root, cx, cy)
                    root.usernameClicked(username, pos.x, pos.y)
                }
            }
        }

        // Input Area
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 76
            color: "#1A1B1E"

            Rectangle {
                anchors.fill: parent
                anchors.margins: 16
                anchors.topMargin: 0
                color: "#242528"
                radius: 12

                TextInput {
                    id: inputField
                    anchors.fill: parent
                    anchors.leftMargin: 16
                    anchors.rightMargin: 16
                    verticalAlignment: Text.AlignVCenter
                    color: "white"
                    font.pixelSize: 14
                    clip: true

                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: "Message " + root.channelName + "..."
                        color: "#884f6a86"
                        font.pixelSize: 14
                        visible: !inputField.text && !inputField.activeFocus
                    }
                }
            }
        }
    }
}
