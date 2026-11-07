import QtQuick
import QtQuick.Shapes
import Decibell.Theme

// components/StatusDot.tsx: one 20-unit glyph drawn as a single shape (a disc; away bites the top-left out,
// DND lays a bar, offline / invisible a hole), inside a ring of the surface colour. 13 px on avatars.
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

    Rectangle { anchors.fill: parent; radius: width / 2; color: dot.ring }
    Shape {
        x: 2; y: 2
        width: 20; height: 20
        preferredRendererType: Shape.CurveRenderer
        transform: Scale { xScale: (dot.width - 4) / 20; yScale: (dot.height - 4) / 20 }
        ShapePath {
            strokeWidth: 0
            strokeColor: "transparent"
            fillColor: dot.fill
            fillRule: ShapePath.OddEvenFill
            PathSvg {
                path: "M0 10a10 10 0 1 0 20 0a10 10 0 1 0 -20 0" +
                      (dot.status === "offline" || dot.status === "invisible" ? " M6 10a4 4 0 1 0 8 0a4 4 0 1 0 -8 0" : "")
            }
        }
        ShapePath {
            strokeWidth: 0
            strokeColor: "transparent"
            fillColor: dot.status === "away" || dot.status === "dnd" ? dot.ring : "transparent"
            PathSvg {
                path: dot.status === "away" ? "M-3 4a7 7 0 1 0 14 0a7 7 0 1 0 -14 0"
                    : dot.status === "dnd" ? "M6 7.5h8a2.5 2.5 0 0 1 0 5h-8a2.5 2.5 0 0 1 0-5z" : "M0 0"
            }
        }
    }
}
