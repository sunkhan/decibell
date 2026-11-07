qt_add_library(decibell_app STATIC)
set_source_files_properties(Icons.qml PROPERTIES QT_QML_SINGLETON_TYPE TRUE)
set(DECIBELL_APP_QML
  Main.qml ThemeCard.qml ChatScreen.qml MessageRow.qml
  Icon.qml Icons.qml Avatar.qml RoundedImage.qml StatusDot.qml)
set(DECIBELL_APP_DEPS Decibell.Theme Decibell.Core Decibell.Ui)
# Watched streams are Windows-only so far (src/video); Main.qml loads VideoScreen.qml by URL, so the module builds
# without it elsewhere.
if(TARGET decibell_video)
  list(APPEND DECIBELL_APP_QML VideoScreen.qml)
  list(APPEND DECIBELL_APP_DEPS Decibell.Video)
endif()
qt_add_qml_module(decibell_app
  URI Decibell.App
  VERSION 1.0
  DEPENDENCIES ${DECIBELL_APP_DEPS}
  QML_FILES ${DECIBELL_APP_QML})
# Shaders are compiled to .qsb at build time (Qt Shader Tools); served at qrc:/qt/qml/Decibell/App/shaders/.
qt_add_shaders(decibell_app "decibell_app_shaders"
  PREFIX "/qt/qml/Decibell/App"
  FILES shaders/roundedimage.frag)
target_link_libraries(decibell_app PRIVATE decibell_themeplugin Qt6::Quick)
