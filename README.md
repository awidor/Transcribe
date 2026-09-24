<img src="app-icon.svg" width="96" height="96" alt="Transcribe icon">

# Transcribe

Desktop dictation, live transcription, and summaries.

- Record speech and insert text into the active app.
- Import audio and search saved transcripts.
- Follow live transcription with continuously updated notes.
- Manually tested: Linux and macOS. Windows is not manually verified.

## Using Transcribe

### Install

Download the installer for your platform from the [latest release](https://github.com/awidor/Transcribe/releases/latest):

- Windows (x64): `Transcribe_<version>_x64-setup.exe`, or the `.msi` for managed installations.
- macOS (Apple Silicon): `Transcribe_<version>_aarch64.dmg`. Open it and drag Transcribe into Applications.
- Linux: no release build. Build it from source (see [Developing Transcribe](#developing-transcribe)).

The installers are not signed with a publisher certificate:

- Windows: if SmartScreen stops the installer, choose **More info**, then **Run anyway**.
- macOS: if Transcribe is blocked on first open, choose **Open Anyway** in System Settings > Privacy & Security.

Transcribe checks for updates in the background. Install them from Settings with **Install and restart**.

### Set up

- Dictation and audio import: add an OpenRouter API key in Settings.
- Live transcription and notes: add Meta and Inception API keys in Settings.
- Internet access and provider credits/access are required.
- macOS: allow microphone, Accessibility, and Input Monitoring access when prompted.
- If macOS permissions stop working after an update, remove and re-add Transcribe in System Settings.

### Dictate

- Press the shortcut to start and again to stop: `Ctrl+Shift+Space` on Windows and Linux, `Cmd+Shift+Space` on macOS. Change it in Settings.
- The transcript is inserted into the active app and saved in History.
- `Escape` cancels a dictation and discards it.

### Data

- Dictation audio goes to OpenRouter; its transcript goes to the selected cleanup model through OpenRouter.
- Live microphone audio goes to Meta.
- Live transcripts and previous notes go to Inception for summaries.
- Transcripts, notes, and API keys are stored locally. Keys are stored as plain text, not in a system keychain.

## Developing Transcribe

Built with Rust, Tauri, and React.

### Requirements

- Node.js 24 and stable Rust.
- macOS: Xcode command-line tools.
- Ubuntu/Debian: the native dependencies below.

```sh
sudo apt-get install build-essential pkg-config libasound2-dev libdbus-1-dev \
  libgtk-3-dev libgtk-layer-shell-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

### Run from source

```sh
git clone https://github.com/awidor/Transcribe.git
cd Transcribe
npm ci
npm run desktop
```

### Build and test

```sh
npm test
npm run build
cargo test --workspace --locked
cargo build --workspace --locked
npm run package
```

- `npm run package` writes installers to `target/release/bundle/`.
- macOS: `npm run package -- --bundles app`, then `npm run install:macos` signs the build and installs it into Applications.
- If macOS permissions stop working after re-signing, remove and re-add the installed app in System Settings.
