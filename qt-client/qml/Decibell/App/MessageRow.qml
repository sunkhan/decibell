import QtQuick
import Decibell.Theme
import Decibell.Ui

// One message row: an optional day divider, then either a full row (avatar, name, time) or a grouped one
// (38 px gutter). Spacing measured from the live Electron client (MessageBubble.tsx: first row
// `pr-2 pt-2.5 pb-0.5` + 12 px left, grouped `py-px pr-2`, `gap-3`; divider `pt-5 pb-1 pl-3 pr-2 gap-3`).
// Spike v1 body: StyledText from Rust; MessageText (QTextLayout + scene graph) replaces it.
Item {
    id: row
    required property int index
    required property string senderName
    required property int colorIndex
    required property string avatar
    required property string initial
    required property string timeLabel
    required property string dayLabel
    required property bool grouped
    required property string body
    required property string content
    required property string codeLang
    required property string codeText
    required property bool hasReply
    required property string replyName
    required property int replyColor
    required property string replySnippet
    required property int imageCount
    required property string image0
    required property int image0W
    required property int image0H
    required property string image1
    required property int image1W
    required property int image1H
    required property string image2
    required property int image2W
    required property int image2H

    property string ablate
    property real viewWidth: 683
    property real viewHeight: 574
    readonly property real contentX: 62     // 12 padding + 38 avatar / gutter + 12 gap
    readonly property real contentW: width - contentX - 8

    width: ListView.view ? ListView.view.width - 11 : 672
    height: divider.height + (grouped ? 2 : 12) + content.height

    function nameColor(i: int): color {
        const t = Theme.t
        switch (i) {
        case 1: return t.colorName1
        case 2: return t.colorName2
        case 3: return t.colorName3
        case 4: return t.colorName4
        case 5: return t.colorName5
        case 6: return t.colorName6
        case 7: return t.colorName7
        default: return t.colorName8
        }
    }
    // attachmentSizing.ts maxImageWidth / maxImageHeight (reserve: the download button beside media).
    function maxW(): real { return Math.max(120, Math.min(550, 18 * Math.sqrt(viewWidth), viewWidth - 80 - 38)) }
    function maxH(): real { return Math.max(120, Math.min(350, 16 * Math.sqrt(viewHeight), viewHeight - 60)) }

    // ── day divider ─────────────────────────────────────────────────────────────────────────────────
    Item {
        id: divider
        width: parent.width
        height: row.dayLabel !== "" ? 41 : 0
        visible: row.dayLabel !== "" && !row.ablate.includes("divider")
        Text {
            id: dayText
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.horizontalCenterOffset: 2
            y: 20
            text: row.dayLabel
            color: Theme.t.colorTextMuted
            font.family: Theme.t.fontMeta
            font.pointSize: 0.75 * (Theme.t.textMeta)
            font.weight: 500
            font.features: { "tnum": 1 }
            lineHeightMode: Text.FixedHeight
            lineHeight: Theme.t.textMeta * 1.5
        }
        Rectangle { x: 12; width: dayText.x - 12 - 12; height: 1; y: dayText.y + dayText.height / 2; color: Theme.t.colorBorderDivider }
        Rectangle { x: dayText.x + dayText.width + 12; width: parent.width - 8 - x; height: 1; y: dayText.y + dayText.height / 2; color: Theme.t.colorBorderDivider }
    }

    // ── avatar (first row only) ───────────────────────────────────────────────────────────────────────
    Avatar {
        visible: !row.grouped && !row.ablate.includes("avatar")
        x: 12
        y: divider.height + 10 + (row.hasReply ? 22 : 0)
        size: 38
        source: row.avatar
        initial: row.initial
        colorIndex: row.colorIndex
    }

    Column {
        id: content
        x: row.contentX
        y: divider.height + (row.grouped ? 1 : 10)
        width: row.contentW
        spacing: 0

        // Reply preview (spine + name + first line), above the header.
        Row {
            visible: row.hasReply && !row.grouped
            height: visible ? 22 : 0
            spacing: 6
            Text {
                text: row.replyName
                color: row.nameColor(row.replyColor)
                font.family: Theme.t.fontChannel
                font.pointSize: 0.75 * (Theme.t.textReply)
                font.weight: 600
            }
            Text {
                width: Math.min(implicitWidth, row.contentW - 140)
                text: row.replySnippet
                elide: Text.ElideRight
                color: Theme.t.colorTextSecondary
                font.family: Theme.t.fontSans
                font.pointSize: 0.75 * (Theme.t.textReply)
            }
        }

        // Header: name + time on one baseline (`flex items-baseline gap-2`).
        Item {
            visible: !row.grouped && !row.ablate.includes("header")
            width: parent.width
            height: !row.grouped ? 22 : 0
            Text {
                id: name
                text: row.senderName
                color: row.nameColor(row.colorIndex)
                font.family: Theme.t.fontChannel
                font.pointSize: 0.75 * (Theme.t.textSender)
                font.weight: Theme.t.fontWeightEmphasis
                lineHeightMode: Text.FixedHeight
                lineHeight: Theme.t.textSender * 1.5
            }
            Text {
                anchors.baseline: name.baseline
                x: name.width + 8
                text: row.timeLabel
                color: Theme.t.colorTextMuted
                font.family: Theme.t.fontMeta
                font.pointSize: 0.75 * (Theme.t.textMeta)
                font.features: { "tnum": 1 }
            }
        }

        // Body (`mt-0.5 whitespace-pre-wrap break-all text-body leading-body`): MessageText draws the markdown
        // subset, inline code chips, fenced blocks and twemoji (src/ui/messagetext.h).
        Item { width: 1; height: !row.grouped && row.content !== "" ? 2 : 0 }
        MessageText {
            visible: row.content !== "" && !row.ablate.includes("body")
            width: parent.width
            height: visible ? implicitHeight : 0
            text: row.content
            fontFamily: Theme.t.fontSans
            monoFamily: Theme.t.fontMono
            pixelSize: Theme.t.textBody
            lineHeight: Theme.t.textBody * Theme.t.leadingBody
            color: Theme.t.colorTextPrimary
            linkColor: Theme.t.colorAccentBright
            codeBackground: Theme.t.colorBgDarkest
            codeRadius: Theme.t.radiusSm
            blockRadius: Theme.t.radiusMd
        }

        // Image attachments: one sized by the renderer's caps, several in a 180 px-row grid.
        Item {
            visible: row.imageCount > 0 && !row.ablate.includes("images")
            width: parent.width
            height: visible ? imgs.height + 4 : 0
            readonly property real scale0: Math.min(1, row.maxW() / Math.max(1, row.image0W), row.maxH() / Math.max(1, row.image0H))
            Item {
                id: imgs
                y: 4
                width: row.imageCount === 1 ? Math.round(row.image0W * parent.scale0) + 2 : Math.min(540, row.maxW())
                height: row.imageCount === 1 ? Math.round(row.image0H * parent.scale0) + 2 : 180
                Rectangle {
                    anchors.fill: parent
                    radius: Theme.t.radiusMd
                    color: "transparent"
                    border.width: 1
                    border.color: Theme.t.colorBorderDivider
                }
                Repeater {
                    model: row.imageCount
                    RoundedImage {
                        required property int index
                        readonly property real cellW: (imgs.width - 2 - 4 * (row.imageCount - 1)) / row.imageCount
                        x: 1 + index * (cellW + 4)
                        y: 1
                        width: row.imageCount === 1 ? imgs.width - 2 : cellW
                        height: imgs.height - 2
                        radius: Theme.t.radiusMd - 1
                        cover: row.imageCount > 1
                        source: index === 0 ? row.image0 : index === 1 ? row.image1 : row.image2
                    }
                }
            }
        }
    }
}
