import QtQuick
import QtQuick.Window

Window {
    id: rootWindow
    width: 420
    height: 520
    minimumWidth: 420
    minimumHeight: 520
    maximumWidth: 420
    maximumHeight: 520
    visible: true
    title: "Decibell"
    color: "#0C0D0F"

    Component.onCompleted: {
        rootWindow.x = (Screen.width - width) / 2
        rootWindow.y = (Screen.height - height) / 2
    }

    FontLoader {
        id: fontAwesome
        source: "assets/FontAwesome7Free-Solid-900.otf"
    }

    function switchToMainScreen() {
        screenLoader.source = "MainScreen.qml"
        rootWindow.maximumWidth = 16777215
        rootWindow.maximumHeight = 16777215
        rootWindow.minimumWidth = 960
        rootWindow.minimumHeight = 540
        rootWindow.width = 1280
        rootWindow.height = 720
        rootWindow.x = (Screen.width - 1280) / 2
        rootWindow.y = (Screen.height - 720) / 2
    }

    function switchToLoginScreen() {
        screenLoader.source = "LoginScreen.qml"
        rootWindow.maximumWidth = 420
        rootWindow.maximumHeight = 520
        rootWindow.minimumWidth = 420
        rootWindow.minimumHeight = 520
        rootWindow.width = 420
        rootWindow.height = 520
        rootWindow.x = (Screen.width - 420) / 2
        rootWindow.y = (Screen.height - 520) / 2
    }

    Connections {
        target: backend
        function onLoginSucceeded() {
            rootWindow.switchToMainScreen()
        }
        function onLoggedOut() {
            rootWindow.switchToLoginScreen()
        }
    }

    Loader {
        id: screenLoader
        anchors.fill: parent
        source: "LoginScreen.qml" // Start at the login screen
    }
}
