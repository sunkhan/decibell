cmake_minimum_required(VERSION 3.20)
project(chatproj VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Dependencies
find_package(Boost REQUIRED COMPONENTS system)
find_package(Protobuf REQUIRED)
find_package(SQLite3 REQUIRED)
find_package(OpenSSL REQUIRED)  # <--- Add this

# Generate Protocol Buffer Sources
protobuf_generate_cpp(PROTO_SRCS PROTO_HDRS proto/messages.proto)

# Common Library
add_library(chatproj_common ${PROTO_SRCS} ${PROTO_HDRS})
target_link_libraries(chatproj_common PUBLIC protobuf::libprotobuf Boost::system)
target_include_directories(chatproj_common PUBLIC ${CMAKE_CURRENT_BINARY_DIR})

# --- SERVER ---
add_executable(chatproj_server src/server/main.cpp)
target_link_libraries(chatproj_server PRIVATE chatproj_common Ws2_32 SQLite::SQLite3 OpenSSL::Crypto OpenSSL::SSL)
# FIX: Force the definition directly on the server target
target_compile_definitions(chatproj_server PRIVATE _WIN32_WINNT=0x0A00 WIN32_LEAN_AND_MEAN)

# --- CLIENT ---
add_executable(chatproj_client src/client/main.cpp)
target_link_libraries(chatproj_client PRIVATE chatproj_common Ws2_32 OpenSSL::Crypto OpenSSL::SSL)
# FIX: Force the definition directly on the client target
target_compile_definitions(chatproj_client PRIVATE _WIN32_WINNT=0x0A00 WIN32_LEAN_AND_MEAN)
