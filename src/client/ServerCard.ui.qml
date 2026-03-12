import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: root
    width: 260
    height: 180

    property alias title: titleText.text
    property alias desc: descText.text
    property alias members: memberText.text
    property alias clickArea: mouseArea
    property alias bgRect: bg

    Rectangle {
        id: bg
        anchors.fill: parent
        radius: 8
        color: "#242528"
        border.width: 1
        border.color: "transparent"
        clip: true

        ColumnLayout {
            anchors.fill: parent
            anchors.margins: 16
            spacing: 8

            Text {
                id: titleText
                Layout.fillWidth: true
                color: "white"
                font.pixelSize: 16
                font.weight: Font.Bold
                elide: Text.ElideRight
            }

            Text {
                id: descText
                Layout.fillWidth: true
                Layout.fillHeight: true
                color: "#884f6a86"
                font.pixelSize: 12
                wrapMode: Text.WordWrap
                elide: Text.ElideRight
                verticalAlignment: Text.AlignTop
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: 6

                Rectangle {
                    width: 8
                    height: 8
                    radius: 4
                    color: "#43B581"
                }

                Text {
                    id: memberText
                    color: "#884f6a86"
                    font.pixelSize: 12
                }
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
                border.color: "#2CA3E8"
            }
        }
    ]
}
