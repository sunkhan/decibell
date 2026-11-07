import QtQuick
import Decibell.Theme
import Decibell.Video

// Spike (a): one or more watched streams side by side (--video a.dbv[,b.dbv]). A stats line on each tile unless
// a benchmark runs.
Rectangle {
    id: root
    property var sources: []
    property bool showStats: true
    color: "#000000"

    Row {
        anchors.fill: parent
        anchors.margins: root.sources.length > 1 ? 8 : 0
        spacing: 8
        Repeater {
            model: root.sources
            Item {
                id: tile
                required property string modelData
                width: (parent.width - (root.sources.length - 1) * 8) / root.sources.length
                height: parent.height
                VideoItem {
                    id: video
                    anchors.fill: parent
                    source: tile.modelData
                }
                Text {
                    visible: root.showStats
                    anchors.left: parent.left
                    anchors.top: parent.top
                    anchors.margins: 8
                    color: "#ffffff"
                    style: Text.Outline
                    styleColor: "#000000"
                    font.family: "Inter"
                    font.pointSize: 0.75 * 12
                    text: video.error !== "" ? video.error
                        : video.codecName + " " + video.videoSize.width + "x" + video.videoSize.height + "@" + video.videoFps
                          + "  decoded " + video.decodedFps + "/s  shown " + video.presentedFps + "/s"
                }
            }
        }
    }
}
