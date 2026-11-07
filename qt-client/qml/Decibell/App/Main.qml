import QtQuick
import Decibell.Theme
import Decibell.Core

// The spike's window. `screenName` (--screen): "chat" (a busy channel, the default) or "gallery" (every theme's
// tokens side by side). Frameless, as the Electron window is: the title bar is drawn by the app.
Window {
    id: root
    property string themeName: "nocturne"
    property string screenName: "chat"
    property string fixtureDir
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
        sourceComponent: root.screenName === "gallery" ? gallery : chatScreen
    }
    Component {
        id: chatScreen
        ChatScreen { fixtureDir: root.fixtureDir; window: root }
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
