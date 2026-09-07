# Native acceptance checks

Run these on Windows, macOS, Linux X11, and a Wayland desktop with the required portals. Do not mark a platform accepted on compilation alone.

- [ ] Record with the global shortcut while a text editor has focus; widget does not activate.
- [ ] Stop and receive cleaned text at the currently focused cursor.
- [ ] Clipboard text, rich text, image and file formats survive a completed paste when supported.
- [ ] A new clipboard copied by the user during insertion is not overwritten by restoration.
- [ ] Switch to a browser during recording or transcription, then return to the editor; insertion succeeds in the current field.
- [ ] Start in one app and finish in another; the focused destination receives the transcript.
- [ ] Start without an editable field, then focus one before completion; insertion succeeds.
- [ ] Switch between an editor and a terminal before completion; the final destination determines terminal formatting and paste keys.
- [ ] Switching fields during the final native clipboard/dispatch transaction aborts that transaction.
- [ ] Holding hotkey modifiers during completion does not send an unintended chord.
- [ ] Rapid start/stop/cancel never produces duplicate insertion or a stale completion.
- [ ] Terminal insertion does not execute the text; original line breaks remain in History.
- [ ] Paste failure leaves a transcript in History; Copy is absent from the widget.
- [ ] A stopped/denied microphone or expired API key produces a short error.
- [ ] Failed API uploads retain audio for an explicit retry; successful uploads remove it.
- [ ] Cancel during upload suppresses insertion; later recordings are independent.
- [ ] History can be edited, searched, copied and deleted after restarting.
- [ ] Closing the main window preserves the tray and shortcut; Quit releases them.
- [ ] Multiple displays and scale factors place the widget visibly.

- [ ] With Orca on each monitor in turn, start recording and switch monitors while transcribing; the widget follows the foreground app without taking focus.
- [ ] Hide the widget through Windows Show Desktop during dictation; it reappears while the session remains active. Cancel and completion keep it hidden afterward.
- [ ] A temporarily unavailable clipboard format recovers before paste; a permanent failure reports the native operation, format, and error code while retaining the transcript and existing clipboard.

Custom application shortcuts, protected fields, Windows elevated targets, unsupported clipboard handles, unavailable Wayland portals, inaccessible editor controls, and delayed target processing need explicit failure/fallback coverage.


## Shortcut revamp

- Capture and save Left Ctrl, Right Alt, Shift alone, Ctrl+Alt, Space, F13/F24, Numpad Enter, and media keys exposed by the OS.
- On Windows/macOS/X11, capture Ctrl+A+B; change press and release order and verify exactly one activation after every key is released.
- With Ctrl alone, verify Ctrl+C, Ctrl+V, and Ctrl+Shift do not activate. Check left/right distinction and both modifiers held together.
- Hold a shortcut through autorepeat; verify one toggle. Add an unrelated key and verify no toggle.
- Cancel capture with ×, switch applications, navigate to History, close Settings, and let the 15-second lease expire. Verify the old binding remains active and no recording starts from the captured keys.
- Deny native input permission, then grant it and Save again; verify listener recovery without losing the stored binding.
- Reject reserved Windows bindings and malformed/foreign bindings; verify previous configuration survives save errors.
- On Wayland, test the desktop's acceptance of Control_L/Alt_R modifier triggers, cancellation of its permission dialog, the accepted label, and releasing a shortcut after capture. Confirm its modifier-only semantics with Ctrl+C; these are desktop-controlled.
- Verify automatic paste cannot activate a Ctrl, V, or Ctrl+V binding. Use the controlled insertion fixture before real application testing.


## macOS notch overlay

- [ ] Start recording while an external editor is focused. The black notch expands sideways; no painted surface or controls extend below the camera housing.
- [ ] Check the curved top shoulders and lower corners against the physical notch. Labels/audio occupy the left wing; timer/Stop/Cancel occupy the right wing.
- [ ] Stop and cancel with native buttons while typing into another app; focus remains in that app. History deliberately opens the main app and selects History.
- [ ] Watch expansion, content reveal, processing audio animation, and contraction; cancel and immediately restart during contraction.
- [ ] Enable Reduce Motion: expansion/contraction and processing animation stop; live audio levels and controls still work.
- [ ] Errors remain visible and expose their details through accessibility and the status tooltip; History retains the transcript. Cancel is disabled during insertion.
- [ ] Move between built-in and external displays, including vertically stacked/negative-coordinate arrangements. External displays show the floating pill below their menu bar.
- [ ] Switch Spaces and full-screen apps, change resolution/menu-bar auto-hide, disconnect a display, and wake from sleep. The active overlay stays visible and correctly positioned.
- [ ] Complete a real microphone → provider → insertion session and verify the panel contracts after completion.
