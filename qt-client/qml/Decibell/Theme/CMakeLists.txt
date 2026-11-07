qt_add_library(decibell_theme STATIC)
set_source_files_properties(Theme.qml ThemeSettings.qml Palettes.qml PROPERTIES QT_QML_SINGLETON_TYPE TRUE)
qt_add_qml_module(decibell_theme
  URI Decibell.Theme
  VERSION 1.0
  QML_FILES Theme.qml ThemeSettings.qml ThemePalette.qml Palettes.qml)
