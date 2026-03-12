import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: root
    width: ListView.view ? ListView.view.width : 248
    height: 44

    property alias username: nameText.text
    property alias statusColor: statusIndicator.color
    property alias bgRect: bg
    property alias clickArea: mouseArea

    Rectangle {
        id: bg
        anchors.fill: parent
        radius: 4
        color: "transparent"

        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 8
            anchors.rightMargin: 8
            spacing: 12

            Rectangle {
                Layout.preferredWidth: 32
                Layout.preferredHeight: 32
                radius: 8 // Changed to 8 for a rounded rectangle
                color: "#2D3245"

                Rectangle {
                    id: statusIndicator
                    width: 12
                    height: 12
                    radius: 6 // Status indicator remains a circle
                    anchors.bottom: parent.bottom
                    anchors.right: parent.right
                    anchors.bottomMargin: -2
                    anchors.rightMargin: -2
                    border.width: 2
                    border.color: "#1A1B1E"
                }
            }

            Text {
                id: nameText
                Layout.fillWidth: true
                color: "#884f6a86"
                font.pixelSize: 14
                font.weight: Font.Medium
                elide: Text.ElideRight
            }
        }

        MouseArea {
            id: mouseArea
            anchors.fill: parent
            hoverEnabled: true
        }
    }

    states: [
        State {
            name: "hovered"
            when: mouseArea.containsMouse
            PropertyChanges {
                target: bg
                color: "#2D3245"
            }
            PropertyChanges {
                target: nameText
                color: "white"
            }
        }
    ]
}
