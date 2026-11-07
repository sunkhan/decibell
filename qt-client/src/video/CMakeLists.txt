# Decibell.Video: watched streams (Windows: FFmpeg D3D11VA decode + the D3D11 video processor, design doc §5).
# FFmpeg is the vcpkg build the Electron native addon links (electron-client/HANDOFF.md §6.1).
set(DECIBELL_FFMPEG_ROOT "C:/dev/vcpkg/installed/x64-windows" CACHE PATH "FFmpeg prefix (include/, lib/, bin/)")
if(NOT EXISTS "${DECIBELL_FFMPEG_ROOT}/include/libavcodec/avcodec.h")
  message(FATAL_ERROR "FFmpeg not found under DECIBELL_FFMPEG_ROOT=${DECIBELL_FFMPEG_ROOT}")
endif()

qt_add_library(decibell_video STATIC)
qt_add_qml_module(decibell_video
  URI Decibell.Video
  VERSION 1.0
  SOURCES videoitem.h videoitem.cpp streamplayer.h streamplayer.cpp streamfile.h)
target_include_directories(decibell_video PRIVATE "${DECIBELL_FFMPEG_ROOT}/include")
target_compile_definitions(decibell_video PRIVATE NOMINMAX WIN32_LEAN_AND_MEAN)
target_link_directories(decibell_video PUBLIC "${DECIBELL_FFMPEG_ROOT}/lib")
target_link_libraries(decibell_video PRIVATE Qt6::Core Qt6::Gui Qt6::Quick avcodec avutil d3d11)

# The DLLs next to the executable (the caller copies them, it knows where the executable lands).
set(DECIBELL_FFMPEG_DLLS
  "${DECIBELL_FFMPEG_ROOT}/bin/avcodec-62.dll"
  "${DECIBELL_FFMPEG_ROOT}/bin/avutil-60.dll"
  "${DECIBELL_FFMPEG_ROOT}/bin/swresample-6.dll"
  PARENT_SCOPE)
