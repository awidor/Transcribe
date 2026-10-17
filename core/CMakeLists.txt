find_package(Threads REQUIRED)
find_package(CURL REQUIRED)
find_package(SQLite3 REQUIRED)
find_package(nlohmann_json REQUIRED)
find_package(LibArchive REQUIRED)
# Neither installs a CMake package.
find_path(MINIAUDIO_INCLUDE_DIR miniaudio.h REQUIRED)
find_path(SOXR_INCLUDE_DIR soxr.h REQUIRED)
find_library(SOXR_LIBRARY soxr REQUIRED)

# Installs and updates the app.
include(FetchContent)
FetchContent_Declare(velopack
    URL https://github.com/velopack/velopack/releases/download/1.2.161/velopack_libc_1.2.161.zip
    URL_HASH SHA256=31de8f2cf05a47bd5328a9f98b0c41aec1582156f6075f1058b26941f7f17c20)
FetchContent_MakeAvailable(velopack)
add_library(velopack STATIC IMPORTED)
set_target_properties(velopack PROPERTIES
    INTERFACE_INCLUDE_DIRECTORIES ${velopack_SOURCE_DIR}/include)
if(WIN32)
    set_target_properties(velopack PROPERTIES
        IMPORTED_LOCATION ${velopack_SOURCE_DIR}/lib-static/velopack_libc_win_x64_msvc.lib
        INTERFACE_LINK_LIBRARIES "advapi32;bcrypt;crypt32;ncrypt;ntdll;ole32;oleaut32;secur32;shell32;user32;userenv;winhttp;ws2_32")
else()
    set_target_properties(velopack PROPERTIES
        IMPORTED_LOCATION ${velopack_SOURCE_DIR}/lib-static/velopack_libc_linux_x64_gnu.a
        INTERFACE_LINK_LIBRARIES "Threads::Threads;${CMAKE_DL_LIBS};m")
endif()

if(NOT WIN32)
    find_package(PkgConfig REQUIRED)
    find_package(OpenSSL REQUIRED)
    pkg_check_modules(GIO REQUIRED IMPORTED_TARGET gio-2.0)
    pkg_check_modules(EVDEV REQUIRED IMPORTED_TARGET libevdev)
    pkg_check_modules(XCB REQUIRED IMPORTED_TARGET xcb xcb-xinput xcb-xtest)
    pkg_check_modules(WAYLAND REQUIRED IMPORTED_TARGET wayland-client)
endif()

if(BUILD_TESTING)
    find_package(GTest REQUIRED)
    include(GoogleTest)
endif()

# Files ending in _linux or _windows only build on that platform.
function(transcribe_platform_filter out)
    set(kept)
    foreach(file IN LISTS ARGN)
        if(file MATCHES "_windows\\.(cpp|c)$" AND NOT WIN32)
            continue()
        endif()
        if(file MATCHES "_linux\\.(cpp|c)$" AND NOT CMAKE_SYSTEM_NAME STREQUAL "Linux")
            continue()
        endif()
        list(APPEND kept ${file})
    endforeach()
    set(${out} ${kept} PARENT_SCOPE)
endfunction()

# A module is the sources in src/<name> plus its tests in tests/<name>.
function(transcribe_module name)
    cmake_parse_arguments(M "" "" "DEPS;SOURCES" ${ARGN})
    file(GLOB_RECURSE sources CONFIGURE_DEPENDS
        ${CMAKE_CURRENT_SOURCE_DIR}/src/${name}/*.cpp ${CMAKE_CURRENT_SOURCE_DIR}/src/${name}/*.c)
    transcribe_platform_filter(sources ${sources} ${M_SOURCES})
    add_library(transcribe_${name} STATIC ${sources})
    target_include_directories(transcribe_${name}
        PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/include
        PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/src ${CMAKE_CURRENT_BINARY_DIR}/generated)
    target_link_libraries(transcribe_${name} PUBLIC ${M_DEPS})
    if(BUILD_TESTING)
        file(GLOB_RECURSE tests CONFIGURE_DEPENDS ${CMAKE_CURRENT_SOURCE_DIR}/tests/${name}/*.cpp)
        transcribe_platform_filter(tests ${tests})
        if(tests)
            add_executable(test_${name} ${tests})
            target_include_directories(test_${name} PRIVATE
                ${CMAKE_CURRENT_SOURCE_DIR}/src ${CMAKE_CURRENT_SOURCE_DIR}/tests
                ${CMAKE_CURRENT_BINARY_DIR}/generated)
            target_link_libraries(test_${name} PRIVATE
                transcribe_${name} transcribe_test_support GTest::gtest_main)
            gtest_discover_tests(test_${name} DISCOVERY_MODE PRE_TEST
                PROPERTIES ENVIRONMENT "TRANSCRIBE_SOURCE_DIR=${PROJECT_SOURCE_DIR}")
        endif()
    endif()
endfunction()

if(WIN32)
    set(PLATFORM_COMMON bcrypt ws2_32)
else()
    set(PLATFORM_COMMON OpenSSL::Crypto)
endif()
transcribe_module(common DEPS Threads::Threads CURL::libcurl nlohmann_json::nlohmann_json
    ${PLATFORM_COMMON})

if(BUILD_TESTING)
    file(GLOB support CONFIGURE_DEPENDS ${CMAKE_CURRENT_SOURCE_DIR}/tests/support/*.cpp)
    add_library(transcribe_test_support STATIC ${support})
    target_include_directories(transcribe_test_support PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/tests)
    target_link_libraries(transcribe_test_support PUBLIC transcribe_common GTest::gtest)
endif()

transcribe_module(credentials DEPS transcribe_common)
transcribe_module(storage DEPS transcribe_common SQLite::SQLite3)
transcribe_module(audio DEPS transcribe_common ${SOXR_LIBRARY})
target_include_directories(transcribe_audio PRIVATE ${MINIAUDIO_INCLUDE_DIR} ${SOXR_INCLUDE_DIR})
if(NOT WIN32)
    target_link_libraries(transcribe_audio PRIVATE ${CMAKE_DL_LIBS} m)
endif()
transcribe_module(azure DEPS transcribe_common)
transcribe_module(s1 DEPS transcribe_common LibArchive::LibArchive)
transcribe_module(provider DEPS transcribe_common transcribe_s1)
if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
    transcribe_module(linux_input DEPS transcribe_common PkgConfig::EVDEV)
    transcribe_module(insertion DEPS transcribe_common transcribe_linux_input
        PkgConfig::GIO PkgConfig::XCB PkgConfig::WAYLAND)
    find_program(WAYLAND_SCANNER wayland-scanner REQUIRED)
    file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/generated)
    foreach(protocol ext-data-control-v1 wlr-foreign-toplevel-management-unstable-v1)
        set(xml ${CMAKE_CURRENT_SOURCE_DIR}/protocols/${protocol}.xml)
        set(out ${CMAKE_CURRENT_BINARY_DIR}/generated/${protocol})
        add_custom_command(OUTPUT ${out}-client-protocol.h ${out}-protocol.c
            COMMAND ${WAYLAND_SCANNER} client-header ${xml} ${out}-client-protocol.h
            COMMAND ${WAYLAND_SCANNER} private-code ${xml} ${out}-protocol.c
            DEPENDS ${xml} VERBATIM)
        target_sources(transcribe_insertion PRIVATE ${out}-client-protocol.h ${out}-protocol.c)
    endforeach()
    if(BUILD_TESTING)
        # The backend test runs a compositor inside the test process.
        pkg_check_modules(WAYLAND_SERVER REQUIRED IMPORTED_TARGET wayland-server)
        target_link_libraries(test_insertion PRIVATE PkgConfig::WAYLAND_SERVER)
    endif()
    transcribe_module(hotkey DEPS transcribe_common transcribe_linux_input
        PkgConfig::XCB PkgConfig::EVDEV)
    set(PLATFORM_APP transcribe_linux_input PkgConfig::GIO)
else()
    transcribe_module(insertion DEPS transcribe_common ole32 oleaut32 uuid user32 gdi32)
    transcribe_module(hotkey DEPS transcribe_common user32)
endif()
transcribe_module(app DEPS transcribe_storage transcribe_credentials transcribe_audio
    transcribe_azure transcribe_provider transcribe_hotkey transcribe_insertion velopack
    ${PLATFORM_APP})

# The whole core, for the shells.
add_library(transcribe_core INTERFACE)
target_link_libraries(transcribe_core INTERFACE transcribe_app)
