# Transcribe

Desktop dictation, live transcription, and summaries. Built with Rust, Tauri, and React.

- Record speech and insert text into the active app.
- Import audio and search saved transcripts.
- Follow live transcription with continuously updated notes.
- Manually tested: Linux and macOS. Windows is not manually verified.

## Run

- Node.js 24 and stable Rust.
- macOS: Xcode command-line tools.
- Ubuntu/Debian: install the native dependencies below.

```sh
sudo apt-get install build-essential pkg-config libasound2-dev libdbus-1-dev \
  libgtk-3-dev libgtk-layer-shell-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

```sh
git clone https://github.com/awidor/Transcribe.git
cd Transcribe
npm ci
npm run desktop
```

## Use

- Dictation and audio import: add an OpenRouter API key in Settings.
- Live transcription and notes: add Meta and Inception API keys in Settings.
- Internet access and provider credits/access are required.
- Dictation shortcut: `Ctrl+Shift+Space` on Linux; `Cmd+Shift+Space` on macOS.
- macOS: allow microphone, Accessibility, and Input Monitoring access when prompted.
- If macOS permissions stop working after re-signing, remove and re-add the installed app in System Settings.

## Data

- Dictation audio goes to OpenRouter; its transcript goes to the selected cleanup model through OpenRouter.
- Live microphone audio goes to Meta.
- Live transcripts and previous notes go to Inception for summaries.
- Transcripts, notes, and API keys are stored locally. Keys are stored as plain text, not in a system keychain.

## Build and test

```sh
npm test
npm run build
cargo test --workspace --locked
cargo build --workspace --locked
npm run package
```
