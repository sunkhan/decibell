import QtQuick
import QtQuick.Effects
import Decibell.Theme

// One theme's anatomy in miniature: chrome (title bar, server bar), sidebar, content with a message, the
// composer and a floating popover. Reads one palette directly, so all seven render at once.
Item {
    id: card
    required property string themeName
    readonly property ThemePalette p: Palettes.palette(themeName)
    readonly property ThemePalette c: Palettes.chrome(themeName)
    width: 296
    height: 268

    // Inter follows its optical size in Chromium (font-optical-sizing: auto); Mona Sans / JetBrains ignore opsz.
    function axes(family: string, px: real, wdth: real): var {
        let a = {}
        if (family === "Inter") a["opsz"] = Theme.opsz(px)
        if (family === "Mona Sans" && wdth > 0) a["wdth"] = wdth
        return a
    }

    Text {
        id: label
        text: card.themeName
        color: "#9aa0aa"
        font.family: "JetBrains Mono"
        font.pixelSize: 11
    }

    Rectangle {
        id: frame
        anchors.top: label.bottom
        anchors.topMargin: 4
        width: parent.width
        height: parent.height - label.height - 4
        radius: card.p.radiusLg
        color: card.p.colorBgMid
        clip: true

        // Title bar (chrome scope)
        Rectangle {
            id: titlebar
            width: parent.width
            height: 22
            color: card.c.colorBgTitlebar
            Text {
                anchors.verticalCenter: parent.verticalCenter
                x: 8
                text: "DECIBELL"
                color: card.c.colorTextMuted
                font.family: card.p.fontTile
                font.pixelSize: 9
                font.weight: 700
                font.letterSpacing: card.p.trackingWordmarkEm * 9
                font.variableAxes: card.axes(card.p.fontTile, 9, card.p.tWordmarkStretchWdth)
            }
        }

        // Server bar (chrome scope): a horizontal strip of tabs under the title bar, like browser tabs
        // (the renderer's tiles are 128 x 44; here at miniature scale).
        Rectangle {
            id: serverbar
            anchors.top: titlebar.bottom
            width: parent.width
            height: 26
            color: card.c.colorBgPrimary
            Rectangle {
                x: 8; y: 3; width: 58; height: 20
                radius: card.p.radiusMd
                color: card.c.colorAccent
                Text {
                    anchors.centerIn: parent
                    text: "DailyGaming"
                    color: card.c.colorOnAccent
                    font.family: card.p.fontTile
                    font.pixelSize: 9
                    font.weight: 600
                    font.variableAxes: card.axes(card.p.fontTile, 9, card.p.tTileStretchWdth)
                }
            }
            Rectangle {
                x: 70; y: 3; width: 58; height: 20
                radius: card.p.radiusMd
                color: card.c.colorSurfaceActive
            }
        }

        // Channel sidebar
        Rectangle {
            id: sidebar
            anchors.top: serverbar.bottom
            anchors.left: parent.left
            width: 92
            height: parent.height - titlebar.height - serverbar.height
            color: card.c.colorBgSecondary
            Column {
                x: 6; y: 8; spacing: 2
                width: parent.width - 12
                Text {
                    text: "TEXT CHANNELS"
                    color: card.c.colorTextMuted
                    font.family: card.p.fontSection
                    font.pixelSize: card.p.textSection * 0.8
                    font.weight: 600
                    font.letterSpacing: card.p.trackingSectionEm * card.p.textSection * 0.8
                    font.variableAxes: card.axes(card.p.fontSection, card.p.textSection * 0.8, 0)
                    bottomPadding: 3
                }
                Repeater {
                    model: ["general", "sessizler", "notes"]
                    Rectangle {
                        required property string modelData
                        required property int index
                        width: parent.width
                        height: 20
                        radius: card.p.radiusSm
                        color: index === 0 ? card.c.colorAccentSoft : "transparent"
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            x: 5
                            text: "# " + parent.modelData
                            color: parent.index === 0 ? card.c.colorTextBright : card.c.colorTextSecondary
                            font.family: card.p.fontChannel
                            font.pixelSize: card.p.textChannel * 0.85
                            font.weight: parent.index === 0 ? 600 : 400
                        }
                    }
                }
            }
        }

        // Content: one message
        Item {
            id: content
            anchors.top: serverbar.bottom
            anchors.left: sidebar.right
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            Rectangle {
                id: avatar
                x: 10; y: 12; width: 28; height: 28
                radius: width * card.p.radiusAvatarFrac
                color: card.p.colorAv1
                Text {
                    anchors.centerIn: parent
                    text: "S"
                    color: card.p.colorAvFg
                    font.family: card.p.fontSans
                    font.pixelSize: 13
                    font.weight: 600
                }
            }
            Row {
                id: header
                anchors.left: avatar.right
                anchors.leftMargin: 8
                y: 11
                spacing: 6
                Text {
                    text: "sunkhan"
                    color: card.p.colorName1
                    font.family: card.p.fontSans
                    font.pixelSize: card.p.textSender * 0.9
                    font.weight: card.p.fontWeightEmphasis
                }
                Text {
                    anchors.baseline: parent.children[0].baseline
                    text: "14:32"
                    color: card.p.colorTextMuted
                    font.family: card.p.fontMeta
                    font.pixelSize: card.p.textMeta * 0.9
                }
            }
            Text {
                id: body
                anchors.left: header.left
                anchors.right: parent.right
                anchors.rightMargin: 8
                anchors.top: header.bottom
                anchors.topMargin: 2
                text: "Stream tonight at <b>9</b>? The new build is up."
                textFormat: Text.StyledText
                wrapMode: Text.Wrap
                color: card.p.colorTextPrimary
                font.family: card.p.fontSans
                font.pixelSize: card.p.textBody * 0.9
                // CSS line-height is a multiple of the font size; QML's default multiplies the font's own leading.
                lineHeightMode: Text.FixedHeight
                lineHeight: font.pixelSize * card.p.leadingBody
            }
            Rectangle {
                id: chip
                anchors.left: header.left
                anchors.top: body.bottom
                anchors.topMargin: 4
                width: chipText.implicitWidth + 8
                height: chipText.implicitHeight + 2
                radius: card.p.radiusSm
                color: card.p.colorBgLight
                Text {
                    id: chipText
                    anchors.centerIn: parent
                    text: "cargo test --lib"
                    color: card.p.colorTextPrimary
                    font.family: card.p.fontMono
                    font.pixelSize: card.p.textBody * 0.8
                }
            }

            // Floating popover with the theme's float elevation (first shadow layer; the second is a ring).
            RectangularShadow {
                anchors.fill: popover
                readonly property var floatLayer: card.p.shadowFloat[0]
                offset.x: floatLayer.x
                offset.y: floatLayer.y
                blur: floatLayer.blur
                spread: floatLayer.spread
                color: floatLayer.color
                radius: popover.radius
            }
            Rectangle {
                id: popover
                x: parent.width - width - 10
                y: 92
                width: 92; height: 40
                radius: card.p.radiusLg
                color: card.p.colorBgLight
                border.width: card.p.shadowFloat.length > 1 ? card.p.shadowFloat[1].spread : 0
                border.color: card.p.shadowFloat.length > 1 ? card.p.shadowFloat[1].color : "transparent"
                Text {
                    anchors.centerIn: parent
                    text: "Popover"
                    color: card.p.colorTextSecondary
                    font.family: card.p.fontSans
                    font.pixelSize: 11
                }
            }

            // Composer
            Rectangle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                anchors.margins: 8
                height: 30
                radius: card.p.radiusLg
                color: card.p.colorBgLight
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    x: 10
                    text: "Message #general"
                    color: card.p.colorTextMuted
                    font.family: card.p.fontSans
                    font.pixelSize: card.p.textBody * 0.85
                }
            }
        }
    }
}
