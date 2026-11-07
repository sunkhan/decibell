import QtQuick
import QtQuick.Shapes

// An inline SVG icon as the renderer draws it: path data in the icon's own viewBox, stroked (or filled) in
// `color`. Scaling the shape scales the stroke with it, exactly as SVG does, so `stroke` is in viewBox units
// (the components' strokeWidth). GPU curve rendering keeps the edges anti-aliased at any size.
Item {
    id: icon
    property string path
    property real viewBox: 24
    property real stroke: 2
    property color color: "white"
    property bool filled: false
    property bool roundJoin: true
    implicitWidth: 16
    implicitHeight: 16

    Shape {
        width: icon.viewBox
        height: icon.viewBox
        preferredRendererType: Shape.CurveRenderer
        transform: Scale { xScale: icon.width / icon.viewBox; yScale: icon.height / icon.viewBox }
        ShapePath {
            strokeColor: icon.filled ? "transparent" : icon.color
            strokeWidth: icon.filled ? 0 : icon.stroke
            fillColor: icon.filled ? icon.color : "transparent"
            capStyle: ShapePath.RoundCap
            joinStyle: icon.roundJoin ? ShapePath.RoundJoin : ShapePath.MiterJoin
            PathSvg { path: icon.path }
        }
    }
}
