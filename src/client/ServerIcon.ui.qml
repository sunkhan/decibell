import QtQuick
import QtQuick.Controls

Item {
    id: root
    width: 120
    height: 48

    property alias textLabel: label.text
    property alias bgRect: bg
    property alias clickArea: mouseArea

    Rectangle {
        id: bg
        anchors.fill: parent
        anchors.topMargin: 0
        radius: 8
        color: "#2D3245"

        Text {
            id: label
            anchors.centerIn: parent
            width: parent.width - 24 // Accounts for horizontal padding
            color: "white"
            font.pixelSize: 14
            font.weight: Font.Medium
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight // Truncates text with "..." if it exceeds 96px
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

    transitions: [
        Transition {
            ColorAnimation {
                property: "color"
                duration: 150
            }
        }
    ]
}
