qt_add_library(decibell_app STATIC)
set_source_files_properties(Icons.qml PROPERTIES QT_QML_SINGLETON_TYPE TRUE)
qt_add_qml_module(decibell_app
  URI Decibell.App
  VERSION 1.0
  DEPENDENCIES Decibell.Theme Decibell.Core Decibell.Ui Decibell.Video
  QML_FILES
    Main.qml ThemeCard.qml ChatScreen.qml MessageRow.qml
    Icon.qml Icons.qml Avatar.qml RoundedImage.qml StatusDot.qml VideoScreen.qml)
# Shaders are compiled to .qsb at build time (Qt Shader Tools); served at qrc:/qt/qml/Decibell/App/shaders/.
qt_add_shaders(decibell_app "decibell_app_shaders"
  PREFIX "/qt/qml/Decibell/App"
  FILES shaders/roundedimage.frag)
target_link_libraries(decibell_app PRIVATE decibell_themeplugin Qt6::Quick Qt6::QuickShapes)
