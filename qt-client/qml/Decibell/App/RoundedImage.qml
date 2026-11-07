import QtQuick

// An image drawn inside a rounded rectangle (shaders/roundedimage.frag), decoded at the drawn size. `cover`
// crops to fill (CSS object-cover), otherwise the box must already have the picture's aspect (object-contain).
Item {
    id: root
    property url source
    property real radius: 0
    property bool cover: true
    readonly property alias status: img.status
    // Debug (--no-images): load no pictures, to measure what decoded pictures cost.
    readonly property bool noImages: Qt.application.arguments.indexOf("--no-images") >= 0

    Image {
        id: img
        source: root.noImages ? "" : root.source
        visible: false
        asynchronous: true
        // Decode at the device-pixel size the picture is drawn at (cover: the box's larger side covers it).
        sourceSize: Qt.size(Math.ceil(root.width * Screen.devicePixelRatio), Math.ceil(root.height * Screen.devicePixelRatio))
        fillMode: root.cover ? Image.PreserveAspectCrop : Image.Stretch
    }

    ShaderEffect {
        anchors.fill: parent
        visible: img.status === Image.Ready
        property variant source: img
        property size itemSize: Qt.size(width, height)
        property real radius: root.radius
        // With sourceSize set, Qt already decoded a crop-filled image for PreserveAspectCrop: show it whole.
        property rect crop: Qt.rect(0, 0, 1, 1)
        fragmentShader: "qrc:/qt/qml/Decibell/App/shaders/roundedimage.frag.qsb"
    }
}
