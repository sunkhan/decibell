import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: root
    width: ListView.view ? ListView.view.width : 400
    height: layout.implicitHeight + (showHeader ? 16 : 4)

    property alias avatarColor: avatar.color
    property alias username: nameText.text
    property alias timestamp: timeText.text
    property alias messageText: msgText.text
    property alias bgRect: bg
    property alias clickArea: mouseArea

    property bool showHeader: true

    signal usernameClicked(string username, real clickX, real clickY)

    Rectangle {
        id: bg
        anchors.fill: parent
        color: "transparent"

        RowLayout {
            id: layout
            anchors.top: parent.top
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.topMargin: root.showHeader ? 12 : 2
            anchors.leftMargin: 24
            anchors.rightMargin: 24
            spacing: 16

            // Left Gutter
            Item {
                id: leftGutter
                Layout.preferredWidth: 40
                Layout.preferredHeight: root.showHeader ? 40 : 16
                Layout.alignment: Qt.AlignTop

                Rectangle {
                    id: avatar
                    anchors.top: parent.top
                    anchors.left: parent.left
                    width: 40
                    height: 40
                    radius: 8
                    color: "#2D3245"
                    visible: root.showHeader
                }

                Text {
                    id: hoverTimeText
                    anchors.verticalCenter: parent.verticalCenter
                    anchors.right: parent.right
                    text: root.timestamp
                    color: "#884f6a86"
                    font.pixelSize: 10
                    visible: false
                }
            }

            // Message Content
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 4

                RowLayout {
                    id: headerRow
                    spacing: 8
                    visible: root.showHeader

                    Text {
                        id: nameText
                        color: "white"
                        font.pixelSize: 15
                        font.weight: Font.Medium
                    }

                    Text {
                        id: timeText
                        color: "#884f6a86"
                        font.pixelSize: 12
                    }
                }

                Text {
                    id: msgText
                    Layout.fillWidth: true
                    color: "#DCDDDE"
                    font.pixelSize: 14
                    wrapMode: Text.WordWrap
                }
            }
        }

        MouseArea {
            id: mouseArea
            anchors.fill: parent
            hoverEnabled: true
            onClicked: function(mouse) {
                if (root.showHeader) {
                    var mapped = nameText.mapFromItem(mouseArea, mouse.x, mouse.y)
                    if (mapped.x >= 0 && mapped.x <= nameText.width &&
                        mapped.y >= 0 && mapped.y <= nameText.height) {
                        root.usernameClicked(nameText.text, mouse.x, mouse.y)
                    }
                }
            }
            cursorShape: {
                if (root.showHeader) {
                    var pos = nameText.mapFromItem(mouseArea, mouseX, mouseY)
                    if (pos.x >= 0 && pos.x <= nameText.width &&
                        pos.y >= 0 && pos.y <= nameText.height) {
                        return Qt.PointingHandCursor
                    }
                }
                return Qt.ArrowCursor
            }
        }
    }

    states: [
        State {
            name: "hovered"
            when: mouseArea.containsMouse
            PropertyChanges {
                target: bg
                color: "#242528"
            }
            PropertyChanges {
                target: hoverTimeText
                visible: !root.showHeader
            }
        }
    ]
}
