find_package(PkgConfig REQUIRED)
pkg_check_modules(LAYER_SHELL REQUIRED IMPORTED_TARGET gtk4-layer-shell-0)
pkg_check_modules(GNOME REQUIRED IMPORTED_TARGET
    gtkmm-4.0 libadwaita-1 gtk4-wayland gtk4-x11 x11 ayatana-appindicator-glib)
find_program(GLIB_COMPILE_RESOURCES glib-compile-resources REQUIRED)

set(resources ${CMAKE_CURRENT_BINARY_DIR}/resources.c)
add_custom_command(OUTPUT ${resources}
    COMMAND ${GLIB_COMPILE_RESOURCES} --sourcedir=${CMAKE_CURRENT_SOURCE_DIR} --generate-source
        --target=${resources} ${CMAKE_CURRENT_SOURCE_DIR}/resources.xml
    DEPENDS resources.xml window.ui style.css)

add_executable(transcribe main.cpp pill.cpp window.cpp ${resources})
# gtk4-layer-shell has to load before libwayland-client.
target_link_libraries(transcribe PRIVATE PkgConfig::LAYER_SHELL PkgConfig::GNOME transcribe_core)
# The X11 placement of the pill uses GDK's deprecated X11 backend API.
target_compile_definitions(transcribe PRIVATE GDK_DISABLE_DEPRECATION_WARNINGS)
configure_file(${PROJECT_SOURCE_DIR}/assets/icons/128.png
    ${CMAKE_CURRENT_BINARY_DIR}/icons/app.transcribe.desktop.png COPYONLY)

install(TARGETS transcribe)
foreach(size 32 64 128 256 512)
    install(FILES ${PROJECT_SOURCE_DIR}/assets/icons/${size}.png
        DESTINATION share/icons/hicolor/${size}x${size}/apps RENAME app.transcribe.desktop.png)
endforeach()
install(FILES app.transcribe.desktop.desktop DESTINATION share/applications)
install(FILES 70-transcribe.rules DESTINATION lib/udev/rules.d)
