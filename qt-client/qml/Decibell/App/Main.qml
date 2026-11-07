import QtQuick
import Decibell.Theme
import Decibell.Core

// Spike screen 0: every theme's surfaces, type and elevation side by side, straight from the generated
// palettes. The first check of tools/gen-theme and the bundled fonts against the renderer.
Window {
    id: root
    property string themeName: "nocturne"
    width: 1280
    height: 800
    minimumWidth: 800
    minimumHeight: 600
    visible: true
    title: "Decibell (Qt spike) " + AppInfo.version + " " + AppInfo.profile
    color: Theme.t.colorBgDarkest

    Component.onCompleted: Theme.name = themeName

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
