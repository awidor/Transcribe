# Repository Guidelines

## Project Structure & Module Organization

Transcribe is a desktop transcription app for GNOME and Windows: a C++20 core with a native shell per platform.

- `core/`: the shared core. Headers in `include/transcribe/`, each module in `src/<name>/` with its GoogleTest tests in `tests/<name>/`. Files ending in `_linux` or `_windows` only build on that platform.
- `shells/gnome/`: the GNOME shell (gtkmm and libadwaita; the window is GtkBuilder XML and CSS compiled into GResources).
- `shells/windows/`: the Windows shell (C++/WinRT and WinUI 3), an MSBuild project that links the CMake-built core.
- `assets/icons/`: app icons.
- `.github/workflows/`: checks and release automation.

## Build, Test, and Development Commands

Linux builds against system packages; the list is in `.github/workflows/check.yml`. Windows builds in a Visual Studio 2022 developer shell with vcpkg (`vcpkg.json`, triplet `x64-windows-static-md`).

- `cmake -S . -B build -G Ninja`: configure. On Windows, add `-DCMAKE_BUILD_TYPE=Release -DVCPKG_TARGET_TRIPLET=x64-windows-static-md -DCMAKE_TOOLCHAIN_FILE=%VCPKG_ROOT%\scripts\buildsystems\vcpkg.cmake`.
- `cmake --build build`: build the core and its tests, and on Linux the GNOME shell (`build/shells/gnome/transcribe`).
- `ctest --test-dir build --output-on-failure`: run the tests.
- `msbuild shells\windows\Transcribe.vcxproj /restore /p:Configuration=Release /p:Platform=x64`: build the Windows shell into `build\windows\Release` once the core is built.
- `-DTRANSCRIBE_SANITIZE=ON`: build with AddressSanitizer and UndefinedBehaviorSanitizer, as Linux CI does.
- `clang-format -i <files>`: format C++; CI rejects unformatted code.
- `cpack` in `build/`: Linux `.deb` and `.rpm` packages.

## Documentation Policy

Code is the source of truth. Do not create documentation describing code or changes, or keep historical records. Only document information that cannot be explained by the code.

## Product Writing

Use labels, controls, and status/error messages only. Do not add subtitles, explanatory sentences, helper copy, or instructional placeholders.

## Commit & Push Policy

Commit and push every change. Always push to the remote before considering the work complete.
