import QtQuick
import QtQuick.Shapes
import Decibell.Theme

// UserAvatar / LetterAvatar: the picture when there is one, else the uid-hashed gradient with the current
// name's initial. Squircle radius from the theme (26 % in the Mona Sans themes, flat in console).
Item {
    id: av
    property string source
    property string initial
    property int colorIndex: 1
    property real size: 38
    width: size
    height: size

    function avColor(i: int): color {
        const t = Theme.t
        switch (i) {
        case 1: return t.colorAv1
        case 2: return t.colorAv2
        case 3: return t.colorAv3
        case 4: return t.colorAv4
        case 5: return t.colorAv5
        case 6: return t.colorAv6
        case 7: return t.colorAv7
        default: return t.colorAv8
        }
    }

    // utils/colors.ts stringToGradient: linear-gradient(135deg, base, color-mix(in srgb, base 72%, #000)). On a
    // square, 135deg runs exactly corner to corner, which a QML Gradient (vertical / horizontal only) can't do.
    Shape {
        id: letterBg
        anchors.fill: parent
        visible: av.source === ""
        preferredRendererType: Shape.CurveRenderer
        readonly property color base: av.avColor(av.colorIndex)
        ShapePath {
            strokeWidth: 0
            strokeColor: "transparent"
            fillGradient: LinearGradient {
                x1: 0; y1: 0; x2: av.size; y2: av.size
                GradientStop { position: 0; color: letterBg.base }
                GradientStop { position: 1; color: Qt.rgba(letterBg.base.r * 0.72, letterBg.base.g * 0.72, letterBg.base.b * 0.72, 1) }
            }
            PathRectangle { width: av.size; height: av.size; radius: av.size * Theme.t.radiusAvatarFrac }
        }
        Text {
            anchors.centerIn: parent
            text: av.initial
            color: Theme.t.colorAvFg
            font.family: Theme.t.fontSans
            font.pixelSize: Math.round(av.size * 0.42)
            font.weight: 600
        }
    }
    RoundedImage {
        anchors.fill: parent
        visible: av.source !== ""
        source: av.source
        radius: av.size * Theme.t.radiusAvatarFrac
    }
}
