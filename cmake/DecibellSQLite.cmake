# SQLite for the community server, built in rather than taken from the
# distro. Message search (docs/superpowers/specs/2026-10-14-message-search-design.md)
# needs FTS5 with contentless_delete, i.e. SQLite >= 3.43; Debian 12 ships
# 3.40, where the server would switch search off. The pinned amalgamation
# is the version the e2e suite and the search benchmark ran on.
#
# Defines DECIBELL_SQLITE_TARGET for target_link_libraries. Pass
# -DDECIBELL_SYSTEM_SQLITE=ON to link the system library instead (it must
# be >= 3.43 with FTS5, or search stays off).

option(DECIBELL_SYSTEM_SQLITE "Link the community server against the system SQLite instead of the pinned copy" OFF)

if(DECIBELL_SYSTEM_SQLITE)
    find_package(SQLite3 3.43 REQUIRED)
    set(DECIBELL_SQLITE_TARGET SQLite::SQLite3)
else()
    include(FetchContent)
    # Extracted files get the extraction time, so a re-download rebuilds.
    if(POLICY CMP0135)
        cmake_policy(SET CMP0135 NEW)
    endif()
    FetchContent_Declare(decibell_sqlite_src
        URL https://www.sqlite.org/2026/sqlite-amalgamation-3530400.zip
        URL_HASH SHA256=1e71ddf93849c6a6ecf58b827c0692073d2dd7ee40196158068f7b29f422e87d
    )
    FetchContent_MakeAvailable(decibell_sqlite_src)

    enable_language(C)
    find_package(Threads REQUIRED)
    add_library(decibell_sqlite STATIC ${decibell_sqlite_src_SOURCE_DIR}/sqlite3.c)
    # Its sqlite3.h comes first, ahead of any system copy.
    target_include_directories(decibell_sqlite PUBLIC ${decibell_sqlite_src_SOURCE_DIR})
    target_compile_definitions(decibell_sqlite PRIVATE
        SQLITE_ENABLE_FTS5
        SQLITE_THREADSAFE=1
        SQLITE_OMIT_LOAD_EXTENSION
    )
    set_target_properties(decibell_sqlite PROPERTIES POSITION_INDEPENDENT_CODE ON)
    target_link_libraries(decibell_sqlite PUBLIC Threads::Threads ${CMAKE_DL_LIBS})
    if(UNIX)
        target_link_libraries(decibell_sqlite PUBLIC m)   # bm25 / FTS5 use log()
    endif()
    set(DECIBELL_SQLITE_TARGET decibell_sqlite)
endif()
