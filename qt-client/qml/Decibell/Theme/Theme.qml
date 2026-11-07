pragma Singleton
import QtQuick

// The active theme, the counterpart of <html data-theme> in the renderer. Every token is generated from
// globals.css (Palettes.qml / ThemePalette.qml, by tools/gen-theme): components read `Theme.t.colorBgMid`
// (the CSS name camel-cased), or `Theme.chrome.*` on chrome-scope surfaces (title bar, server bar), as
// `.chrome-scope` does in globals.css. A switch is one assignment to `name`, like flipping data-theme.
QtObject {
    property string name: "nocturne"
    readonly property ThemePalette t: Palettes.palette(name)
    readonly property ThemePalette chrome: Palettes.chrome(name)
    readonly property var scopes: Palettes.scopes(name)
    readonly property list<string> names: Palettes.names

    // Chromium applies `font-optical-sizing: auto`: Inter's opsz axis follows the font size (clamped to the
    // font's 14–32 range). Qt doesn't, so text set in Inter passes its size through here.
    function opsz(px: real): real { return Math.max(14, Math.min(32, px)) }
}
