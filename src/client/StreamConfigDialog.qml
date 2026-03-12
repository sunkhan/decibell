import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Popup {
    id: root
    width: 420
    height: 580
    modal: true
    focus: true
    anchors.centerIn: parent

    property var captureSources: []

    signal startStream(int fps, int bitrateKbps, bool includeAudio, string sourceType, string sourceId, int resWidth, int resHeight, bool adaptiveBitrate)

    background: Rectangle {
        color: "#1A1B1E"
        radius: 12
        border.color: "#2D3245"
        border.width: 1
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 24
        spacing: 12

        Text {
            text: "Screen Share Settings"
            color: "white"
            font.pixelSize: 20
            font.weight: Font.Bold
            Layout.alignment: Qt.AlignHCenter
            Layout.bottomMargin: 4
        }

        // Capture Source Selection
        Text {
            text: "Capture Source"
            color: "#884f6a86"
            font.pixelSize: 12
            font.weight: Font.Bold
        }

        ComboBox {
            id: sourceCombo
            Layout.fillWidth: true
            model: root.captureSources
            textRole: "name"
            currentIndex: 0

            background: Rectangle {
                color: "#242528"
                radius: 6
                border.color: "#2D3245"
            }
            contentItem: Text {
                text: sourceCombo.currentText
                color: "white"
                font.pixelSize: 14
                verticalAlignment: Text.AlignVCenter
                leftPadding: 12
                elide: Text.ElideRight
            }
        }

        // Resolution Selection
        Text {
            text: "Resolution"
            color: "#884f6a86"
            font.pixelSize: 12
            font.weight: Font.Bold
            Layout.topMargin: 4
        }

        ComboBox {
            id: resCombo
            Layout.fillWidth: true
            model: [
                { text: "720p (1280x720)", width: 1280, height: 720 },
                { text: "1080p (1920x1080)", width: 1920, height: 1080 },
                { text: "1440p (2560x1440)", width: 2560, height: 1440 },
                { text: "Source (Native Resolution)", width: 0, height: 0 }
            ]
            textRole: "text"
            currentIndex: 1

            background: Rectangle {
                color: "#242528"
                radius: 6
                border.color: "#2D3245"
            }
            contentItem: Text {
                text: resCombo.currentText
                color: "white"
                font.pixelSize: 14
                verticalAlignment: Text.AlignVCenter
                leftPadding: 12
            }
        }

        // FPS Selection
        Text {
            text: "Frame Rate (FPS)"
            color: "#884f6a86"
            font.pixelSize: 12
            font.weight: Font.Bold
            Layout.topMargin: 4
        }

        ComboBox {
            id: fpsCombo
            Layout.fillWidth: true
            model: [
                { text: "5 FPS (Presentations / Low Bandwidth)", value: 5 },
                { text: "30 FPS (Standard Video)", value: 30 },
                { text: "60 FPS (High Quality Gaming)", value: 60 }
            ]
            textRole: "text"
            currentIndex: 1 // Default to 30 FPS

            background: Rectangle {
                color: "#242528"
                radius: 6
                border.color: "#2D3245"
            }
            contentItem: Text {
                text: fpsCombo.currentText
                color: "white"
                font.pixelSize: 14
                verticalAlignment: Text.AlignVCenter
                leftPadding: 12
            }
        }

        // Bitrate Selection
        Text {
            text: "Video Quality (Bitrate)"
            color: "#884f6a86"
            font.pixelSize: 12
            font.weight: Font.Bold
            Layout.topMargin: 4
        }

        ComboBox {
            id: bitrateCombo
            Layout.fillWidth: true
            model: [
                { text: "1500 kbps (Low Quality)", value: 1500 },
                { text: "3500 kbps (Standard Quality)", value: 3500 },
                { text: "6000 kbps (High Quality)", value: 6000 },
                { text: "8000 kbps (Source Quality)", value: 8000 },
                { text: "10000 kbps (Ultra Quality)", value: 10000 }
            ]
            textRole: "text"
            currentIndex: 1 // Default to 3500 kbps

            background: Rectangle {
                color: "#242528"
                radius: 6
                border.color: "#2D3245"
            }
            contentItem: Text {
                text: bitrateCombo.currentText
                color: "white"
                font.pixelSize: 14
                verticalAlignment: Text.AlignVCenter
                leftPadding: 12
            }
        }

        // Audio Toggle
        RowLayout {
            Layout.fillWidth: true
            Layout.topMargin: 4

            Text {
                text: "Include System Audio"
                color: "white"
                font.pixelSize: 14
                Layout.fillWidth: true
            }

            Switch {
                id: audioSwitch
                checked: false
            }
        }

        // Adaptive Bitrate Toggle
        RowLayout {
            Layout.fillWidth: true
            Layout.topMargin: 0

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2

                Text {
                    text: "Adaptive Bitrate"
                    color: "white"
                    font.pixelSize: 14
                }

                Text {
                    text: "Auto-adjusts quality based on network conditions"
                    color: "#884f6a86"
                    font.pixelSize: 11
                }
            }

            Switch {
                id: adaptiveBitrateSwitch
                checked: true
            }
        }

        Item { Layout.fillHeight: true } // Spacer

        // Action Buttons
        RowLayout {
            Layout.fillWidth: true
            spacing: 16

            Rectangle {
                Layout.fillWidth: true
                Layout.preferredHeight: 40
                color: cancelArea.containsMouse ? "#2D3245" : "transparent"
                radius: 6

                Text {
                    anchors.centerIn: parent
                    text: "Cancel"
                    color: "white"
                    font.pixelSize: 14
                    font.weight: Font.Bold
                }

                MouseArea {
                    id: cancelArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.close()
                }
            }

            Rectangle {
                Layout.fillWidth: true
                Layout.preferredHeight: 40
                color: startArea.containsPress ? "#1E8BC3" : (startArea.containsMouse ? "#4DB8F0" : "#2CA3E8")
                radius: 6

                Text {
                    anchors.centerIn: parent
                    text: "Start Streaming"
                    color: "white"
                    font.pixelSize: 14
                    font.weight: Font.Bold
                }

                MouseArea {
                    id: startArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        var fps = fpsCombo.model[fpsCombo.currentIndex].value
                        var bitrate = bitrateCombo.model[bitrateCombo.currentIndex].value
                        var hasAudio = audioSwitch.checked

                        var sourceIdx = sourceCombo.currentIndex
                        var source = root.captureSources[sourceIdx]
                        var sourceType = source.type
                        var sourceId = source.id

                        var res = resCombo.model[resCombo.currentIndex]
                        var resWidth = res.width
                        var resHeight = res.height
                        if (resWidth === 0) {
                            // "Source" resolution - use the capture source's native size
                            resWidth = source.width
                            resHeight = source.height
                        }

                        var adaptiveBr = adaptiveBitrateSwitch.checked
                        root.startStream(fps, bitrate, hasAudio, sourceType, sourceId, resWidth, resHeight, adaptiveBr)
                        root.close()
                    }
                }
            }
        }
    }
}
