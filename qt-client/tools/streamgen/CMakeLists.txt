# streamgen: synthetic NVENC test streams for the video spike, against the vcpkg FFmpeg (the one Electron uses).
cmake_minimum_required(VERSION 3.24)
project(streamgen LANGUAGES CXX)
set(CMAKE_CXX_STANDARD 20)
set(FFMPEG_ROOT "$ENV{VCPKG_ROOT}/installed/x64-windows" CACHE PATH "FFmpeg (vcpkg) prefix")
add_executable(streamgen streamgen.cpp)
target_include_directories(streamgen PRIVATE "${FFMPEG_ROOT}/include")
target_link_directories(streamgen PRIVATE "${FFMPEG_ROOT}/lib")
target_link_libraries(streamgen PRIVATE avcodec avutil)
