cmake_minimum_required(VERSION 3.20)
project(decibell VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Dependencies
if(POLICY CMP0167)
    cmake_policy(SET CMP0167 NEW)
endif()
find_package(Boost REQUIRED)
find_package(Protobuf REQUIRED)
find_package(OpenSSL REQUIRED)
find_package(PkgConfig REQUIRED)

find_package(jwt-cpp REQUIRED)
pkg_check_modules(LIBPQXX REQUIRED IMPORTED_TARGET libpqxx)

find_package(nlohmann_json REQUIRED)
find_package(absl REQUIRED)

# Generate Protocol Buffer Sources
protobuf_generate_cpp(PROTO_SRCS PROTO_HDRS proto/messages.proto)

# Common Library
add_library(decibell_common ${PROTO_SRCS} ${PROTO_HDRS})
target_link_libraries(decibell_common PUBLIC protobuf::libprotobuf Boost::boost
    absl::log_internal_check_op absl::log_internal_message)
target_include_directories(decibell_common PUBLIC ${CMAKE_CURRENT_BINARY_DIR})

# --- CENTRAL SERVER ---
add_executable(decibell_server
    src/server/main.cpp
    src/server/auth_manager.cpp
)
target_link_libraries(decibell_server PRIVATE
    decibell_common
    OpenSSL::Crypto
    OpenSSL::SSL
    jwt-cpp::jwt-cpp
    PkgConfig::LIBPQXX
    nlohmann_json::nlohmann_json
)

# --- COMMUNITY SERVER ---
add_executable(decibell_community
    src/community/main.cpp
)
target_link_libraries(decibell_community PRIVATE
    decibell_common
    protobuf::libprotobuf
    Boost::boost
    OpenSSL::SSL
    OpenSSL::Crypto
    jwt-cpp::jwt-cpp
    nlohmann_json::nlohmann_json
)
