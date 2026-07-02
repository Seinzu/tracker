# Tracker

A small macOS-friendly time tracker built with Rust, Tauri, and SQLite.

The first slice supports:

- timing free-form tasks
- optionally associating tasks with a GitHub issue or pull request
- searching GitHub issues and pull requests with an optional token
- assigning a subtask to a timer entry
- stopping the current timer from the app or tray
- storing task, subtask, and time entry data in SQLite
- a starter reporting view grouped by task and subtask

## Run

This app uses a static frontend, so Node/npm are not required.

```sh
cargo run --manifest-path src-tauri/Cargo.toml
```

The SQLite database is created in the app data directory as `tracker.sqlite3`.

GitHub search works without a token for public repositories. Add a token in the app when you need higher rate limits or access to private repositories; the token is stored in the OS credential store, which is macOS Keychain on a Mac, and is not written to SQLite.

## Release

The GitHub Actions workflow at `.github/workflows/build-macos-release.yml` builds macOS release assets for Apple Silicon and Intel Macs.

If Apple signing secrets are not configured, the workflow creates an ad-hoc signed macOS build. This is usable for personal/internal testing, but macOS Gatekeeper will still block it on first launch because it is not notarized. Use Finder's right-click `Open` action, or remove quarantine after installing:

```sh
xattr -dr com.apple.quarantine /Applications/Tracker.app
```

For a polished release that opens normally after download, add these repository secrets before cutting a release:

- `APPLE_CERTIFICATE`: base64 encoded `.p12` export of a Developer ID Application certificate
- `APPLE_CERTIFICATE_PASSWORD`: password used when exporting the `.p12`
- `APPLE_ID`: Apple ID email for notarization
- `APPLE_PASSWORD`: app-specific password for that Apple ID
- `APPLE_TEAM_ID`: Apple Developer Team ID
- `KEYCHAIN_PASSWORD`: temporary CI keychain password

Convert an exported `.p12` certificate for `APPLE_CERTIFICATE` with:

```sh
openssl base64 -A -in /path/to/certificate.p12 -out certificate-base64.txt
```

Create a tag to trigger a draft release:

```sh
git tag v0.1.0
git push origin v0.1.0
```

You can also run the workflow manually from GitHub Actions and provide a release tag.

## Notes

The reporting screen is intentionally simple at this stage. The backend already exposes summary rows and recent entries, so a richer reports UI can be added without changing the storage model.
