# Reliability investigation — 2026-09-07

Fix follow-up: the findings below describe the original implementation. Windows widget placement/visibility recovery, transient clipboard preparation retries, and operation-specific errors have since been implemented and exercised with native fixtures; see `docs/validation.md` for validation and remaining Orca acceptance coverage.

The saved history confirms automatic insertion failures. The user clarified that the missing window is the recording widget, transcription continues when it is absent, the insertion failure displays a clipboard error, the main destination is Orca, and two monitors are in use. Widget placement is therefore the leading code-backed explanation for visibility; failed shortcut activation or microphone startup does not explain these completed dictations. This investigation did not change application behavior.

## Observed evidence

Read the local SQLite history in read-only mode, querying timestamps, statuses, error messages, and text lengths without extracting transcript contents or credentials. Of 48 records:

| Records | Status | Meaning |
| --- | --- | --- |
| 43 | `unverified`, no error | Native paste was dispatched; target acceptance is unknown. |
| 1 | `saved`, `Clipboard unavailable` | Transcript retained after insertion failed. Recording timestamp: September 7, 11:12:45 Europe/Berlin. |
| 1 | `saved`, `Choose a destination` | Transcript retained because Transcribe itself was foreground at destination capture. Recording timestamp: September 7, 11:12:54 Europe/Berlin. |
| 3 | `failed`, `No speech detected` | No transcript returned. |

The configured shortcut is Right Alt, with the system-default microphone. No running Transcribe process was found during inspection. The existing shortcut diagnostic log contains capture events, not evidence of these window or paste failures.

## Findings

1. **Clipboard failure is confirmed, but its exact operation is unknown.** `crates/core/src/insertion/windows.rs:231` snapshots every clipboard format before automatic insertion. Failure to retrieve or lock any format aborts insertion. Window creation, allocation, clipboard reads, clearing, and writes reuse `Clipboard unavailable`, so the saved error cannot distinguish them. Opening a busy clipboard retries for 700 ms; format reads and writes do not retry. A metadata-only probe of the current clipboard found its text and Chromium formats available and lockable, which does not reproduce the historical failure. Manual History copying uses a separate path without the snapshot; its errors are not persisted in history.

2. **Automatic insertion aborts when Transcribe is foreground.** `crates/core/src/insertion/windows.rs:74` explicitly rejects its own process. This explains the saved `Choose a destination` error, though the record does not establish how the app gained focus. Opening History while transcription completes can produce this outcome. Sessions started with the main Record button also intentionally skip automatic insertion (`src-tauri/src/lib.rs:341`, `:555`), whereas shortcut-started sessions attempt it. These distinct behaviors can appear inconsistent without clear feedback.

3. **Widget visibility depends on its previous monitor.** `src-tauri/src/lib.rs:307` positions the widget on `w.current_monitor()`, rather than the monitor of the foreground destination. With the user's two monitors, it can appear away from Orca. Position/show errors are discarded, and there is no subsequent visibility or bounds verification during recording or transcription. This is the strongest placement defect matching the clarified symptom, although its occurrence during an incident has not been observed. Separately, the widget waits for microphone initialization (`:397`), and `Recorder::start` has an unbounded startup wait (`crates/core/src/audio.rs:141`); that would not explain a completed dictation with an absent widget.

4. **The Windows shortcut listener does not recover automatically from silent hook removal.** `crates/core/src/hotkey/windows.rs:154` refreshes the hook and reconciles stale held keys only when shortcut capture is started in Settings. `Service::start` trusts its existing `started` flag. Windows documents that a timed-out low-level hook can be silently removed without notifying the application. Consequently, a removed hook can stop recording activation until capture refreshes it or the app restarts. There is no runtime evidence establishing that this happened here. [Microsoft documentation](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc).

5. **A successful dispatch can still produce no text.** Automatic insertion restores the clipboard after a fixed 1.2 seconds (`crates/core/src/insertion/windows.rs:392`). A target that reads later can receive the restored data. All detected terminals receive Ctrl+Shift+V, regardless of their actual bindings. The history correctly records dispatch as `unverified`, but the widget displays a Finished checkmark. Neither the 43 records nor current unit tests prove those target applications accepted the paste. No automatic retry should be added after an ambiguous dispatch.

## Validation and next work

- `npm test`: 20 passed.
- `cargo test --workspace`: 25 passed (22 core, three integration); four native/interactive tests ignored by the suite.
- Inspected source, existing acceptance notes, executable timestamps, saved error metadata, and current clipboard format availability.
- Did not perform physical shortcut activation, change foreground focus, overwrite the clipboard, or make a live transcription request.

Prioritize operation-specific clipboard errors and widget visibility diagnostics, including elapsed time, window bounds, chosen monitor, and native error codes, without recording dictated text or global keystrokes. Choose widget placement from the foreground destination's monitor and work area, then verify it remains visible through recording and transcription. Reproduce insertion with Orca and clipboard contents from normal Orca/browser use. The current metadata probe did not reproduce a failing format, and the existing error cannot identify the failing Win32 operation. Preserve the documented clipboard-restoration, current-destination, and single-dispatch requirements when implementing fixes. Hook recovery and bounded microphone startup remain secondary hardening work, not established causes of these incidents.

Orca was running during inspection, but Transcribe was not. Live widget and insertion behavior could therefore not be inspected in the existing app session. No Orca-specific incompatibility has been established.
