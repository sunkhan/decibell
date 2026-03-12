import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: root
    width: 1280
    height: 720
    visible: true

    property alias chatView: chatView
    property alias mainStackLayout: mainStackLayout
    property alias primaryStackLayout: primaryStackLayout
    property alias voiceView: voiceView
    property alias channelsSidebar: channelsSidebar
    property alias serverListWheelArea: serverListWheelArea
    property alias serverListView: serverListView
    property alias publicServersGrid: publicServersGrid
    property alias friendsListView: friendsListView
    property alias friendInput: friendInput
    signal profileRequested(string username, real clickX, real clickY)

    Rectangle {
        id: windowBackground
        anchors.fill: parent
        color: "#0C0D0F"

        // Main Layout Divider (Left Sidebar vs. The Rest)
        RowLayout {
            anchors.fill: parent
            spacing: 0

            // 1. Left Sidebar (Home & Direct Messages)
            Rectangle {
                id: leftSidebar
                Layout.preferredWidth: 72
                Layout.fillHeight: true
                color: "#0C0D0F"

                ColumnLayout {
                    anchors.fill: parent
                    anchors.topMargin: 12
                    spacing: 8

                    Rectangle {
                        Layout.preferredWidth: 48
                        Layout.preferredHeight: 48
                        Layout.alignment: Qt.AlignHCenter
                        radius: 16
                        color: "#2CA3E8"

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                mainStackLayout.currentIndex = 0
                                root.activeServerId = -1
                            }
                        }
                    }

                    Rectangle {
                        Layout.preferredWidth: 32
                        Layout.preferredHeight: 2
                        Layout.alignment: Qt.AlignHCenter
                        Layout.topMargin: 4
                        Layout.bottomMargin: 4
                        color: "#2D3245"
                        radius: 1
                    }

                    ListView {
                        id: dmListView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        clip: true
                        spacing: 8

                        model: ListModel {
                            ListElement {
                                userInitials: "JD"
                            }
                            ListElement {
                                userInitials: "AK"
                            }
                            ListElement {
                                userInitials: "MR"
                            }
                        }

                        delegate: DmDelegate {
                            anchors.horizontalCenter: parent.horizontalCenter
                            initials: "U" + index
                            clickArea.onClicked: mainStackLayout.currentIndex = 1
                        }
                    }
                }
            }

            // Container for Top Bar and Main Content/Friends List
            ColumnLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                spacing: 0

                // 2. Top Server Bar (Spans full width of this container)
                Rectangle {
                    id: serverBar
                    Layout.fillWidth: true
                    Layout.preferredHeight: 64
                    color: "#0C0D0F"

                    ListView {
                        id: serverListView
                        anchors.left: parent.left
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        height: 48
                        anchors.leftMargin: 16
                        anchors.rightMargin: 16
                        orientation: ListView.Horizontal
                        spacing: 12
                        clip: true

                        // Forces smooth mathematical interpolation for manual coordinate changes
                        Behavior on contentX {
                            NumberAnimation {
                                duration: 250
                                easing.type: Easing.OutCubic
                            }
                        }

                        // Translates vertical mouse wheel to horizontal scrolling (Logic handled in MainScreen.qml)
                        MouseArea {
                            id: serverListWheelArea
                            anchors.fill: parent
                            acceptedButtons: Qt.NoButton
                        }

                        delegate: ServerIcon {
                            textLabel: model.serverName.substring(0, 2).toUpperCase()

                            clickArea.onClicked: {
                                backend.connectToCommunityServer(model.serverId, "", 0)
                            }
                        }
                    }
                }

                // Split between Main Content and Friends List
                RowLayout {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    spacing: 0

                    ChannelsSidebarForm {
                        id: channelsSidebar
                        Layout.preferredWidth: 240
                        Layout.fillHeight: true
                        visible: mainStackLayout.currentIndex === 1
                    }

                    StackLayout {
                        id: primaryStackLayout
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        currentIndex: 0 // 0 = Standard Split, 1 = Voice View

                        // Index 0: Standard Split (Chat & Friends)
                        RowLayout {
                            Layout.fillWidth: true
                            Layout.fillHeight: true
                            spacing: 0

                            // 3. Main Content Area (Search & Grid vs Chat View)
                        Item {
                            id: mainContentArea
                            Layout.fillWidth: true
                            Layout.fillHeight: true

                            StackLayout {
                                id: mainStackLayout
                                anchors.fill: parent
                                currentIndex: 0 // 0 = Public Servers, 1 = Chat View

                                // Index 0: Public Servers Directory
                                ColumnLayout {
                                    Layout.fillWidth: true
                                    Layout.fillHeight: true
                                    spacing: 24

                                    // Added an item to enforce the layout margins cleanly
                                    Item {
                                        Layout.fillWidth: true
                                        Layout.preferredHeight: 24
                                    }

                                    Rectangle {
                                        id: searchBarContainer
                                        Layout.fillWidth: true
                                        Layout.preferredHeight: 48
                                        Layout.leftMargin: 24
                                        Layout.rightMargin: 24
                                        color: "#1A1B1E"
                                        radius: 12
                                        border.color: "#2D3245"
                                        border.width: 1
                                    }

                                    GridView {
                                        id: publicServersGrid
                                        Layout.fillWidth: true
                                        Layout.fillHeight: true
                                        Layout.leftMargin: 24
                                        Layout.rightMargin: 24
                                        Layout.bottomMargin: 24
                                        cellWidth: 280
                                        cellHeight: 200
                                        clip: true

                                        ScrollBar.vertical: ScrollBar {
                                            policy: ScrollBar.AsNeeded
                                            contentItem: Rectangle {
                                                implicitWidth: 6
                                                radius: 3
                                                color: "#2D3245"
                                            }
                                        }

                                        delegate: ServerCard {
                                            title: model.name
                                            desc: model.description
                                            members: model.member_count + " Online"

                                            clickArea.onClicked: {
                                                backend.connectToCommunityServer(model.id, model.host_ip, model.port)
                                            }
                                        }
                                    }
                                }

                                // Index 1: Chat View
                                ChatViewForm {
                                    id: chatView
                                    Layout.fillWidth: true
                                    Layout.fillHeight: true
                                }
                            }
                        }

                        // 4. Right Sidebar (Friends List)
                        Rectangle {
                            id: rightSidebar
                            Layout.preferredWidth: 280
                            Layout.fillHeight: true
                            color: "#0C0D0F"

                            Rectangle {
                                anchors.left: parent.left
                                anchors.top: parent.top
                                anchors.bottom: parent.bottom
                                width: 1
                                color: "#0C0E13"
                            }

                            ColumnLayout {
                                anchors.fill: parent
                                anchors.margins: 16
                                spacing: 16

                                Rectangle {
                                    id: friendsSearchBar
                                    Layout.fillWidth: true
                                    Layout.preferredHeight: 32
                                    color: "#242528"
                                    radius: 12
                                    border.color: "#2D3245"
                                    border.width: 1

                                    TextInput {
                                        id: friendInput
                                        anchors.fill: parent
                                        anchors.leftMargin: 8
                                        anchors.rightMargin: 8
                                        verticalAlignment: Text.AlignVCenter
                                        color: "white"
                                        font.pixelSize: 12
                                        clip: true
                                        
                                        Text {
                                            anchors.verticalCenter: parent.verticalCenter
                                            text: "Add friend by username..."
                                            color: "#884f6a86"
                                            font.pixelSize: 12
                                            visible: !parent.text && !parent.activeFocus
                                        }
                                    }
                                }

                                ListView {
                                    id: friendsListView
                                    Layout.fillWidth: true
                                    Layout.fillHeight: true
                                    clip: true
                                    spacing: 2

                                    ScrollBar.vertical: ScrollBar {
                                        policy: ScrollBar.AsNeeded
                                        contentItem: Rectangle {
                                            implicitWidth: 6
                                            radius: 3
                                            color: "#2D3245"
                                        }
                                    }

                                    delegate: UserDelegate {
                                        id: friendDel
                                        username: model.usernameLabel
                                        statusColor: model.statusColor !== undefined ? model.statusColor : "#43B581"

                                        clickArea.onClicked: function(mouse) {
                                            if (model.status === 2) {
                                                backend.sendFriendAction(3, model.usernameLabel) // ACCEPT
                                            } else if (model.status === 3) {
                                                backend.sendFriendAction(1, model.usernameLabel) // REMOVE/CANCEL
                                            } else {
                                                var pos = friendDel.mapToItem(root, mouse.x, mouse.y)
                                                root.profileRequested(model.usernameLabel, pos.x, pos.y)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Index 1: Voice View
                    VoiceChannelViewForm {
                        id: voiceView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                    }
                }
                }
            }
        }
    }
}
