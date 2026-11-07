import QtQuick
import Decibell.Theme
import Decibell.Core
import Decibell.Ui

// Spike screen (b): the app frame with a busy channel open, laid out from the live Electron client's measured
// boxes (MainLayout: title bar 32, server bar 64 with 44 px tiles, the workspace inset 8 px as one rounded,
// bordered panel; sidebar 319, chat header 48, members 260).
Item {
    id: screen
    required property string fixtureDir
    property Window window
    property real benchSpeed: 0
    property string ablate
    // --page-size: history pages in N messages at a time (0: all 10k at once, for the scroll benchmark).
    property int pageSize: 0

    ChatModel { id: chat; objectName: "chatModel"; pageSize: screen.pageSize; Component.onCompleted: load(screen.fixtureDir) }
    RosterModel { id: roster; Component.onCompleted: load(screen.fixtureDir) }
    ChannelModel { id: channels; Component.onCompleted: load(screen.fixtureDir) }
    // Chromium's wheel feel (src/ui/smoothwheel.h) on every scrolling list.
    SmoothWheel { id: listWheel; target: list }
    SmoothWheel { target: channelList }
    SmoothWheel { target: memberList }
    // Text selection across messages, link clicks (src/ui/chatselection.h). Links go to the OS browser, http(s) only,
    // as lib/openExternal.ts does.
    property alias chatSelection: chatSelection
    ChatSelection {
        id: chatSelection
        objectName: "chatSelection"
        target: list
        model: chat
        onLinkActivated: (url) => Qt.openUrlExternally(url)
    }
    Shortcut {
        sequences: [StandardKey.Copy]
        enabled: chatSelection.hasSelection
        onActivated: chatSelection.copy()
    }

    // ── title bar (chrome scope) ──────────────────────────────────────────────────────────────────────
    Rectangle {
        id: titlebar
        width: parent.width
        height: 32
        color: Theme.chrome.colorBgTitlebar
        DragHandler {
            target: null
            onActiveChanged: if (active && screen.window) screen.window.startSystemMove()
        }
        Text {
            anchors.centerIn: parent
            text: chat.serverName
            color: Theme.chrome.colorTextSecondary
            font.family: Theme.t.fontChannel
            font.pointSize: 0.75 * (12)
            font.weight: 500
        }
        Row {
            anchors.right: parent.right
            height: 31
            Repeater {
                model: [
                    { path: Icons.download, box: 24, size: 16, stroke: 2 },
                    { path: Icons.winMin, box: 12, size: 14, stroke: 1 },
                    { path: Icons.winMax, box: 12, size: 14, stroke: 1 },
                    { path: Icons.winClose, box: 12, size: 14, stroke: 1.1 }
                ]
                Item {
                    required property var modelData
                    required property int index
                    width: 44
                    height: 31
                    Rectangle {
                        anchors.fill: parent
                        color: parent.index === 3 ? "#e81123" : Theme.chrome.colorSurfaceHover
                        visible: hover.hovered
                    }
                    Icon {
                        anchors.centerIn: parent
                        width: parent.modelData.size
                        height: parent.modelData.size
                        viewBox: parent.modelData.box
                        stroke: parent.modelData.stroke
                        path: parent.modelData.path
                        color: Theme.chrome.colorTextSecondary
                        roundJoin: parent.index === 0
                    }
                    HoverHandler { id: hover }
                    TapHandler {
                        onTapped: {
                            if (!screen.window) return
                            if (parent.index === 1) screen.window.showMinimized()
                            else if (parent.index === 2) screen.window.visibility === Window.Maximized ? screen.window.showNormal() : screen.window.showMaximized()
                            else if (parent.index === 3) screen.window.close()
                        }
                    }
                }
            }
        }
    }

    // ── server bar: a horizontal strip of tabs (chrome scope) ──────────────────────────────────────────
    Rectangle {
        id: serverbar
        anchors.top: titlebar.bottom
        width: parent.width
        height: 64
        color: Theme.chrome.colorBgPrimary
        Rectangle {
            x: 8; y: 10; width: 44; height: 44
            radius: Theme.t.radiusSm
            color: Theme.chrome.colorSurfaceActive
            Icon { anchors.centerIn: parent; width: 20; height: 20; filled: true; path: Icons.home; color: Theme.chrome.colorTextSecondary }
        }
        Item {
            x: 69; y: 10; width: 128; height: 44
            RoundedImage {
                anchors.fill: parent
                radius: Theme.t.radiusMd
                source: "file:///" + screen.fixtureDir + "/img/photo-29-3000x2000.jpg"
            }
            HoverHandler { id: tileHover }
            Rectangle {
                anchors.fill: parent
                radius: Theme.t.radiusMd
                color: "#59000000"
                visible: tileHover.hovered
            }
            Text {
                visible: tileHover.hovered
                anchors.centerIn: parent
                width: parent.width - 24
                horizontalAlignment: Text.AlignHCenter
                elide: Text.ElideRight
                text: chat.serverName
                color: "white"
                font.family: Theme.chrome.fontTile
                font.pointSize: 0.75 * (13)
                font.weight: 600
                font.variableAxes: ({ "wdth": Theme.chrome.tTileStretchWdth })
            }
        }
        Rectangle {
            x: 222; y: 10; width: 44; height: 44
            radius: Theme.t.radiusMd
            color: "transparent"
            border.width: 1
            border.color: Theme.chrome.colorTextMuted
            Icon { anchors.centerIn: parent; width: 18; height: 18; stroke: 2.5; path: Icons.plus; color: Theme.chrome.colorTextMuted }
        }
    }

    // ── workspace: inset 8 px in the chrome, one rounded bordered panel ───────────────────────────────
    Rectangle {
        anchors.top: serverbar.bottom
        anchors.bottom: parent.bottom
        width: parent.width
        color: Theme.chrome.colorBgDarkest

        Rectangle {
            id: panel
            anchors.fill: parent
            anchors.leftMargin: 8
            anchors.rightMargin: 8
            anchors.bottomMargin: 8
            radius: Theme.t.radiusLg
            color: Theme.t.colorBgMid
            border.width: 1
            border.color: Theme.t.colorBorder
            clip: true

            // Sidebar (chrome scope)
            Rectangle {
                id: sidebar
                x: 1; y: 1
                width: 319
                height: parent.height - 2
                color: Theme.chrome.colorBgSecondary

                Item {
                    id: sideHeader
                    width: parent.width
                    height: 48
                    Row {
                        x: 16
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 6
                        Text {
                            text: chat.serverName
                            color: Theme.chrome.colorTextBright
                            font.family: Theme.t.fontDisplay
                            font.pointSize: 0.75 * (Theme.t.textTitle)
                            font.weight: Theme.t.fontWeightEmphasis
                            font.letterSpacing: Theme.t.trackingTitleEm * Theme.t.textTitle
                            font.variableAxes: ({ "opsz": Theme.opsz(Theme.t.textTitle) })
                        }
                        Icon { anchors.verticalCenter: parent.verticalCenter; width: 12; height: 12; stroke: 2.5; path: Icons.chevronDown; color: Theme.chrome.colorTextSecondary }
                    }
                    Rectangle {
                        anchors.right: parent.right
                        anchors.rightMargin: 8
                        anchors.verticalCenter: parent.verticalCenter
                        width: publicText.implicitWidth + 12
                        height: 15
                        radius: Theme.t.radiusSm
                        color: Qt.rgba(Theme.t.colorSuccess.r, Theme.t.colorSuccess.g, Theme.t.colorSuccess.b, 0.15)
                        Text {
                            id: publicText
                            anchors.centerIn: parent
                            text: "PUBLIC"
                            color: Theme.t.colorSuccess
                            font.family: Theme.t.fontMeta
                            font.pointSize: 0.75 * (11)
                            font.weight: 500
                            font.letterSpacing: 0.6
                        }
                    }
                }

                ListView {
                    id: channelList
                    anchors.top: sideHeader.bottom
                    anchors.bottom: parent.bottom
                    anchors.bottomMargin: 76
                    x: 8
                    width: parent.width - 16
                    topMargin: 10
                    clip: true
                    model: channels
                    reuseItems: true
                    boundsBehavior: Flickable.StopAtBounds
                    delegate: Item {
                        id: chRow
                        required property int index
                        required property string name
                        required property string kind
                        required property bool encrypted
                        width: ListView.view.width
                        height: kind === "category" ? 36 : 35
                        readonly property bool active: index === 0
                        Rectangle {
                            visible: chRow.kind !== "category"
                            anchors.fill: parent
                            radius: Theme.t.radiusSm
                            color: chRow.active ? Theme.chrome.colorAccentSoft : rowHover.hovered ? Theme.chrome.colorSurfaceHover : "transparent"
                        }
                        HoverHandler { id: rowHover }
                        // `.channel-icon`: a 1em slot at 1.45 × the channel size, `#` as a glyph, the speaker drawn.
                        Item {
                            visible: chRow.kind !== "category"
                            x: 10
                            anchors.verticalCenter: parent.verticalCenter
                            width: Theme.t.textChannel * 1.45
                            height: width
                            Text {
                                anchors.centerIn: parent
                                visible: chRow.kind === "text"
                                text: "#"
                                color: chRow.active ? Theme.chrome.colorAccentBright : Theme.chrome.colorTextMuted
                                font.family: Theme.t.fontChannel
                                font.pointSize: 0.75 * (parent.width)
                                font.weight: 500
                            }
                            Icon {
                                anchors.centerIn: parent
                                visible: chRow.kind === "voice"
                                width: parent.width * 0.82
                                height: width
                                path: Icons.speaker
                                color: Theme.chrome.colorTextMuted
                            }
                        }
                        Row {
                            visible: chRow.kind !== "category"
                            x: 10 + Theme.t.textChannel * 1.45 + 10
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 6
                            Text {
                                text: chRow.name
                                color: chRow.active ? Theme.chrome.colorTextBright : Theme.chrome.colorTextSecondary
                                font.family: Theme.t.fontChannel
                                font.pointSize: 0.75 * (Theme.t.textChannel)
                                font.weight: chRow.active ? 600 : 400
                                lineHeightMode: Text.FixedHeight
                                lineHeight: Theme.t.textChannel * 1.5
                            }
                            Icon {
                                visible: chRow.encrypted
                                anchors.verticalCenter: parent.verticalCenter
                                width: 11; height: 11; stroke: 2.2
                                path: Icons.lock
                                color: Theme.chrome.colorTextMuted
                            }
                        }
                        Row {
                            visible: chRow.kind === "category"
                            x: 4
                            anchors.bottom: parent.bottom
                            anchors.bottomMargin: 6
                            spacing: 4
                            Icon { anchors.verticalCenter: parent.verticalCenter; width: 10; height: 10; stroke: 2.5; path: Icons.chevronDown; color: Theme.chrome.colorTextMuted }
                            Text {
                                text: chRow.name.toUpperCase()
                                color: Theme.chrome.colorTextMuted
                                font.family: Theme.t.fontSection
                                font.pointSize: 0.75 * (Theme.t.textSection)
                                font.weight: 600
                                font.letterSpacing: Theme.t.trackingSectionEm * Theme.t.textSection
                                font.variableAxes: ({ "opsz": Theme.opsz(Theme.t.textSection) })
                            }
                        }
                    }
                }

                // Floating user panel (bottom-2 left-2 right-2)
                Rectangle {
                    x: 8
                    width: parent.width - 16
                    height: 60
                    anchors.bottom: parent.bottom
                    anchors.bottomMargin: 8
                    radius: Theme.t.radiusLg
                    color: Theme.chrome.colorBgLight
                    border.width: 1
                    border.color: Theme.chrome.colorBorder
                    Avatar { x: 13; anchors.verticalCenter: parent.verticalCenter; size: 36; initial: "S"; colorIndex: 7; source: "file:///" + screen.fixtureDir + "/img/avatar-64.jpg" }
                    Column {
                        x: 57
                        anchors.verticalCenter: parent.verticalCenter
                        Text {
                            text: "sunkhan"
                            color: Theme.chrome.colorTextBright
                            font.family: Theme.t.fontChannel
                            font.pointSize: 0.75 * (Theme.t.textMember)
                            font.weight: Theme.t.fontWeightEmphasis
                        }
                        Text {
                            text: "Online"
                            color: Theme.chrome.colorTextMuted
                            font.family: Theme.t.fontMeta
                            font.pointSize: 0.75 * (Theme.t.textMicro)
                        }
                    }
                    Row {
                        anchors.right: parent.right
                        anchors.rightMargin: 13
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 4
                        Repeater {
                            model: [Icons.mic, Icons.headphones, Icons.settings]
                            Item {
                                required property string modelData
                                width: 32; height: 32
                                Icon { anchors.centerIn: parent; width: 16; height: 16; path: parent.modelData; color: Theme.chrome.colorTextSecondary }
                            }
                        }
                    }
                }
            }

            // Main area: header across chat + members, then the two side by side
            Item {
                id: main
                x: 320; y: 1
                width: parent.width - 321
                height: parent.height - 2

                Item {
                    id: header
                    width: parent.width
                    height: 48
                    Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: Theme.t.colorBorderDivider }
                    Row {
                        x: 16
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 6
                        Repeater {
                            model: ["#", "general"]
                            Text {
                                required property string modelData
                                required property int index
                                text: modelData
                                color: index === 0 ? Theme.t.colorTextMuted : Theme.t.colorTextBright
                                font.family: Theme.t.fontDisplay
                                font.pointSize: 0.75 * (Theme.t.textTitle)
                                font.weight: Theme.t.fontWeightEmphasis
                                font.letterSpacing: Theme.t.trackingTitleEm * Theme.t.textTitle
                                font.variableAxes: ({ "opsz": Theme.opsz(Theme.t.textTitle) })
                            }
                        }
                    }
                    Item {
                        x: search.x - 16 - 32; anchors.verticalCenter: parent.verticalCenter
                        width: 32; height: 32
                        Icon { anchors.centerIn: parent; width: 18; height: 18; path: Icons.panelRight; color: Theme.t.colorTextMuted }
                    }
                    Rectangle {
                        id: search
                        anchors.right: parent.right
                        anchors.rightMargin: 8
                        anchors.verticalCenter: parent.verticalCenter
                        width: 244; height: 32
                        radius: Theme.t.radiusMd
                        color: Theme.t.colorBgLighter
                        border.width: 1
                        border.color: Theme.t.colorBorder
                        Text {
                            x: 10
                            anchors.verticalCenter: parent.verticalCenter
                            text: "Search"
                            color: Theme.t.colorTextMuted
                            font.family: Theme.t.fontSans
                            font.pointSize: 0.75 * (14)
                        }
                        Icon { anchors.right: parent.right; anchors.rightMargin: 9; anchors.verticalCenter: parent.verticalCenter; width: 14; height: 14; path: Icons.search; color: Theme.t.colorTextMuted }
                    }
                }

                // Chat column
                Item {
                    id: chatCol
                    anchors.top: header.bottom
                    width: parent.width - 260
                    height: parent.height - header.height

                    ListView {
                        id: list
                        objectName: "messageList"
                        width: parent.width
                        height: parent.height - composer.height
                        clip: true
                        model: chat
                        reuseItems: true
                        cacheBuffer: 600
                        boundsBehavior: Flickable.StopAtBounds
                        // Newest at the bottom (the model is newest-first): the view opens at the present, and
                        // history pages append at the far end of the model, above the viewport, moving nothing.
                        verticalLayoutDirection: ListView.BottomToTop
                        // Keyboard scrolling once the list has focus (a press in it gives it focus), as a focused
                        // Chromium scroller does; the composer keeps its keys while it has focus.
                        Keys.onPressed: (event) => { if (listWheel.handleKey(event.key, event.modifiers)) event.accepted = true }
                        // Older history pages in as the top comes within 1000 px; it lands above what is on screen
                        // (the end of a newest-first model), so nothing visible moves.
                        onContentYChanged: if (chat.pageSize > 0 && chat.hasOlder && contentY - originY < 1000) Qt.callLater(chat.loadOlder)
                        // Benchmark driver: a constant scroll into history, the distance per frame from the frame's
                        // own time so the speed doesn't depend on the frame rate.
                        FrameAnimation {
                            running: screen.benchSpeed > 0
                            onTriggered: list.contentY -= screen.benchSpeed * frameTime
                        }
                        delegate: MessageRow {
                            selection: chatSelection
                            ablate: screen.ablate
                            viewWidth: list.width
                            viewHeight: list.height
                        }
                    }

                    Item {
                        id: composer
                        anchors.bottom: parent.bottom
                        width: parent.width
                        height: 72
                        Rectangle {
                            x: 12; y: 8
                            width: parent.width - 24
                            height: 56
                            radius: Theme.t.radiusLg
                            color: Theme.t.colorBgLight
                            border.width: 1
                            border.color: Theme.t.colorBorder
                            Rectangle {
                                x: 15; y: 11; width: 34; height: 34
                                radius: Theme.t.radiusMd
                                color: Theme.t.colorSurfaceHover
                                Icon { anchors.centerIn: parent; width: 16; height: 16; path: Icons.plusCircle; color: Theme.t.colorTextMuted }
                            }
                            Text {
                                x: 59
                                anchors.verticalCenter: parent.verticalCenter
                                text: "Message #general"
                                color: Theme.t.colorTextMuted
                                font.family: Theme.t.fontSans
                                font.pointSize: 0.75 * (14)
                            }
                            Row {
                                anchors.right: parent.right
                                anchors.rightMargin: 15
                                y: 11
                                spacing: 4
                                Item { width: 34; height: 34; Icon { anchors.centerIn: parent; width: 17; height: 17; path: Icons.code; color: Theme.t.colorTextMuted } }
                                Item { width: 34; height: 34; Icon { anchors.centerIn: parent; width: 18; height: 18; path: Icons.smile; color: Theme.t.colorTextMuted } }
                                Rectangle {
                                    width: 34; height: 34
                                    radius: Theme.t.radiusMd
                                    color: Theme.t.colorSurfaceActive
                                    Icon { anchors.centerIn: parent; width: 16; height: 16; path: Icons.send; color: Theme.t.colorTextMuted }
                                }
                            }
                        }
                    }
                }

                // Member list
                Rectangle {
                    anchors.top: header.bottom
                    anchors.right: parent.right
                    width: 260
                    height: parent.height - header.height
                    color: Theme.t.colorBgDark
                    Rectangle { width: 1; height: parent.height; color: Theme.t.colorBorder }
                    ListView {
                        id: memberList
                        x: 13
                        width: parent.width - 25
                        height: parent.height
                        topMargin: 4
                        clip: true
                        model: roster
                        reuseItems: true
                        boundsBehavior: Flickable.StopAtBounds
                        delegate: Item {
                            id: mRow
                            required property int index
                            required property bool isSection
                            required property string label
                            required property string name
                            required property int colorIndex
                            required property string avatar
                            required property string initial
                            required property string status
                            required property bool online
                            width: ListView.view.width
                            height: isSection ? (index === 0 ? 28 : 32) : 48
                            Text {
                                visible: mRow.isSection
                                x: 4
                                anchors.bottom: parent.bottom
                                anchors.bottomMargin: 4
                                text: mRow.label.toUpperCase()
                                color: Theme.t.colorTextMuted
                                font.family: Theme.t.fontSection
                                font.pointSize: 0.75 * (Theme.t.textSection)
                                font.weight: 500
                                font.letterSpacing: Theme.t.trackingSectionEm * Theme.t.textSection
                                font.variableAxes: ({ "opsz": Theme.opsz(Theme.t.textSection) })
                            }
                            Rectangle {
                                visible: !mRow.isSection && memberHover.hovered
                                anchors.fill: parent
                                radius: Theme.t.radiusSm
                                color: Theme.t.colorSurfaceHover
                            }
                            HoverHandler { id: memberHover; enabled: !mRow.isSection }
                            Item {
                                visible: !mRow.isSection
                                x: 8; y: 7
                                width: 34; height: 34
                                Avatar {
                                    size: 34
                                    opacity: mRow.online ? 1 : 0.62
                                    source: mRow.avatar
                                    initial: mRow.initial
                                    colorIndex: mRow.colorIndex
                                }
                                StatusDot {
                                    x: 22; y: 22
                                    status: mRow.status
                                    ring: Theme.t.colorBgDark
                                }
                            }
                            Text {
                                visible: !mRow.isSection
                                x: 52
                                width: parent.width - 60
                                anchors.verticalCenter: parent.verticalCenter
                                elide: Text.ElideRight
                                text: mRow.name
                                color: mRow.online ? Theme.t.colorTextPrimary : Theme.t.colorTextSecondary
                                font.family: Theme.t.fontChannel
                                font.pointSize: 0.75 * (Theme.t.textMember)
                            }
                        }
                    }
                }
            }
        }
    }
}
