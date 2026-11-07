pragma Singleton
import QtQuick

// SVG path data of the shell's icons, converted from the renderer components' inline SVGs (24-unit viewBox
// unless noted). Elements become path commands: line → M L, rect rx → arcs, circle → two arcs, polyline → M L.
QtObject {
    // ChannelHeader.tsx: members panel toggle (rect rx 2 + divider)
    readonly property string panelRight: "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z M9 3v18"
    // SearchBox: magnifier
    readonly property string search: "M3 11a8 8 0 1 0 16 0a8 8 0 1 0 -16 0 M21 21l-4.35-4.35"
    // ServerChannelsSidebar.tsx: header / category chevron
    readonly property string chevronDown: "M6 9L12 15L18 9"
    // ServerChannelsSidebar.tsx: voice channel speaker
    readonly property string speaker: "M11 5L6 9H2v6h4l5 4z M15.54 8.46a5 5 0 0 1 0 7.07 M19.07 4.93a10 10 0 0 1 0 14.14"
    // UserPanel.tsx: mic, headphones, settings
    readonly property string mic: "M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z M19 10v2a7 7 0 0 1-14 0v-2 M12 19v4 M8 23h8"
    readonly property string headphones: "M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3"
    readonly property string settings: "M9 12a3 3 0 1 0 6 0a3 3 0 1 0 -6 0 M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"
    // ServerBar.tsx: home (filled), add (stroke 2.5)
    readonly property string home: "M12 3l-9.5 8.5c-.3.27-.15.5.25.5H5v8a1 1 0 0 0 1 1h4v-5.5a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1V21h4a1 1 0 0 0 1-1v-8h2.25c.4 0 .55-.23.25-.5L12 3z"
    readonly property string plus: "M12 5v14 M5 12h14"
    // RichComposer.tsx: code panel; attach / emoji / send (lucide shapes)
    readonly property string code: "M16 18L22 12L16 6 M8 6L2 12L8 18"
    readonly property string plusCircle: "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0 M8 12h8 M12 8v8"
    readonly property string smile: "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0 M8 14s1.5 2 4 2 4-2 4-2 M9 9h.01 M15 9h.01"
    readonly property string send: "M3.71 3.05a.5.5 0 0 0-.68.62l2.84 7.63a2 2 0 0 1 0 1.4l-2.84 7.63a.5.5 0 0 0 .68.62l18-8.5a.5.5 0 0 0 0-.9z M6 12h16"
    // Titlebar.tsx: window controls (12-unit viewBox, stroke 1) and the transfers button
    readonly property string winMin: "M2 6H10"
    readonly property string winMax: "M2 2H10V10H2Z"
    readonly property string winClose: "M2.5 2.5L9.5 9.5 M9.5 2.5L2.5 9.5"
    readonly property string download: "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4 M7 10l5 5 5-5 M12 15V3"
    // MessageBubble.tsx LockGlyph (stroke 2.2)
    readonly property string lock: "M5 11h14a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2z M7 11V7a5 5 0 0 1 10 0v4"
}
