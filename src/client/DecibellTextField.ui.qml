import QtQuick
import QtQuick.Controls

TextField {
    id: control
    width: 300
    height: 40
    color: "white"
    placeholderTextColor: "#884f6a86"

    hoverEnabled: true

    // Expose the highlight color so it can be themed externally if needed
    property color highlightColor: "#2CA3E8"

    FontLoader {
        id: openSans
        source: "assets/OpenSans-Italic.ttf"
    }

    font.family: openSans.name
    font.pixelSize: 14
    font.styleName: "Italic"

    background: Rectangle {
        id: bgRect
        color: "#0c0f16"
        radius: 8
        border.width: 1
        border.color: "transparent"
    }

    states: [
        State {
            name: "focused"
            when: control.activeFocus
            PropertyChanges {
                target: bgRect
                border.color: control.highlightColor
            }
        },
        State {
            name: "hovered"
            when: control.hovered && !control.activeFocus
            PropertyChanges {
                target: bgRect
                border.color: "#4f6a86"
            }
        }
    ]
}
