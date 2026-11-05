# Repository Guidelines

## Project Structure & Module Organization

Transcribe is a desktop transcription app for Windows and Linux, written in Rust: a core crate and one GPUI app that draws the same interface on both platforms.

- `core/`: the `transcribe-core` crate. `session.rs` runs recording, transcription, cleanup and paste; `rules.rs` holds the UI rules (labels, choices, the shortcut recorder); platform code sits behind `cfg(target_os)` (`*/linux.rs`, `*/windows.rs`). Tests are `#[cfg(test)]` modules beside the code.
- `app/`: the `transcribe` binary (gpui and gpui-component). `theme.rs` holds every colour and font, `controls.rs` the shared controls, `main_window.rs`, `history.rs` and `settings.rs` the main window, `pill.rs` the recording pill, `shell.rs` the routing between core events and windows. On GNOME's Wayland session the pill runs as a child process on XWayland (`helper.rs`); `windows.rs` holds the Win32 pieces.
- `app/assets/`: embedded fonts (Figtree, OFL) and icons (Lucide, ISC).
- `assets/icons/`: app icons.
- `packaging/linux/`: desktop entry, udev rule and post-install script for the `.deb` and `.rpm`.
- `.github/workflows/`: checks and release automation.

## Build, Test, and Development Commands

Linux needs the system libraries listed in `.github/workflows/check.yml`; Windows needs the Visual Studio 2022 C++ build tools.

- `cargo run`: build and start the app.
- `cargo test -p transcribe-core`: run the tests.
- `cargo fmt --all`: format; CI rejects unformatted code.
- `cargo xwin check -p transcribe --target x86_64-pc-windows-msvc`: check the Windows build from Linux.
- `cargo deb -p transcribe` and `cargo generate-rpm -p app` after `cargo build --release`: Linux packages.

## Documentation Policy

Code is the source of truth. Do not create documentation describing code or changes, or keep historical records. Only document information that cannot be explained by the code.

## Product Writing

Use labels, controls, and status/error messages only. Do not add subtitles, explanatory sentences, helper copy, or instructional placeholders.

## Commit & Push Policy

Commit and push every change. Always push to the remote before considering the work complete.
