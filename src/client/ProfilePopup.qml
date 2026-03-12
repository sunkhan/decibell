import QtQuick 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15

Popup {
    id: popup
    width: 300
    height: 350
    modal: false
    dim: false
    padding: 0
    closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside

    signal messageSent(string username, string message)

    property string targetUsername: ""

    function showForUser(username, px, py) {
        targetUsername = username
        messageInput.text = ""

        // Position near click point
        var popupX = px + 10
        var popupY = py - height / 2

        // Clamp to parent bounds
        if (parent) {
            if (popupX + width + 10 > parent.width)
                popupX = px - width - 10
            if (popupY < 4)
                popupY = 4
            if (popupY + height + 4 > parent.height)
                popupY = parent.height - height - 4
        }

        x = popupX
        y = popupY
        open()
        messageInput.forceActiveFocus()
    }

    function avatarColor(name) {
        var hash = 0
        for (var i = 0; i < name.length; i++) {
            hash = name.charCodeAt(i) + ((hash << 5) - hash)
        }
        var colors = ["#E91E63", "#9C27B0", "#673AB7", "#3F51B5",
                       "#2196F3", "#00BCD4", "#009688", "#4CAF50",
                       "#FF9800", "#FF5722", "#795548", "#607D8B"]
        return colors[Math.abs(hash) % colors.length]
    }

    enter: Transition {
        NumberAnimation { property: "opacity"; from: 0; to: 1; duration: 120; easing.type: Easing.OutCubic }
    }

    exit: Transition {
        NumberAnimation { property: "opacity"; from: 1; to: 0; duration: 80; easing.type: Easing.InCubic }
    }

    background: Item {
        // Outer shadow
        Rectangle {
            x: -6; y: -3
            width: parent.width + 12; height: parent.height + 14
            radius: 16
            color: "#20000000"
        }
        // Inner shadow
        Rectangle {
            x: -3; y: -1
            width: parent.width + 6; height: parent.height + 7
            radius: 14
            color: "#15000000"
        }
        // Main background
        Rectangle {
            anchors.fill: parent
            color: "#242528"
            radius: 12
        }
    }

    contentItem: Item {
        // Banner
        Rectangle {
            id: banner
            anchors.top: parent.top
            anchors.left: parent.left
            anchors.right: parent.right
            height: 60
            color: "#0C0E13"
            radius: 12

            // Square off bottom corners
            Rectangle {
                anchors.bottom: parent.bottom
                anchors.left: parent.left
                anchors.right: parent.right
                height: 12
                color: parent.color
            }
        }

        // Avatar circle
        Rectangle {
            id: avatar
            width: 72
            height: 72
            radius: 36
            color: popup.avatarColor(popup.targetUsername)
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.top: banner.bottom
            anchors.topMargin: -36
            border.color: "#242528"
            border.width: 4

            Text {
                anchors.centerIn: parent
                text: popup.targetUsername.length > 0 ? popup.targetUsername.charAt(0).toUpperCase() : "?"
                color: "white"
                font.pixelSize: 28
                font.weight: Font.Bold
            }
        }

        // Username
        Row {
            id: usernameRow
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.top: avatar.bottom
            anchors.topMargin: 12
            spacing: 8

            Text {
                text: popup.targetUsername
                color: "white"
                font.pixelSize: 18
                font.weight: Font.Bold
            }

            // Online status dot
            Rectangle {
                width: 10
                height: 10
                radius: 5
                color: "#43B581"
                anchors.verticalCenter: parent.verticalCenter
            }
        }

        // Divider
        Rectangle {
            id: divider
            anchors.top: usernameRow.bottom
            anchors.topMargin: 16
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.leftMargin: 16
            anchors.rightMargin: 16
            height: 1
            color: "#2D3245"
        }

        // Label
        Text {
            id: aboutLabel
            anchors.top: divider.bottom
            anchors.topMargin: 12
            anchors.left: parent.left
            anchors.leftMargin: 16
            text: "MEMBER"
            color: "#72767d"
            font.pixelSize: 11
            font.weight: Font.Bold
        }

        // Message input area
        Rectangle {
            anchors.bottom: parent.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.margins: 12
            height: 40
            radius: 8
            color: "#0C0E13"

            TextInput {
                id: messageInput
                anchors.fill: parent
                anchors.leftMargin: 12
                anchors.rightMargin: 12
                verticalAlignment: TextInput.AlignVCenter
                color: "white"
                font.pixelSize: 14
                clip: true

                onAccepted: {
                    var msg = messageInput.text.trim()
                    if (msg !== "") {
                        popup.messageSent(popup.targetUsername, msg)
                        popup.close()
                    }
                }

                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: "Message @" + popup.targetUsername
                    color: "#72767d"
                    font.pixelSize: 14
                    visible: messageInput.text.length === 0
                }
            }
        }
    }
}
