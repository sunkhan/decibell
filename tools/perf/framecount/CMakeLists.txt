# framecount: displayed video frames per second of any window, through DXGI Desktop Duplication (framecount.cpp).
#   cmake -S tools/perf/framecount -B C:\b\framecount -G Ninja -DCMAKE_BUILD_TYPE=Release && cmake --build C:\b\framecount
cmake_minimum_required(VERSION 3.24)
project(framecount LANGUAGES CXX)
set(CMAKE_CXX_STANDARD 20)
add_executable(framecount framecount.cpp)
target_compile_definitions(framecount PRIVATE UNICODE _UNICODE NOMINMAX WIN32_LEAN_AND_MEAN)
target_link_libraries(framecount PRIVATE d3d11 dxgi user32)
