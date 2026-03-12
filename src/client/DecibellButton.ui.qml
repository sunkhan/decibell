import QtQuick
import QtQuick.Controls

Button {
    id: control
    width: 116
    height: 40
    text: "Button"

    // Explicitly enable hover events for the hovered state to trigger
    hoverEnabled: true

    FontLoader {
        id: openSans
        source: "assets/OpenSans-Regular.ttf"
    }

    contentItem: Text {
        text: control.text
        color: "white"
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
        font.family: openSans.name
        font.pixelSize: 14 // Standard modern UI button size
        font.weight: Font.Medium // Adds slight thickness for legibility
    }

    background: Rectangle {
        id: bgRect
        color: "#2CA3E8"
        radius: 6
    }

    states: [
        State {
            name: "pressed"
            when: control.down
            PropertyChanges {
                target: bgRect
                color: "#1E8BC3"
            }
        },
        State {
            name: "hovered"
            when: control.hovered && !control.down
            PropertyChanges {
                target: bgRect
                color: "#4DB8F0"
            }
        }
    ]
}
