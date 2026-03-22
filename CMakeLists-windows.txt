cmake_minimum_required(VERSION 3.20)
project(chatproj VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Dependencies
find_package(Boost REQUIRED COMPONENTS system)
find_package(Protobuf REQUIRED)
find_package(SQLite3 REQUIRED)
find_package(OpenSSL REQUIRED)
find_package(Opus CONFIG REQUIRED)
find_package(jwt-cpp CONFIG REQUIRED)
find_package(libpqxx CONFIG REQUIRED)
find_package(nlohmann_json CONFIG REQUIRED)
find_package(libyuv CONFIG REQUIRED)
find_package(unofficial-libvpx CONFIG REQUIRED)

# Generate Protocol Buffer Sources
protobuf_generate_cpp(PROTO_SRCS PROTO_HDRS proto/messages.proto)

# Common Library
add_library(chatproj_common ${PROTO_SRCS} ${PROTO_HDRS})
target_link_libraries(chatproj_common PUBLIC protobuf::libprotobuf Boost::system)
target_include_directories(chatproj_common PUBLIC ${CMAKE_CURRENT_BINARY_DIR})

# --- SERVER ---
add_executable(chatproj_server 
    src/server/main.cpp 
    src/server/auth_manager.cpp
)
target_link_libraries(chatproj_server PRIVATE
    chatproj_common
    Ws2_32
    SQLite::SQLite3
    OpenSSL::Crypto
    OpenSSL::SSL
    jwt-cpp::jwt-cpp
    libpqxx::pqxx
    nlohmann_json::nlohmann_json
)
# FIX: Force the definition directly on the server target
target_compile_definitions(chatproj_server PRIVATE _WIN32_WINNT=0x0A00 WIN32_LEAN_AND_MEAN)

# --- GUI Dependencies ---
set(CMAKE_AUTOMOC ON)
set(CMAKE_AUTORCC ON)
set(CMAKE_AUTOUIC ON)

find_package(Qt6 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2 Multimedia)

# --- CLIENT ---
add_executable(chatproj_client
    src/client/main.cpp
    src/client/backend.cpp
    src/client/backend.hpp
    src/client/audio_engine.cpp
    src/client/audio_engine.hpp
    src/client/video_engine.cpp
    src/client/video_engine.hpp
)

# Force the virtual filename to be "Main.qml" without the folder structure
set_source_files_properties(src/client/Main.qml PROPERTIES QT_RESOURCE_ALIAS "Main.qml")
set_source_files_properties("src/client/assets/Font Awesome 7 Free-Solid-900.otf" PROPERTIES QT_RESOURCE_ALIAS "fa-solid.otf")

qt_add_qml_module(chatproj_client
    URI ChatProj
    VERSION 1.0
    RESOURCE_PREFIX "/"  # Force the base prefix to strictly be "/"
    QML_FILES
        src/client/App.qml
        src/client/LoginScreen.qml
        src/client/LoginScreenForm.ui.qml
        src/client/MainScreen.qml
        src/client/MainScreenForm.ui.qml
        src/client/ChannelsSidebarForm.ui.qml
        src/client/ChatViewForm.ui.qml
        src/client/VoiceChannelViewForm.ui.qml
        src/client/StreamConfigDialog.qml
        src/client/ProfilePopup.qml
        src/client/DecibellButton.ui.qml
        src/client/DecibellTextField.ui.qml
        src/client/DmDelegate.ui.qml
        src/client/MessageDelegate.ui.qml
        src/client/ServerCard.ui.qml
        src/client/ServerIcon.ui.qml
        src/client/UserDelegate.ui.qml
    RESOURCES
        src/client/qtquickcontrols2.conf
        src/client/assets/OpenSans-Bold.ttf
        src/client/assets/OpenSans-Italic.ttf
        src/client/assets/OpenSans-Regular.ttf
        src/client/assets/OpenSans-LightItalic.ttf
        src/client/assets/FontAwesome7Free-Solid-900.otf
)

target_link_libraries(chatproj_client PRIVATE
    chatproj_common
    Ws2_32
    Ole32
    d3d11
    dxgi
    user32
    gdi32
    OpenSSL::Crypto
    OpenSSL::SSL
    Qt6::Core
    Qt6::Gui
    Qt6::Qml
    Qt6::Quick
    Qt6::QuickControls2
    Qt6::Multimedia
    Opus::opus
    yuv
    unofficial::libvpx::libvpx
    Mf
    Mfplat
    Mfuuid
    winmm
    windowsapp
    dwmapi
)

# --- Community Server Target ---
add_executable(chatproj_community 
    src/community/main.cpp 
    # Additional source files will be added here
)

target_link_libraries(chatproj_community PRIVATE
    chatproj_common
    protobuf::libprotobuf
    Boost::headers
    OpenSSL::SSL
    OpenSSL::Crypto
    jwt-cpp::jwt-cpp
    nlohmann_json::nlohmann_json
)

# FIX: Force the definition directly on the client target
# target_compile_definitions(chatproj_client PRIVATE _WIN32_WINNT=0x0A00 WIN32_LEAN_AND_MEAN)

# --- AUTOMATED DEPLOYMENT ---
if(WIN32)
    # Extract the bin directory from the Qt installation
    get_target_property(_qmake_executable Qt6::qmake IMPORTED_LOCATION)
    get_filename_component(_qt_bin_dir "${_qmake_executable}" DIRECTORY)
    
    # Run windeployqt automatically after building the client
    add_custom_command(TARGET chatproj_client POST_BUILD
        COMMAND "${_qt_bin_dir}/windeployqt.exe" 
                --qmldir "${CMAKE_CURRENT_SOURCE_DIR}/src/client" 
                "$<TARGET_FILE:chatproj_client>"
        COMMENT "Deploying Qt and QML dependencies..."
    )
endif()