qt_add_library(decibell_app STATIC)
qt_add_qml_module(decibell_app
  URI Decibell.App
  VERSION 1.0
  DEPENDENCIES Decibell.Theme Decibell.Core
  QML_FILES Main.qml ThemeCard.qml)
target_link_libraries(decibell_app PRIVATE decibell_themeplugin)
