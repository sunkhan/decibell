import QtQuick
import Decibell.Ui
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
    // square, 135deg runs exactly corner to corner (an SVG gradient from 0,0 to 1,1 of the box), which a QML
    // Gradient (vertical / horizontal only) can't do. Rasterised at the drawn size (src/ui/svgimage.h).
    function rgb(c: color): string {
        return "rgb(" + Math.round(c.r * 255) + "," + Math.round(c.g * 255) + "," + Math.round(c.b * 255) + ")"
    }
    SvgImage {
        id: letterBg
        anchors.fill: parent
        visible: av.source === ""
        readonly property color base: av.avColor(av.colorIndex)
        readonly property color dark: Qt.rgba(base.r * 0.72, base.g * 0.72, base.b * 0.72, 1)
        svg: visible ? "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 " + av.size + " " + av.size + "'>"
             + "<defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'>"
             + "<stop offset='0' stop-color='" + av.rgb(base) + "'/><stop offset='1' stop-color='" + av.rgb(dark) + "'/>"
             + "</linearGradient></defs><rect width='" + av.size + "' height='" + av.size + "' rx='"
             + av.size * Theme.t.radiusAvatarFrac + "' fill='url(#g)'/></svg>" : ""
        Text {
            anchors.centerIn: parent
            text: av.initial
            color: Theme.t.colorAvFg
            font.family: Theme.t.fontSans
            font.pointSize: 0.75 * (Math.round(av.size * 0.42))
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
