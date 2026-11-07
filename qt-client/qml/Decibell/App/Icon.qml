import QtQuick
import Decibell.Ui

// An inline SVG icon as the renderer draws it: path data in the icon's own viewBox, stroked (or filled) in
// `color`, rasterised at the drawn size (src/ui/svgimage.h). `stroke` is in viewBox units (the components'
// strokeWidth), so it scales with the icon as SVG's does.
SvgImage {
    id: icon
    property string path
    property real viewBox: 24
    property real stroke: 2
    property color color: "white"
    property bool filled: false
    property bool roundJoin: true
    implicitWidth: 16
    implicitHeight: 16

    // SVG has no #aarrggbb: rgb() plus an opacity attribute.
    function rgb(c: color): string {
        return "rgb(" + Math.round(c.r * 255) + "," + Math.round(c.g * 255) + "," + Math.round(c.b * 255) + ")"
    }
    svg: "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 " + viewBox + " " + viewBox + "'><path d='" + path + "' "
         + (filled ? "fill='" + rgb(color) + "' fill-opacity='" + color.a + "' stroke='none'"
                   : "fill='none' stroke='" + rgb(color) + "' stroke-opacity='" + color.a + "' stroke-width='" + stroke
                     + "' stroke-linecap='round' stroke-linejoin='" + (roundJoin ? "round" : "miter") + "'")
         + "/></svg>"
}
