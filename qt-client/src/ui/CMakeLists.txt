# Decibell.Ui: scene-graph items written in C++ (the message body renderer, its emoji).
qt_add_library(decibell_ui STATIC)
qt_add_qml_module(decibell_ui
  URI Decibell.Ui
  VERSION 1.0
  SOURCES messagetext.h messagetext.cpp emoji.h emoji.cpp smoothwheel.h smoothwheel.cpp)
target_link_libraries(decibell_ui PRIVATE Qt6::Core Qt6::Gui Qt6::Quick Qt6::Svg)
