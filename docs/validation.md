# Validation — 2026-09-06

## macOS notch implementation — 2026-09-07

The recording overlay is now a native nonactivating NSPanel. On notched screens its solid-black silhouette expands sideways from the physical notch, with concave top shoulders and rounded bottom corners. The camera region remains empty; status/audio occupy the left wing and elapsed time/Stop/Cancel occupy the right. Nothing is painted below the physical notch. The final compact layout adds 90-point wings on each side (400 points total around this Mac’s 220-point notch), using smaller controls and short visible status labels with full accessibility descriptions. A floating pill is used on non-notched displays.

The panel uses the foreground application's display, native point coordinates, screen/Space notifications, and periodic movement/visibility recovery. Session changes and controls use the existing Rust recording lifecycle. History explicitly selects the History page. All native calls run on the main thread except the atomic audio-level update. The panel stops its animation timer on dismissal, ignores stale contraction completions after a restart, and honors Reduce Motion.

Validation so far: 21 React tests, 20 Rust test executions (one additional native hotkey test remains ignored), workspace Clippy with warnings denied, strict Objective-C fixture compilation, and macOS application packaging pass. The final native fixture passed, including the animated mask’s intermediate width, reduced-motion behavior, and checks covering the Mac's 2056×1329 logical display at 2× scale, 38-point safe area, 220-point notch, solid black, side-only controls, session states, level sanitization, frozen timer, button dispatch, focus preservation, visibility recovery, and rapid restart. The final compact visual fixture was inspected through macOS accessibility and screenshots; clicking Stop entered Transcribing with Cancel still available. No visible labels or timer values are clipped.

The first native button test found that AppKit's default programmatic/accessibility click activates the process even in a nonactivating panel. The button now sends its action directly; the subsequent native test retained the external app's foreground PID throughout showing and both button clicks. No input is synthesized by the fixture.

The new compiler also exposed an existing Clippy simplification in shortcut inhibition; the equivalent expression now passes. macOS 27 rejected stripped build-time procedural macros with “mis-aligned LINKEDIT string pool.” Release build dependencies are now left unstripped; application release stripping and LTO are preserved. See [Rust issue #157750](https://github.com/rust-lang/rust/issues/157750).

The packaged app launches and Settings/History navigation work. Live recording currently requires an OpenRouter key and macOS input permissions, which are not configured. Physical multiple-monitor/Space/full-screen acceptance and a real microphone/provider/insertion request remain unverified. The fixtures do not access microphone audio, credentials, the provider, or the clipboard.


## Windows reliability fixes — 2026-09-07

Monitor-switch latency follow-up: foreground changes and movement of the foreground window now trigger widget updates through Windows WinEvent notifications. Updates are queued outside the native callback and coalesced while the UI thread is busy; caret, child-control, background-window, and this process's own events do not trigger tracking. The 750 ms check remains for visibility recovery. Application tests cover event filtering, placement, startup, and focus, and Windows Clippy passes. The user confirmed smooth monitor switching with the preceding build; this follow-up removes its polling delay.

The widget resolves the foreground app's monitor and work area on the UI thread, uses that display's scale factor, and repairs native hiding every 750 ms during active dictation without activating the window. Queued checks recheck session state, so cancellation, completion, and explicit History dismissal suppress further showing. Startup feedback appears before microphone initialization. Window errors reach the main UI.

Clipboard snapshot preparation retries transient native failures with a fresh clipboard lock and full snapshot before any clipboard mutation or paste dispatch. Unsupported formats still abort. Errors identify the native operation, format number, and Windows error code. Dispatch rechecks clipboard ownership/sequence; rollback and restoration attempt all saved formats and report failures.

The native insertion fixture passed Unicode insertion, a delayed format that initially fails to render, exactly-once insertion, clipboard format restoration, preservation of a newer clipboard, terminal formatting, and changed-destination rejection. A new native widget fixture passed repeated hide/show recovery, native bounds checks, and unchanged foreground focus. Unit tests cover mixed-DPI/negative-coordinate placement and transient-versus-permanent snapshot failures.

The native tests use controlled fixtures. Live dictation into Orca across the user's two displays remains a manual acceptance check; no live provider request was made for this change.

Final checks passed: Windows core and application tests (28 passing test executions across the workspace, including the placement test in both the library and startup integration harness), both explicitly run native fixtures, and Windows Clippy across all targets with warnings denied. `npm run package -- --bundles nsis` completed the TypeScript/Vite production build and produced the updated release executable and NSIS installer at `target/release/bundle/nsis/Transcribe_0.1.0_x64-setup.exe`.

Passed locally:

- React UI tests: 18. History-only copying; no widget copy action; no enhanced-mode toggle or subtitle controls.
- Rust core tests on Windows and Ubuntu 26.04: 19 on each platform. Clean MAI request contract through a local HTTP fixture, cancelled requests, Unicode history/settings persistence, terminal input sanitization.
- Windows native regression fixture: Unicode paste, terminal paste chord, single-line terminal insertion, duplicate rejection, text/HTML/DIB clipboard restoration, preservation of a newer clipboard, and changed-destination rejection.
- Windows workspace build and Clippy with warnings denied.
- Linux workspace check and Clippy with warnings denied, using WSL and the native Linux dependencies.
- Browser inspection of history and settings layouts, including standalone Ctrl capture in the isolated shortcut preview.
- Native keyboard listener startup on Windows and X11 through WSLg, without injecting input.
- Chord tests: modifier-only release, ordinary modifier use, arbitrary key chords, wrong modifier side, repeat suppression, extra/sequential keys, capture cancellation/expiry, keyboard activation of the recorder, stale cancellation, synthetic-paste pause, legacy settings, reserved Windows bindings, and Wayland trigger conversion.
- Windows NSIS installer packaging.
- Packaged Windows application launched and remained responsive without the Vite server.

The native regression test creates its own editor process; it does not establish compatibility with every terminal or application. It is deliberately excluded from unattended test runs. Run it on an unlocked Windows desktop with:

```sh
cargo test -p transcribe-core native_roundtrip -- --ignored --nocapture --test-threads=1
```

Still requires interactive acceptance on macOS and Linux desktops, real terminal applications, and a live MAI request with the user's OpenRouter key. No live service request was made without a key. macOS code and the three-platform CI workflow are included; a Mac runner was not available in this environment.

The shortcut revamp has not been exercised with physical keystrokes in the packaged app on every platform. Native startup checks prove listener installation, and deterministic event tests prove chord behavior separately. Browser capture checks use an isolated mocked desktop API. macOS compilation and interactive portal acceptance remain for their native runners; no unsupported universal hotkey guarantee is made.


Dynamic destination resolution: recordings retain only automatic-insertion intent, never an application or field snapshot. Tests cover switching away/back, choosing a different final destination, resolving focus after a clipboard wait, unavailable fields, and duplicate dispatch prevention. These new tests use fake destinations and do not modify the system clipboard. Physical app-switching acceptance still requires a live dictation session.

Startup state regression: the main window and widget opt out of automatic creation and are built only after AppState registration. A Tauri mock-runtime integration test invokes the real bootstrap IPC handler from both windows; a configuration test prevents re-enabling early window creation. Windows integration test executables embed the same Common Controls v6 manifest as the application.

Shortcut focus regression: Windows capture checks the native foreground window instead of Tao's parent keyboard-focus flag, which can be false when WebView2 has focus. The same check handles focus-loss cancellation. A native test with invisible windows covers nested browser children, unrelated windows, separately owned widgets, missing foreground windows, and destroyed handles without changing desktop focus or injecting keys. This test, the two startup/configuration tests, all 18 UI tests, and Windows/Linux workspace Clippy pass. Physical shortcut capture in the packaged app still requires interactive acceptance.

Windows shortcut capture follow-up: the user confirmed key capture works with focused Settings keyboard events routed through token-scoped IPC into the shared chord engine. This path returns capture updates directly to the UI and preserves press/release order. The native hook remains available; starting capture refreshes it on its owning thread and reconciles released keys. Tests cover native-code mapping (including right Alt/AltGraph and numpad Enter), capture without hook events, duplicate input, stale tokens, expiry, and released-key reconciliation. A desktop-only test also passes three successive native listener refreshes without injecting keyboard input. Temporary diagnostic logging was removed. This confirmation covers recording a shortcut, not global activation in every application.
