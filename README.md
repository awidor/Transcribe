<div align="center">

<img src="app-icon.svg" width="112" height="112" alt="Transcribe icon">

# Transcribe

**Speak instead of typing.** Press a shortcut, talk, and clean text lands in whatever app you're using.

[![Latest release](https://img.shields.io/github/v/release/awidor/Transcribe?style=flat-square&color=111111&label=release)](https://github.com/awidor/Transcribe/releases/latest)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%C2%B7%20macOS-111111?style=flat-square)](#install)

**[Download the latest release](https://github.com/awidor/Transcribe/releases/latest)**

<br>

<img src=".github/readme/pill.png" width="560" alt="The recording pill: a black capsule with a cancel button, voice bars, a timer, and a stop button">

</div>

## Why Transcribe

- **Works in any app.** A small pill floats over your screen while you talk. When you stop, the text is pasted where your cursor is.
- **See your words as you speak.** Transcription streams live, so the pill shows each word as you say it, and the text is ready the moment you stop.
- **Hear it start and stop.** A soft chime plays when recording starts and another when it ends.
- **Cleans up what you say, if you want.** Filler words, stutters, and false starts disappear and punctuation is fixed, while your wording stays your own. Turn cleanup off to paste exactly what you said.
- **Nothing gets lost.** Every dictation is saved to History, where you can search, edit, and copy it. If a paste fails, the pill holds on to the text so you can drag it where it belongs.
- **Changed your mind?** Press `Esc` while dictating to cancel and discard it.
- **Import recordings.** Transcribe WAV, MP3, and FLAC files.
- **Stays up to date.** New versions install from Settings in one click.

<img src=".github/readme/history.png" alt="History: a searchable list of dictations with the selected transcript open for editing">

## Install

Download the installer for your platform from the [latest release](https://github.com/awidor/Transcribe/releases/latest).

| Platform                  | Download                                                                   |
| ------------------------- | -------------------------------------------------------------------------- |
| Windows (x64)             | `Transcribe_<version>_x64-setup.exe`, or the `.msi` for managed installs   |
| macOS 12+ (Apple Silicon) | `Transcribe_<version>_aarch64.dmg`, then drag Transcribe into Applications |
| Linux                     | No release build yet. See [Build from source](#build-from-source).         |

SHA-256 checksums for every download are in `SHA256SUMS.txt` on the release page.

> [!NOTE]
> The installers aren't signed with a publisher certificate, so your system asks before running them the first time.
>
> - **Windows:** when SmartScreen appears, choose **More info**, then **Run anyway**.
> - **macOS:** if Transcribe won't open, choose **Open Anyway** in System Settings > Privacy & Security.

## Get started

1. Create a [Microsoft Foundry or Speech resource](https://portal.azure.com/#create/Microsoft.CognitiveServicesAIFoundry) in a region that serves MAI-Transcribe-2-Streaming, such as Sweden Central or East US 2. No model deployment is needed.
2. Open **Settings** and add the resource's key under **Azure key**. For cleanup through OpenRouter, add an [OpenRouter API key](https://openrouter.ai/keys) too.
3. On macOS, allow microphone, Accessibility, and Input Monitoring access when asked.
4. Click into any text field, press the shortcut, and start talking. Press it again to stop, and the text is pasted in.

|                         | Windows and Linux      | macOS                 |
| ----------------------- | ---------------------- | --------------------- |
| Start or stop dictation | `Ctrl` `Shift` `Space` | `Cmd` `Shift` `Space` |
| Cancel and discard      | `Esc`                  | `Esc`                 |

Record a different shortcut in Settings. A dictation stops on its own after five minutes.

### Cleanup

Cleanup runs through OpenRouter with the model you choose. You can instead download **S1-mini by Superwhisper** in Settings to clean up transcripts on your own machine, with a style from casual to formal, or set **Cleanup** to **Off**.

### Updates

Transcribe checks for new versions in the background. When one is ready, Settings shows it with an **Install and restart** button.

## Privacy

Transcribe has no account and no server of its own. Besides the services you add keys for, it only contacts GitHub to check for updates, and Hugging Face and GitHub if you download S1-mini.

| What                         | Where it goes                                                              |
| ---------------------------- | -------------------------------------------------------------------------- |
| Dictation and imported audio | Azure, streamed for transcription. Transcribe doesn't save it.             |
| Transcripts                  | OpenRouter for cleanup, or nowhere with S1-mini or with cleanup turned off |

History and API keys stay on your computer. API keys are stored as plain text, not in the system keychain.

## Troubleshooting

- **The shortcut or pasting stops working on macOS after an update.** Remove Transcribe from Accessibility and Input Monitoring in System Settings, then add it again.

---

## Development

This part is for working on Transcribe itself. To use it, [install a release](#install).

Transcribe is a [Tauri 2](https://tauri.app) app with a React and TypeScript interface and a Rust core.

### Requirements

- [Bun](https://bun.com) canary (`bun upgrade --canary`) and stable Rust.
- macOS: Xcode command-line tools.
- Ubuntu or Debian: the native libraries below.

```sh
sudo apt-get install build-essential pkg-config libasound2-dev libdbus-1-dev \
  libgtk-3-dev libgtk-layer-shell-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

### Build from source

```sh
git clone https://github.com/awidor/Transcribe.git
cd Transcribe
bun install
bun run desktop
```

`bun run desktop` starts the app with live reload.

### Test

```sh
bun run test
bun check
cargo test --workspace --locked
```

### Package

```sh
bun run package
```

Installers are written to `target/release/bundle/`.

On macOS, `bun run package --bundles app` followed by `bun run install:macos` signs the build and installs it into Applications. If permissions stop working after re-signing, remove and re-add the app in System Settings.

### Release

Bump the version in `package.json`, `src-tauri/tauri.conf.json`, both `Cargo.toml` files, and `Cargo.lock`, then push a matching tag such as `v0.1.6`. GitHub Actions checks every platform, builds the Windows and macOS installers with update signatures, and drafts the release. Publishing the draft makes it available to installed copies.
