import QtQuick

LoginScreenForm {
    id: form

    Shortcut {
        sequence: "Esc"
        onActivated: {
            form.forceActiveFocus()
        }
    }

    // Handle Log In / Back Button
    Connections {
        target: form.logInButton
        function onClicked() {
            form.errorMessageText.text = "" // Clear previous errors
            if (form.state === "registerState") {
                form.state = ""
            } else {
                backend.attemptLogin(form.usernameInputField.text, form.passwordInputField.text)
            }
        }
    }

    // Handle Registration / Back Button
    Connections {
        target: form.registrationButton
        function onClicked() {
            form.errorMessageText.text = "" 
            if (form.state === "registerState") {
                var user = form.usernameInputField.text.trim()
                var email = form.emailAddressField.text.trim()
                var pass = form.passwordInputField.text
                var conf = form.confirmPasswordField.text

                if (user === "" || email === "" || pass === "") {
                    form.errorMessageText.text = "All fields are required."
                    return
                }
                
                if (pass !== conf) {
                    form.errorMessageText.text = "Passwords do not match."
                    return
                }

                backend.attemptRegister(user, email, pass)
            } else {
                form.state = "registerState"
            }
        }
    }

    // Drops focus from text fields when the background is clicked
    Connections {
        target: form.backgroundClickArea
        function onClicked() {
            form.forceActiveFocus()
        }
    }

    // Handle Backend Status and Errors
    Connections {
        target: backend

        function onStatusMessageChanged(msg) {
            form.errorMessageText.text = msg
            // Use green for success, red for errors
            form.errorMessageText.color = (msg === "Login successful") ? "#43B581" : "#FF4C4C"
        }

        function onRegisterResponded(success, msg) {
            form.errorMessageText.text = msg
            form.errorMessageText.color = success ? "#43B581" : "#FF4C4C"
            if (success) {
                // Return to login screen automatically on successful registration
                form.state = ""
            }
        }

        function onConnectionLost(msg) {
            form.errorMessageText.text = msg
            form.errorMessageText.color = "#FF4C4C"
        }
    }
}
