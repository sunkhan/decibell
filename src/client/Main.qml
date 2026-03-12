import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Window

Window {
    id: mainWindow
    width: 1280
    height: 800
    visible: true
    title: "Secure Chat Client"
    color: "#0e121a"

    // 1. Remove OS Window Borders
    flags: Qt.Window | Qt.FramelessWindowHint

    property string currentChannel: "global"
    property bool isMaximized: false // Explicit state tracker
    property real localMicLevel: 0.0

    ListModel {
        id: channelModel
        ListElement { name: "global"; unreadCount: 0 }
        ListElement { name: "tech"; unreadCount: 0 }
        ListElement { name: "random"; unreadCount: 0 }
    }

    ListModel {
        id: voiceChannelModel
        ListElement { name: "General Voice" }
        ListElement { name: "Gaming" }
    }

    property var typingMap: ({})

    function updateTypingText() {
        let users = typingMap[currentChannel] || []
        if (users.length === 0) return ""
        if (users.length === 1) return users[0] + " is typing..."
        return users.join(", ") + " are typing..."
    }

    onCurrentChannelChanged: {
        typingIndicator.text = updateTypingText()
    }

    ListModel { id: chatModel }
    ListModel { id: userModel }

    Connections {
        target: backend
        function onStatusMessageChanged(newMessage) { statusText.text = newMessage }
        function onLoginSucceeded() { 
            mainStack.currentIndex = 1 
            backend.joinChannel(currentChannel) // Fetch history for initial channel
        }

        function onMessageReceived(channel, sender, content, timestamp) {
            if (channel === currentChannel) {
                let date = new Date(timestamp * 1000)
                let formattedTime = date.toLocaleTimeString(Qt.locale(), "hh:mm ap")
                
                chatModel.append({
                    "senderName": sender, 
                    "messageText": content, 
                    "timeText": formattedTime,
                    "rawTimestamp": timestamp
                })
                chatListView.positionViewAtEnd()
            } else {
                // Increment unread count for inactive views
                if (channel.startsWith("@")) {
                    let uname = channel.substring(1)
                    for (let i = 0; i < userModel.count; ++i) {
                        if (userModel.get(i).username === uname) {
                            userModel.setProperty(i, "unreadCount", userModel.get(i).unreadCount + 1)
                            break
                        }
                    }
                } else {
                    for (let i = 0; i < channelModel.count; ++i) {
                        if (channelModel.get(i).name === channel) {
                            channelModel.setProperty(i, "unreadCount", channelModel.get(i).unreadCount + 1)
                            break
                        }
                    }
                }
            }
        }

        function onUserListUpdated(users) {
            let oldCounts = {}
            for (let i = 0; i < userModel.count; ++i) {
                let item = userModel.get(i)
                if (item.unreadCount > 0) {
                    oldCounts[item.username] = item.unreadCount
                }
            }
            
            userModel.clear()
            for (let i = 0; i < users.length; ++i) {
                let uname = users[i]
                let count = oldCounts[uname] ? oldCounts[uname] : 0
                userModel.append({"username": uname, "unreadCount": count})
            }
        }

        function onRegisterResponded(success, message) {
            if (success) {
                statusText.text = "Registered successfully. Logging in..."
                backend.attemptLogin(usernameInput.text, passwordInput.text)
            } else {
                statusText.text = message
            }
        }

        function onLoggedOut() {
            mainStack.currentIndex = 0
            statusText.text = "Logged out successfully."
            passwordInput.text = ""
            chatModel.clear()
            userModel.clear()
        }

        function onConnectionLost(errorMsg) {
            mainStack.currentIndex = 0
            statusText.text = errorMsg
            passwordInput.text = ""
            chatModel.clear()
            userModel.clear()
        }

        function onTypingStatusReceived(channel, user, isTyping) {
            // Requires backend.username property to be exposed
            if (user === backend.username) return 
            
            let current = typingMap[channel] || []
            if (isTyping) {
                if (!current.includes(user)) current.push(user)
            } else {
                current = current.filter(u => u !== user)
            }
            
            typingMap[channel] = current
            if (channel === currentChannel) {
                typingIndicator.text = updateTypingText()
            }
        }

        function onMessageDeleted(channel, timestamp) {
            if (channel === currentChannel) {
                for (let i = 0; i < chatModel.count; ++i) {
                    if (chatModel.get(i).rawTimestamp === timestamp) {
                        chatModel.remove(i, 1)
                        break
                    }
                }
            }
        }

        function onLocalAudioLevelChanged(level) {
            localMicLevel = level
        }
    }

    FontLoader {
        id: faSolid
        source: "qrc:/ChatProj/fa-solid.otf"
    }

    // 2. Custom Title Bar & Drag Area
    Rectangle {
        id: titleBar
        anchors.top: parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        height: 32
        color: "#0e121a"
        z: 10 // Force it to render on top

        DragHandler {
            target: null // Explicitly prevents the UI element from moving
            onActiveChanged: if (active) mainWindow.startSystemMove()
        }

        RowLayout {
            anchors.fill: parent
            anchors.rightMargin: 8
            spacing: 4

            // App Icon / Title Placeholder
            Item { Layout.preferredWidth: 16 } 

            Text {
                text: "Chat Client"
                color: "#8e9297"
                font.pixelSize: 12
                font.bold: true
                Layout.fillWidth: true
            }

            // Window Controls

            // Minimize button
            Button {
                text: "\uf2d1" // fa-window-minimize
                Layout.preferredWidth: 32
                Layout.preferredHeight: 24
                background: Rectangle { color: parent.hovered ? "#2d323b" : "transparent" }
                contentItem: Text { 
                    text: parent.text; 
                    color: "#ffffff"; 
                    font.family: faSolid.name; 
                    font.pixelSize: 12; 
                    horizontalAlignment: Text.AlignHCenter; 
                    verticalAlignment: Text.AlignVCenter 
                }
                onClicked: mainWindow.showMinimized()
            }
            
            // Maximize button
            Button {
                text: mainWindow.isMaximized ? "\uf2d2" : "\uf2d0"
                Layout.preferredWidth: 32
                Layout.preferredHeight: 24
                background: Rectangle { color: parent.hovered ? "#2d323b" : "transparent" }
                contentItem: Text { 
                    text: parent.text; 
                    color: "#ffffff"; 
                    font.family: faSolid.name; 
                    font.pixelSize: 12; 
                    horizontalAlignment: Text.AlignHCenter; 
                    verticalAlignment: Text.AlignVCenter 
                }
                onClicked: {
                    if (mainWindow.isMaximized) {
                        mainWindow.showNormal()
                        mainWindow.isMaximized = false
                    } else {
                        mainWindow.showMaximized()
                        mainWindow.isMaximized = true
                    }
                }
            }

            // Close button
            Button {
                text: "\uf00d" // fa-xmark
                Layout.preferredWidth: 32
                Layout.preferredHeight: 24
                background: Rectangle { color: parent.hovered ? "#ed4245" : "transparent" }
                contentItem: Text { 
                    text: parent.text; 
                    color: "#ffffff"; 
                    font.family: faSolid.name; 
                    font.pixelSize: 14; 
                    horizontalAlignment: Text.AlignHCenter; 
                    verticalAlignment: Text.AlignVCenter 
                }
                onClicked: Qt.quit()
            }
        }
    }

    // 3. Main Application Area
    
    StackLayout {
        id: mainStack
        anchors.top: titleBar.bottom
        anchors.bottom: parent.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        currentIndex: 0

        // --- STATE 0: LOGIN VIEW ---
        Item {
            Rectangle {
                width: 320
                height: 300
                anchors.centerIn: parent
                color: "#1a1f26"
                radius: 8

                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: 24
                    spacing: 16

                    Text {
                        text: "Login"
                        color: "#ffffff"
                        font.pixelSize: 22
                        font.bold: true
                        Layout.alignment: Qt.AlignHCenter
                        Layout.bottomMargin: 8
                    }

                    TextField {
                        id: usernameInput
                        placeholderText: "Username"
                        Layout.fillWidth: true
                        color: "#ffffff"
                        background: Rectangle { color: "#0e121a"; radius: 4 }
                    }

                    TextField {
                        id: passwordInput
                        placeholderText: "Password"
                        echoMode: TextInput.Password
                        Layout.fillWidth: true
                        color: "#ffffff"
                        background: Rectangle { color: "#0e121a"; radius: 4 }
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        Layout.topMargin: 8
                        spacing: 8

                        Button {
                            text: "Login"
                            Layout.fillWidth: true
                            onClicked: {
                                if (usernameInput.text === "" || passwordInput.text === "") {
                                    statusText.text = "Fields cannot be empty."
                                    return
                                }
                                statusText.text = "Sending login request..."
                                backend.attemptLogin(usernameInput.text, passwordInput.text)
                            }
                        }

                        Button {
                            text: "Register"
                            Layout.fillWidth: true
                            onClicked: {
                                if (usernameInput.text === "" || passwordInput.text === "") {
                                    statusText.text = "Fields cannot be empty."
                                    return
                                }
                                statusText.text = "Sending registration request..."
                                backend.attemptRegister(usernameInput.text, passwordInput.text)
                            }
                        }
                    }

                    Text {
                        id: statusText
                        text: ""
                        color: "#ff8888"
                        font.pixelSize: 13
                        Layout.alignment: Qt.AlignHCenter
                        Layout.fillWidth: true
                        horizontalAlignment: Text.AlignHCenter
                        wrapMode: Text.WordWrap
                    }
                    
                    Item { Layout.fillHeight: true } 
                }
            }
        }

        // --- STATE 1: CHAT VIEW ---
        RowLayout {
            spacing: 0

            // Left Sidebar (Channels & Settings)
            Rectangle {
                Layout.preferredWidth: 240
                Layout.fillHeight: true
                color: "#11161d"

                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: 16
                    spacing: 8

                    ListView {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: channelModel
                        spacing: 4
                        clip: true

                        delegate: Rectangle {
                            width: ListView.view.width
                            height: 32
                            color: currentChannel === name ? "#2d323b" : "transparent"
                            radius: 4

                            Text {
                                anchors.verticalCenter: parent.verticalCenter
                                anchors.left: parent.left
                                anchors.leftMargin: 8
                                text: "# " + name
                                color: currentChannel === name ? "#ffffff" : "#8e9297"
                                font.pixelSize: 16
                                font.bold: currentChannel === name
                            }

                            // Channel Unread Badge
                            Rectangle {
                                anchors.right: parent.right
                                anchors.rightMargin: 8
                                anchors.verticalCenter: parent.verticalCenter
                                width: 18
                                height: 18
                                radius: 9
                                color: "#ed4245"
                                visible: unreadCount > 0

                                Text {
                                    anchors.centerIn: parent
                                    text: unreadCount > 99 ? "99+" : unreadCount
                                    color: "#ffffff"
                                    font.pixelSize: 10
                                    font.bold: true
                                }
                            }

                            MouseArea {
                                anchors.fill: parent
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    if (currentChannel !== name) {
                                        currentChannel = name
                                        channelModel.setProperty(index, "unreadCount", 0) // Reset
                                        chatModel.clear()
                                        backend.joinChannel(name)
                                    }
                                }
                            }
                        }
                    }

                    // Voice Channels Header
                    Text {
                        text: "VOICE CHANNELS"
                        color: "#8e9297"
                        font.pixelSize: 12
                        font.bold: true
                        Layout.topMargin: 16
                        Layout.leftMargin: 8
                    }

                    // Voice Channels List
                    ListView {
                        Layout.fillWidth: true
                        Layout.preferredHeight: voiceChannelModel.count * 32
                        interactive: false
                        model: voiceChannelModel
                        delegate: Rectangle {
                            width: ListView.view.width
                            height: 32
                            color: backend.currentVoiceChannel === name ? "#2d323b" : "transparent"
                            radius: 4

                            Text {
                                anchors.verticalCenter: parent.verticalCenter
                                anchors.left: parent.left
                                anchors.leftMargin: 8
                                text: "\uf028  " + name // fa-volume-up
                                font.family: faSolid.name
                                color: backend.currentVoiceChannel === name ? "#23a559" : "#8e9297"
                                font.pixelSize: 15
                                font.bold: backend.currentVoiceChannel === name
                            }

                            MouseArea {
                                anchors.fill: parent
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    if (backend.currentVoiceChannel !== name) {
                                        backend.joinVoiceChannel(name)
                                    }
                                }
                            }
                        }
                    }

                    // Logout Button
                    Button {
                        text: "Logout"
                        Layout.fillWidth: true
                        Layout.preferredHeight: 36
                        background: Rectangle { 
                            color: parent.hovered ? "#ed4245" : "#2d323b" 
                            radius: 4 
                        }
                        contentItem: Text { 
                            text: parent.text 
                            color: "#ffffff" 
                            font.pixelSize: 14 
                            font.bold: true 
                            horizontalAlignment: Text.AlignHCenter 
                            verticalAlignment: Text.AlignVCenter
                        }
                        onClicked: backend.logout()
                    }
                }

                // Voice Status Panel
                Rectangle {
                    anchors.bottom: parent.bottom
                    anchors.left: parent.left
                    anchors.right: parent.right
                    height: 50
                    color: "#292b2f"
                    visible: backend.currentVoiceChannel !== ""

                    RowLayout {
                        anchors.fill: parent
                        anchors.margins: 8
                        spacing: 8

                        Text {
                            text: "\uf012" // fa-signal
                            font.family: faSolid.name
                            color: "#23a559"
                            font.pixelSize: 16
                        }

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2
                            Text {
                                text: localMicLevel > 0.05 ? "Transmitting..." : "Voice Connected"
                                color: localMicLevel > 0.05 ? "#ffffff" : "#23a559"
                                font.pixelSize: 12
                                font.bold: true
                            }
                            Text {
                                text: backend.currentVoiceChannel
                                color: "#8e9297"
                                font.pixelSize: 11
                            }
                            
                            // Audio Volume Meter
                            Rectangle {
                                Layout.fillWidth: true
                                Layout.preferredHeight: 4
                                Layout.topMargin: 2
                                color: "#1e1f22"
                                radius: 2
                                clip: true

                                Rectangle {
                                    width: parent.width * Math.min(localMicLevel * 2.0, 1.0) // Multiplier adds visual sensitivity
                                    height: parent.height
                                    color: "#23a559"
                                    radius: 2

                                    Behavior on width {
                                        NumberAnimation { duration: 50 } // Smooths the 20ms jitter
                                    }
                                }
                            }
                        }

                        // Disconnect Button
                        Rectangle {
                            width: 32
                            height: 32
                            radius: 16
                            color: "transparent"
                            
                            Text {
                                anchors.centerIn: parent
                                text: "\uf095" // fa-phone
                                font.family: faSolid.name
                                color: "#ed4245"
                                font.pixelSize: 16
                            }

                            MouseArea {
                                anchors.fill: parent
                                cursorShape: Qt.PointingHandCursor
                                hoverEnabled: true
                                onEntered: parent.color = "#3ba55c20"
                                onExited: parent.color = "transparent"
                                onClicked: backend.leaveVoiceChannel()
                            }
                        }
                    }
                }
            }

            // Main Chat Area
            Rectangle {
                Layout.fillWidth: true
                Layout.fillHeight: true
                color: "#0e121a"

                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: 20
                    spacing: 16

                    // Header
                    Text {
                        text: currentChannel.startsWith("@") ? "Direct Message: " + currentChannel : "Welcome to #" + currentChannel + "!"
                        color: "#ffffff"
                        font.pixelSize: 24
                        font.bold: true
                    }

                    // Message List
                    ListView {
                        id: chatListView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: chatModel
                        clip: true
                        spacing: 12

                        delegate: Column {
                            width: ListView.view.width
                            spacing: 4

                            RowLayout {
                                spacing: 8
                                Text {
                                    text: senderName
                                    color: "#4f545c"
                                    font.pixelSize: 14
                                    font.bold: true
                                }
                                Text {
                                    text: timeText
                                    color: "#72767d"
                                    font.pixelSize: 11
                                    Layout.alignment: Qt.AlignBottom
                                }
                                // Delete Button
                                Text {
                                    text: "\uf2ed" // fa-trash
                                    font.family: faSolid.name
                                    color: "#ed4245"
                                    font.pixelSize: 12
                                    Layout.alignment: Qt.AlignBottom
                                    Layout.leftMargin: 8
                                    visible: senderName === backend.username
                                    
                                    MouseArea {
                                        anchors.fill: parent
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: backend.deleteMessage(currentChannel, rawTimestamp)
                                    }
                                }
                            }

                            Text {
                                text: messageText
                                color: "#dcddde"
                                font.pixelSize: 15
                                width: parent.width
                                wrapMode: Text.Wrap
                            }
                        }
                    }

                    // Typing Indicator Text
                    Text {
                        id: typingIndicator
                        color: "#8e9297"
                        font.pixelSize: 12
                        font.italic: true
                        Layout.fillWidth: true
                        Layout.preferredHeight: 16
                    }

                    // Input Area
                    Rectangle {
                        Layout.fillWidth: true
                        Layout.preferredHeight: 44
                        color: "#1a1f26"
                        radius: 8

                        Timer {
                            id: typingTimer
                            interval: 2000 // 2 seconds idle timeout
                            onTriggered: {
                                chatInput.isTyping = false
                                backend.sendTypingStatus(currentChannel, false)
                            }
                        }

                        TextField {
                            id: chatInput
                            property bool isTyping: false
                            anchors.fill: parent
                            anchors.leftMargin: 12
                            anchors.rightMargin: 12
                            color: "#ffffff"
                            font.pixelSize: 15
                            placeholderText: currentChannel.startsWith("@") ? "Message " + currentChannel : "Message #" + currentChannel
                            background: Item {}
                            
                            onTextChanged: {
                                if (text.length > 0 && !isTyping) {
                                    isTyping = true
                                    backend.sendTypingStatus(currentChannel, true)
                                }
                                if (text.length === 0 && isTyping) {
                                    isTyping = false
                                    backend.sendTypingStatus(currentChannel, false)
                                    typingTimer.stop()
                                } else if (isTyping) {
                                    typingTimer.restart()
                                }
                            }
                            
                            Keys.onReturnPressed: {
                                if (text.trim() !== "") {
                                    if (isTyping) {
                                        isTyping = false
                                        backend.sendTypingStatus(currentChannel, false)
                                        typingTimer.stop()
                                    }
                                    
                                    if (currentChannel.startsWith("@")) {
                                        backend.sendPrivateMessage(currentChannel.substring(1), text)
                                    } else {
                                        backend.sendMessage(currentChannel, text)
                                    }
                                    text = ""
                                }
                            }
                        }
                    }
                }
            }

            // Right Sidebar (Users)
            Rectangle {
                Layout.preferredWidth: 250
                Layout.fillHeight: true
                color: "#11161d"

                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: 16
                    spacing: 12
                    
                    Text {
                        text: "ONLINE — " + userModel.count
                        color: "#8e9297"
                        font.pixelSize: 12
                        font.bold: true
                    }

                    ListView {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: userModel
                        clip: true
                        spacing: 8

                        delegate: Item {
                            width: ListView.view.width
                            height: 24
                            
                            RowLayout {
                                anchors.fill: parent
                                spacing: 8

                                Rectangle {
                                    width: 10
                                    height: 10
                                    radius: 5
                                    color: "#23a559"
                                }

                                Text {
                                    text: username
                                    color: "#8e9297"
                                    font.pixelSize: 15
                                    Layout.fillWidth: true
                                }

                                // DM Unread Badge
                                Rectangle {
                                    Layout.alignment: Qt.AlignRight
                                    Layout.rightMargin: 8
                                    width: 18
                                    height: 18
                                    radius: 9
                                    color: "#ed4245"
                                    visible: unreadCount > 0

                                    Text {
                                        anchors.centerIn: parent
                                        text: unreadCount > 99 ? "99+" : unreadCount
                                        color: "#ffffff"
                                        font.pixelSize: 10
                                        font.bold: true
                                    }
                                }
                            }

                            MouseArea {
                                anchors.fill: parent
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    let targetView = "@" + username
                                    if (currentChannel !== targetView) {
                                        currentChannel = targetView
                                        userModel.setProperty(index, "unreadCount", 0) // Reset
                                        chatModel.clear()
                                        backend.joinChannel(targetView)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}