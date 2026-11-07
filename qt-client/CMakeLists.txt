# Decibell Qt client. Build (Windows, inside vcvars64; see HANDOFF.md §1):
#   cmake -S qt-client -B C:\b\qt -G Ninja -DCMAKE_BUILD_TYPE=Release && cmake --build C:\b\qt
# Keep the build directory short: cxx-qt's generated paths overflow MAX_PATH under a long root.
cmake_minimum_required(VERSION 3.24)
project(decibell_qt VERSION 0.1.0 LANGUAGES CXX)

if(CMAKE_CXX_COMPILER_ID STREQUAL "MSVC")
  # Rust links the release MSVC runtime on *-msvc targets; everything else must match.
  set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
endif()
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# GuiPrivate: the RHI headers (<rhi/qrhi.h>, semi-public), for MessageText's per-QRhi texture caches.
find_package(Qt6 6.8 REQUIRED COMPONENTS Core Gui GuiPrivate Qml Quick QuickShapes Svg ShaderTools QmlImportScanner)
qt_standard_project_setup(REQUIRES 6.8)
if(MSVC)
  set_property(TARGET Qt6::Core Qt6::Gui Qt6::Qml Qt6::Quick PROPERTY MAP_IMPORTED_CONFIG_DEBUG "RELEASE")
endif()
# All QML modules build into one tree, so qmlcachegen / qmllint resolve imports between them.
set(QT_QML_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/qml)

include(FetchContent)
FetchContent_Declare(CxxQt GIT_REPOSITORY https://github.com/kdab/cxx-qt-cmake.git GIT_TAG 0.10.0)
FetchContent_MakeAvailable(CxxQt)

cxx_qt_import_crate(
  MANIFEST_PATH rust/Cargo.toml
  CRATES decibell_qt
  QT_MODULES Qt::Core Qt::Gui Qt::Qml Qt::Quick)
cxx_qt_import_qml_module(decibell_core_qml URI "Decibell.Core" SOURCE_CRATE decibell_qt)

set(DECIBELL_FONTS
  ${CMAKE_CURRENT_SOURCE_DIR}/assets/fonts/MonaSans.ttf
  ${CMAKE_CURRENT_SOURCE_DIR}/assets/fonts/Inter.ttf
  ${CMAKE_CURRENT_SOURCE_DIR}/assets/fonts/JetBrainsMono.ttf)
foreach(f IN LISTS DECIBELL_FONTS)
  if(NOT EXISTS ${f})
    message(FATAL_ERROR "${f} is missing: run `node qt-client/tools/fetch-assets.mjs`")
  endif()
endforeach()

add_subdirectory(src/ui)
add_subdirectory(src/video)
add_subdirectory(qml/Decibell/Theme)
add_subdirectory(qml/Decibell/App)

qt_add_executable(decibell-qt WIN32 src/main.cpp src/gpu_device.cpp src/gpu_device.h src/frame_stats.cpp src/frame_stats.h src/crash_trace.cpp src/crash_trace.h
  src/event_trace.cpp src/event_trace.h src/selftest.cpp src/selftest.h)
if(WIN32)
  target_link_libraries(decibell-qt PRIVATE d3d11 dxgi dbghelp)
endif()
# Fonts stay uncompressed in the binary so they are registered straight from the mapped image (no copy).
qt_add_resources(decibell-qt "fonts" PREFIX "/fonts" BASE ${CMAKE_CURRENT_SOURCE_DIR}/assets/fonts
  FILES ${DECIBELL_FONTS} OPTIONS --no-compress)
target_compile_definitions(decibell-qt PRIVATE
  DECIBELL_FIXTURE_DIR="${CMAKE_CURRENT_SOURCE_DIR}/assets/fixture"
  DECIBELL_ASSETS_DIR="${CMAKE_CURRENT_SOURCE_DIR}/assets")
target_include_directories(decibell-qt PRIVATE src)
target_link_libraries(decibell-qt PRIVATE
  Qt6::Core Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickShapes
  decibell_core_qml decibell_themeplugin decibell_appplugin decibell_uiplugin decibell_ui
  decibell_videoplugin decibell_video Qt6::Svg)
qt_import_qml_plugins(decibell-qt)
add_custom_command(TARGET decibell-qt POST_BUILD
  COMMAND ${CMAKE_COMMAND} -E copy_if_different ${DECIBELL_FFMPEG_DLLS} $<TARGET_FILE_DIR:decibell-qt>
  VERBATIM)
