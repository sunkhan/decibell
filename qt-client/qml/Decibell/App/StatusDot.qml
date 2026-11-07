import QtQuick
import Decibell.Theme
import Decibell.Ui

// components/StatusDot.tsx: one 20-unit glyph drawn as a single shape (a disc; away bites the top-left out,
// DND lays a bar, offline / invisible a hole), inside a ring of the surface colour. 13 px on avatars.
// Rasterised at the drawn size (src/ui/svgimage.h).
Item {
    id: dot
    property string status: "offline"
    property color ring: Theme.t.colorBgDark
    width: 13
    height: 13
    readonly property color fill: status === "online" ? Theme.t.colorSuccess
                                : status === "away" ? Theme.t.colorWarning
                                : status === "dnd" ? Theme.t.colorError
                                : Theme.t.colorTextMuted

    function rgb(c: color): string {
        return "rgb(" + Math.round(c.r * 255) + "," + Math.round(c.g * 255) + "," + Math.round(c.b * 255) + ")"
    }

    Rectangle { anchors.fill: parent; radius: width / 2; color: dot.ring }
    SvgImage {
        x: 2; y: 2
        width: dot.width - 4
        height: dot.height - 4
        readonly property string hole: dot.status === "offline" || dot.status === "invisible"
                                       ? " M6 10a4 4 0 1 0 8 0a4 4 0 1 0 -8 0" : ""
        readonly property string bite: dot.status === "away" ? "M-3 4a7 7 0 1 0 14 0a7 7 0 1 0 -14 0"
                                     : dot.status === "dnd" ? "M6 7.5h8a2.5 2.5 0 0 1 0 5h-8a2.5 2.5 0 0 1 0-5z" : ""
        svg: "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 20 20'>"
             + "<path fill-rule='evenodd' fill='" + dot.rgb(dot.fill) + "' d='M0 10a10 10 0 1 0 20 0a10 10 0 1 0 -20 0" + hole + "'/>"
             + (bite !== "" ? "<path fill='" + dot.rgb(dot.ring) + "' d='" + bite + "'/>" : "")
             + "</svg>"
    }
}
