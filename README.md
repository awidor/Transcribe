# Transcribe

Minimal Tauri desktop dictation app. React/TypeScript UI, Rust recording and transcription core, native text insertion. MAI Transcribe 2 through OpenRouter always uses enhanced mode with `transcribeStyle: clean`. No subtitle export or clean-mode toggle.

## Run

Install Node.js, Rust, and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS.

```sh
npm ci
npm run desktop
```

Enter an OpenRouter API key in Settings. Keys are stored in the operating system credential store. The default toggle shortcut is **Ctrl+Shift+Space** (macOS: **Cmd+Shift+Space**). The shortcut field captures a replacement chord. Closing the main window leaves the app in the tray; use the tray menu to quit.

```sh
npm test
npm run build
cargo test --workspace
npm run package
```

## Installers and releases

GitHub Actions checks Windows, macOS (Apple Silicon), and Linux on branch pushes and pull requests. Pushing a version tag such as `v0.1.0` runs those checks and builds Windows `.exe`/`.msi` installers plus an Apple Silicon `.dmg`. Once every build succeeds, the installers and SHA-256 checksums are attached to a draft GitHub release.

See [CI and installer releases](docs/releases.md) for tagging, publishing, local packaging, and optional Apple signing/notarization setup. Windows installers are unsigned by default; macOS builds use an ad-hoc signature unless Apple signing secrets are configured.

## Architecture

- `crates/core/src/provider.rs`: provider trait and registry. Implement `SttProvider`, register it, and supply model metadata. The shipped OpenRouter adapter sends audio directly to `/api/v1/audio/transcriptions`; there is no application server.
- `crates/core/src/audio.rs`: microphone stream on a dedicated thread, mono WAV encoding, bounded capture, level events.
- `crates/core/src/hotkey/`: native Windows hook, macOS event tap, XInput2 listener, and Wayland portal binding; shared chord recognition and bounded key capture.
- `crates/core/src/insertion/`: serialized insertion and clipboard operations; native Windows, macOS, X11, and Wayland adapters.
- `crates/core/src/storage.rs`: SQLite history and settings. Audio awaiting transcription or a retry lives in the app data directory; successful transcription removes the saved audio.
- `src-tauri/src/lib.rs`: session lifecycle, IPC boundaries, shortcuts, tray, and windows.
- `src-tauri/native/notch.m`: native macOS nonactivating panel, notch geometry, expansion animation, audio visualization, display tracking, and accessible controls. `src-tauri/src/widget.rs` bridges the panel to the shared session lifecycle.
- `src/`: history, settings, Windows/Linux recording widget. Manual copying is available only from history and the corresponding IPC command rejects widget callers.

## Shortcuts

Click the shortcut control, press the keys together, release them, then Save. Use the × button to cancel. Leaving Settings, switching windows, or waiting 15 seconds cancels capture. Escape and Tab can themselves be bound; they do not cancel recording a shortcut.

Windows, macOS, and X11 support single keys, left/right modifiers, modifier-only chords, and chords containing multiple ordinary keys. A chord activates once after all its keys are released. Extra keys, key repetition, and sequences invalidate the candidate: binding Ctrl alone does not make Ctrl+C start dictation. Existing shortcuts remain readable and keep their default either-side modifiers; newly recorded bindings store native key identities and display names. Raw global keystrokes are neither logged nor persisted.

Windows and macOS suppress input while capturing; during normal operation they suppress a matching non-modifier trigger key. Modifier and chord-prefix keys pass through to preserve other keyboard shortcuts. X11 observes raw input, so shortcut keys can also reach the focused application. Hardware-only keys which the OS does not expose (often Fn on Windows) cannot be recorded. System-reserved combinations cannot be overridden.

Wayland uses the desktop's GlobalShortcuts portal. Its format supports a trigger keysym plus a modifier mask, including modifier-key triggers when the desktop accepts them. It cannot express arbitrary multiple-letter chords or distinguish the sides of modifiers in the mask. The portal controls conflict resolution and modifier-only activation semantics; the app displays the accepted trigger description after Save. An unsupported or rejected binding does not replace the previous one. The native Ctrl+C suppression rule cannot be independently enforced through a portal which exposes only activation/deactivation events.

The listener ignores synthetic Windows events, this app's macOS events, and XTest devices. Shortcut activation is also paused throughout automatic insertion. macOS needs Accessibility/Input Monitoring permission for native key capture. A failed listener startup can be retried by saving Settings after granting permission. Permission checks do not repeatedly open macOS prompts. If an enabled macOS permission still fails after a differently signed build was installed, remove the stale Transcribe entry from that privacy pane and add the installed app again, then reopen it.

## Insertion contract

Starting a recording with the shortcut while Transcribe's main window is focused saves to History, just like the Record button. A shortcut started in another application keeps automatic-insertion intent. Missing external targets and unavailable focused elements are reported separately from denied Accessibility permission.

Resolve the focused application and editable field when the completed transcription is ready to paste, after acquiring the clipboard lock. Switching applications or fields while recording or transcribing is supported, including switching away and back. The current destination determines terminal formatting and the paste chord. Recheck that freshly resolved destination immediately before dispatch to avoid sending a partially prepared paste to another field. Do not steal focus back from a different application. Save the transcript before insertion, attempt insertion once, and retain it on all failures. A dispatched native paste is recorded as **unverified**, because native event APIs cannot prove every editor accepted the text.

The clipboard is a temporary transport. Windows uses native Unicode clipboard data and sequence numbers; macOS preserves pasteboard items and compares change counts; X11 checks ownership; Wayland uses the Clipboard and RemoteDesktop portals. Supported formats are restored only while the transaction still owns the clipboard. Windows refuses insertion when it cannot preserve a handle-based clipboard format. Input APIs and destination applications do not offer a universal paste-consumed acknowledgement; Windows/macOS/X11 use a bounded 1.2-second retention interval. A slow or heavily loaded target can still reject or delay paste. Never automatically retry an ambiguous dispatch.

Terminal profiles use the terminal's paste chord, never an Enter event. Terminal insertion collapses whitespace to one line and removes control characters to avoid submitting embedded commands; history retains the original transcript. Integrated terminal recognition uses accessibility metadata. Custom terminal keybindings can prevent automatic insertion; use the history fallback.

macOS requires Accessibility and microphone permission. Windows input cannot cross into higher integrity-level applications. Linux X11 requires XTest; Wayland requires the GlobalShortcuts, RemoteDesktop, and Clipboard portals plus AT-SPI accessibility information from the destination. Missing facilities are reported as failures, with transcripts retained in history. Portal permission dialogs are supplied by the desktop environment.

Recording is bounded to five minutes. WAV/MP3/FLAC imports are limited to 64 MiB. Uploads are not silently retried; failed recordings have a Retry action in History. On macOS, a native solid-black overlay expands sideways from the physical notch, with curved shoulders and bottom corners. Status and live audio sit to its left; elapsed time, Stop, and Cancel sit to its right. It contracts back into the notch on dismissal, respects Reduce Motion, and never takes keyboard focus. Displays without a notch use a compact black pill below the menu bar. The overlay follows the foreground application’s display and is configured for Spaces and full-screen apps. Errors stay visible until dismissed; History opens the saved transcript list.

Windows retries transient clipboard snapshot failures before changing the clipboard or dispatching paste. Each retry releases the clipboard and takes a fresh snapshot; unsupported formats still abort safely. Failures identify the native operation, format number when applicable, and Windows error code. A clipboard change detected immediately before dispatch aborts paste and preserves the newer clipboard.

The Windows recording widget responds to foreground-window changes and movement immediately, with periodic visibility recovery. It follows the foreground application's monitor, respects its work area and scaling, and repairs native hiding while dictation is active without taking focus. Opening History from the widget dismisses it for that session. Widget display failures appear in the main window's error banner.

## Validation

Unit tests cover the clean-mode API contract, cancellation, Unicode persistence, terminal input sanitization, and the separation of widget and history actions. Native builds and manual acceptance checks must also be performed on each target OS. The CI workflow builds Windows, macOS, and Linux independently; it does not replace interactive native acceptance testing.

Test browsers and native editors, VS Code editor/terminal, Windows Terminal, Terminal.app/iTerm2, and Linux terminals. Include Unicode, multiline dictation, hotkey held during completion, destination switching, clipboard changes during transcription, permission denial, cancellation, and double-stop. See `docs/acceptance.md` for the checklist.


### Native notch validation (macOS)

Run `npm run test:notch` on an unlocked Mac. This compiles and runs the actual native panel with controlled session states; it checks placement, solid-black styling, controls outside the camera area, expansion, reduced motion, focus preservation, waveform input, cancellation, and interrupted contraction. It briefly displays the panel and writes state captures to `target/notch-qa/`. It does not record audio, use credentials, call a provider, or change the clipboard.

`src-tauri/native/notch-preview.m` is a separate local visual fixture for clicking through recording, transcription, insertion, and completion; it is not compiled into the application. Production audio and all actions use the normal Rust session lifecycle.

Release builds leave build-time procedural macro libraries unstripped because macOS 27 can reject their stripped LINKEDIT data. The shipped application keeps its release stripping and thin LTO.


### Installing on this Mac

Build with `npm run package -- --bundles app`, quit Transcribe completely, then run `npm run install:macos`. The installer signs a staged bundle with the sole valid code-signing certificate in the login keychain, verifies it, and replaces `/Applications/Transcribe.app` only after signing. If there are multiple certificates, set `TRANSCRIBE_SIGNING_IDENTITY` explicitly. It refuses to replace a running app or use an ad-hoc signature. No privacy settings are changed by the installer.

Certificate-backed signatures preserve the app's designated signing identity across rebuilds. Moving from the old ad-hoc build still requires a one-time refresh of its stale Accessibility/Input Monitoring entries in System Settings. Reopen the installed app afterward. Build-directory copies are not installations and should not be granted persistent access.

Run `npm run test:macos-permissions` for the native startup-policy regression fixture; it uses mocked permission results and never opens a system prompt or captures input. On newer macOS releases the Accessibility privacy pane may be labeled **Device Control and Data Access**.
