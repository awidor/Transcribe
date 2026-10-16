# Repository Guidelines

## Project Structure & Module Organization

Transcribe is a Tauri 2 desktop transcription app with a React/TypeScript frontend and Rust backend.

- `src/`: UI components, styles, types, Tauri API bridge, and colocated `*.test.tsx` tests.
- `crates/core/src/`: audio capture, transcription providers, credentials, storage, and OS-specific hotkey/insertion modules.
- `src-tauri/`: desktop integration, configuration, capabilities, icons, and Rust integration tests in `tests/`. Native macOS overlay code lives in `native/`.
- `scripts/`: macOS installation and native verification scripts.
- `.github/workflows/`: cross-platform checks and release automation.

## Build, Test, and Development Commands

Use Bun canary (`bun upgrade --canary`, needed for `bun fmt`) and stable Rust. Run `bun install` to install frontend dependencies. Linux native builds require the development libraries listed in `.github/workflows/check.yml`.

- `bun run dev`: start the Vite frontend at port 1420; native functionality requires the desktop app.
- `bun run desktop`: run the Tauri app locally.
- `bun check`: type-check TypeScript.
- `bun fmt`: format the frontend, config and docs.
- `bun run build`: type-check and build the frontend.
- `bun run test`: run Vitest once.
- `cargo test --workspace --locked`: run Rust workspace tests.
- `cargo build --workspace --locked`: build the Rust workspace.
- `bun run package`: build desktop release bundles.

## Documentation Policy

Code is the source of truth. Do not create documentation describing code or changes, or keep historical records. Only document information that cannot be explained by the code.

## Product Writing

Use labels, controls, and status/error messages only. Do not add subtitles, explanatory sentences, helper copy, or instructional placeholders.

## Commit & Push Policy

Commit and push every change. Always push to the remote before considering the work complete.
