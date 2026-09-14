# CI and installer releases

The **Check** workflow runs on branch pushes, pull requests, and manual dispatches. It installs locked npm dependencies, checks Rust formatting, runs frontend and Rust workspace tests, and builds the frontend and native app. It covers Windows x64, macOS Apple Silicon, and Linux x64. Node.js 24 and stable Rust are used, with npm and workspace-level Rust caches.

The **Release** workflow runs when a `vMAJOR.MINOR.PATCH` tag is pushed. It checks that the tag matches the versions in both npm manifests, Tauri's configuration, both Rust crates, and `Cargo.lock`, then runs the complete Check workflow against the tagged commit. After the checks pass, it builds:

| Platform            | Downloads                                      | Build runner   |
| ------------------- | ---------------------------------------------- | -------------- |
| Windows x64         | NSIS setup `.exe` and Windows Installer `.msi` | `windows-2022` |
| macOS Apple Silicon | `aarch64.dmg`                                  | `macos-15`     |

Installers appear in the workflow's artifacts and in a **draft GitHub release**, together with `SHA256SUMS.txt`. The draft is created only after every installer job succeeds. Linux remains covered by CI; this release workflow packages Windows and macOS.

## Create a release

1. Commit and push the workflow/configuration changes before tagging. For subsequent releases, update the version in `package.json`, `package-lock.json` (including `packages[""].version`), `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and `crates/core/Cargo.toml`. Run `cargo check --workspace` to refresh the workspace package versions in `Cargo.lock`, then commit the version changes. Use a stable three-part version; prerelease tags are not supported by this workflow.
2. Tag the release commit and push the tag. The initial version is `0.1.0`:

   ```sh
   git tag -a v0.1.0 -m "Transcribe v0.1.0"
   git push origin v0.1.0
   ```

3. Open GitHub **Actions → Release** and wait for the checks and two installer build jobs to finish. No personal access token is needed: only the final release job receives `contents: write` through GitHub's automatic token. Repository/organization policy must allow that permission and the referenced actions.
4. Open the draft under **Releases**, download the installers, and perform the [native acceptance checks](acceptance.md). Check installation, first launch, microphone permission, Accessibility/Input Monitoring permissions on macOS, recording, insertion, and uninstall on each target architecture.
5. Edit the generated notes as needed and click **Publish release** when the installers are ready.

To retry an unpublished version, rerun the failed workflow, or choose **Actions → Release → Run workflow** and enter its existing tag, such as `v0.1.0`. Manual runs resolve that tag and build its commit, even if a different branch was selected in the Actions UI. A rerun can replace assets on an existing draft. Published releases are not overwritten; use a new version tag for changes. Keep the tag fixed once a build has started.

## Signing

No signing secrets are required to generate the installers. By default, Windows installers are unsigned and macOS apps receive an ad-hoc signature. Windows may show an unknown-publisher/SmartScreen prompt, and macOS may require approval in **System Settings → Privacy & Security** before first launch. Ad-hoc signing does not provide Apple notarization or a verified publisher. See [Tauri's macOS signing guide](https://v2.tauri.app/distribute/sign/macos/) and [Windows signing guide](https://v2.tauri.app/distribute/sign/windows/).

For Apple Developer ID signing and notarization, add all six repository Actions secrets:

| Secret                       | Value                                                                                 |
| ---------------------------- | ------------------------------------------------------------------------------------- |
| `APPLE_CERTIFICATE`          | Base64-encoded Developer ID Application `.p12` certificate, including its private key |
| `APPLE_CERTIFICATE_PASSWORD` | Password used to encrypt the exported `.p12`                                          |
| `APPLE_SIGNING_IDENTITY`     | Full certificate identity, such as `Developer ID Application: Your Name (TEAMID)`     |
| `APPLE_ID`                   | Apple account email used for notarization                                             |
| `APPLE_PASSWORD`             | An Apple **app-specific password**, not the account's login password                  |
| `APPLE_TEAM_ID`              | Apple Developer team ID                                                               |

The macOS build imports the certificate through Tauri, signs the app, submits it for notarization, and staples the result. A partially configured set of secrets fails the build. Keep all six unset to use ad-hoc signing. `src-tauri/Entitlements.plist` enables microphone access under the hardened runtime; the app also supplies its microphone usage description in `Info.plist`.

Windows publisher signing is not configured. It requires a Windows code-signing certificate/service and the corresponding Tauri signing configuration before tagging a signed release. Never commit certificates, passwords, or signing keys.

## Build installers locally

Install the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/), then run `npm ci`. Build Windows installers on Windows:

```sh
npm run package -- --ci --target x86_64-pc-windows-msvc --bundles nsis,msi -- --locked
```

On macOS, build for Apple Silicon with the `aarch64-apple-darwin` target:

```sh
rustup target add aarch64-apple-darwin
APPLE_SIGNING_IDENTITY=- npm run package -- --ci --target aarch64-apple-darwin --bundles dmg -- --locked
```

Bundles are written under the workspace root's `target/<target>/release/bundle/`, rather than `src-tauri/target/`. Replace the ad-hoc identity with the signing configuration above when preparing a notarized local build. Compilation and packaging do not establish native acceptance; test the actual installed app on each platform.
