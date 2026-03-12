import QtQuick
import QtQuick.Controls
import QtQuick.Effects

Item {
    id: root
    width: 420
    height: 520
    opacity: 1
    visible: true
    property alias backgroundClickArea: backgroundClickArea
    property alias registrationButton: registrationButton
    property alias logInButton: logInButton
    property alias usernameInputField: usernameInputField
    property alias passwordInputField: passwordInputField
    property alias emailAddressField: emailAddressField
    property alias confirmPasswordField: confirmPasswordField
    property alias errorMessageText: errorMessageText
    property color highlighting: "#2CA3E8"
    clip: true

    FontLoader {
        id: openSansBold
        source: "assets/OpenSans-Bold.ttf"
    }

    Rectangle {
        id: background
        radius: 10
        border.color: "#010710"
        anchors.fill: parent
        anchors.margins: 5
        color: "#0C0D0F"

        MouseArea {
            id: backgroundClickArea
            anchors.fill: parent
        }

        Text {
            id: loginTitle
            x: 118
            y: 70
            width: 167
            height: 58
            color: "#1FB2FF"
            text: qsTr("Decibell")
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
            font.family: openSansBold.name
            font.pixelSize: 56
        }

        DecibellTextField {
            id: emailAddressField
            x: 50
            y: 194
            opacity: 0
            placeholderText: "e-mail address"
            highlightColor: root.highlighting
        }

        DecibellTextField {
            id: usernameInputField
            x: 50
            y: 194
            opacity: 1
            placeholderText: "username"
            highlightColor: root.highlighting
        }

        DecibellTextField {
            id: passwordInputField
            x: 50
            y: 260
            placeholderText: "password"
            echoMode: TextInput.Password
            highlightColor: root.highlighting
        }

        DecibellTextField {
            id: confirmPasswordField
            x: 50
            y: 260
            opacity: 0
            visible: false
            placeholderText: "confirm password"
            echoMode: TextInput.Password
            highlightColor: root.highlighting
        }

        DecibellButton {
            id: logInButton
            x: 50
            y: 337
            text: "Log In"
        }

        DecibellButton {
            id: registrationButton
            x: 234
            y: 337
            text: "Register"
        }

        Text {
            id: errorMessageText
            x: 50
            y: 160
            width: 310
            height: 20
            color: "#FF4C4C" // Default error red
            text: ""
            horizontalAlignment: Text.AlignHCenter
            font.family: openSansBold.name
            font.pixelSize: 12
        }
    }

    MultiEffect {
        source: background
        anchors.fill: background
        shadowEnabled: true
        shadowColor: "black"
        shadowBlur: 1.0
        shadowVerticalOffset: 3
        shadowHorizontalOffset: 0
    }

    states: [
        State {
            name: "registerState"

            PropertyChanges {
                target: loginTitle
                opacity: 0 // Fades the title out
            }

            PropertyChanges {
                target: usernameInputField
                y: 120
            }
            PropertyChanges {
                target: emailAddressField
                opacity: 1
                y: 50
            }
            PropertyChanges {
                target: passwordInputField
                y: 190
            }
            PropertyChanges {
                target: confirmPasswordField
                opacity: 1
                visible: true
                y: 260
            }

            PropertyChanges {
                target: logInButton
                text: "Back"
            }
            PropertyChanges {
                target: registrationButton
                text: "Sign Up"
            }
        }
    ]

    transitions: [
        Transition {
            from: ""
            to: "registerState"
            reversible: true
            NumberAnimation {
                properties: "y,opacity"
                duration: 400
                easing.type: Easing.InOutQuad
            }
        }
    ]
}
