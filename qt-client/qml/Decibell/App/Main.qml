import QtQuick
import Decibell.Theme
import Decibell.Core

// The spike's window. `screenName` (--screen): "chat" (a busy channel, the default), "gallery" (every theme's
// tokens side by side) or "video" (watched streams, --video). Frameless, as the Electron window is: the title bar is drawn by the app.
Window {
    id: root
    property string themeName: "nocturne"
    property string screenName: "chat"
    property string fixtureDir
    // --bench-scroll: the chat scrolls through history at this speed (px/s) for frame-pacing measurements.
    property real benchSpeed: 0
    property string ablate
    property int pageSize: 0
    // --video: test streams (.dbv) to watch; --bench-video hides the stats lines.
    property var videoSources: []
    property bool benchVideo: false
    width: 1280
    height: 800
    minimumWidth: 800
    minimumHeight: 600
    visible: true
    flags: screenName === "chat" ? (Qt.Window | Qt.FramelessWindowHint) : Qt.Window
    title: "Decibell (Qt spike) " + AppInfo.version
    color: Theme.chrome.colorBgDarkest

    Component.onCompleted: Theme.name = themeName

    Loader {
        anchors.fill: parent
        // "empty" (debug): no screen at all, the floor for memory measurements.
        sourceComponent: root.screenName === "gallery" ? gallery
                       : root.screenName === "video" || root.screenName === "empty" ? null : chatScreen
    }
    // Watched streams, by URL: VideoScreen.qml and Decibell.Video are only built on Windows so far, and a typed
    // reference would stop this module from compiling elsewhere.
    Loader {
        anchors.fill: parent
        active: root.screenName === "video"
        source: "VideoScreen.qml"
        onLoaded: {
            item.sources = Qt.binding(() => root.videoSources)
            item.showStats = Qt.binding(() => !root.benchVideo)
        }
    }
    Component {
        id: chatScreen
        ChatScreen { fixtureDir: root.fixtureDir; window: root; benchSpeed: root.benchSpeed; ablate: root.ablate; pageSize: root.pageSize }
    }
    Component {
        id: gallery
        Flow {
            anchors.fill: parent
            anchors.margins: 16
            spacing: 16
            Repeater {
                model: Theme.names
                ThemeCard {
                    required property string modelData
                    themeName: modelData
                }
            }
        }
    }
}
