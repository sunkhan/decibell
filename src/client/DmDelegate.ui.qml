import QtQuick
import QtQuick.Controls

Item {
    id: root
    width: 48
    height: 48

    property alias initials: label.text
    property alias bgRect: bg
    property alias clickArea: mouseArea

    Rectangle {
        id: bg
        anchors.fill: parent
        radius: 8 // Squircle shape
        color: "#2D3245"

        Text {
            id: label
            anchors.centerIn: parent
            color: "white"
            font.pixelSize: 16
            font.weight: Font.Bold
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
                color: "#3A405A"
            }
        }
    ]
}
