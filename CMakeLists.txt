cmake_minimum_required(VERSION 3.25)
project(Transcribe VERSION 0.2.0 LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

option(TRANSCRIBE_SANITIZE "Build with AddressSanitizer and UndefinedBehaviorSanitizer" OFF)

if(MSVC)
    add_compile_options(/W4 /utf-8 /permissive- /EHsc /Zc:__cplusplus)
    add_compile_definitions(NOMINMAX WIN32_LEAN_AND_MEAN UNICODE _UNICODE)
else()
    add_compile_options(-Wall -Wextra)
endif()
if(TRANSCRIBE_SANITIZE)
    add_compile_options(-fsanitize=address,undefined -fno-omit-frame-pointer)
    add_link_options(-fsanitize=address,undefined)
endif()

add_compile_definitions(TRANSCRIBE_VERSION="${PROJECT_VERSION}")

include(CTest)
add_subdirectory(core)
if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
    add_subdirectory(shells/gnome)
endif()
